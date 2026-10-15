"""Safe response headers for assistant artifacts."""

from urllib.parse import quote


def attachment_content_disposition(filename: str) -> str:
    """Build an attachment header without allowing filename header injection."""

    encoded = quote(filename, safe="")
    if encoded == filename:
        return f'attachment; filename="{filename}"'
    return f"attachment; filename*=UTF-8''{encoded}"


__all__ = ["attachment_content_disposition"]
