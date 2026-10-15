"""Scope-bound, fail-closed attachment metadata/content broker.

The capability worker receives only this route's bounded response. Storage
backends and any provider credentials remain Gateway-owned. Office documents
are parsed as bounded OOXML archives; unsafe archives, unsupported formats and
parser failures are rejected rather than returned as partial content.
"""

from __future__ import annotations

import hashlib
import hmac
import io
import os
import re
import zipfile
from typing import Any, Literal
from xml.etree import ElementTree

from ai_gateway_contracts.capability_proof import CapabilityProofError, verify_capability_proof
from ai_gateway_core.storage import get_artifact_storage
from fastapi import APIRouter, Body, Header, HTTPException
from pydantic import BaseModel, ConfigDict, Field

router = APIRouter(prefix="/internal/v2/agent-capabilities", tags=["internal-agent-capabilities"])

_PATH = "/internal/v2/agent-capabilities/attachments/read"
_ARTIFACT_ID = re.compile(r"^art_[A-Za-z0-9]{8,64}$")
_SHA256 = re.compile(r"^[0-9a-f]{64}$")
_MAX_BYTES = 32 * 1024 * 1024
_MAX_RESPONSE_BYTES = 8 * 1024 * 1024
_MAX_CONTENT_CHARS = 500_000
_MAX_ZIP_MEMBERS = 256
_MAX_ZIP_MEMBER_BYTES = 16 * 1024 * 1024
_MAX_ZIP_TOTAL_BYTES = 64 * 1024 * 1024
_MAX_ZIP_RATIO = 100
_OOXML_MIMES = {
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document": "docx",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet": "xlsx",
    "application/vnd.openxmlformats-officedocument.presentationml.presentation": "pptx",
}
_IMAGE_MIMES = {"image/png", "image/jpeg", "image/gif", "image/webp"}


class AttachmentReadRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    attachment_id: str = Field(min_length=1, max_length=128)
    operation: Literal["metadata", "content"] = "content"
    max_chars: int = Field(default=100_000, ge=1, le=_MAX_CONTENT_CHARS)


def _scope(value: str | None, name: str) -> str:
    if not value or len(value) > 255 or any(ord(char) < 32 for char in value):
        raise HTTPException(status_code=403, detail=f"{name} is invalid")
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
    scope = (_scope(tenant_id, "tenant"), _scope(user_id, "user"), _scope(session_id, "session"))
    if not execution_id or not run_id or not proof:
        raise HTTPException(status_code=401, detail="capability proof required")
    try:
        verify_capability_proof(
            os.getenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", ""),
            proof,
            method="POST",
            path=_PATH,
            body=body,
            tenant_id=scope[0],
            user_id=scope[1],
            session_id=scope[2],
            execution_id=execution_id,
            run_id=run_id,
        )
    except CapabilityProofError:
        raise HTTPException(status_code=401, detail="capability proof invalid") from None
    return scope


def _safe_zip_members(raw: bytes) -> dict[str, bytes]:
    try:
        archive = zipfile.ZipFile(io.BytesIO(raw), "r")
        infos = archive.infolist()
        if not infos or len(infos) > _MAX_ZIP_MEMBERS:
            raise ValueError("zip member limit")
        names: set[str] = set()
        total = 0
        for info in infos:
            name = info.filename.replace("\\", "/")
            parts = name.split("/")
            mode = (info.external_attr >> 16) & 0o170000
            if (
                not name
                or name.startswith("/")
                or any(part in {"", ".", ".."} for part in parts)
                or "\x00" in name
                or mode == 0o120000
                or name in names
                or info.is_dir()
                or info.file_size > _MAX_ZIP_MEMBER_BYTES
                or info.compress_size == 0 and info.file_size > 0
                or info.file_size > max(1, info.compress_size) * _MAX_ZIP_RATIO
            ):
                raise ValueError("unsafe zip member")
            total += info.file_size
            if total > _MAX_ZIP_TOTAL_BYTES:
                raise ValueError("zip expansion limit")
            names.add(name)
        result: dict[str, bytes] = {}
        for info in infos:
            content = archive.read(info)
            if len(content) != info.file_size:
                raise ValueError("zip member size mismatch")
            result[info.filename.replace("\\", "/")] = content
        return result
    except (OSError, RuntimeError, ValueError, zipfile.BadZipFile, zipfile.LargeZipFile) as exc:
        raise ValueError("attachment archive is unsafe") from exc


def _xml_text(raw: bytes) -> str:
    if len(raw) > _MAX_ZIP_MEMBER_BYTES:
        raise ValueError("XML member too large")
    # OOXML text extraction needs no DTD/entity support. Reject both before
    # the stdlib streaming parser, then bound depth, node count and text size.
    if b"<!DOCTYPE" in raw.upper() or b"<!ENTITY" in raw.upper():
        raise ValueError("XML declarations are unsupported")
    try:
        raw.decode("utf-8", errors="strict")
        parser = ElementTree.XMLPullParser(events=("start", "end"))
        depth = nodes = characters = 0
        parts: list[str] = []
        for offset in range(0, len(raw), 8192):
            parser.feed(raw[offset:offset + 8192])
            for event, element in parser.read_events():
                if event == "start":
                    depth += 1
                    nodes += 1
                    if depth > 64 or nodes > 100_000:
                        raise ValueError("XML structure limit")
                else:
                    text = (element.text or "").strip()
                    if text:
                        characters += len(text)
                        if characters > _MAX_CONTENT_CHARS:
                            raise ValueError("XML text limit")
                        parts.append(text)
                    element.clear()
                    depth -= 1
        parser.close()
        if depth != 0:
            raise ValueError("XML structure invalid")
    except (UnicodeDecodeError, ElementTree.ParseError) as exc:
        raise ValueError("XML cannot be safely parsed") from exc
    return " ".join(parts)


def _image_metadata(raw: bytes, mime_type: str) -> dict[str, Any]:
    if mime_type == "image/png":
        if len(raw) < 24 or raw[:8] != b"\x89PNG\r\n\x1a\n" or raw[12:16] != b"IHDR":
            raise ValueError("invalid PNG")
        return {"width": int.from_bytes(raw[16:20], "big"), "height": int.from_bytes(raw[20:24], "big"), "format": "png"}
    if mime_type == "image/gif":
        if len(raw) < 10 or raw[:6] not in {b"GIF87a", b"GIF89a"}:
            raise ValueError("invalid GIF")
        return {"width": int.from_bytes(raw[6:8], "little"), "height": int.from_bytes(raw[8:10], "little"), "format": "gif"}
    if mime_type == "image/jpeg":
        if len(raw) < 4 or raw[:2] != b"\xff\xd8":
            raise ValueError("invalid JPEG")
        offset = 2
        while offset + 9 < len(raw):
            if raw[offset] != 0xFF:
                offset += 1
                continue
            marker = raw[offset + 1]
            offset += 2
            if marker in {0xD8, 0xD9}:
                continue
            length = int.from_bytes(raw[offset : offset + 2], "big")
            if length < 2 or offset + length > len(raw):
                raise ValueError("invalid JPEG segment")
            if marker in set(range(0xC0, 0xC4)) | set(range(0xC5, 0xC8)) | set(range(0xC9, 0xCC)) | set(range(0xCD, 0xD0)):
                return {"width": int.from_bytes(raw[offset + 5 : offset + 7], "big"), "height": int.from_bytes(raw[offset + 3 : offset + 5], "big"), "format": "jpeg"}
            offset += length
        raise ValueError("JPEG dimensions missing")
    if mime_type == "image/webp":
        if len(raw) < 30 or raw[:4] != b"RIFF" or raw[8:12] != b"WEBP":
            raise ValueError("invalid WebP")
        if raw[12:16] == b"VP8X":
            return {"width": 1 + int.from_bytes(raw[24:27], "little"), "height": 1 + int.from_bytes(raw[27:30], "little"), "format": "webp"}
        raise ValueError("unsupported WebP variant")
    raise ValueError("unsupported image")


def _extract_content(raw: bytes, filename: str, mime_type: str, max_chars: int) -> tuple[dict[str, Any], str | None, bool]:
    suffix = filename.rsplit(".", 1)[-1].lower() if "." in filename else ""
    if mime_type == "text/plain" or suffix in {"txt", "md", "csv", "json"}:
        text = raw.decode("utf-8", errors="strict")
        metadata = {"format": suffix or "txt", "encoding": "utf-8", "characters": len(text)}
    elif mime_type in _IMAGE_MIMES:
        return _image_metadata(raw, mime_type), None, False
    elif mime_type == "application/pdf" or suffix == "pdf":
        try:
            from pypdf import PdfReader  # type: ignore[import-not-found]
            reader = PdfReader(io.BytesIO(raw), strict=True)
            if len(reader.pages) > 100:
                raise ValueError("PDF page limit")
            text = "\n".join(page.extract_text() or "" for page in reader.pages)
            metadata = {"format": "pdf", "pages": len(reader.pages)}
        except Exception as exc:
            raise ValueError("PDF cannot be safely parsed") from exc
    elif mime_type in _OOXML_MIMES or suffix in {"docx", "xlsx", "pptx"}:
        members = _safe_zip_members(raw)
        expected = _OOXML_MIMES.get(mime_type, suffix)
        if expected == "docx":
            names = ["word/document.xml"]
        elif expected == "xlsx":
            names = [name for name in members if name.startswith("xl/worksheets/") and name.endswith(".xml")]
        else:
            names = [name for name in members if name.startswith("ppt/slides/") and name.endswith(".xml")]
        if not names:
            raise ValueError("OOXML main part missing")
        text = "\n".join(_xml_text(members[name]) for name in sorted(names))
        metadata = {"format": expected, "parts": len(names)}
    else:
        raise ValueError("unsupported attachment format")
    truncated = len(text) > max_chars
    return metadata, text[:max_chars], truncated


@router.post("/attachments/read")
async def read_attachment(
    payload: AttachmentReadRequest = Body(...),
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
        body=payload.model_dump(mode="json"),
    )
    if not _ARTIFACT_ID.fullmatch(payload.attachment_id):
        raise HTTPException(status_code=404, detail="attachment not found")
    storage = get_artifact_storage()
    getter = getattr(storage, "get_artifact", None) if storage else None
    if not callable(getter):
        raise HTTPException(status_code=404, detail="attachment not found")
    try:
        artifact = await getter(payload.attachment_id, tenant_id=tenant_id, user_id=user_id)
        if not artifact or artifact.session_id != session_id or artifact.size_bytes <= 0 or artifact.size_bytes > _MAX_BYTES:
            raise ValueError("attachment scope or size invalid")
        backend = getattr(storage, "_backend", None)
        raw = await backend.download(artifact.storage_key) if backend and callable(getattr(backend, "download", None)) else None
        if not isinstance(raw, bytes) or len(raw) != artifact.size_bytes or len(raw) > _MAX_BYTES:
            raise ValueError("attachment content unavailable")
        mime_type = str(artifact.mime_type or "application/octet-stream").lower()
        filename = str(artifact.filename or "")
        if not filename or any(ord(char) < 32 for char in filename) or "/" in filename or "\\" in filename:
            raise ValueError("attachment filename invalid")
        metadata, content, truncated = _extract_content(raw, filename, mime_type, payload.max_chars)
        digest = hashlib.sha256(raw).hexdigest()
        result = {
            "attachment_id": payload.attachment_id,
            "filename": filename,
            "mime_type": mime_type,
            "format": metadata.get("format", mime_type.split("/")[-1]),
            "size_bytes": len(raw),
            "sha256": digest,
            "metadata": {**dict(getattr(artifact, "metadata", None) or {}), **metadata},
            "content": content if payload.operation == "content" else None,
            "truncated": truncated if payload.operation == "content" else False,
        }
        if len(str(result).encode("utf-8")) > _MAX_RESPONSE_BYTES:
            raise ValueError("attachment response too large")
        return result
    except Exception:
        # Do not leak parser/backend details or return a partially parsed file.
        raise HTTPException(status_code=422, detail="attachment cannot be safely parsed") from None
