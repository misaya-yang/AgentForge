"""Gateway-owned image generation service.

This module deliberately owns only image state and provider orchestration.  The
provider is injected by the Gateway and all durable state is written to the
assistant schema; no in-process task dictionary is authoritative.
"""

from __future__ import annotations

import base64
import binascii
import hashlib
import logging
import time
import uuid
from types import SimpleNamespace
from typing import Any

from ai_gateway_core.image import apply_watermark_b64, make_thumbnail
from ai_gateway_core.image.image_state import (
    advance_latest_artifact_cas,
    compute_owner_scope,
    compute_request_hash,
    get_image_session,
    get_image_task,
    get_turn_by_task,
    insert_turn,
    list_turns,
    reserve_scoped_image_task,
    update_image_task,
    update_turn_status,
    upsert_image_session,
)
from ai_gateway_core.logging import record_internal_exception
from ai_gateway_core.media.image_generation import validate_image_bytes
from ai_gateway_core.security import SafeFetchError, safe_fetch_with_response
from ai_gateway_core.storage import get_artifact_storage
from fastapi import HTTPException, Request

logger = logging.getLogger(__name__)


def _pool(request: Request):
    database = getattr(request.app.state, "database", None)
    return getattr(database, "_pool", None)


def public_image_error(status: str | None, error_code: str | None) -> str | None:
    """Project old and new task rows without exposing stored provider errors."""

    if status == "unknown" or error_code == "outcome_unknown":
        return "image generation outcome unknown"
    if status == "failed":
        return "image generation failed"
    return None


def _owner(user: Any, body: Any) -> str:
    # The public request body is not proof of an embedded principal.  Keep the
    # owner scope bound to the authenticated Gateway identity until a verified
    # delegated-principal claim is available.
    del body
    return compute_owner_scope(
        user.user_id,
        app_tenant_id=user.tenant_id,
        app_user_id=user.user_id,
    )


def _request_owner(request: Request, user: Any) -> str:
    # App scope headers are not sufficient proof of an embedded principal.
    # Until the embed auth dependency exposes a verified principal, use the
    # authenticated Gateway identity only.
    del request
    return compute_owner_scope(
        user.user_id,
        app_tenant_id=user.tenant_id,
        app_user_id=user.user_id,
    )


_MAX_REFERENCE_BYTES = 8 * 1024 * 1024
_SUPPORTED_REFERENCE_MIMES = {"image/png", "image/jpeg", "image/gif", "image/webp"}


def _has_reference(body: Any, session_row: dict[str, Any] | None = None) -> bool:
    return any(
        (
            getattr(body, "reference_artifact_id", None),
            getattr(body, "reference_blob_id", None),
            getattr(body, "reference_image", None),
            getattr(body, "reference_image_url", None),
            getattr(body, "parent_artifact_id", None),
            session_row and session_row.get("latest_artifact_id"),
        )
    )


def _provider_supports_reference_images(provider: Any) -> bool:
    """Require an explicit provider capability before resolving input bytes."""

    config = getattr(provider, "config", None)
    return bool(
        getattr(provider, "supports_reference_images", False)
        or getattr(config, "supports_reference_images", False)
    )


def _decode_image(item: dict[str, Any]) -> tuple[bytes, str]:
    encoded = item.get("content_base64")
    mime = str(item.get("mime_type") or "image/png")
    if not isinstance(encoded, str) or not mime.startswith("image/"):
        raise ValueError("image provider returned invalid image")
    try:
        data = base64.b64decode(encoded, validate=True)
    except (ValueError, binascii.Error) as exc:
        raise ValueError("image provider returned invalid image") from exc
    if not data or len(data) > 20 * 1024 * 1024 or not validate_image_bytes(data, mime):
        raise ValueError("image provider returned invalid image")
    extension = mime.split("/", 1)[1].split(";", 1)[0].lower()
    if extension == "jpeg":
        extension = "jpg"
    if extension not in {"png", "jpg", "webp", "gif"}:
        raise ValueError("image provider returned unsupported format")
    return data, extension


def _content_sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def _deterministic_artifact_id(
    tenant_id: str, user_id: str, session_id: str, turn_id: str, index: int, content_sha256: str
) -> str:
    material = "\x1f".join((tenant_id, user_id, session_id, turn_id, str(index), content_sha256))
    return "art_" + hashlib.sha256(material.encode()).hexdigest()[:16]


async def _resolve_reference(
    body: Any, *, session_row: dict[str, Any] | None, storage: Any, owner: str, user: Any, pool: Any
) -> tuple[bytes, str] | None:
    sources = [
        bool(getattr(body, "reference_artifact_id", None)),
        bool(getattr(body, "reference_blob_id", None)),
        bool(getattr(body, "reference_image", None)),
        bool(getattr(body, "reference_image_url", None)),
        bool(getattr(body, "parent_artifact_id", None)),
        bool(session_row and session_row.get("latest_artifact_id")),
    ]
    if sum(sources) > 1:
        raise ValueError("only one image reference source is allowed")
    aid = (
        getattr(body, "reference_artifact_id", None)
        or getattr(body, "parent_artifact_id", None)
        or (session_row or {}).get("latest_artifact_id")
    )
    if aid:
        artifact = await storage.get_artifact(
            aid, owner_scope=owner, tenant_id=user.tenant_id, user_id=user.user_id
        )
        if not artifact:
            raise FileNotFoundError("reference artifact not found")
        # The public artifact id may refer to display/thumbnail.  Editing must
        # always consume the raw family member, never a watermarked derivative.
        if getattr(artifact, "variant", "raw") != "raw":
            find_variant = getattr(storage, "find_variant", None)
            raw_artifact = (
                await find_variant(aid, "raw") if callable(find_variant) else None
            )
            if raw_artifact is None or (
                (
                    getattr(raw_artifact, "owner_scope", None)
                    and raw_artifact.owner_scope != owner
                )
                or getattr(raw_artifact, "tenant_id", None) != user.tenant_id
                or getattr(raw_artifact, "user_id", None) != user.user_id
            ):
                raise FileNotFoundError("reference artifact not found")
            artifact = raw_artifact
        content = await storage.download_artifact(str(artifact.artifact_id))
        if content is None:
            raise FileNotFoundError("reference artifact not found")
    elif getattr(body, "reference_blob_id", None):
        from ai_gateway_core.image.image_state import get_image_blob

        blob = await get_image_blob(pool, blob_id=body.reference_blob_id, owner_scope=owner)
        if not blob or blob.get("status") != "ready":
            raise FileNotFoundError("reference blob not found")
        content = await storage.read_image_blob_object(
            blob["storage_key"], max_bytes=_MAX_REFERENCE_BYTES
        )
    elif getattr(body, "reference_image", None):
        value = body.reference_image.split(",", 1)[-1]
        content = base64.b64decode(value, validate=True)
    elif getattr(body, "reference_image_url", None):
        try:
            fetched = await safe_fetch_with_response(
                body.reference_image_url, max_bytes=_MAX_REFERENCE_BYTES, timeout=30.0
            )
        except (SafeFetchError, ValueError) as exc:
            raise ValueError("reference image URL is invalid") from exc
        fetched_mime = getattr(fetched, "content_type", "").split(";", 1)[0].strip().lower()
        if fetched_mime and fetched_mime != _sniff_reference(fetched.body):
            raise ValueError("reference image MIME does not match content")
        content = fetched.body
    else:
        return None
    mime = _sniff_reference(content)
    if (
        not content
        or len(content) > _MAX_REFERENCE_BYTES
        or not mime
        or mime not in _SUPPORTED_REFERENCE_MIMES
        or not validate_image_bytes(content, mime)
    ):
        raise ValueError("reference image is invalid")
    return content, mime


def _sniff_reference(content: bytes) -> str | None:
    if content.startswith(b"\x89PNG\r\n\x1a\n"):
        return "image/png"
    if content.startswith(b"\xff\xd8\xff"):
        return "image/jpeg"
    if content.startswith((b"GIF87a", b"GIF89a")):
        return "image/gif"
    if content.startswith(b"RIFF") and content[8:12] == b"WEBP":
        return "image/webp"
    return None


async def _urls(storage: Any, artifacts: list[Any], user: Any, owner: str) -> list[dict[str, Any]]:
    output = []
    for artifact in artifacts:
        aid = str(artifact.artifact_id)
        url, _ = await storage.get_presigned_download_url_for_variant(
            aid, "display", owner_scope=owner, tenant_id=user.tenant_id, user_id=user.user_id
        )
        output.append(
            {"url": url or f"/api/v1/assistant/artifacts/{aid}/download", "artifact_id": aid}
        )
    return output


class ImageGenerationService:
    """Synchronous and durable asynchronous image operations."""

    def __init__(self, request: Request, user: Any):
        self.request, self.user = request, user
        self.pool = _pool(request)
        self.provider = getattr(request.app.state, "image_generation_service", None)
        self.storage = get_artifact_storage()

    def effective_model_id(self) -> str | None:
        """Return the configured image model, not the chat model in the request."""
        configured = getattr(getattr(self.provider, "config", None), "model", None)
        return configured.strip() if isinstance(configured, str) and configured.strip() else None

    async def generate(
        self,
        body: Any,
        *,
        task_id: str | None = None,
        turn_id_override: str | None = None,
        session_id_override: str | None = None,
    ) -> dict[str, Any]:
        if (
            self.pool is None
            or self.storage is None
            or not callable(getattr(self.provider, "generate", None))
        ):
            raise HTTPException(
                503,
                detail={
                    "error_code": "storage_unavailable",
                    "message": "image service unavailable",
                },
            )
        owner = getattr(self, "_task_owner_scope", None) or _owner(self.user, body)
        payload = body.model_dump(mode="json")
        request_hash = compute_request_hash(payload)
        existing_task = task_id is not None
        task_id = task_id or f"imt_{uuid.uuid4().hex[:20]}"
        turn_id = turn_id_override or f"itn_{uuid.uuid4().hex[:16]}"
        session_id = session_id_override or body.session_id or f"img_{uuid.uuid4().hex}"
        existing = await get_image_session(self.pool, session_id)
        if existing and existing.get("owner_scope") != owner:
            raise HTTPException(
                404, detail={"error_code": "not_found", "message": "image session not found"}
            )
        if _has_reference(body, existing) and not _provider_supports_reference_images(
            self.provider
        ):
            raise HTTPException(
                422,
                detail={
                    "error_code": "reference_unsupported",
                    "message": "configured image provider does not support reference images",
                },
            )
        requested_variants = set(body.return_variants or [])
        if not requested_variants <= {"raw", "display", "thumbnail"}:
            raise HTTPException(422, detail={"error_code": "invalid_variants"})
        if not existing_task:
            reservation = await reserve_scoped_image_task(
                self.pool,
                task_id=task_id,
                tenant_id=self.user.tenant_id,
                user_id=self.user.user_id,
                owner_scope=owner,
                status="running",
                prompt=body.prompt,
                model_id=self.effective_model_id() or body.model_id,
                request_payload=payload,
                progress=1,
                turn_id=turn_id,
                session_id=session_id,
                parent_artifact_id=body.parent_artifact_id,
                client_request_id=body.client_request_id,
                request_hash=request_hash,
            )
            if reservation["state"] == "unavailable":
                raise HTTPException(503, detail={"error_code": "storage_unavailable"})
            if reservation["state"] in {"existing", "conflict"}:
                if reservation["state"] == "conflict":
                    raise HTTPException(409, detail={"error_code": "idempotency_conflict"})
                old_task = await get_image_task(self.pool, reservation["task_id"])
                if old_task and old_task.get("result"):
                    return {**old_task["result"], "idempotent_replay": True}
                raise HTTPException(409, detail={"error_code": "duplicate_request_in_flight"})
        await upsert_image_session(
            self.pool,
            session_id=session_id,
            owner_scope=owner,
            app_user_id=self.user.user_id,
            app_tenant_id=self.user.tenant_id,
        )
        # Re-read after the upsert: a concurrent first writer may have won the
        # session primary-key race.  Never append a turn to that owner's session.
        persisted_session = await get_image_session(self.pool, session_id)
        if not persisted_session or persisted_session.get("owner_scope") != owner:
            await update_image_task(
                self.pool,
                task_id=task_id,
                status="failed",
                progress=100,
                error="image session not found",
                error_code="not_found",
            )
            raise HTTPException(
                404, detail={"error_code": "not_found", "message": "image session not found"}
            )
        existing = persisted_session
        resolved_parent = body.parent_artifact_id or (
            existing.get("latest_artifact_id") if existing else None
        )
        try:
            reference_bytes = await _resolve_reference(
                body,
                session_row=existing,
                storage=self.storage,
                owner=owner,
                user=self.user,
                pool=self.pool,
            )
        except FileNotFoundError:
            await update_image_task(
                self.pool,
                task_id=task_id,
                status="failed",
                progress=100,
                error="reference not found",
                error_code="reference_not_found",
            )
            raise HTTPException(404, detail={"error_code": "reference_not_found"}) from None
        except (ValueError, binascii.Error):
            await update_image_task(
                self.pool,
                task_id=task_id,
                status="failed",
                progress=100,
                error="invalid reference",
                error_code="invalid_reference",
            )
            raise HTTPException(422, detail={"error_code": "invalid_reference"}) from None
        if (
            body.expected_parent_artifact_id is not None
            and existing
            and existing.get("latest_artifact_id") != body.expected_parent_artifact_id
        ):
            await update_image_task(
                self.pool,
                task_id=task_id,
                status="failed",
                progress=100,
                error="session latest artifact changed",
                error_code="latest_artifact_conflict",
            )
            raise HTTPException(
                409,
                detail={
                    "error_code": "latest_artifact_conflict",
                    "message": "session latest artifact changed",
                },
            )
        started = time.monotonic()
        await insert_turn(
            self.pool,
            turn_id=turn_id,
            session_id=session_id,
            owner_scope=owner,
            task_id=task_id,
            prompt=body.prompt,
            model_id=self.effective_model_id() or body.model_id,
            style=str(body.style),
            add_watermark=body.add_watermark,
            parent_artifact_id=resolved_parent,
            output_artifact_id=None,
            status="running",
            error=None,
            error_code=None,
            client_request_id=body.client_request_id,
            request_hash=request_hash,
        )
        provider_succeeded = False
        try:
            generate_kwargs = {
                "prompt": body.prompt,
                "n": body.n,
                "size": body.size or "1536*1536",
                "style": str(body.style),
                "negative_prompt": "",
            }
            if reference_bytes is not None:
                generate_kwargs["reference_image"] = reference_bytes[0]
                generate_kwargs["reference_mime"] = reference_bytes[1]
            result = await self.provider.generate(
                **generate_kwargs,
            )
            if getattr(result, "outcome_unknown", False):
                raise RuntimeError("image generation outcome unknown")
            if not getattr(result, "success", False) or not getattr(result, "images", None):
                raise RuntimeError(getattr(result, "error", None) or "image generation failed")
            provider_succeeded = True
            artifacts = []
            display_artifacts = []
            thumbnail_artifacts = []
            for index, item in enumerate(result.images[: body.n], 1):
                data, extension = _decode_image(item)
                artifacts.append(
                    await self.storage.create_artifact(
                        session_id=session_id,
                        tenant_id=self.user.tenant_id,
                        user_id=self.user.user_id,
                        type="image",
                        format=extension,
                        title=f"Generated: {body.prompt[:60]}",
                        filename=f"generated_{turn_id}_{index}.{extension}",
                        content=data,
                        source="image_generation",
                        metadata={
                            "turn_id": turn_id,
                            "owner_scope": owner,
                            "content_sha256": _content_sha256(data),
                        },
                        variant="raw",
                        parent_artifact_id=resolved_parent,
                        turn_id=turn_id,
                        owner_scope=owner,
                        provider=getattr(result, "provider", None),
                        model_id=self.effective_model_id(),
                        prompt=body.prompt,
                        artifact_id=_deterministic_artifact_id(
                            self.user.tenant_id,
                            self.user.user_id,
                            session_id,
                            turn_id,
                            index,
                            _content_sha256(data),
                        ),
                    )
                )
                if body.add_watermark:
                    wm_b64, wm_mime = apply_watermark_b64(base64.b64encode(data).decode())
                    wm_data = base64.b64decode(wm_b64)
                    display_artifacts.append(
                        await self.storage.create_artifact(
                            session_id=session_id,
                            tenant_id=self.user.tenant_id,
                            user_id=self.user.user_id,
                            type="image",
                            format="png" if wm_mime == "image/png" else extension,
                            title=f"Generated: {body.prompt[:60]}",
                            filename=f"display_{turn_id}_{index}.png",
                            content=wm_data,
                            source="image_generation_watermarked",
                            metadata={"content_sha256": _content_sha256(wm_data)},
                            variant="display",
                            parent_artifact_id=str(artifacts[-1].artifact_id),
                            turn_id=turn_id,
                            owner_scope=owner,
                            provider=getattr(result, "provider", None),
                            model_id=self.effective_model_id(),
                            prompt=body.prompt,
                            artifact_id=_deterministic_artifact_id(
                                self.user.tenant_id,
                                self.user.user_id,
                                session_id,
                                turn_id,
                                index + 100,
                                _content_sha256(wm_data),
                            ),
                        )
                    )
                if body.return_variants and "thumbnail" in body.return_variants:
                    thumb = make_thumbnail(data)
                    if thumb:
                        thumbnail_artifacts.append(await self.storage.create_artifact(
                            session_id=session_id,
                            tenant_id=self.user.tenant_id,
                            user_id=self.user.user_id,
                            type="image",
                            format="png",
                            title=f"Generated: {body.prompt[:60]}",
                            filename=f"thumbnail_{turn_id}_{index}.png",
                            content=thumb,
                            source="image_generation_thumbnail",
                            metadata={"content_sha256": _content_sha256(thumb)},
                            variant="thumbnail",
                            parent_artifact_id=str(artifacts[-1].artifact_id),
                            turn_id=turn_id,
                            owner_scope=owner,
                            provider=getattr(result, "provider", None),
                            model_id=self.effective_model_id(),
                            prompt=body.prompt,
                            artifact_id=_deterministic_artifact_id(
                                self.user.tenant_id,
                                self.user.user_id,
                                session_id,
                                turn_id,
                                index + 200,
                                _content_sha256(thumb),
                            ),
                        ))
            raw = str(artifacts[0].artifact_id)
            advanced = await advance_latest_artifact_cas(
                self.pool,
                session_id=session_id,
                expected_parent=body.expected_parent_artifact_id or resolved_parent,
                new_artifact_id=raw,
            )
            urls = await _urls(self.storage, display_artifacts or artifacts, self.user, owner)
            variants: dict[str, str] = {}
            variant_artifacts = {
                "raw": artifacts,
                "display": display_artifacts or artifacts,
                "thumbnail": thumbnail_artifacts,
            }
            for variant in requested_variants:
                selected = variant_artifacts[variant]
                if not selected:
                    continue
                aid = str(selected[0].artifact_id)
                variant_url, _ = await self.storage.get_presigned_download_url_for_variant(
                    aid,
                    variant,
                    owner_scope=owner,
                    tenant_id=self.user.tenant_id,
                    user_id=self.user.user_id,
                )
                if variant_url:
                    variants[variant] = variant_url
            response = {
                "success": True,
                "images": urls,
                "provider": getattr(result, "provider", None),
                "effective_model_id": self.effective_model_id(),
                "duration_ms": (time.monotonic() - started) * 1000,
                "session_id": session_id,
                "turn_id": turn_id,
                "parent_artifact_id": resolved_parent,
                "output_artifact_id": raw,
                "client_request_id": body.client_request_id,
                "latest_advanced": advanced,
                "variants": variants or None,
            }
            await update_turn_status(
                self.pool, turn_id=turn_id, status="completed", output_artifact_id=raw
            )
            if task_id:
                await update_image_task(
                    self.pool,
                    task_id=task_id,
                    status="completed",
                    progress=100,
                    provider=getattr(result, "provider", None),
                    result=response,
                    output_artifact_id=raw,
                )
            return response
        except Exception as exc:
            if isinstance(exc, HTTPException) and not provider_succeeded:
                raise
            record_internal_exception(logger, "assistant.image_generation.failure", exc)
            error_code = (
                "outcome_unknown"
                if provider_succeeded or "outcome unknown" in str(exc)
                else "provider_failed"
            )
            safe_error = (
                "image generation outcome unknown"
                if error_code == "outcome_unknown"
                else "image generation failed"
            )
            await update_turn_status(
                self.pool,
                turn_id=turn_id,
                status="unknown" if error_code == "outcome_unknown" else "failed",
                error=safe_error,
                error_code=error_code,
            )
            if task_id:
                await update_image_task(
                    self.pool,
                    task_id=task_id,
                    status="unknown" if error_code == "outcome_unknown" else "failed",
                    progress=100,
                    error=safe_error,
                    error_code=error_code,
                )
            raise HTTPException(
                502,
                detail={
                    "error_code": error_code,
                    "task_id": task_id,
                    "message": "image generation outcome unknown"
                    if error_code == "outcome_unknown"
                    else "image generation failed",
                },
            ) from None

    async def submit(self, body: Any) -> dict[str, Any]:
        if self.pool is None:
            raise HTTPException(503, detail={"error_code": "storage_unavailable"})
        owner = _owner(self.user, body)
        task_id = f"imt_{uuid.uuid4().hex[:20]}"
        payload = body.model_dump(mode="json")
        request_hash = compute_request_hash(payload)
        turn_id = f"itn_{uuid.uuid4().hex[:16]}"
        session_id = body.session_id or f"img_{uuid.uuid4().hex}"
        existing = await get_image_session(self.pool, session_id)
        if existing and existing.get("owner_scope") != owner:
            raise HTTPException(
                404, detail={"error_code": "not_found", "message": "image session not found"}
            )
        await upsert_image_session(
            self.pool,
            session_id=session_id,
            owner_scope=owner,
            app_user_id=self.user.user_id,
            app_tenant_id=self.user.tenant_id,
        )
        existing = await get_image_session(self.pool, session_id)
        if not existing or existing.get("owner_scope") != owner:
            raise HTTPException(
                404, detail={"error_code": "not_found", "message": "image session not found"}
            )
        if _has_reference(body, existing) and not _provider_supports_reference_images(
            self.provider
        ):
            raise HTTPException(
                422,
                detail={
                    "error_code": "reference_unsupported",
                    "message": "configured image provider does not support reference images",
                },
            )
        if _has_reference(body, existing):
            if self.storage is None:
                raise HTTPException(503, detail={"error_code": "storage_unavailable"})
            try:
                await _resolve_reference(
                    body,
                    session_row=existing,
                    storage=self.storage,
                    owner=owner,
                    user=self.user,
                    pool=self.pool,
                )
            except FileNotFoundError:
                raise HTTPException(404, detail={"error_code": "reference_not_found"}) from None
            except (ValueError, binascii.Error):
                raise HTTPException(422, detail={"error_code": "invalid_reference"}) from None
        reservation = await reserve_scoped_image_task(
            self.pool,
            task_id=task_id,
            tenant_id=self.user.tenant_id,
            user_id=self.user.user_id,
            owner_scope=owner,
            status="pending",
            prompt=body.prompt,
            model_id=body.model_id,
            request_payload=payload,
            turn_id=turn_id,
            session_id=session_id,
            parent_artifact_id=body.parent_artifact_id,
            client_request_id=body.client_request_id,
            request_hash=request_hash,
        )
        if reservation["state"] == "unavailable":
            raise HTTPException(503, detail={"error_code": "storage_unavailable"})
        if reservation["state"] == "conflict":
            raise HTTPException(409, detail={"error_code": "idempotency_conflict"})
        if reservation["state"] == "existing":
            existing = await get_image_task(self.pool, reservation["task_id"])
            if existing is None:
                raise HTTPException(409, detail={"error_code": "duplicate_request_in_flight"})
            return {
                "task_id": reservation["task_id"],
                "status": str(existing.get("status") or "pending"),
                "message": "Image generation task already submitted",
            }
        return {
            "task_id": task_id,
            "status": "pending",
            "message": "Image generation task submitted",
        }

    async def claim_pending(
        self, *, limit: int = 1, visibility_seconds: int = 300
    ) -> list[dict[str, Any]]:
        """Claim durable work for an external worker; no in-process authority."""
        if self.pool is None:
            return []
        limit = max(1, min(limit, 10))
        async with self.pool.acquire() as conn, conn.transaction():
            await conn.execute(
                """UPDATE assistant.image_tasks
                   SET status='dead_letter', completed_at=NOW(), updated_at=NOW(),
                       error='image task exceeded retry limit', error_code='max_attempts'
                 WHERE runtime_scope_version=1 AND status='claimed' AND locked_until < NOW()
                   AND attempt_count >= max_attempts"""
            )
            await conn.execute(
                """UPDATE assistant.image_tasks
                   SET status='unknown', completed_at=NOW(), updated_at=NOW(),
                       error='image worker lost after provider dispatch',
                       error_code='worker_lost_after_dispatch'
                 WHERE runtime_scope_version=1 AND status='running'
                   AND locked_until IS NOT NULL AND locked_until < NOW()"""
            )
            rows = await conn.fetch(
                """WITH picked AS (
                    SELECT task_id FROM assistant.image_tasks
                     WHERE runtime_scope_version=1
                       AND (status='pending' OR (status='claimed' AND locked_until < NOW()))
                       AND attempt_count < max_attempts
                     ORDER BY created_at ASC LIMIT $1 FOR UPDATE SKIP LOCKED
                )
                UPDATE assistant.image_tasks AS task
                   SET status='claimed', locked_until=NOW()+($2::int * INTERVAL '1 second'),
                       attempt_count=attempt_count+1, started_at=COALESCE(started_at,NOW()),
                       updated_at=NOW()
                  FROM picked WHERE task.task_id=picked.task_id RETURNING task.*""",
                limit,
                visibility_seconds,
            )
        return [dict(row) for row in rows]

    async def mark_unknown(self, task_id: str, *, error: str) -> bool:
        """Terminally fence an unexpected worker failure without requeueing it."""

        if self.pool is None:
            return False
        changed = await self.pool.execute(
            """UPDATE assistant.image_tasks
                  SET status='unknown', completed_at=NOW(), updated_at=NOW(),
                      error=$2, error_code='worker_failed'
                WHERE task_id=$1 AND status IN ('claimed', 'running')""",
            task_id,
            error[:1000],
        )
        return str(changed).endswith(" 1")

    async def execute_claimed(self, body: Any, task_id: str) -> dict[str, Any]:
        """Worker hook: execute a row already claimed with SKIP LOCKED."""
        row = await get_image_task(self.pool, task_id)
        if not row:
            raise HTTPException(409, detail={"error_code": "task_not_claimed"})
        # A worker has no request principal.  Establish the execution context
        # from the row claimed under PostgreSQL's tenant/user scope, never from
        # the serialized client payload or a worker-global user.
        task_user = SimpleNamespace(
            tenant_id=str(row.get("tenant_id") or ""),
            user_id=str(row.get("user_id") or ""),
        )
        if not task_user.tenant_id or not task_user.user_id:
            raise HTTPException(409, detail={"error_code": "task_scope_invalid"})
        # Accept both the current user-bound owner and the older tenant-bound
        # owner encoding.  In either case the durable row remains authoritative
        # after the explicit tenant/user checks above.
        valid_owners = {
            compute_owner_scope(task_user.user_id),
            compute_owner_scope(
                task_user.user_id,
                app_tenant_id=task_user.tenant_id,
                app_user_id=task_user.user_id,
            ),
        }
        if (
            row.get("status") != "claimed"
            or row.get("runtime_scope_version") != 1
            or row.get("owner_scope") not in valid_owners
            or row.get("request_hash") != compute_request_hash(body.model_dump(mode="json"))
        ):
            raise HTTPException(409, detail={"error_code": "task_not_claimed"})
        owner = str(row["owner_scope"])
        dispatched = await self.pool.execute(
            """UPDATE assistant.image_tasks
                  SET status='running', locked_until=NOW() + INTERVAL '15 minutes',
                      updated_at=NOW()
                WHERE task_id=$1 AND tenant_id=$2 AND user_id=$3
                  AND owner_scope=$4 AND runtime_scope_version=1
                  AND status='claimed' AND locked_until > NOW()""",
            task_id,
            task_user.tenant_id,
            task_user.user_id,
            owner,
        )
        if not str(dispatched).endswith(" 1"):
            raise HTTPException(409, detail={"error_code": "task_not_claimed"})
        task_service = self
        if (
            getattr(self.user, "tenant_id", None) != task_user.tenant_id
            or getattr(self.user, "user_id", None) != task_user.user_id
        ):
            # Do not call __init__: a durable worker request is an internal
            # execution context and need not expose a public Request/app
            # object.  Copy only the already trusted service dependencies.
            task_service = object.__new__(ImageGenerationService)
            task_service.request = self.request
            task_service.user = task_user
            task_service.pool = self.pool
            task_service.provider = getattr(self, "provider", None)
            task_service.storage = getattr(self, "storage", None)
        task_service._task_owner_scope = owner
        try:
            return await task_service.generate(
                body,
                task_id=task_id,
                turn_id_override=row.get("turn_id"),
                session_id_override=row.get("session_id"),
            )
        finally:
            if task_service is self:
                del self._task_owner_scope

    async def task(self, task_id: str) -> dict[str, Any]:
        row = await get_image_task(self.pool, task_id) or await get_turn_by_task(self.pool, task_id)
        owner = _request_owner(self.request, self.user)
        task_scope_valid = (
            row is not None
            and row.get("owner_scope") == owner
            and (
                "runtime_scope_version" not in row
                or (
                    row.get("runtime_scope_version") == 1
                    and row.get("tenant_id") == self.user.tenant_id
                    and row.get("user_id") == self.user.user_id
                )
            )
        )
        if not task_scope_valid:
            raise HTTPException(
                404, detail={"error_code": "not_found", "message": "image task not found"}
            )
        return row

    async def session(
        self, session_id: str, limit: int, cursor: str | None, include_urls: bool
    ) -> dict[str, Any]:
        row = await get_image_session(self.pool, session_id)
        owner = _request_owner(self.request, self.user)
        if not row or row.get("owner_scope") != owner:
            raise HTTPException(
                404, detail={"error_code": "not_found", "message": "image session not found"}
            )
        turns, next_cursor = await list_turns(
            self.pool, session_id=session_id, owner_scope=owner, limit=limit, cursor=cursor
        )
        public_turns = []
        for turn in turns:
            visible = {
                key: turn.get(key)
                for key in (
                    "turn_id", "session_id", "task_id", "prompt", "model_id", "style",
                    "add_watermark", "parent_artifact_id", "output_artifact_id",
                    "output_artifact_ids", "status", "error_code", "client_request_id",
                )
            }
            visible["error"] = public_image_error(turn.get("status"), turn.get("error_code"))
            visible["created_at"] = turn["created_at"].isoformat() if turn.get("created_at") else ""
            visible["completed_at"] = (
                turn["completed_at"].isoformat() if turn.get("completed_at") else None
            )
            if include_urls and turn.get("output_artifact_id") and self.storage:
                url, _ = await self.storage.get_presigned_download_url_for_variant(
                    turn["output_artifact_id"],
                    "display",
                    owner_scope=owner,
                    tenant_id=self.user.tenant_id,
                    user_id=self.user.user_id,
                )
                visible["output_url"] = url
            public_turns.append(visible)
        return {
            "session_id": session_id,
            "latest_artifact_id": row.get("latest_artifact_id"),
            "locked_style": row.get("locked_style"),
            "created_at": row["created_at"].isoformat(),
            "updated_at": row["updated_at"].isoformat(),
            "turns": public_turns,
            "next_cursor": next_cursor,
        }
