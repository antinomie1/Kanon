"""Tests for tool schemas inferred from handler signatures and docstrings."""

import sys
import unittest
from pathlib import Path
from typing import Annotated, Any, Dict, List, Literal, Optional, Union

_PYTHON_SDK_DIR = Path(__file__).resolve().parents[1]
if str(_PYTHON_SDK_DIR) not in sys.path:
    sys.path.insert(0, str(_PYTHON_SDK_DIR))

from kanon_sdk.event import MessageEvent
from kanon_sdk.schema import infer_tool, restore_integers


class TestInferTool(unittest.TestCase):
    def test_signature_and_docstring_become_the_schema(self) -> None:
        async def weather(
            self,
            city: str,
            event: MessageEvent,
            days: int = 1,
            unit: Literal["c", "f"] = "c",
            tags: Optional[List[str]] = None,
            note: Annotated[str, "Free text for the forecaster"] = "",
        ) -> dict:
            """Current weather and forecast for a city.

            Longer explanation that is not part of the summary.

            Args:
                city: City name, e.g. "Paris".
                days: Days of forecast,
                    between 1 and 7.

            Returns:
                The forecast.
            """

        signature = infer_tool(weather)

        self.assertEqual(signature.description, "Current weather and forecast for a city.")
        self.assertTrue(signature.wants_event)
        self.assertEqual(signature.arguments, ["city", "days", "unit", "tags", "note"])
        self.assertEqual(
            signature.parameters,
            {
                "type": "object",
                "properties": {
                    "city": {"type": "string", "description": 'City name, e.g. "Paris".'},
                    "days": {
                        "type": "integer",
                        "description": "Days of forecast, between 1 and 7.",
                        "default": 1,
                    },
                    "unit": {"enum": ["c", "f"], "type": "string", "default": "c"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "note": {
                        "type": "string",
                        "description": "Free text for the forecaster",
                        "default": "",
                    },
                },
                "required": ["city"],
            },
        )

    def test_optional_without_default_is_not_required(self) -> None:
        def lookup(key: Optional[str], options: Dict[str, int], extra: Any) -> None:
            """Looks something up."""

        parameters = infer_tool(lookup).parameters
        self.assertEqual(parameters["required"], ["options", "extra"])
        self.assertEqual(
            parameters["properties"]["options"],
            {"type": "object", "additionalProperties": {"type": "integer"}},
        )
        self.assertEqual(parameters["properties"]["extra"], {})

    def test_what_cannot_be_described_is_refused_by_name(self) -> None:
        def unannotated(city) -> None: ...

        def varargs(*cities: str) -> None: ...

        def unsupported(when: complex) -> None: ...

        def positional(city: str, /) -> None: ...

        for handler, needle in [
            (unannotated, "'city'"), (varargs, "*cities"), (unsupported, "complex"),
            (positional, "positional-only"),
        ]:
            with self.subTest(handler=handler.__name__):
                with self.assertRaises(TypeError) as raised:
                    infer_tool(handler)
                self.assertIn(needle, str(raised.exception))


class TestRestoreIntegers(unittest.TestCase):
    def test_struct_doubles_become_int_only_where_the_schema_says_integer(self) -> None:
        def plan(
            days: int,
            ratio: float,
            counts: Dict[str, List[int]],
            either: Union[int, str],
            maybe: Optional[int] = None,
        ) -> None: ...

        schema = infer_tool(plan).parameters
        restored = restore_integers(
            {"days": 3.0, "ratio": 2.0, "counts": {"a": [1.0, 2.0]}, "either": 4.0, "maybe": 1.5},
            schema,
        )
        self.assertEqual(
            restored,
            {"days": 3, "ratio": 2.0, "counts": {"a": [1, 2]}, "either": 4, "maybe": 1.5},
        )
        self.assertIsInstance(restored["days"], int)
        self.assertIsInstance(restored["ratio"], float)
        self.assertIsInstance(restored["counts"]["a"][0], int)


if __name__ == "__main__":
    unittest.main()
