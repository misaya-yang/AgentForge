"""Bind user uploads to a session before the Runtime can read them.

The upload API returns a user-scoped path. Runtime's ``read_attachment``
capability accepts only session-owned ``art_*`` IDs, so the boundary copies a
selected upload into the existing artifact store with a stable identity.
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import json
import re
from pathlib import Path

from ai_gateway_core.logging import get_logger
from ai_gateway_core.storage import (
    FileStorageService,
    StorageBackend,
    StorageConfig,
    get_artifact_storage,
    get_file_storage,
)
from fastapi import HTTPException, Request

from ....core.auth.user_resolver import UserContext
from ....services.assistant_entry.model_access import assistant_model_service
from ..files import get_user_uploads_path

_ARTIFACT_ID = re.compile(r"^art_[0-9a-f]{16}$")
_UPLOAD_NAME = re.compile(r"^[0-9a-f]{8}_[0-9]{8}_[0-9]{6}\.[a-z0-9]{1,8}$")
_SUPPORTED_FORMATS = {"pdf", "docx", "md", "txt", "csv", "json", "xlsx", "pptx", "png", "jpg", "jpeg", "gif", "webp"}
_IMAGE_FORMATS = {"png", "jpg", "jpeg", "gif", "webp"}
_MAX_BYTES = 32 * 1024 * 1024
_MAX_IMAGE_BYTES = 2 * 1024 * 1024
_IMAGE_MIMES = {
    "image/png": b"\x89PNG\r\n\x1a\n",
    "image/jpeg": b"\xff\xd8\xff",
    "image/gif": b"GIF8",
    "image/webp": b"RIFF",
}
logger = get_logger(__name__)


def _upload_name(ref: str, user: UserContext) -> str:
    prefix = f"/uploads/{user.tenant_id}/{user.user_id}/"
    if not ref.startswith(prefix):
        logger.warning("Assistant attachment rejected stage=owner_prefix")
        raise HTTPException(422, detail={"code": "ATTACHMENT_UNAVAILABLE"})
    name = ref[len(prefix):]
    if not _UPLOAD_NAME.fullmatch(name):
        logger.warning("Assistant attachment rejected stage=filename")
        raise HTTPException(422, detail={"code": "ATTACHMENT_UNAVAILABLE"})
    return name


async def _user_upload_bytes(
    ref: str, name: str, user: UserContext, *, metadata: dict[str, str] | None = None,
) -> bytes:
    try:
        storage = get_file_storage()
    except RuntimeError:
        # The upload endpoint may run with a separately initialized service.
        # Use its configured backend, including the local base path and key
        # prefix, instead of assuming the legacy ./uploads fallback.
        storage = FileStorageService(StorageConfig.from_env())
    try:
        # The key is constructed only after the exact owner prefix and
        # filename grammar above have been checked.
        key = ref.removeprefix("/")
        local_metadata_path: Path | None = None
        try:
            content = await storage.download_file(key)
        except FileNotFoundError:
            # Local streamed uploads predate key-prefix handling and live at
            # base_path/uploads/... even when the backend reads dev/uploads/....
            # The legacy upload endpoint also has a separate local fallback.
            # Both paths remain inside the validated owner scope.
            if storage.config.backend != StorageBackend.LOCAL:
                raise
            root = Path(storage.config.local_base_path).resolve()
            path = (root / key).resolve()
            if path.is_relative_to(root) and path.is_file() and path.stat().st_size <= _MAX_BYTES:
                content = await asyncio.to_thread(path.read_bytes)
                candidate = path.with_suffix(path.suffix + ".meta").resolve()
                if candidate.is_relative_to(root):
                    local_metadata_path = candidate
            else:
                legacy_root = get_user_uploads_path(user.user_id, user.tenant_id).resolve()
                path = (legacy_root / name).resolve()
                if (
                    not path.is_relative_to(legacy_root)
                    or not path.is_file()
                    or path.stat().st_size > _MAX_BYTES
                ):
                    raise FileNotFoundError from None
                content = await asyncio.to_thread(path.read_bytes)
                candidate = path.with_suffix(path.suffix + ".meta").resolve()
                if candidate.is_relative_to(legacy_root):
                    local_metadata_path = candidate
        if metadata is not None:
            # Preserve upload display metadata once at binding, never HEAD on history.
            # Metadata failure must not turn valid bytes into an unavailable upload.
            try:
                original: object = None
                if local_metadata_path is not None and local_metadata_path.is_file() and local_metadata_path.stat().st_size <= 65536:
                    value = json.loads(await asyncio.to_thread(local_metadata_path.read_text))
                    if isinstance(value, dict):
                        original = value.get("original_filename")
                elif getattr(storage, "_backend", None) is not None:
                    info = await storage._backend.head(key)
                    if info is not None:
                        original = info.metadata.get("original_filename")
                if (isinstance(original, str) and 0 < len(original) <= 255
                    and "/" not in original and "\\" not in original
                    and original not in {".", ".."} and not any(ord(char) < 32 or ord(char) == 127 for char in original)):
                    metadata["original_filename"] = original
            except Exception:  # optional display metadata; validated bytes remain usable
                pass
    except Exception as exc:
        logger.warning(
            "Assistant attachment rejected stage=storage_read error_type=%s ref_sha256=%s",
            type(exc).__name__, hashlib.sha256(ref.encode()).hexdigest()[:12],
        )
        raise HTTPException(422, detail={"code": "ATTACHMENT_UNAVAILABLE"}) from None
    if not content:
        raise HTTPException(422, detail={"code": "ATTACHMENT_UNAVAILABLE"})
    if len(content) > _MAX_BYTES:
        raise HTTPException(413, detail={"code": "ATTACHMENT_TOO_LARGE"})
    return content


async def _require_vision_model(
    request: Request, user: UserContext, model_id: str, provider_id: str | None = None,
) -> None:
    service = assistant_model_service(request)
    if service is None or not callable(getattr(service, "get_model", None)):
        raise HTTPException(503, detail={"code": "MODEL_CAPABILITY_UNAVAILABLE"})
    try:
        model = await service.get_model(
            user.tenant_id, model_id, **({"provider_id": provider_id} if provider_id else {})
        )
    except Exception:
        raise HTTPException(503, detail={"code": "MODEL_CAPABILITY_UNAVAILABLE"}) from None
    if not isinstance(model, dict) or not model.get("is_enabled") or not model.get("supports_vision"):
        raise HTTPException(409, detail={"code": "VISION_MODEL_REQUIRED"})


async def bind_assistant_attachment_refs(
    request: Request,
    user: UserContext,
    *,
    session_id: str,
    model_id: str,
    refs: list[str],
    provider_id: str | None = None,
) -> list[str]:
    """Return only verified, session-owned attachment IDs for one turn."""
    if not refs:
        return []
    artifacts = get_artifact_storage()
    if artifacts is None:
        raise HTTPException(503, detail={"code": "ATTACHMENT_STORAGE_UNAVAILABLE"})
    bound: list[str] = []
    vision_checked = False
    for ref in refs:
        if _ARTIFACT_ID.fullmatch(ref):
            artifact = await artifacts.get_artifact(ref, tenant_id=user.tenant_id, user_id=user.user_id)
            if not artifact or artifact.session_id != session_id or artifact.size_bytes <= 0:
                raise HTTPException(422, detail={"code": "ATTACHMENT_UNAVAILABLE"})
            is_image = str(artifact.mime_type).startswith("image/")
            artifact_id = ref
        else:
            name = _upload_name(ref, user)
            fmt = name.rsplit(".", 1)[-1]
            if fmt not in _SUPPORTED_FORMATS:
                raise HTTPException(415, detail={"code": "ATTACHMENT_FORMAT_UNSUPPORTED"})
            is_image = fmt in _IMAGE_FORMATS
            if is_image and not vision_checked:
                await _require_vision_model(request, user, model_id, provider_id)
                vision_checked = True
            upload_metadata: dict[str, str] = {}
            content = await _user_upload_bytes(ref, name, user, metadata=upload_metadata)
            content_hash = hashlib.sha256(content).hexdigest()
            identity = f"{user.tenant_id}:{user.user_id}:{session_id}:{ref}:{content_hash}"
            artifact_id = f"art_{hashlib.sha256(identity.encode()).hexdigest()[:16]}"
            try:
                await artifacts.create_artifact(
                    session_id=session_id, tenant_id=user.tenant_id, user_id=user.user_id,
                    type="image" if is_image else "file", format=fmt,
                    title=name, filename=name, content=content, source="user",
                    metadata={"content_sha256": content_hash, "upload_ref": ref, **upload_metadata},
                    artifact_id=artifact_id,
                )
            except Exception:
                raise HTTPException(503, detail={"code": "ATTACHMENT_BIND_FAILED"}) from None
        if is_image and not vision_checked:
            await _require_vision_model(request, user, model_id, provider_id)
            vision_checked = True
        if artifact_id not in bound:
            bound.append(artifact_id)
    return bound


async def selected_image_inputs(
    user: UserContext, *, session_id: str, refs: list[str]
) -> list[str]:
    """Read selected image bytes transiently for the model input of this turn.

    Raw pixels are excluded from the durable launch snapshot and tool events;
    the Runtime receives only this bounded, authenticated start request.
    """
    artifacts = get_artifact_storage()
    if artifacts is None:
        raise HTTPException(503, detail={"code": "ATTACHMENT_STORAGE_UNAVAILABLE"})
    images: list[str] = []
    for ref in refs:
        artifact = await artifacts.get_artifact(ref, tenant_id=user.tenant_id, user_id=user.user_id)
        if not artifact or artifact.session_id != session_id or artifact.size_bytes <= 0:
            raise HTTPException(422, detail={"code": "ATTACHMENT_UNAVAILABLE"})
        mime = str(artifact.mime_type).lower()
        if mime not in _IMAGE_MIMES:
            continue
        if len(images) >= 5 or artifact.size_bytes > _MAX_IMAGE_BYTES:
            raise HTTPException(413, detail={"code": "IMAGE_INPUT_TOO_LARGE"})
        try:
            content = await artifacts.download_artifact(ref)
        except Exception:
            content = None
        if not isinstance(content, bytes) or len(content) != artifact.size_bytes:
            raise HTTPException(422, detail={"code": "ATTACHMENT_UNAVAILABLE"})
        if not content.startswith(_IMAGE_MIMES[mime]) or (mime == "image/webp" and content[8:12] != b"WEBP"):
            raise HTTPException(422, detail={"code": "IMAGE_INPUT_INVALID"})
        images.append(f"data:{mime};base64,{base64.b64encode(content).decode('ascii')}")
    return images
