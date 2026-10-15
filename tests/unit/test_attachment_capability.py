from __future__ import annotations

import io
import zipfile

import pytest

from src.api.internal.attachment_capabilities import _extract_content, _safe_zip_members


def test_text_attachment_is_bounded_and_truncates() -> None:
    metadata, content, truncated = _extract_content(b"hello world", "note.txt", "text/plain", 5)
    assert metadata["format"] == "txt"
    assert content == "hello"
    assert truncated is True


def test_zip_traversal_is_rejected() -> None:
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w") as archive:
        archive.writestr("../escape.xml", b"bad")
    with pytest.raises(ValueError, match="unsafe"):
        _safe_zip_members(stream.getvalue())


@pytest.mark.parametrize(
    ("filename", "mime", "member"),
    [
        ("report.docx", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", "word/document.xml"),
        ("table.xlsx", "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", "xl/worksheets/sheet1.xml"),
        ("slides.pptx", "application/vnd.openxmlformats-officedocument.presentationml.presentation", "ppt/slides/slide1.xml"),
    ],
)
def test_office_attachment_extracts_bounded_text(filename: str, mime: str, member: str) -> None:
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w") as archive:
        archive.writestr(member, b"<root><text>Safe office text</text></root>")
    metadata, content, truncated = _extract_content(stream.getvalue(), filename, mime, 100)
    assert metadata["format"] == filename.rsplit(".", 1)[-1]
    assert content == "Safe office text"
    assert truncated is False


def test_office_attachment_rejects_entity_payload() -> None:
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w") as archive:
        archive.writestr("word/document.xml", b'<!DOCTYPE x [<!ENTITY secret "leak">]><x>&secret;</x>')
    with pytest.raises(ValueError, match="declarations"):
        _extract_content(
            stream.getvalue(), "report.docx",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document", 100,
        )
