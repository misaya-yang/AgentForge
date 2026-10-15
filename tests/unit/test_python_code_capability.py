from __future__ import annotations

import base64
import hashlib

import pytest
from pydantic import ValidationError

from src.api.internal.python_code_capabilities import (
    PythonArtifactRequest,
    PythonCodeCapability,
    SandboxBrokerError,
    _arguments_hash,
    _python_artifact_id,
)


class FakeTransport:
    def __init__(self, result):
        self.result = result
        self.payload = None

    async def execute(self, payload):
        self.payload = payload
        return self.result


class FakeArtifacts:
    def __init__(self):
        self.items = []

    async def put_code_output(self, **item):
        self.items.append(item)
        return {"artifact_id": "artifact-1", "filename": item["filename"]}


def request(code="print(2)", inputs=None):
    inputs = inputs or []
    return {
        "lease": {"tenant_id": "t1", "user_id": "u1", "session_id": "s1"},
        "code": code,
        "inputs": inputs,
        "arguments_hash": _arguments_hash(code, inputs),
    }


@pytest.mark.asyncio
async def test_broker_persists_outputs_and_keeps_scope():
    content = base64.b64encode(b"png").decode()
    transport = FakeTransport({
        "execution_id": "exec-1", "status": "succeeded", "side_effect_state": "known",
        "output_files": [{"filename": "plot.png", "mime_type": "image/png", "content_base64": content, "sha256": "8f8cbb7dcf46e0bc7d53265749a6c17d116093a6ba95e442764060c76fd4a86c", "size_bytes": 3}],
    })
    artifacts = FakeArtifacts()
    result = await PythonCodeCapability(transport, artifacts).execute(request())
    assert result["artifacts"][0]["artifact_id"] == "artifact-1"
    assert artifacts.items[0]["tenant_id"] == "t1"
    assert transport.payload["lease"]["session_id"] == "s1"


@pytest.mark.asyncio
async def test_arguments_hash_mismatch_is_rejected_before_transport():
    transport = FakeTransport({})
    with pytest.raises(SandboxBrokerError, match="arguments hash"):
        await PythonCodeCapability(transport, FakeArtifacts()).execute({**request(), "arguments_hash": "sha256:wrong"})
    assert transport.payload is None


@pytest.mark.asyncio
async def test_unknown_sandbox_outcome_is_not_retried_or_persisted():
    transport = FakeTransport({"execution_id": "exec-1", "side_effect_state": "side_effect_unknown"})
    with pytest.raises(SandboxBrokerError, match="unknown"):
        await PythonCodeCapability(transport, FakeArtifacts()).execute(request())


def _artifact_payload(content: bytes = b"plot") -> dict[str, object]:
    return {
        "tool_call_id": "call-1",
        "arguments_hash": "sha256:" + "a" * 64,
        "filename": "plot.png",
        "mime_type": "image/png",
        "sha256": hashlib.sha256(content).hexdigest(),
        "size_bytes": len(content),
        "content_base64": base64.b64encode(content).decode(),
    }


def test_python_artifact_request_is_exact_and_accepts_null_mime() -> None:
    payload = _artifact_payload()
    request = PythonArtifactRequest.model_validate(payload)
    assert request.mime_type == "image/png"

    null_mime = {**payload, "mime_type": None}
    assert PythonArtifactRequest.model_validate(null_mime).mime_type is None

    with pytest.raises(ValidationError):
        PythonArtifactRequest.model_validate({k: v for k, v in payload.items() if k != "mime_type"})
    with pytest.raises(ValidationError):
        PythonArtifactRequest.model_validate({**payload, "storage_key": "must-not-cross-boundary"})
    with pytest.raises(ValidationError):
        PythonArtifactRequest.model_validate(_artifact_payload(b""))


def test_python_artifact_id_binds_the_complete_execution_identity() -> None:
    digest = hashlib.sha256(b"plot").hexdigest()
    artifact_id = _python_artifact_id(
        execution_id="c39884a5-e8f5-466d-8a74-12141ea08a69",
        tool_call_id="call-1",
        arguments_hash="sha256:" + "a" * 64,
        filename="plot.png",
        sha256=digest,
    )
    assert artifact_id.startswith("art_") and len(artifact_id) == 20
    assert artifact_id != _python_artifact_id(
        execution_id="c39884a5-e8f5-466d-8a74-12141ea08a69",
        tool_call_id="call-2",
        arguments_hash="sha256:" + "a" * 64,
        filename="plot.png",
        sha256=digest,
    )
