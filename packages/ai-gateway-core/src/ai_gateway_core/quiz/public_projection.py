"""Answer-free projection of quiz options for pre-submission responses."""

from __future__ import annotations

import json
from typing import Any


def safe_quiz_options(raw: Any) -> list[dict[str, str]]:
    """Keep only the visible option label and text, including for legacy rows."""
    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except (TypeError, ValueError):
            return []
    if not isinstance(raw, list):
        return []
    return [
        {"label": item["label"], "text": item["text"]}
        for item in raw
        if isinstance(item, dict)
        and isinstance(item.get("label"), str)
        and isinstance(item.get("text"), str)
    ]
