"""Contract tests for the Rust-to-Gateway Office artifact boundary."""

from __future__ import annotations

import base64
import hashlib

import pytest
from pydantic import ValidationError

from src.api.internal.office_artifacts import OfficeArtifactRequest, _receipt


def _payload(content: bytes = b"office") -> dict[str, object]:
    return {
        "tool_call_id": "call-1",
        "arguments_hash": "sha256:" + "a" * 64,
        "artifact_type": "document",
        "format": "docx",
        "title": "Report",
        "filename": "report.docx",
        "mime_type": (
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        ),
        "sha256": hashlib.sha256(content).hexdigest(),
        "size_bytes": len(content),
        "content_base64": base64.b64encode(content).decode(),
        "metadata": {"schema_version": "ai-platform/office-artifact/v1"},
    }


def test_request_validates_bounded_bytes_and_hash() -> None:
    request = OfficeArtifactRequest.model_validate(_payload())
    assert request.size_bytes == 6


@pytest.mark.parametrize(
    "field, value",
    [
        ("content_base64", "not-base64"),
        ("size_bytes", 7),
        ("sha256", "sha256:" + "b" * 64),
    ],
)
def test_request_rejects_tampered_content(field: str, value: object) -> None:
    payload = _payload()
    payload[field] = value
    with pytest.raises(ValidationError):
        OfficeArtifactRequest.model_validate(payload)


def test_request_rejects_unknown_fields() -> None:
    payload = _payload()
    payload["storage_key"] = "must-not-cross-boundary"
    with pytest.raises(ValidationError):
        OfficeArtifactRequest.model_validate(payload)


def test_receipt_is_strict_and_replayable() -> None:
    artifact = {
        "artifact_id": "art_12345678",
        "download_path": "/api/v1/assistant/artifacts/art_12345678/download",
        "filename": "report.docx",
        "mime_type": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "size_bytes": 6,
        "sha256": hashlib.sha256(b"office").hexdigest(),
    }
    assert _receipt(
        {
            "schema_version": "ai-platform/durable-capability-receipt/v1",
            "capability_id": "mcp_docgen__generate_document",
            "result": {},
            "broker_response": artifact,
        }
    ) == {"artifact": artifact}
    assert _receipt({"schema_version": "wrong", "broker_response": artifact}) is None


def test_receipt_rejects_invalid_hash() -> None:
    artifact = {
        "artifact_id": "art_12345678",
        "download_path": "/download",
        "filename": "report.docx",
        "mime_type": "application/octet-stream",
        "size_bytes": 1,
        "sha256": "bad",
    }
    assert (
        _receipt(
            {
                "schema_version": "ai-platform/durable-capability-receipt/v1",
                "capability_id": "mcp_docgen__generate_document",
                "result": {},
                "broker_response": artifact,
            }
        )
        is None
    )
