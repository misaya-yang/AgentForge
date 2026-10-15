"""Canonical type-tagged values used by the reference-data fingerprint."""

from __future__ import annotations

import base64
import hashlib
import json
import math
from collections.abc import Iterable, Mapping
from datetime import date, datetime, time
from decimal import Decimal
from typing import Any
from uuid import UUID


class UnsupportedFingerprintValue(TypeError):
    pass


def canonical_digest(lines: Iterable[str]) -> str:
    """SHA-256 over newline-joined canonical lines."""
    return hashlib.sha256("\n".join(lines).encode("utf-8")).hexdigest()


def diff_line_lists(expected: list[str], actual: list[str]) -> list[str]:
    """Render drift between two canonical line lists."""
    expected_set = set(expected)
    actual_set = set(actual)
    drift = [f"- {line}" for line in sorted(expected_set - actual_set)]
    drift.extend(f"+ {line}" for line in sorted(actual_set - expected_set))
    return drift


def canonical_value_tree(value: Any) -> list[Any]:
    if value is None:
        return ["null"]
    if isinstance(value, bool):
        return ["bool", value]
    if isinstance(value, int):
        return ["int", str(value)]
    if isinstance(value, Decimal):
        if value.is_finite():
            normalized = format(value.normalize(), "f")
            if normalized == "-0":
                normalized = "0"
        else:
            normalized = str(value)
        return ["decimal", normalized]
    if isinstance(value, float):
        if math.isnan(value):
            rendered = "nan"
        elif math.isinf(value):
            rendered = "+inf" if value > 0 else "-inf"
        else:
            rendered = value.hex()
        return ["float", rendered]
    if isinstance(value, str):
        return ["str", value]
    if isinstance(value, (bytes, bytearray, memoryview)):
        return ["bytes", base64.b64encode(bytes(value)).decode("ascii")]
    if isinstance(value, datetime):
        return ["datetime", value.isoformat(timespec="microseconds")]
    if isinstance(value, date):
        return ["date", value.isoformat()]
    if isinstance(value, time):
        return ["time", value.isoformat(timespec="microseconds")]
    if isinstance(value, UUID):
        return ["uuid", str(value)]
    if isinstance(value, Mapping):
        items = [
            [canonical_value_tree(key), canonical_value_tree(item)] for key, item in value.items()
        ]
        items.sort(key=lambda pair: json.dumps(pair[0], ensure_ascii=True, separators=(",", ":")))
        return ["map", items]
    if isinstance(value, list):
        return ["list", [canonical_value_tree(item) for item in value]]
    if isinstance(value, tuple):
        return ["tuple", [canonical_value_tree(item) for item in value]]
    raise UnsupportedFingerprintValue(
        f"unsupported reference-data value type: "
        f"{type(value).__module__}.{type(value).__qualname__}"
    )
