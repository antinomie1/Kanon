"""Protobuf and gRPC definitions for Kanon plugin protocol."""

import math
import os
import sys
from typing import Any

from google.protobuf.json_format import ParseDict
from google.protobuf.message import Message

# Ensure proto directory is in sys.path for generated pb2 relative imports
_proto_dir = os.path.dirname(__file__)
if _proto_dir not in sys.path:
    sys.path.insert(0, _proto_dir)

from . import plugin_pb2 as pb
from . import plugin_pb2_grpc as pb_grpc


def _check_finite_numbers(value: Any) -> None:
    """Validate JSON numbers once at the shared protobuf conversion boundary."""
    if isinstance(value, float) and not math.isfinite(value):
        raise ValueError("Struct numbers must be finite")
    if isinstance(value, dict):
        for item in value.values():
            _check_finite_numbers(item)
    elif isinstance(value, (list, tuple)):
        for item in value:
            _check_finite_numbers(item)


def parse_dict(value: Any, message: Message) -> Message:
    """Parse protobuf JSON without admitting non-finite Struct numbers the core would lose.

    Protobuf's binary double accepts NaN and infinities, but their JSON representation does not.
    Validate without a JSON serialization round trip before delegating the wire mapping.
    """
    _check_finite_numbers(value)
    return ParseDict(value, message)


__all__ = ["pb", "pb_grpc", "parse_dict"]
