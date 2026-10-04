#!/usr/bin/env python3
"""Split regenerated wire bindings into stable modules without changing their wire contracts.

Run with --milky <milkygen.rs> or --python <plugin_pb2_grpc.py> after generation.
Inputs must be the unsplit generator output; all destinations are relative to this repository.
"""

import argparse
from pathlib import Path
import re

ROOT = Path(__file__).resolve().parent.parent


def item_start(source: str, needle: str) -> int:
    """Keep the documentation and derives attached to their generated item."""
    offset = source.index(needle)
    offset = source.rfind("\n", 0, offset) + 1
    while offset:
        previous = source.rfind("\n", 0, offset - 1) + 1
        if not source[previous:offset].strip().startswith(("///", "#[")):
            break
        offset = previous
    return offset


def split_milky(source: str) -> None:
    """Keep generated definitions grouped by protocol responsibility."""
    directory = ROOT / "crates/kanon-adapter-milky/src/protocol/generated"
    directory.mkdir(exist_ok=True)
    sections = [
        ("events", item_start(source, "pub enum Event {")),
        ("entities", item_start(source, "pub struct FriendEntity {")),
        ("segments", item_start(source, "pub enum IncomingSegment {")),
        ("system_api", source.index("// ---- System APIs ----")),
        ("message_api", source.index("// ---- Message APIs ----")),
        ("group_api", source.index("// ---- Group APIs ----")),
        ("file_api", source.index("// ---- File APIs ----")),
        ("serde_helpers", source.index("fn serialize_segment_with_data")),
        ("endpoints", source.index("pub trait ApiEndpoint")),
    ]
    header = source[:sections[0][1]]
    header += "\n// Split by tools/split-protocol-bindings.py; wire definitions remain generated.\n"
    for index, (name, offset) in enumerate(sections):
        end = sections[index + 1][1] if index + 1 < len(sections) else len(source)
        body = source[offset:end]
        if name == "serde_helpers":
            body = re.sub(r"^fn ", "pub(super) fn ", body, flags=re.M)
        (directory / f"{name}.rs").write_text(
            f"//! Generated Milky {name.replace('_', ' ')}.\n\nuse super::*;\n\n" + body
        )
        header += f"mod {name};\n"
        header += f"{'use' if name == 'serde_helpers' else 'pub use'} {name}::*;\n"
    (directory / "mod.rs").write_text(header)


def split_python(source: str) -> None:
    """Keep each gRPC service separate and retain the conventional import facade."""
    directory = ROOT / "sdks/python/kanon_sdk/proto/_grpc"
    directory.mkdir(exist_ok=True)
    (directory / "__init__.py").write_text('"""Generated gRPC service implementations."""\n')
    boundaries = [
        ("host", "class PluginHostServiceStub:"),
        ("pipeline", "class MessagePipelineServiceStub:"),
        ("bot_api", "class BotApiServiceStub:"),
        ("bot_experimental", "class BotApiService:"),
    ]
    offsets = [(name, source.index(marker)) for name, marker in boundaries]
    header = source[:offsets[0][1]]
    for index, (name, offset) in enumerate(offsets):
        end = offsets[index + 1][1] if index + 1 < len(offsets) else len(source)
        body = source[offset:end]
        (directory / f"{name}.py").write_text(
            '# Generated service slice; see tools/split-protocol-bindings.py.\n'
            f'"""Generated {name.replace("_", " ")} bindings."""\n'
            'import grpc\nfrom .. import plugin_pb2 as plugin__pb2\n\n' + body.rstrip() + '\n'
        )
        names = re.findall(r"^(?:class|def) (\w+)", body, flags=re.M)
        header += f"from ._grpc.{name} import (\n"
        header += "".join(f"    {symbol} as {symbol},\n" for symbol in names)
        header += ")\n"
    (directory.parent / "plugin_pb2_grpc.py").write_text(header)


def main() -> None:
    """Validate the requested generator input before rewriting the relevant bindings."""
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--milky", type=Path)
    group.add_argument("--python", type=Path)
    arguments = parser.parse_args()
    if arguments.milky:
        split_milky(arguments.milky.read_text())
    else:
        split_python(arguments.python.read_text())


if __name__ == "__main__":
    main()
