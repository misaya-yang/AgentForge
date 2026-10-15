"""Private Gateway boundary for Python code execution.

The Gateway brokers a separately managed sandbox service; it never evaluates
the submitted source itself.  The worker supplies a signed runtime lease and
the sandbox service owns process isolation, cgroups, no-network policy and
output collection.  If the broker response is lost, callers must reconcile
the execution as ``side_effect_unknown`` rather than retrying.
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import hmac
import json
import os
import re
import uuid
from dataclasses import dataclass
from typing import Any, Protocol

import httpx
from ai_gateway_contracts.capability_proof import CapabilityProofError, verify_capability_proof
from ai_gateway_core.storage import get_artifact_storage
from fastapi import APIRouter, Body, Header, HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator

router = APIRouter(prefix="/internal/v2/agent-capabilities", tags=["internal-agent-capabilities"])

MAX_CODE_BYTES = 2_000_000
MAX_INPUT_BYTES = 24 * 1024 * 1024
MAX_OUTPUT_BYTES = 24 * 1024 * 1024
MAX_STREAM_BYTES = 2_000_000
MAX_OUTPUT_FILES = 64
MAX_TIMEOUT_SECONDS = 300
_SHA256 = re.compile(r"^[0-9a-f]{64}$")
_ARGUMENTS_HASH = re.compile(r"^sha256:[0-9a-f]{64}$")
_MAX_FILENAME_BYTES = 255
_PYTHON_ARTIFACT_PATH = "/internal/v2/agent-capabilities/python/artifacts"
_PYTHON_ARTIFACT_MAX_B64 = ((MAX_OUTPUT_BYTES + 2) // 3) * 4 + 4
_MIME = re.compile(
    r"^[A-Za-z0-9][A-Za-z0-9!#$&^_.+-]*/[A-Za-z0-9][A-Za-z0-9!#$&^_.+-]*$"
)


class SandboxBrokerError(RuntimeError):
    """A sandbox request could not be completed or verified."""


@dataclass(frozen=True)
class PythonSandboxLimits:
    timeout_seconds: int = 30
    memory_bytes: int = 512 * 1024 * 1024
    cpu_millis: int = 500
    pids: int = 32
    stdout_bytes: int = MAX_STREAM_BYTES
    stderr_bytes: int = MAX_STREAM_BYTES
    output_bytes: int = MAX_OUTPUT_BYTES
    output_files: int = MAX_OUTPUT_FILES

    def validate(self) -> None:
        if not 1 <= self.timeout_seconds <= MAX_TIMEOUT_SECONDS:
            raise SandboxBrokerError("invalid sandbox timeout")
        if min(self.memory_bytes, self.cpu_millis, self.pids) <= 0:
            raise SandboxBrokerError("invalid sandbox resource limit")
        if self.stdout_bytes > MAX_STREAM_BYTES or self.stderr_bytes > MAX_STREAM_BYTES:
            raise SandboxBrokerError("stream limit exceeds host bound")
        if self.output_bytes <= 0 or self.output_bytes > MAX_OUTPUT_BYTES or self.output_files <= 0 or self.output_files > MAX_OUTPUT_FILES:
            raise SandboxBrokerError("output limit exceeds host bound")


class SandboxTransport(Protocol):
    async def execute(self, payload: dict[str, Any]) -> dict[str, Any]: ...


class HttpSandboxTransport:
    """One-shot transport to a dedicated sandbox service.

    No Docker SDK, subprocess, provider credential, or local execution fallback
    is present here by design.
    """

    def __init__(self, base_url: str, internal_token: str, client: httpx.AsyncClient | None = None):
        parsed = httpx.URL(base_url)
        if parsed.scheme not in {"http", "https"} or not parsed.host or parsed.username or parsed.password:
            raise ValueError("sandbox broker URL must be an http(s) origin")
        if len(internal_token) < 32:
            raise ValueError("sandbox broker token is too short")
        self._url = parsed.copy_with(path="/internal/v2/sandbox/python/execute", query=None, fragment=None)
        self._token = internal_token
        self._client = client or httpx.AsyncClient()

    async def execute(self, payload: dict[str, Any]) -> dict[str, Any]:
        try:
            response = await self._client.post(
                self._url,
                headers={"x-ai-platform-internal-token": self._token},
                json=payload,
                timeout=float(payload["limits"]["timeout_seconds"]) + 10,
            )
        except httpx.TimeoutException as exc:
            raise SandboxBrokerError("sandbox outcome is unknown") from exc
        except httpx.HTTPError as exc:
            raise SandboxBrokerError("sandbox is unavailable") from exc
        if response.status_code >= 500:
            raise SandboxBrokerError("sandbox outcome is unknown")
        if response.status_code >= 400:
            raise SandboxBrokerError("sandbox rejected execution")
        try:
            result = response.json()
        except ValueError as exc:
            raise SandboxBrokerError("sandbox returned malformed result") from exc
        if not isinstance(result, dict):
            raise SandboxBrokerError("sandbox returned malformed result")
        return result


class ArtifactSink(Protocol):
    async def put_code_output(
        self, *, tenant_id: str, user_id: str, session_id: str, execution_id: str,
        filename: str, mime_type: str | None, content_base64: str, sha256: str, size_bytes: int,
    ) -> dict[str, Any]: ...


def _arguments_hash(code: str, inputs: list[dict[str, Any]]) -> str:
    body = json.dumps({"code": code, "inputs": inputs}, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
    return "sha256:" + hashlib.sha256(body.encode()).hexdigest()


def _validate_filename(filename: Any) -> str:
    if not isinstance(filename, str) or not filename or len(filename.encode()) > _MAX_FILENAME_BYTES:
        raise SandboxBrokerError("artifact filename is invalid")
    if filename in {".", ".."} or "/" in filename or "\\" in filename:
        raise SandboxBrokerError("artifact filename is invalid")
    if any(ord(char) < 32 for char in filename):
        raise SandboxBrokerError("artifact filename is invalid")
    return filename


class PythonArtifactRequest(BaseModel):
    """One code-execution output artifact supplied by the isolated worker."""

    model_config = ConfigDict(extra="forbid")

    tool_call_id: str = Field(min_length=1, max_length=160)
    arguments_hash: str = Field(pattern=r"^sha256:[0-9a-f]{64}$")
    filename: str = Field(min_length=1, max_length=_MAX_FILENAME_BYTES)
    mime_type: str | None = Field(..., max_length=128)
    sha256: str = Field(pattern=r"^[0-9a-f]{64}$")
    size_bytes: int = Field(ge=1, le=MAX_OUTPUT_BYTES)
    content_base64: str = Field(min_length=1, max_length=_PYTHON_ARTIFACT_MAX_B64)

    @field_validator("filename", "tool_call_id")
    @classmethod
    def validate_name(cls, value: str) -> str:
        try:
            return _validate_filename(value)
        except SandboxBrokerError as exc:
            raise ValueError("invalid artifact field") from exc

    @field_validator("mime_type")
    @classmethod
    def validate_mime_type(cls, value: str | None) -> str | None:
        if value is not None and not _MIME.fullmatch(value):
            raise ValueError("mime type invalid")
        return value.lower() if value else None

    @model_validator(mode="after")
    def validate_content(self) -> PythonArtifactRequest:
        try:
            content = base64.b64decode(self.content_base64, validate=True)
        except (ValueError, binascii.Error) as exc:
            raise ValueError("content_base64 invalid") from exc
        if len(content) != self.size_bytes:
            raise ValueError("content size mismatch")
        if not hmac.compare_digest(hashlib.sha256(content).hexdigest(), self.sha256):
            raise ValueError("content hash mismatch")
        return self


def _capability_scope(value: str | None, *, name: str) -> str:
    if not value or len(value) > 255 or any(ord(char) < 32 for char in value):
        raise HTTPException(status_code=403, detail=f"{name} is invalid")
    return value


def _python_artifact_id(
    *,
    execution_id: str,
    tool_call_id: str,
    arguments_hash: str,
    filename: str,
    sha256: str,
) -> str:
    """Stable id for one output; content and filename changes must not replay."""

    return "art_" + hashlib.sha256(
        (
            "python-output\0"
            + execution_id
            + "\0"
            + tool_call_id
            + "\0"
            + arguments_hash
            + "\0"
            + filename
            + "\0"
            + sha256
        ).encode("utf-8")
    ).hexdigest()[:16]


def _validate_input(item: Any) -> dict[str, Any]:
    if not isinstance(item, dict):
        raise SandboxBrokerError("input attachment is malformed")
    filename = _validate_filename(item.get("filename"))
    try:
        size = int(item["size_bytes"])
        raw = base64.b64decode(str(item["content_base64"]), validate=True)
    except (KeyError, TypeError, ValueError, binascii.Error) as exc:
        raise SandboxBrokerError("input attachment is malformed") from exc
    if size < 0 or len(raw) != size:
        raise SandboxBrokerError("input attachment size mismatch")
    return {**item, "filename": filename, "size_bytes": size}


class PythonCodeCapability:
    """Validate, broker and persist a single idempotent code execution."""

    def __init__(self, transport: SandboxTransport, artifacts: ArtifactSink):
        self._transport = transport
        self._artifacts = artifacts

    async def execute(self, request: dict[str, Any]) -> dict[str, Any]:
        lease = request.get("lease")
        if not isinstance(lease, dict):
            raise SandboxBrokerError("runtime lease is required")
        for field in ("tenant_id", "user_id", "session_id"):
            value = lease.get(field)
            if not isinstance(value, str) or not value or len(value) > 255 or any(ord(char) < 32 for char in value):
                raise SandboxBrokerError("runtime lease scope is invalid")
        if lease.get("capability_id") not in {None, "execute_python_code"}:
            raise SandboxBrokerError("runtime lease capability is invalid")
        code = request.get("code")
        inputs = request.get("inputs") or []
        if not isinstance(inputs, list):
            raise SandboxBrokerError("input attachments are malformed")
        inputs = [_validate_input(item) for item in inputs]
        try:
            limits = PythonSandboxLimits(**(request.get("limits") or {}))
        except (TypeError, ValueError) as exc:
            raise SandboxBrokerError("sandbox limits are malformed") from exc
        if not isinstance(code, str) or not code.strip():
            raise SandboxBrokerError("code is required")
        if len(code.encode()) > MAX_CODE_BYTES:
            raise SandboxBrokerError("code exceeds host bound")
        input_bytes = sum(item["size_bytes"] for item in inputs)
        if input_bytes > MAX_INPUT_BYTES:
            raise SandboxBrokerError("input attachments exceed host bound")
        limits.validate()
        hash_arguments: dict[str, Any] = {"code": code, "inputs": inputs}
        if "limits" in request:
            hash_arguments["limits"] = limits.__dict__
        expected_hash = "sha256:" + hashlib.sha256(
            json.dumps(hash_arguments, ensure_ascii=False, separators=(",", ":"), sort_keys=True).encode()
        ).hexdigest()
        if request.get("arguments_hash") != expected_hash:
            raise SandboxBrokerError("arguments hash mismatch")
        payload = {"lease": lease, "arguments_hash": expected_hash, "code": code, "inputs": inputs, "limits": limits.__dict__}
        result = await self._transport.execute(payload)
        execution_id = result.get("execution_id")
        if not isinstance(execution_id, str) or not execution_id:
            raise SandboxBrokerError("sandbox result missing execution id")
        if result.get("side_effect_state") in {"side_effect_unknown", "unknown"}:
            raise SandboxBrokerError("sandbox outcome is unknown")
        if result.get("status") not in {"succeeded", "failed", "timeout", "cancelled"}:
            raise SandboxBrokerError("sandbox result status is invalid")
        output_files = result.get("output_files") or []
        if not isinstance(output_files, list) or len(output_files) > MAX_OUTPUT_FILES:
            raise SandboxBrokerError("too many output files")
        total_output_bytes = 0
        persisted: list[dict[str, Any]] = []
        for item in output_files:
            if not isinstance(item, dict):
                raise SandboxBrokerError("malformed output artifact")
            filename = _validate_filename(item.get("filename"))
            try:
                size_bytes = int(item["size_bytes"])
                encoded = str(item["content_base64"])
                content = base64.b64decode(encoded, validate=True)
            except (KeyError, TypeError, ValueError, binascii.Error) as exc:
                raise SandboxBrokerError("malformed output artifact") from exc
            if size_bytes < 0 or len(content) != size_bytes:
                raise SandboxBrokerError("output artifact exceeds host bound")
            total_output_bytes += size_bytes
            if total_output_bytes > MAX_OUTPUT_BYTES:
                raise SandboxBrokerError("output artifacts exceed host bound")
            sha256 = item.get("sha256")
            if not isinstance(sha256, str) or not _SHA256.fullmatch(sha256) or not hmac.compare_digest(hashlib.sha256(content).hexdigest(), sha256):
                raise SandboxBrokerError("output artifact hash mismatch")
            persisted.append(await self._artifacts.put_code_output(
                tenant_id=str(lease["tenant_id"]), user_id=str(lease["user_id"]),
                session_id=str(lease["session_id"]), execution_id=execution_id,
                filename=filename, mime_type=item.get("mime_type"),
                content_base64=encoded, sha256=sha256,
                size_bytes=size_bytes,
            ))
        result["artifacts"] = persisted
        result["side_effect_state"] = "known"
        return result


@router.post("/python/execute")
async def execute_python_capability(
    request: Request,
    payload: dict[str, Any] = Body(...),
    x_ai_platform_internal_token: str | None = Header(default=None),
) -> dict[str, Any]:
    """Gateway boundary for the capability worker's sandbox request."""

    expected = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "")
    if not expected or not x_ai_platform_internal_token or not hmac.compare_digest(
        expected, x_ai_platform_internal_token
    ):
        raise HTTPException(status_code=401, detail="internal authorization failed")
    capability = getattr(request.app.state, "python_code_capability", None)
    if capability is None or not callable(getattr(capability, "execute", None)):
        raise HTTPException(status_code=503, detail="python capability unavailable")
    try:
        return await capability.execute(payload)
    except SandboxBrokerError as exc:
        raise HTTPException(status_code=424, detail=str(exc)) from None


@router.post("/python/artifacts")
async def put_python_artifact(
    request: Request,
    payload: dict[str, Any] = Body(...),
    x_ai_platform_internal_token: str | None = Header(default=None),
    x_ai_tenant_id: str | None = Header(default=None),
    x_ai_user_id: str | None = Header(default=None),
    x_ai_session_id: str | None = Header(default=None),
    x_ai_execution_id: str | None = Header(default=None),
    x_ai_run_id: str | None = Header(default=None),
    x_ai_tool_call_id: str | None = Header(default=None),
    x_ai_arguments_hash: str | None = Header(default=None),
    x_ai_capability_proof: str | None = Header(default=None),
) -> dict[str, Any]:
    """Persist one isolated Python output without exposing storage credentials."""

    expected_token = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "")
    if not expected_token or not x_ai_platform_internal_token or not hmac.compare_digest(
        x_ai_platform_internal_token, expected_token
    ):
        raise HTTPException(status_code=401, detail="internal authorization failed")
    tenant = _capability_scope(x_ai_tenant_id, name="tenant")
    user = _capability_scope(x_ai_user_id, name="user")
    session = _capability_scope(x_ai_session_id, name="session")
    if not x_ai_execution_id or not x_ai_run_id or not x_ai_tool_call_id:
        raise HTTPException(status_code=401, detail="capability proof required")
    try:
        execution_id = uuid.UUID(x_ai_execution_id)
        run_id = uuid.UUID(x_ai_run_id)
    except ValueError:
        raise HTTPException(status_code=401, detail="capability scope invalid") from None
    if not x_ai_arguments_hash or not _ARGUMENTS_HASH.fullmatch(x_ai_arguments_hash):
        raise HTTPException(status_code=401, detail="arguments hash required")
    try:
        verify_capability_proof(
            os.getenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", ""),
            x_ai_capability_proof or "",
            method="POST",
            path=_PYTHON_ARTIFACT_PATH,
            body=payload,
            tenant_id=tenant,
            user_id=user,
            session_id=session,
            execution_id=x_ai_execution_id,
            run_id=x_ai_run_id,
        )
    except CapabilityProofError:
        raise HTTPException(status_code=401, detail="capability proof invalid") from None
    try:
        data = PythonArtifactRequest.model_validate(payload)
    except ValueError:
        raise HTTPException(status_code=422, detail="invalid python output artifact") from None
    if data.tool_call_id != x_ai_tool_call_id or data.arguments_hash != x_ai_arguments_hash:
        raise HTTPException(status_code=403, detail="capability scope mismatch")

    storage = get_artifact_storage()
    database = getattr(storage, "database", None) if storage else None
    pool = getattr(database, "_pool", None)
    if storage is None or pool is None or not callable(getattr(storage, "create_artifact", None)):
        raise HTTPException(status_code=503, detail="artifact storage unavailable")

    try:
        async with pool.acquire() as conn, conn.transaction():
            row = await conn.fetchrow(
                """SELECT tenant_id, user_id, session_id, run_id, tool_call_id,
                              arguments_sha256, capability_id, effect, approval_status, status
                         FROM assistant_capability_executions
                        WHERE execution_id = $1 AND tenant_id = $2 AND user_id = $3
                          AND session_id = $4 AND run_id = $5
                        FOR UPDATE""",
                execution_id,
                tenant,
                user,
                session,
                run_id,
            )
            if (
                not row
                or row["tool_call_id"] != data.tool_call_id
                or not isinstance(row["arguments_sha256"], str)
                or not hmac.compare_digest(row["arguments_sha256"].strip(), data.arguments_hash[7:])
            ):
                raise HTTPException(status_code=403, detail="capability scope mismatch")
            artifact_id = _python_artifact_id(
                execution_id=x_ai_execution_id,
                tool_call_id=data.tool_call_id,
                arguments_hash=data.arguments_hash,
                filename=data.filename,
                sha256=data.sha256,
            )
            if row["capability_id"] != "execute_python_code":
                raise HTTPException(status_code=403, detail="capability not authorized")
            if (
                row["effect"] != "write"
                or row["approval_status"] != "consumed"
                or row["status"] not in {"dispatched", "running"}
            ):
                raise HTTPException(status_code=403, detail="capability execution not active")

            prior = await conn.fetch(
                """SELECT artifact_id, filename, size_bytes
                     FROM artifacts
                    WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
                      AND source = 'code_execution'
                      AND metadata ->> 'execution_id' = $4
                    FOR UPDATE""",
                tenant,
                user,
                session,
                x_ai_execution_id,
            )
            conflicting_name = next(
                (
                    item
                    for item in prior
                    if item["filename"] == data.filename and item["artifact_id"] != artifact_id
                ),
                None,
            )
            if conflicting_name:
                raise HTTPException(status_code=409, detail="artifact filename conflict")
            is_replay = any(item["artifact_id"] == artifact_id for item in prior)
            if not is_replay and (
                len(prior) >= MAX_OUTPUT_FILES
                or sum(int(item["size_bytes"]) for item in prior) + data.size_bytes
                > MAX_OUTPUT_BYTES
            ):
                raise HTTPException(status_code=413, detail="artifact output limit exceeded")

            content = base64.b64decode(data.content_base64, validate=True)
            suffix = data.filename.rpartition(".")[2].lower()
            try:
                artifact = await storage.create_artifact(
                    session_id=session,
                    tenant_id=tenant,
                    user_id=user,
                    type="code",
                    format=suffix or "bin",
                    title=data.filename,
                    filename=data.filename,
                    content=content,
                    source="code_execution",
                    metadata={
                        "schema_version": "ai-platform/python-code-artifact/v1",
                        "execution_id": x_ai_execution_id,
                        "run_id": x_ai_run_id,
                        "tool_call_id": data.tool_call_id,
                        "arguments_hash": data.arguments_hash,
                        "content_sha256": data.sha256,
                        "requested_mime_type": data.mime_type,
                    },
                    artifact_id=artifact_id,
                )
            except ValueError as exc:
                if "idempotency conflict" in str(exc):
                    raise HTTPException(
                        status_code=409, detail="artifact idempotency conflict"
                    ) from None
                raise
            download_url = f"/api/v1/assistant/artifacts/{artifact.artifact_id}/download"
            result = {
                "artifact_id": str(artifact.artifact_id),
                "download_url": download_url,
                "filename": str(artifact.filename),
                "mime_type": str(
                    getattr(artifact, "mime_type", None)
                    or data.mime_type
                    or "application/octet-stream"
                ),
                "size_bytes": int(artifact.size_bytes),
                "sha256": data.sha256,
            }
            return result
    except HTTPException:
        raise
    except Exception:
        # Upload can succeed while the durable execution receipt fails.  Do not
        # acknowledge a write whose outcome the Runtime cannot safely recover.
        raise HTTPException(status_code=502, detail="artifact persistence outcome unknown") from None
