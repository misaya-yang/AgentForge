"""Private Gateway broker for Rust-owned image capability executions.

This endpoint is intentionally narrower than the public image API.  It accepts
only a lease-bound prompt, resolves the configured image provider from Gateway
state, and stores every returned image through the authoritative
``ArtifactStorageService``.  Provider credentials and image bytes never cross
the Runtime/Worker boundary; the response contains artifact metadata only.

Composition must inject ``app.state.image_generation_service`` (an existing
configured provider router exposing ``generate``) before enabling this route.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import logging
import os
import uuid
from typing import Any

from ai_gateway_contracts.capability_proof import (
    CapabilityProofError,
    canonical_body_hash,
    verify_capability_proof,
)
from ai_gateway_core.media.image_generation import validate_image_bytes
from ai_gateway_core.storage import get_artifact_storage
from fastapi import APIRouter, Body, Header, HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field, model_validator

router = APIRouter(prefix="/internal/v2/agent-capabilities", tags=["internal-agent-capabilities"])

logger = logging.getLogger(__name__)

_PATH = "/internal/v2/agent-capabilities/image/generate"
_MAX_IMAGE_BYTES = 20 * 1024 * 1024
_MAX_IMAGES = 4
_MAX_PROMPT = 8_000
_MAX_NEGATIVE_PROMPT = 4_000
_PENDING_SCHEMA = "ai-platform/capability-broker-dispatch/v1"
_RECEIPT_SCHEMA = "ai-platform/durable-capability-receipt/v1"


class ImageGenerationRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    prompt: str = Field(min_length=1, max_length=_MAX_PROMPT)
    negative_prompt: str = Field(default="", max_length=_MAX_NEGATIVE_PROMPT)
    size: str = Field(default="1536*1536", pattern=r"^(1536\*1536|1024\*1024|720\*1280|1280\*720)$")
    style: str = Field(default="<auto>", max_length=64)
    n: int = Field(default=1, ge=1, le=_MAX_IMAGES)


class ImageGenerationEnvelope(BaseModel):
    model_config = ConfigDict(extra="forbid")

    arguments: ImageGenerationRequest
    arguments_hash: str = Field(pattern=r"^sha256:[0-9a-f]{64}$")

    @model_validator(mode="before")
    @classmethod
    def validate_hash(cls, value: Any) -> Any:
        if not isinstance(value, dict) or not isinstance(value.get("arguments"), dict):
            return value
        if f"sha256:{canonical_body_hash(value['arguments'])}" != value.get("arguments_hash"):
            raise ValueError("arguments hash mismatch")
        return value


def _scope(value: str | None) -> str:
    if not value or len(value) > 255 or any(ord(char) < 32 for char in value):
        raise HTTPException(status_code=403, detail="scope is invalid")
    return value


def _authorize(
    *,
    internal_token: str | None,
    tenant_id: str | None,
    user_id: str | None,
    session_id: str | None,
    execution_id: str | None,
    run_id: str | None,
    proof: str | None,
    body: dict[str, Any],
) -> tuple[str, str, str]:
    expected = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "")
    if not expected or not internal_token or not hmac.compare_digest(internal_token, expected):
        raise HTTPException(status_code=401, detail="internal authorization failed")
    tenant, user, session = _scope(tenant_id), _scope(user_id), _scope(session_id)
    if not execution_id or len(execution_id) > 255 or not run_id or len(run_id) > 255:
        raise HTTPException(status_code=401, detail="capability proof required")
    secret = os.getenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "")
    if not secret or not proof:
        raise HTTPException(status_code=401, detail="capability proof required")
    try:
        verify_capability_proof(
            secret,
            proof,
            method="POST",
            path=_PATH,
            body=body,
            tenant_id=tenant,
            user_id=user,
            session_id=session,
            execution_id=execution_id,
            run_id=run_id,
        )
    except CapabilityProofError:
        raise HTTPException(status_code=401, detail="capability proof invalid") from None
    return tenant, user, session


def _provider_result_images(result: Any) -> list[dict[str, Any]]:
    images = getattr(result, "images", None)
    if not isinstance(images, list):
        return []
    return [item for item in images if isinstance(item, dict)][: _MAX_IMAGES]


def _durable_image_result(value: Any) -> dict[str, Any] | None:
    if (
        not isinstance(value, dict)
        or value.get("schema_version") != _RECEIPT_SCHEMA
        or value.get("capability_id") != "generate_image"
        or not isinstance(value.get("result"), dict)
    ):
        return None
    result = value["result"]
    if set(result) != {
        "receipt_id",
        "capability_id",
        "external_id",
        "external_url",
        "artifacts",
    } or result.get("capability_id") != "generate_image":
        return None
    if not isinstance(result.get("receipt_id"), str) or not result["receipt_id"]:
        return None
    if result.get("external_id") is not None or result.get("external_url") is not None:
        return None
    artifacts = result.get("artifacts")
    if not isinstance(artifacts, list) or not 1 <= len(artifacts) <= _MAX_IMAGES:
        return None
    required = {"artifact_id", "kind", "mime_type", "filename", "size_bytes"}
    for artifact in artifacts:
        if (
            not isinstance(artifact, dict)
            or set(artifact) != required
            or artifact.get("kind") != "image"
            or not isinstance(artifact.get("size_bytes"), int)
            or artifact["size_bytes"] <= 0
            or any(
                not isinstance(artifact.get(field), str) or not artifact[field]
                for field in required - {"size_bytes"}
            )
        ):
            return None
    return result


def _gateway_response(result: dict[str, Any]) -> dict[str, Any]:
    return {
        "receipt_id": result["receipt_id"],
        "external_id": "",
        "external_url": "",
        "artifacts": result["artifacts"],
    }


async def _claim_execution(
    request: Request,
    *,
    tenant_id: str,
    user_id: str,
    session_id: str,
    execution_id: str,
    run_id: str,
    tool_call_id: str,
    arguments_hash: str,
) -> dict[str, Any] | None:
    database = getattr(request.app.state, "database", None)
    pool = getattr(database, "_pool", None)
    if pool is None:
        raise HTTPException(status_code=424, detail="capability execution store unavailable")
    try:
        execution_uuid = uuid.UUID(execution_id)
        run_uuid = uuid.UUID(run_id)
    except ValueError:
        raise HTTPException(status_code=403, detail="capability scope invalid") from None
    async with pool.acquire() as connection, connection.transaction():
        row = await connection.fetchrow(
            """SELECT result_summary, capability_id, effect, approval_status, status,
                          tool_call_id, arguments_sha256
                     FROM assistant_capability_executions
                    WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3
                      AND session_id=$4 AND run_id=$5
                    FOR UPDATE""",
            execution_uuid,
            tenant_id,
            user_id,
            session_id,
            run_uuid,
        )
        if (
            not row
            or row["capability_id"] != "generate_image"
            or row["effect"] != "write"
            or row["approval_status"] != "consumed"
            or row["status"] not in {"dispatched", "running"}
            or row["tool_call_id"] != tool_call_id
            or str(row["arguments_sha256"]).strip() != arguments_hash.removeprefix("sha256:")
        ):
            raise HTTPException(status_code=403, detail="capability execution not active")
        existing = _durable_image_result(row["result_summary"])
        if existing is not None:
            return existing
        if row["result_summary"] is not None:
            raise HTTPException(status_code=502, detail="image generation outcome unknown")
        pending = {
            "schema_version": _PENDING_SCHEMA,
            "capability_id": "generate_image",
        }
        changed = await connection.execute(
            "UPDATE assistant_capability_executions "
            "SET result_summary=$2, updated_at=NOW() WHERE execution_id=$1",
            execution_uuid,
            json.dumps(pending),
        )
        if not str(changed).endswith(" 1"):
            raise HTTPException(status_code=502, detail="image generation outcome unknown")
    return None


async def _clear_execution_claim(request: Request, execution_id: str) -> None:
    database = getattr(request.app.state, "database", None)
    pool = getattr(database, "_pool", None)
    if pool is None:
        raise HTTPException(status_code=502, detail="image generation outcome unknown")
    changed = await pool.execute(
        "UPDATE assistant_capability_executions SET result_summary=NULL, updated_at=NOW() "
        "WHERE execution_id=$1 AND result_summary->>'schema_version'=$2",
        uuid.UUID(execution_id),
        _PENDING_SCHEMA,
    )
    if not str(changed).endswith(" 1"):
        raise HTTPException(status_code=502, detail="image generation outcome unknown")


async def _store_execution_receipt(
    request: Request, execution_id: str, result: dict[str, Any]
) -> None:
    database = getattr(request.app.state, "database", None)
    pool = getattr(database, "_pool", None)
    if pool is None:
        raise HTTPException(status_code=502, detail="image generation outcome unknown")
    receipt = {
        "schema_version": _RECEIPT_SCHEMA,
        "capability_id": "generate_image",
        "result": result,
    }
    changed = await pool.execute(
        "UPDATE assistant_capability_executions SET result_summary=$2, updated_at=NOW() "
        "WHERE execution_id=$1 AND result_summary->>'schema_version'=$3",
        uuid.UUID(execution_id),
        json.dumps(receipt),
        _PENDING_SCHEMA,
    )
    if not str(changed).endswith(" 1"):
        raise HTTPException(status_code=502, detail="image generation outcome unknown")


def _decode_image(image: dict[str, Any]) -> tuple[bytes, str]:
    encoded = image.get("content_base64")
    mime_type = str(image.get("mime_type") or "image/png")
    if not isinstance(encoded, str) or not encoded or not mime_type.startswith("image/"):
        raise ValueError("image response invalid")
    try:
        content = base64.b64decode(encoded, validate=True)
    except Exception as exc:
        raise ValueError("image response invalid") from exc
    if not content or len(content) > _MAX_IMAGE_BYTES:
        raise ValueError("image response too large")
    if not validate_image_bytes(content, mime_type):
        raise ValueError("image response invalid")
    extension = mime_type.partition("/")[2].split(";", 1)[0].lower()
    if extension not in {"png", "jpeg", "jpg", "webp", "gif"}:
        raise ValueError("image mime type unsupported")
    return content, mime_type


async def _persist_images(
    *,
    storage: Any,
    images: list[dict[str, Any]],
    tenant_id: str,
    user_id: str,
    session_id: str,
    execution_id: str,
    prompt: str,
    provider: str,
) -> list[dict[str, Any]]:
    if not storage or not callable(getattr(storage, "create_artifact", None)):
        raise RuntimeError("artifact storage unavailable")
    persisted: list[dict[str, Any]] = []
    for index, image in enumerate(images, start=1):
        content, mime_type = _decode_image(image)
        content_sha256 = hashlib.sha256(content).hexdigest()
        artifact_id = "art_" + hashlib.sha256(
            (
                "image\0"
                + tenant_id
                + "\0"
                + user_id
                + "\0"
                + session_id
                + "\0"
                + execution_id
                + "\0"
                + str(index)
            ).encode("utf-8")
        ).hexdigest()[:16]
        extension = mime_type.partition("/")[2].split(";", 1)[0].lower().replace("jpeg", "jpg")
        artifact = await storage.create_artifact(
            session_id=session_id,
            tenant_id=tenant_id,
            user_id=user_id,
            type="image",
            format=extension,
            title=f"Generated: {prompt[:60]}",
            filename=f"generated_{execution_id[:16]}_{index}.{extension}",
            content=content,
            source="image_generation",
            metadata={
                "execution_id": execution_id,
                "provider": provider,
                "content_sha256": content_sha256,
                "artifact_index": index,
            },
            provider=provider,
            model_id=None,
            prompt=prompt,
            artifact_id=artifact_id,
        )
        artifact_id = str(getattr(artifact, "artifact_id", ""))
        if not artifact_id:
            raise RuntimeError("artifact persistence returned no id")
        persisted.append(
            {
                "artifact_id": artifact_id,
                "kind": "image",
                "mime_type": mime_type,
                "filename": str(getattr(artifact, "filename", "")) or f"generated_{index}.{extension}",
                "size_bytes": len(content),
            }
        )
    return persisted


@router.post("/image/generate")
async def generate_image_capability(
    request: Request,
    payload: dict[str, Any] = Body(...),
    x_ai_platform_internal_token: str | None = Header(default=None),
    x_ai_tenant_id: str | None = Header(default=None),
    x_ai_user_id: str | None = Header(default=None),
    x_ai_session_id: str | None = Header(default=None),
    x_ai_execution_id: str | None = Header(default=None),
    x_ai_run_id: str | None = Header(default=None),
    x_ai_tool_call_id: str | None = Header(default=None),
    x_ai_capability_proof: str | None = Header(default=None),
) -> dict[str, Any]:
    tenant_id, user_id, session_id = _authorize(
        internal_token=x_ai_platform_internal_token,
        tenant_id=x_ai_tenant_id,
        user_id=x_ai_user_id,
        session_id=x_ai_session_id,
        execution_id=x_ai_execution_id,
        run_id=x_ai_run_id,
        proof=x_ai_capability_proof,
        body=payload,
    )
    try:
        envelope = ImageGenerationEnvelope.model_validate(payload)
    except ValueError as exc:
        raise HTTPException(status_code=422, detail="invalid image arguments") from exc
    execution_id = _scope(x_ai_execution_id)
    run_id = _scope(x_ai_run_id)
    tool_call_id = _scope(x_ai_tool_call_id)
    service = getattr(request.app.state, "image_generation_service", None)
    generate = getattr(service, "generate", None)
    if not callable(generate):
        raise HTTPException(status_code=424, detail="image provider unavailable")
    storage = get_artifact_storage()
    if not storage or not callable(getattr(storage, "create_artifact", None)):
        raise HTTPException(status_code=424, detail="artifact storage unavailable")
    existing = await _claim_execution(
        request,
        tenant_id=tenant_id,
        user_id=user_id,
        session_id=session_id,
        execution_id=execution_id,
        run_id=run_id,
        tool_call_id=tool_call_id,
        arguments_hash=envelope.arguments_hash,
    )
    if existing is not None:
        return _gateway_response(existing)
    args = envelope.arguments
    generate_kwargs: dict[str, Any] = {
        "prompt": args.prompt,
        "negative_prompt": args.negative_prompt,
        "size": args.size,
        "style": args.style,
        "n": args.n,
    }
    try:
        result = await generate(**generate_kwargs)
    except Exception as exc:
        # The message can carry provider URLs or request bodies, so log the
        # exception type only. Without this the 502 is undiagnosable.
        logger.warning(
            "Image capability provider call raised %s (execution=%s)",
            type(exc).__name__,
            execution_id,
        )
        raise HTTPException(status_code=502, detail="image provider unavailable") from None
    if not getattr(result, "success", False):
        logger.warning(
            "Image capability generation failed code=%s unknown=%s (execution=%s)",
            str(getattr(result, "error_code", "unknown"))[:64],
            bool(getattr(result, "outcome_unknown", False)),
            execution_id,
        )
        if getattr(result, "outcome_unknown", False):
            raise HTTPException(status_code=502, detail="image generation outcome unknown")
        await _clear_execution_claim(request, execution_id)
        raise HTTPException(status_code=422, detail="image generation failed")
    images = _provider_result_images(result)
    if not images:
        raise HTTPException(status_code=502, detail="image provider returned no images")
    try:
        artifacts = await _persist_images(
            storage=storage,
            images=images,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            execution_id=execution_id,
            prompt=args.prompt,
            provider=str(getattr(result, "provider", "configured")),
        )
    except (ValueError, RuntimeError):
        raise HTTPException(status_code=502, detail="image artifact persistence failed") from None
    durable_result = {
        "receipt_id": f"img_{hashlib.sha256(execution_id.encode()).hexdigest()[:32]}",
        "capability_id": "generate_image",
        "external_id": None,
        "external_url": None,
        "artifacts": artifacts,
    }
    await _store_execution_receipt(request, execution_id, durable_result)
    return _gateway_response(durable_result)
