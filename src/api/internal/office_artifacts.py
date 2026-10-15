"""Private Gateway broker for Rust-rendered office artifacts.

The capability worker owns rendering, but never receives storage credentials.
This route verifies the worker's scope-bound proof, persists through the
configured ``ArtifactStorageService``, and records the receipt on the durable
execution row before acknowledging the side effect.
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
from typing import Any

from ai_gateway_contracts.capability_proof import CapabilityProofError, verify_capability_proof
from ai_gateway_core.storage import get_artifact_storage
from fastapi import APIRouter, Body, Header, HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator

router = APIRouter(prefix="/internal/v2/agent-capabilities", tags=["internal-agent-capabilities"])

_PATH = "/internal/v2/agent-capabilities/office/artifacts"
_MAX_BYTES = 32 * 1024 * 1024
_MAX_B64 = ((_MAX_BYTES + 2) // 3) * 4 + 4
_ARGUMENTS_HASH = re.compile(r"^sha256:[0-9a-f]{64}$")
_CONTENT_HASH = re.compile(r"^[0-9a-f]{64}$")
_MIME = re.compile(r"^[A-Za-z0-9][A-Za-z0-9!#$&^_.+-]*/[A-Za-z0-9][A-Za-z0-9!#$&^_.+-]*$")


class OfficeArtifactRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    tool_call_id: str = Field(min_length=1, max_length=160)
    arguments_hash: str = Field(pattern=r"^sha256:[0-9a-f]{64}$")
    artifact_type: str = Field(min_length=1, max_length=32)
    format: str = Field(min_length=1, max_length=32, pattern=r"^[a-z0-9][a-z0-9._-]{0,31}$")
    title: str = Field(min_length=1, max_length=255)
    filename: str = Field(min_length=1, max_length=255)
    mime_type: str = Field(min_length=1, max_length=128)
    sha256: str = Field(pattern=r"^[0-9a-f]{64}$")
    size_bytes: int = Field(ge=1, le=_MAX_BYTES)
    content_base64: str = Field(min_length=1, max_length=_MAX_B64)
    metadata: dict[str, Any] = Field(default_factory=dict)

    @field_validator("mime_type")
    @classmethod
    def validate_mime(cls, value: str) -> str:
        if not _MIME.fullmatch(value):
            raise ValueError("mime type invalid")
        return value.lower()

    @field_validator("filename", "title", "artifact_type", "format", "tool_call_id")
    @classmethod
    def reject_controls(cls, value: str) -> str:
        if any(ord(char) < 32 for char in value):
            raise ValueError("control character not allowed")
        return value

    @model_validator(mode="after")
    def validate_content(self) -> OfficeArtifactRequest:
        try:
            content = base64.b64decode(self.content_base64, validate=True)
        except (ValueError, binascii.Error) as exc:
            raise ValueError("content_base64 invalid") from exc
        if len(content) != self.size_bytes:
            raise ValueError("content size mismatch")
        digest = hashlib.sha256(content).hexdigest()
        if not hmac.compare_digest(digest, self.sha256):
            raise ValueError("content hash mismatch")
        return self


def _scope(value: str | None, *, name: str) -> str:
    if not value or len(value) > 255 or any(ord(char) < 32 for char in value):
        raise HTTPException(status_code=403, detail=f"{name} is invalid")
    return value


def _receipt(value: Any) -> dict[str, Any] | None:
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != "ai-platform/durable-capability-receipt/v1"
        or value.get("capability_id") != "mcp_docgen__generate_document"
        or not isinstance(value.get("result"), dict)
    ):
        return None
    artifact = value.get("broker_response")
    if not isinstance(artifact, dict):
        return None
    required = {"artifact_id", "download_path", "filename", "mime_type", "size_bytes", "sha256"}
    if set(artifact) != required or any(
        not isinstance(artifact[key], str) for key in required - {"size_bytes"}
    ):
        return None
    if not isinstance(artifact["size_bytes"], int) or not _CONTENT_HASH.fullmatch(
        artifact["sha256"]
    ):
        return None
    if (
        not artifact["artifact_id"].startswith("art_")
        or not artifact["download_path"].startswith("/api/v1/assistant/artifacts/")
        or not _MIME.fullmatch(artifact["mime_type"])
        or any(
            ord(character) < 32
            for field in ("artifact_id", "download_path", "filename", "mime_type")
            for character in artifact[field]
        )
    ):
        return None
    return {"artifact": artifact}


@router.post("/office/artifacts")
async def put_office_artifact(
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
    expected_token = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "")
    if not expected_token or not x_ai_platform_internal_token or not hmac.compare_digest(
        x_ai_platform_internal_token, expected_token
    ):
        raise HTTPException(status_code=401, detail="internal authorization failed")
    tenant = _scope(x_ai_tenant_id, name="tenant")
    user = _scope(x_ai_user_id, name="user")
    session = _scope(x_ai_session_id, name="session")
    if not x_ai_execution_id or not x_ai_run_id or not x_ai_tool_call_id:
        raise HTTPException(status_code=401, detail="capability proof required")
    try:
        uuid.UUID(x_ai_execution_id)
        uuid.UUID(x_ai_run_id)
    except ValueError:
        raise HTTPException(status_code=401, detail="capability scope invalid") from None
    if not x_ai_arguments_hash or not _ARGUMENTS_HASH.fullmatch(x_ai_arguments_hash):
        raise HTTPException(status_code=401, detail="arguments hash required")
    try:
        proof_secret = os.getenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "")
        verify_capability_proof(
            proof_secret,
            x_ai_capability_proof or "",
            method="POST",
            path=_PATH,
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
        data = OfficeArtifactRequest.model_validate(payload)
    except ValueError:
        raise HTTPException(status_code=422, detail="invalid office artifact") from None
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
                """SELECT result_summary, tenant_id, user_id, session_id, run_id, tool_call_id,
                              arguments_sha256, capability_id, effect, approval_status, status
                         FROM assistant_capability_executions
                        WHERE execution_id = $1 AND tenant_id = $2 AND user_id = $3
                          AND session_id = $4 AND run_id = $5
                        FOR UPDATE""",
                uuid.UUID(x_ai_execution_id), tenant, user, session, uuid.UUID(x_ai_run_id),
            )
            if (
                not row
                or row["tool_call_id"] != data.tool_call_id
                or row["arguments_sha256"].strip() != data.arguments_hash[7:]
            ):
                raise HTTPException(status_code=403, detail="capability scope mismatch")
            existing = _receipt(row["result_summary"])
            if existing:
                artifact = existing["artifact"]
                if artifact["sha256"] != data.sha256 or artifact["size_bytes"] != data.size_bytes:
                    raise HTTPException(status_code=409, detail="artifact idempotency conflict")
                return artifact
            if row["capability_id"] != "mcp_docgen__generate_document":
                raise HTTPException(status_code=403, detail="capability not authorized")
            if (
                row["effect"] != "write"
                or row["approval_status"] != "consumed"
                or row["status"] not in {"dispatched", "running"}
            ):
                raise HTTPException(status_code=403, detail="capability execution not active")
            content = base64.b64decode(data.content_base64, validate=True)
            artifact_id = "art_" + hashlib.sha256(
                (
                    "office\0"
                    + tenant
                    + "\0"
                    + user
                    + "\0"
                    + session
                    + "\0"
                    + x_ai_execution_id
                    + "\0"
                    + data.tool_call_id
                    + "\0"
                    + data.arguments_hash
                ).encode("utf-8")
            ).hexdigest()[:16]
            artifact = await storage.create_artifact(
                session_id=session,
                tenant_id=tenant,
                user_id=user,
                type=data.artifact_type,
                format=data.format,
                title=data.title,
                filename=data.filename,
                content=content,
                source="ai",
                metadata={
                    **data.metadata,
                    "schema_version": "ai-platform/office-artifact/v1",
                    "execution_id": x_ai_execution_id,
                    "tool_call_id": data.tool_call_id,
                    "arguments_hash": data.arguments_hash,
                    "content_sha256": data.sha256,
                },
                artifact_id=artifact_id,
            )
            download_path = f"/api/v1/assistant/artifacts/{artifact.artifact_id}/download"
            result = {
                "artifact_id": str(artifact.artifact_id),
                "download_path": str(download_path),
                "filename": str(artifact.filename),
                "mime_type": str(artifact.mime_type or data.mime_type),
                "size_bytes": int(artifact.size_bytes),
                "sha256": data.sha256,
            }
            await conn.execute(
                "UPDATE assistant_capability_executions "
                "SET result_summary=$2, updated_at=NOW() "
                "WHERE execution_id=$1",
                uuid.UUID(x_ai_execution_id),
                json.dumps(
                    {
                        "schema_version": "ai-platform/durable-capability-receipt/v1",
                        "capability_id": "mcp_docgen__generate_document",
                        "result": {
                            "artifact_id": result["artifact_id"],
                            "download_url": result["download_path"],
                            "filename": result["filename"],
                            "size_bytes": result["size_bytes"],
                            "sha256": result["sha256"],
                            "plan_outline": str(data.metadata.get("plan_outline") or ""),
                            "critic_passed": False,
                        },
                        "broker_response": result,
                    }
                ),
            )
            return result
    except HTTPException:
        raise
    except Exception:
        # The object upload may have completed while the durable receipt did
        # not.  Never claim success: Runtime must close the write as unknown.
        raise HTTPException(status_code=502, detail="artifact persistence outcome unknown") from None
