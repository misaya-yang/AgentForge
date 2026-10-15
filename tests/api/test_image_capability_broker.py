"""Contract tests for the private Rust Runtime image broker."""

from __future__ import annotations

from types import SimpleNamespace

import pytest
from ai_gateway_contracts.capability_proof import canonical_body_hash

from src.api.internal.image_capabilities import (
    ImageGenerationEnvelope,
    _decode_image,
    _persist_images,
)


def test_envelope_rejects_arguments_hash_mismatch() -> None:
    with pytest.raises(ValueError, match="arguments hash mismatch"):
        ImageGenerationEnvelope.model_validate(
            {
                "arguments": {"prompt": "draw a cat"},
                "arguments_hash": "sha256:" + "0" * 64,
            }
        )


def test_image_response_is_bounded_and_decoded() -> None:
    content, mime_type = _decode_image(
        {"content_base64": "iVBORw0KGgo=", "mime_type": "image/png"}
    )
    assert content == b"\x89PNG\r\n\x1a\n"
    assert mime_type == "image/png"
    with pytest.raises(ValueError, match="invalid"):
        _decode_image({"content_base64": "not base64", "mime_type": "image/png"})


@pytest.mark.asyncio
async def test_persistence_returns_metadata_without_bytes_or_urls() -> None:
    class FakeStorage:
        def __init__(self) -> None:
            self.calls: list[dict] = []

        async def create_artifact(self, **fields):
            self.calls.append(fields)
            return SimpleNamespace(artifact_id="art_image_1", filename=fields["filename"])

    storage = FakeStorage()
    metadata = await _persist_images(
        storage=storage,
        images=[{"content_base64": "iVBORw0KGgo=", "mime_type": "image/png"}],
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        execution_id="execution-a",
        prompt="draw a cat",
        provider="configured",
    )
    assert metadata == [
        {
            "artifact_id": "art_image_1",
            "kind": "image",
            "mime_type": "image/png",
            "filename": "generated_execution-a_1.png",
            "size_bytes": 8,
        }
    ]
    assert "content" in storage.calls[0]
    assert storage.calls[0]["artifact_id"].startswith("art_")
    assert len(storage.calls[0]["artifact_id"]) == 20
    assert storage.calls[0]["metadata"]["content_sha256"] == (
        "4c4b6a3be1314ab86138bef4314dde022e600960d8689a2c8f8631802d20dab6"
    )
    assert "content_base64" not in metadata[0]
    assert "download_url" not in metadata[0]


def test_hash_fixture_matches_rust_canonical_body() -> None:
    arguments = {
        "prompt": "draw a cat",
        "negative_prompt": "",
        "size": "1536*1536",
        "style": "<auto>",
        "n": 1,
    }
    envelope = ImageGenerationEnvelope(
        arguments=arguments,
        arguments_hash=f"sha256:{canonical_body_hash(arguments)}",
    )
    assert envelope.arguments.prompt == "draw a cat"
