"""JSON Schema for LLM tools, inferred from a handler's signature and docstring.

A tool declared without ``parameters=`` describes itself::

    @tool
    async def weather(self, city: str, days: int = 1, unit: Literal["c", "f"] = "c") -> dict:
        \"\"\"Current weather and forecast for a city.

        Args:
            city: City name, e.g. "Paris".
            days: Days of forecast, 1-7.
        \"\"\"

becomes a tool named ``weather`` described by the docstring's first paragraph, whose parameters
are ``city`` (required string), ``days`` (integer, default 1) and ``unit`` (``"c"`` or ``"f"``).
The model's arguments are then passed as keyword arguments. Descriptions come from the
docstring's ``Args:`` section or from ``Annotated[str, "description"]``.

Inference is strict on purpose: a parameter without an annotation, or with a type JSON Schema
cannot express, raises ``TypeError`` when the class is defined, naming the parameter — a schema
that silently says "anything" would let the model guess.
"""

import enum
import inspect
import re
import types
import typing
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, List, Literal, Optional, Tuple, Union

#: Parameter names that receive the chat message instead of a model argument.
EVENT_PARAMETER = "event"

_PRIMITIVES = {str: "string", int: "integer", float: "number", bool: "boolean"}

#: Docstring headers that open the parameter section (Google and NumPy-ish spellings).
_ARGS_HEADERS = {"args:", "arguments:", "parameters:", "params:"}


@dataclass
class ToolSignature:
    """What a tool handler's signature says about the tool.

    Attributes:
        description: First paragraph of the docstring; ``""`` without one.
        parameters: JSON Schema of the model's arguments (an object schema).
        arguments: Names passed as keyword arguments, in signature order.
        wants_event: Whether the handler takes an ``event`` parameter.
        optional_without_default: Omitted nullable parameters that must receive ``None``.
    """

    description: str
    parameters: Dict[str, Any]
    arguments: List[str] = field(default_factory=list)
    wants_event: bool = False
    optional_without_default: List[str] = field(default_factory=list)


def infer_tool(handler: Callable) -> ToolSignature:
    """Builds a tool's description and parameter schema from ``handler``.

    A leading ``self``/``cls`` is skipped, so this works on plain functions, on functions
    decorated inside a class body, and on bound methods alike.

    Raises:
        TypeError: For ``*args``/``**kwargs``, a missing annotation, or an unsupported type.
    """
    function = inspect.unwrap(getattr(handler, "__func__", handler))
    signature = inspect.signature(function)
    hints = typing.get_type_hints(function, include_extras=True)
    summary, documented = _parse_docstring(inspect.getdoc(function) or "")

    properties: Dict[str, Any] = {}
    required: List[str] = []
    arguments: List[str] = []
    wants_event = False
    optional_without_default: List[str] = []
    for index, (name, parameter) in enumerate(signature.parameters.items()):
        if index == 0 and name in ("self", "cls"):
            continue
        if parameter.kind is parameter.POSITIONAL_ONLY:
            raise TypeError(
                f"tool {function.__name__}: positional-only parameter {name!r} cannot be passed by keyword"
            )
        if name == EVENT_PARAMETER:
            wants_event = True
            continue
        if parameter.kind in (parameter.VAR_POSITIONAL, parameter.VAR_KEYWORD):
            raise TypeError(
                f"tool {function.__name__}: *{name} cannot be described to the model; "
                "declare each argument, or pass parameters= explicitly"
            )
        if name not in hints:
            raise TypeError(
                f"tool {function.__name__}: parameter {name!r} needs a type annotation "
                "(or pass parameters= explicitly)"
            )
        schema, optional = _schema_of(hints[name], f"{function.__name__}({name})")
        description = schema.pop("description", None) or documented.get(name)
        if description:
            schema["description"] = description
        has_default = parameter.default is not inspect.Parameter.empty
        if has_default and _is_json_scalar(parameter.default) and parameter.default is not None:
            schema["default"] = parameter.default
        if not has_default:
            if optional:
                optional_without_default.append(name)
            else:
                required.append(name)
        properties[name] = schema
        arguments.append(name)

    parameters: Dict[str, Any] = {"type": "object", "properties": properties}
    if required:
        parameters["required"] = required
    return ToolSignature(
        description=summary,
        parameters=parameters,
        arguments=arguments,
        wants_event=wants_event,
        optional_without_default=optional_without_default,
    )


def _schema_of(annotation: Any, where: str) -> Tuple[Dict[str, Any], bool]:
    """JSON Schema for one annotation, and whether ``None`` is allowed (making it optional)."""
    origin = typing.get_origin(annotation)
    args = typing.get_args(annotation)

    if origin is typing.Annotated:
        schema, optional = _schema_of(args[0], where)
        text = next((meta for meta in args[1:] if isinstance(meta, str)), None)
        if text:
            schema["description"] = text
        return schema, optional

    if origin is Union or (hasattr(types, "UnionType") and origin is types.UnionType):
        members = [member for member in args if member is not type(None)]
        optional = len(members) != len(args)
        if len(members) == 1:
            schema, _ = _schema_of(members[0], where)
            return schema, optional
        return {"anyOf": [_schema_of(member, where)[0] for member in members]}, optional

    if annotation is Any:
        return {}, False
    if annotation in _PRIMITIVES:
        return {"type": _PRIMITIVES[annotation]}, False

    if origin is Literal:
        values = list(args)
        schema: Dict[str, Any] = {"enum": values}
        kinds = {_PRIMITIVES.get(type(value)) for value in values}
        if len(kinds) == 1 and None not in kinds:
            schema["type"] = kinds.pop()
        return schema, False

    if inspect.isclass(annotation) and issubclass(annotation, enum.Enum):
        raise TypeError(
            f"tool {where}: Enum types are not converted back from the model's value; "
            "use Literal[...] instead"
        )

    if annotation in (list, tuple) or origin in (list, tuple) or (
        origin is not None and _is_abc(origin, "Sequence")
    ):
        schema = {"type": "array"}
        item_args = [arg for arg in args if arg is not Ellipsis]
        if len(item_args) == 1:
            schema["items"] = _schema_of(item_args[0], where)[0]
        elif len(item_args) > 1:
            raise TypeError(f"tool {where}: fixed-length tuples are not supported; use list[...]")
        return schema, False

    if annotation is dict or origin is dict or (origin is not None and _is_abc(origin, "Mapping")):
        schema = {"type": "object"}
        if len(args) == 2 and args[1] is not Any:
            schema["additionalProperties"] = _schema_of(args[1], where)[0]
        return schema, False

    raise TypeError(
        f"tool {where}: cannot describe {annotation!r} to the model; use str, int, float, bool, "
        "Literal, list, dict, Optional or Annotated, or pass parameters= explicitly"
    )


def restore_integers(value: Any, schema: Optional[Dict[str, Any]]) -> Any:
    """Turns whole-number floats back into ``int`` wherever ``schema`` declares an integer.

    Tool arguments travel as ``google.protobuf.Struct``, whose only number type is a double, so
    ``{"days": 3}`` from the model arrives as ``3.0``. Walking the tool's own schema restores what
    the handler declared (``days: int`` gets ``3``) without guessing for ``number`` fields, which
    keep their float. Values the schema does not describe are left as they are.
    """
    if not isinstance(schema, dict):
        return value
    branches = [schema, *[b for b in schema.get("anyOf", []) if isinstance(b, dict)]]
    if isinstance(value, float) and value.is_integer():
        if any(_declares(branch, "integer") for branch in branches):
            return int(value)
        return value
    if isinstance(value, list):
        branch = next((b for b in branches if _declares(b, "array")), None)
        if branch is None:
            return value
        return [restore_integers(item, branch.get("items")) for item in value]
    if isinstance(value, dict):
        branch = next((b for b in branches if _declares(b, "object")), None)
        if branch is None:
            return value
        properties = branch.get("properties") or {}
        extra = branch.get("additionalProperties")
        return {
            key: restore_integers(item, properties.get(key, extra))
            for key, item in value.items()
        }
    return value


def _declares(schema: Dict[str, Any], kind: str) -> bool:
    """Whether ``schema``'s ``type`` (a name or a list of names) includes ``kind``."""
    declared = schema.get("type")
    return declared == kind or (isinstance(declared, list) and kind in declared)


def _is_abc(origin: Any, name: str) -> bool:
    """Whether ``origin`` is ``collections.abc.<name>``."""
    import collections.abc

    return origin is getattr(collections.abc, name)


def _is_json_scalar(value: Any) -> bool:
    return value is None or isinstance(value, (str, int, float, bool))


def _parse_docstring(doc: str) -> Tuple[str, Dict[str, str]]:
    """Splits a docstring into its first paragraph and its ``Args:`` descriptions.

    Inside the section, a line at the entries' indentation starts an entry (``name: text`` or
    ``name (type): text``); deeper lines continue the previous one. A header back at the left
    margin (``Returns:``) ends the section.
    """
    lines = doc.splitlines()
    summary_lines: List[str] = []
    for line in lines:
        if not line.strip() or line.strip().lower() in _ARGS_HEADERS:
            break
        summary_lines.append(line.strip())

    documented: Dict[str, str] = {}
    entry = re.compile(r"^(\*{0,2}\w+)\s*(?:\([^)]*\))?\s*:\s*(.*)$")
    in_args = False
    entry_indent: Optional[int] = None
    current: Optional[str] = None
    for line in lines:
        stripped = line.strip()
        if not in_args:
            in_args = stripped.lower() in _ARGS_HEADERS
            continue
        if not stripped:
            continue
        indent = len(line) - len(line.lstrip())
        if indent == 0:
            break
        if entry_indent is None:
            entry_indent = indent
        match = entry.match(stripped)
        if indent == entry_indent and match:
            current = match.group(1).lstrip("*")
            documented[current] = match.group(2).strip()
        elif indent > entry_indent and current is not None:
            documented[current] = f"{documented[current]} {stripped}".strip()
    return " ".join(summary_lines), documented
