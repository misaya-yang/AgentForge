"""File-size fences shared by Knowledge upload routes and document parsers."""

from ...core.exceptions import ValidationFailedError
from .document_processor import (
    MAX_DOCX_SOURCE_BYTES,
    MAX_HTML_SOURCE_BYTES,
    MAX_PDF_SOURCE_BYTES,
    MAX_TEXT_SOURCE_BYTES,
)

_PARSER_LIMITS = {
    ".pdf": MAX_PDF_SOURCE_BYTES,
    ".docx": MAX_DOCX_SOURCE_BYTES,
    ".html": MAX_HTML_SOURCE_BYTES,
    ".txt": MAX_TEXT_SOURCE_BYTES,
    ".md": MAX_TEXT_SOURCE_BYTES,
}


def require_parser_budget(filename: str, extension: str, size_bytes: int) -> None:
    """Reject a source the bounded parser cannot consume."""

    parser_limit = _PARSER_LIMITS[extension]
    if size_bytes > parser_limit:
        limit_mb = parser_limit // (1024 * 1024)
        raise ValidationFailedError(
            f"{filename} exceeds the {limit_mb}MB parser limit for {extension} files"
        )
