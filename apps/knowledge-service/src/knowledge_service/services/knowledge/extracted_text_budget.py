"""Single owner for extracted-text size limits and their validation helpers."""
from __future__ import annotations

from typing import Any

from ...core.exceptions import ValidationFailedError

MAX_EXTRACTED_TEXT_CHARS = 16_000_000
MAX_EXTRACTED_TEXT_BYTES = 48 * 1024 * 1024


def _require_extracted_text_counts_budget(total_chars: int, total_bytes: int) -> None:
    if total_chars > MAX_EXTRACTED_TEXT_CHARS:
        raise ValidationFailedError(
            f"extracted text exceeds the {MAX_EXTRACTED_TEXT_CHARS} character limit"
        )
    if total_bytes > MAX_EXTRACTED_TEXT_BYTES:
        raise ValidationFailedError(
            f"extracted text exceeds the {MAX_EXTRACTED_TEXT_BYTES} byte limit"
        )


def _require_extracted_text_budget(value: Any) -> str:
    text = str(value or "")
    _require_extracted_text_counts_budget(len(text), len(text.encode("utf-8")))
    return text


