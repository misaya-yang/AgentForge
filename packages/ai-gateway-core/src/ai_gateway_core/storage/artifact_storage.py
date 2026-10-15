"""
Artifact Storage Service.

Provides storage and database operations for conversation artifacts.
Artifacts include: images, documents, charts, code files, etc.
"""

from __future__ import annotations

import hashlib
import logging
import re
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from typing import TYPE_CHECKING, Any

import asyncpg

from .image_storage import (
    BaseStorageBackend,
    LocalStorageBackend,
    OSSStorageBackend,
    S3StorageBackend,
    StorageBackend,
    StorageConfig,
    StorageObjectInfo,
)

if TYPE_CHECKING:
    from ai_gateway_core.persistence.database import DatabaseStorage


logger = logging.getLogger(__name__)


@dataclass
class ArtifactInfo:
    """Artifact metadata."""

    artifact_id: str
    session_id: str
    tenant_id: str
    user_id: str
    type: str  # image, document, chart, code, file
    format: str  # png, pdf, docx, md, csv, json, etc.
    title: str
    filename: str
    storage_key: str
    size_bytes: int = 0
    mime_type: str | None = None
    source: str = "ai"  # ai | user | code_execution
    message_id: str | None = None
    metadata: dict[str, Any] = field(default_factory=dict)
    created_at: datetime | None = None
    updated_at: datetime | None = None
    # ---- First-class image variant fields (added by 001_image_session_artifacts) ----
    # All optional / nullable so non-image artifacts and pre-migration rows still load.
    variant: str = "raw"  # 'raw' | 'display' | 'thumbnail'
    parent_artifact_id: str | None = None  # For 'display'/'thumbnail' rows → raw.artifact_id
    turn_id: str | None = None
    owner_scope: str | None = None
    width: int | None = None
    height: int | None = None
    provider: str | None = None
    model_id: str | None = None
    prompt: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for API response."""
        return {
            "artifact_id": self.artifact_id,
            "session_id": self.session_id,
            "tenant_id": self.tenant_id,
            "user_id": self.user_id,
            "type": self.type,
            "format": self.format,
            "title": self.title,
            "filename": self.filename,
            "storage_key": self.storage_key,
            "size_bytes": self.size_bytes,
            "mime_type": self.mime_type,
            "source": self.source,
            "message_id": self.message_id,
            "metadata": self.metadata,
            "created_at": self.created_at.isoformat() if self.created_at else None,
            "updated_at": self.updated_at.isoformat() if self.updated_at else None,
            "variant": self.variant,
            "parent_artifact_id": self.parent_artifact_id,
            "turn_id": self.turn_id,
            "owner_scope": self.owner_scope,
            "width": self.width,
            "height": self.height,
            "provider": self.provider,
            "model_id": self.model_id,
            "prompt": self.prompt,
        }


# MIME type mappings
FORMAT_TO_MIME = {
    # Images
    "png": "image/png",
    "jpg": "image/jpeg",
    "jpeg": "image/jpeg",
    "gif": "image/gif",
    "webp": "image/webp",
    "svg": "image/svg+xml",
    # Documents
    "pdf": "application/pdf",
    "docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "doc": "application/msword",
    "xlsx": "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    "xls": "application/vnd.ms-excel",
    "pptx": "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    # Text
    "md": "text/markdown",
    "txt": "text/plain",
    "csv": "text/csv",
    "json": "application/json",
    "html": "text/html",
    # Code
    "py": "text/x-python",
    "js": "text/javascript",
    "ts": "text/typescript",
}


def get_mime_type(format: str) -> str:
    """Get MIME type from format."""
    return FORMAT_TO_MIME.get(format.lower(), "application/octet-stream")


class ArtifactStorageService:
    """
    Service for storing and managing conversation artifacts.

    Handles both file storage (S3/OSS/local) and database metadata.
    """

    def __init__(
        self,
        config: StorageConfig,
        database: DatabaseStorage | None = None,
    ):
        self.config = config
        self.database = database
        self._backend = self._create_backend()

    def _create_backend(self) -> BaseStorageBackend:
        """Create storage backend based on configuration."""
        kp = self.config.key_prefix
        if self.config.backend == StorageBackend.S3:
            return S3StorageBackend(
                bucket=self.config.s3_bucket,
                region=self.config.s3_region,
                access_key=self.config.s3_access_key,
                secret_key=self.config.s3_secret_key,
                endpoint_url=self.config.s3_endpoint_url,
                key_prefix=kp,
            )
        elif self.config.backend == StorageBackend.OSS:
            return OSSStorageBackend(
                bucket=self.config.oss_bucket,
                endpoint=self.config.oss_endpoint,
                access_key=self.config.oss_access_key,
                secret_key=self.config.oss_secret_key,
                key_prefix=kp,
            )
        else:
            return LocalStorageBackend(self.config.local_base_path, key_prefix=kp)

    def supports_scoped_artifact_reads(self) -> bool:
        """Return whether host-owned scope metadata can be revalidated."""

        return bool(self.database and self.database._pool)

    @staticmethod
    def generate_artifact_id() -> str:
        """Generate unique artifact ID."""
        return f"art_{uuid.uuid4().hex[:16]}"

    @staticmethod
    def _generate_storage_key(
        tenant_id: str,
        session_id: str,
        artifact_id: str,
        filename: str,
    ) -> str:
        """Generate storage key for artifact."""
        # Sanitize filename
        safe_filename = "".join(c for c in filename if c.isalnum() or c in "._-")
        return f"artifacts/{tenant_id}/{session_id}/{artifact_id}_{safe_filename}"

    @staticmethod
    def image_blob_storage_key(owner_scope: str, blob_id: str, filename: str) -> str:
        """Build an opaque, traversal-safe key for a direct image upload."""

        if not re.fullmatch(r"iblob_[0-9a-f]{24}", blob_id):
            raise ValueError("image blob id is invalid")
        if not owner_scope or any(ord(character) < 32 for character in owner_scope):
            raise ValueError("image blob owner is invalid")
        owner_hash = hashlib.sha256(owner_scope.encode("utf-8")).hexdigest()[:24]
        safe_filename = "".join(
            character if character.isalnum() or character in "._-" else "_"
            for character in filename.replace("\x00", "")
        ).strip("._")
        safe_filename = safe_filename[:180] or "reference.png"
        return f"image-blobs/{owner_hash}/{blob_id}/{safe_filename}"

    async def create_image_blob_upload_url(
        self,
        *,
        owner_scope: str,
        blob_id: str,
        filename: str,
        mime_type: str,
        expiry_seconds: int = 900,
    ) -> tuple[str, dict[str, str] | None]:
        """Create a direct-upload URL without exposing the storage backend."""

        if not mime_type.startswith("image/") or not 60 <= expiry_seconds <= 3600:
            raise ValueError("image blob upload parameters are invalid")
        storage_key = self.image_blob_storage_key(owner_scope, blob_id, filename)
        upload = await self._backend.generate_presigned_upload_url(
            key=storage_key,
            content_type=mime_type,
            expiry_seconds=expiry_seconds,
            metadata={
                "blob-id": blob_id,
                "owner-scope-sha256": hashlib.sha256(owner_scope.encode("utf-8")).hexdigest(),
            },
        )
        return storage_key, upload

    async def inspect_image_blob_object(
        self,
        storage_key: str,
        *,
        max_bytes: int,
    ) -> StorageObjectInfo | None:
        """Head one broker-created image blob and reject oversized objects."""

        if (
            not storage_key.startswith("image-blobs/")
            or ".." in storage_key
            or "\x00" in storage_key
            or not 1 <= max_bytes <= 32 * 1024 * 1024
        ):
            raise ValueError("image blob storage key is invalid")
        info = await self._backend.head(storage_key)
        if info is not None and (info.size_bytes <= 0 or info.size_bytes > max_bytes):
            raise ValueError("image blob exceeds size limit")
        return info

    async def store_image_blob_object(
        self,
        storage_key: str,
        *,
        content: bytes,
        mime_type: str,
        metadata: dict[str, str] | None = None,
        max_bytes: int,
    ) -> None:
        """Persist a bounded broker-created image blob without exposing credentials."""

        if (
            not content
            or len(content) > max_bytes
            or not mime_type.startswith("image/")
        ):
            raise ValueError("image blob content is invalid")
        # Reuse the same key and size policy enforced by the read path.
        if (
            not storage_key.startswith("image-blobs/")
            or ".." in storage_key
            or "\x00" in storage_key
            or not 1 <= max_bytes <= 32 * 1024 * 1024
        ):
            raise ValueError("image blob storage key is invalid")
        await self._backend.upload(
            key=storage_key,
            content=content,
            content_type=mime_type,
            metadata=metadata,
        )

    async def read_image_blob_object(self, storage_key: str, *, max_bytes: int) -> bytes:
        """Read a previously headed image blob while preserving the byte bound."""

        info = await self.inspect_image_blob_object(storage_key, max_bytes=max_bytes)
        if info is None:
            raise FileNotFoundError("image blob object not found")
        content = await self._backend.download(storage_key)
        if len(content) != info.size_bytes or len(content) > max_bytes:
            raise ValueError("image blob size changed during verification")
        return content

    async def create_artifact(
        self,
        session_id: str,
        tenant_id: str,
        user_id: str,
        type: str,
        format: str,
        title: str,
        filename: str,
        content: bytes,
        source: str = "ai",
        message_id: str | None = None,
        metadata: dict[str, Any] | None = None,
        *,
        variant: str = "raw",
        parent_artifact_id: str | None = None,
        turn_id: str | None = None,
        owner_scope: str | None = None,
        width: int | None = None,
        height: int | None = None,
        provider: str | None = None,
        model_id: str | None = None,
        prompt: str | None = None,
        artifact_id: str | None = None,
    ) -> ArtifactInfo:
        """
        Create a new artifact: upload to storage and save metadata to database.

        Args:
            session_id: Conversation session ID
            tenant_id: Tenant ID
            user_id: User ID
            type: Artifact type (image, document, chart, code, file)
            format: File format (png, pdf, docx, etc.)
            title: Display title
            filename: Original filename
            content: File content bytes
            source: Source of artifact (ai, user, code_execution)
            message_id: Optional associated message ID
            metadata: Optional additional metadata

        Returns:
            ArtifactInfo with created artifact details
        """
        idempotent_artifact = artifact_id is not None
        artifact_id = artifact_id or self.generate_artifact_id()
        if not re.fullmatch(r"art_[0-9a-f]{16}", artifact_id):
            raise ValueError("artifact_id is invalid")
        storage_key = self._generate_storage_key(tenant_id, session_id, artifact_id, filename)
        mime_type = get_mime_type(format)
        artifact_metadata = dict(metadata or {})
        if idempotent_artifact:
            content_sha256 = artifact_metadata.get("content_sha256")
            if not isinstance(content_sha256, str) or not re.fullmatch(
                r"[0-9a-f]{64}", content_sha256
            ):
                raise ValueError("idempotent artifact requires content_sha256")
            if self.database and self.database._pool:
                existing = await self.get_artifact(artifact_id)
                if existing is not None:
                    if (
                        existing.tenant_id != tenant_id
                        or existing.user_id != user_id
                        or existing.session_id != session_id
                        or existing.type != type
                        or existing.format != format
                        or existing.title != title
                        or existing.filename != filename
                        or existing.storage_key != storage_key
                        or existing.size_bytes != len(content)
                        or existing.mime_type != mime_type
                        or existing.source != source
                        or existing.message_id != message_id
                        or existing.variant != variant
                        or existing.parent_artifact_id != parent_artifact_id
                        or existing.turn_id != turn_id
                        or existing.owner_scope != owner_scope
                        or existing.width != width
                        or existing.height != height
                        or existing.provider != provider
                        or existing.model_id != model_id
                        or existing.prompt != prompt
                        or existing.metadata.get("content_sha256") != content_sha256
                        or any(
                            existing.metadata.get(key) != value
                            for key, value in artifact_metadata.items()
                        )
                    ):
                        raise ValueError("artifact idempotency conflict")
                    return existing

        # Upload to storage
        await self._backend.upload(
            key=storage_key,
            content=content,
            content_type=mime_type,
            metadata={
                "artifact_id": artifact_id,
                "session_id": session_id,
                "tenant_id": tenant_id,
                "type": type,
            },
        )

        logger.info(f"Uploaded artifact {artifact_id} to {storage_key} ({len(content)} bytes)")

        # Save to database
        artifact = ArtifactInfo(
            artifact_id=artifact_id,
            session_id=session_id,
            tenant_id=tenant_id,
            user_id=user_id,
            type=type,
            format=format,
            title=title,
            filename=filename,
            storage_key=storage_key,
            size_bytes=len(content),
            mime_type=mime_type,
            source=source,
            message_id=message_id,
            metadata=artifact_metadata,
            created_at=datetime.now(timezone.utc),
            updated_at=datetime.now(timezone.utc),
            variant=variant,
            parent_artifact_id=parent_artifact_id,
            turn_id=turn_id,
            owner_scope=owner_scope,
            width=width,
            height=height,
            provider=provider,
            model_id=model_id,
            prompt=prompt,
        )

        if self.database and self.database._pool:
            await self._save_artifact_to_db(artifact)

        return artifact

    async def _save_artifact_to_db(self, artifact: ArtifactInfo) -> None:
        """Save artifact metadata to database.

        Tries the full insert first (post-001_image_session_artifacts schema).
        Falls back to the minimal-column insert if the new columns aren't present
        (e.g. running against a DB where the migration hasn't been applied yet).
        New fields default to NULL/'raw' there, so legacy rows still satisfy the
        post-migration NOT NULL constraint on `variant` (it has a server-side
        DEFAULT 'raw').
        """
        import json

        async with self.database._pool.acquire() as conn:
            try:
                await conn.execute(
                    """
                    INSERT INTO assistant.artifacts (
                        artifact_id, session_id, message_id, tenant_id, user_id,
                        type, format, title, filename, storage_key, size_bytes,
                        mime_type, source, metadata, created_at, updated_at,
                        variant, parent_artifact_id, turn_id, owner_scope,
                        width, height, provider, model_id, prompt
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                        $17, $18, $19, $20, $21, $22, $23, $24, $25
                    )
                    ON CONFLICT (artifact_id) DO UPDATE SET
                        updated_at = EXCLUDED.updated_at
                    """,
                    artifact.artifact_id,
                    artifact.session_id,
                    artifact.message_id,
                    artifact.tenant_id,
                    artifact.user_id,
                    artifact.type,
                    artifact.format,
                    artifact.title,
                    artifact.filename,
                    artifact.storage_key,
                    artifact.size_bytes,
                    artifact.mime_type,
                    artifact.source,
                    json.dumps(artifact.metadata),
                    artifact.created_at,
                    artifact.updated_at,
                    artifact.variant,
                    artifact.parent_artifact_id,
                    artifact.turn_id,
                    artifact.owner_scope,
                    artifact.width,
                    artifact.height,
                    artifact.provider,
                    artifact.model_id,
                    artifact.prompt,
                )
            except asyncpg.UndefinedColumnError as exc:
                # Schema pre-dates 001_image_session_artifacts. Only this exact
                # error class triggers the legacy fallback — any other failure
                # (constraint, dtype, network) must surface so we don't silently
                # write a row missing owner_scope (which would let a later
                # cross-tenant download read it). Owner-scope is embedded in
                # metadata as defense-in-depth so a future read can still
                # validate even if the column doesn't exist yet.
                logger.warning(
                    "Full artifact insert hit UndefinedColumnError (%s) — falling back to "
                    "legacy column list for artifact %s. Apply migration "
                    "assistant/001_image_session_artifacts.",
                    exc,
                    artifact.artifact_id,
                )
                legacy_metadata = dict(artifact.metadata or {})
                legacy_metadata["__owner_scope"] = artifact.owner_scope
                legacy_metadata["__variant"] = artifact.variant
                legacy_metadata["__parent_artifact_id"] = artifact.parent_artifact_id
                legacy_metadata["__turn_id"] = artifact.turn_id
                await conn.execute(
                    """
                    INSERT INTO assistant.artifacts (
                        artifact_id, session_id, message_id, tenant_id, user_id,
                        type, format, title, filename, storage_key, size_bytes,
                        mime_type, source, metadata, created_at, updated_at
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
                    ON CONFLICT (artifact_id) DO UPDATE SET
                        updated_at = EXCLUDED.updated_at
                    """,
                    artifact.artifact_id,
                    artifact.session_id,
                    artifact.message_id,
                    artifact.tenant_id,
                    artifact.user_id,
                    artifact.type,
                    artifact.format,
                    artifact.title,
                    artifact.filename,
                    artifact.storage_key,
                    artifact.size_bytes,
                    artifact.mime_type,
                    artifact.source,
                    json.dumps(legacy_metadata),
                    artifact.created_at,
                    artifact.updated_at,
                )

    async def get_artifact(
        self,
        artifact_id: str,
        *,
        owner_scope: str | None = None,
        tenant_id: str | None = None,
        user_id: str | None = None,
    ) -> ArtifactInfo | None:
        """Get artifact by ID.

        ``owner_scope``: when supplied, returns ``None`` if the artifact's
        ``owner_scope`` doesn't match. This is the post-image-redesign auth
        check for the new variant/download endpoints. Legacy callers pass
        ``None`` and continue to use the existing tenant_id/user_id check
        in their own code paths (kept for backward compatibility).
        """
        if not self.database or not self.database._pool:
            return None

        async with self.database._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM assistant.artifacts WHERE artifact_id = $1",
                artifact_id,
            )

        if not row:
            return None

        artifact = self._row_to_artifact(row)
        if not self._matches_requested_owner(
            artifact,
            owner_scope=owner_scope,
            tenant_id=tenant_id,
            user_id=user_id,
        ):
            return None
        return artifact

    @staticmethod
    def _matches_requested_owner(
        artifact: ArtifactInfo,
        *,
        owner_scope: str | None,
        tenant_id: str | None,
        user_id: str | None,
    ) -> bool:
        """Fail closed for scoped reads, including legacy NULL owner rows."""

        if owner_scope is None:
            return True
        if artifact.owner_scope:
            return (
                artifact.owner_scope == owner_scope
                and (tenant_id is None or artifact.tenant_id == tenant_id)
                and (user_id is None or artifact.user_id == user_id)
            )
        return bool(
            tenant_id
            and user_id
            and artifact.tenant_id == tenant_id
            and artifact.user_id == user_id
        )

    async def read_artifact_scoped(
        self,
        artifact_id: str,
        *,
        tenant_id: str,
        session_id: str,
        user_id: str,
        max_bytes: int,
        expected_source: str = "tool_output_spill",
    ) -> tuple[ArtifactInfo, bytes] | None:
        """Read one artifact only when every host-owned scope field matches."""

        allowed_sources = {"tool_output_spill", "sse_event_spill"}
        if (
            not all((artifact_id, tenant_id, session_id, user_id))
            or max_bytes <= 0
            or expected_source not in allowed_sources
        ):
            return None
        if not self.database or not self.database._pool:
            return None
        async with self.database._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT * FROM assistant.artifacts
                WHERE artifact_id = $1 AND tenant_id = $2
                  AND session_id = $3 AND user_id = $4
                  AND source = $5
                  AND turn_id IS NOT NULL
                """,
                artifact_id,
                tenant_id,
                session_id,
                user_id,
                expected_source,
            )
        if not row:
            return None
        artifact = self._row_to_artifact(row)
        if artifact.size_bytes < 0 or artifact.size_bytes > max_bytes:
            return None
        content = await self._backend.download(artifact.storage_key)
        if content is None or len(content) > max_bytes:
            return None
        return artifact, content

    async def find_variant(
        self,
        parent_or_self_artifact_id: str,
        variant: str,
    ) -> ArtifactInfo | None:
        """Resolve a sibling variant from any artifact_id in the variant family.

        Strategy: every variant row stores ``parent_artifact_id`` = the raw
        artifact_id; raw rows have ``parent_artifact_id IS NULL`` themselves.
        So:
          * if the input IS the requested variant → return it
          * if the input is the raw root → look for ``parent_artifact_id=root``
            with ``variant=<wanted>``
          * if the input is a non-raw variant → first hop to its raw parent,
            then either return raw (if wanted='raw') or look up the sibling

        Returns ``None`` when the requested variant doesn't exist.
        """
        if not self.database or not self.database._pool:
            # No DB pool — best-effort fallback for dev/tests: ask
            # ``get_artifact`` and only return it if it actually IS the
            # requested variant. NEVER return a different variant — that
            # would smuggle e.g. watermarked display bytes into a
            # raw-only path (multi-turn reference editing).
            try:
                got = await self.get_artifact(parent_or_self_artifact_id)
            except Exception:
                return None
            if got is None:
                return None
            got_variant = getattr(got, "variant", None)
            # Only refuse on a real, string variant mismatch. Legacy / mock
            # artifacts without a variant column are assumed to be raw (which
            # is the only legacy variant we ever stored pre-migration).
            if isinstance(got_variant, str) and got_variant != variant:
                logger.debug(
                    "find_variant fallback: artifact %s has variant %r, requested %r — refusing",
                    parent_or_self_artifact_id,
                    got_variant,
                    variant,
                )
                return None
            return got

        try:
            async with self.database._pool.acquire() as conn:
                base = await conn.fetchrow(
                    "SELECT * FROM assistant.artifacts WHERE artifact_id = $1",
                    parent_or_self_artifact_id,
                )
                if not base:
                    return None
                base_artifact = self._row_to_artifact(base)

                # Same-row hit
                if base_artifact.variant == variant:
                    return base_artifact

                # Resolve the raw root for this family
                if base_artifact.variant == "raw":
                    raw_id = base_artifact.artifact_id
                else:
                    raw_id = base_artifact.parent_artifact_id
                    if not raw_id:
                        # Orphan variant row — best-effort: return None
                        return None

                if variant == "raw":
                    row = await conn.fetchrow(
                        "SELECT * FROM assistant.artifacts WHERE artifact_id = $1",
                        raw_id,
                    )
                else:
                    row = await conn.fetchrow(
                        """
                        SELECT * FROM assistant.artifacts
                        WHERE parent_artifact_id = $1 AND variant = $2
                        ORDER BY created_at DESC
                        LIMIT 1
                        """,
                        raw_id,
                        variant,
                    )
        except Exception as exc:
            # Non-asyncpg pool (test mocks) or transient DB error — fall back
            # to ``get_artifact``. Only return when it IS the requested
            # variant — never smuggle a different variant on a DB blip.
            logger.debug(
                "find_variant: DB unavailable (%s) — falling back to get_artifact",
                exc,
            )
            try:
                got = await self.get_artifact(parent_or_self_artifact_id)
            except Exception:
                return None
            if got is None:
                return None
            got_variant = getattr(got, "variant", None)
            # Only refuse on a real, string variant mismatch. Legacy / mock
            # artifacts without a variant column are assumed to be raw (which
            # is the only legacy variant we ever stored pre-migration).
            if isinstance(got_variant, str) and got_variant != variant:
                logger.warning(
                    "find_variant DB-error fallback: artifact %s variant %r ≠ requested %r — refusing",
                    parent_or_self_artifact_id,
                    got_variant,
                    variant,
                )
                return None
            return got

        if not row:
            return None
        return self._row_to_artifact(row)

    async def get_presigned_download_url_for_variant(
        self,
        artifact_id: str,
        variant: str = "display",
        expiry_seconds: int = 3600,
        *,
        owner_scope: str | None = None,
        tenant_id: str | None = None,
        user_id: str | None = None,
    ) -> tuple[str | None, str | None]:
        """Resolve a presigned download URL for the requested image variant.

        Fallback chain:
          * 'thumbnail' → 'display' → 'raw'
          * 'display'   → 'raw'
          * 'raw'       → no fallback (404 if missing)

        ``owner_scope``: when supplied, the resolved artifact must match it
        or we return (None, None) — same response shape as "not found", to
        avoid IDOR enumeration via timing/error-code distinction.

        Returns ``(url, actual_variant)``. ``actual_variant`` reflects the
        variant we *actually* served after fallback (so the caller can show
        e.g. "thumbnail unavailable, served display" in their response).
        """
        order = (
            ("thumbnail", "display", "raw")
            if variant == "thumbnail"
            else ("display", "raw")
            if variant == "display"
            else ("raw",)
        )

        for try_variant in order:
            artifact = await self.find_variant(artifact_id, try_variant)
            if artifact is None:
                continue
            if not self._matches_requested_owner(
                artifact,
                owner_scope=owner_scope,
                tenant_id=tenant_id,
                user_id=user_id,
            ):
                # Owner check fails; treat as not found
                continue
            url = await self.get_presigned_download_url(artifact, expiry_seconds)
            if url:
                return url, try_variant
        return None, None

    async def get_session_artifacts(
        self,
        session_id: str,
        tenant_id: str | None = None,
    ) -> list[ArtifactInfo]:
        """Get all artifacts for a session."""
        if not self.database or not self.database._pool:
            return []

        async with self.database._pool.acquire() as conn:
            if tenant_id:
                rows = await conn.fetch(
                    """
                    SELECT * FROM assistant.artifacts
                    WHERE session_id = $1 AND tenant_id = $2
                    ORDER BY created_at ASC
                    """,
                    session_id,
                    tenant_id,
                )
            else:
                rows = await conn.fetch(
                    """
                    SELECT * FROM assistant.artifacts
                    WHERE session_id = $1
                    ORDER BY created_at ASC
                    """,
                    session_id,
                )

        return [self._row_to_artifact(row) for row in rows]

    async def delete_artifact(self, artifact_id: str) -> bool:
        """Delete artifact from storage and database."""
        artifact = await self.get_artifact(artifact_id)
        if not artifact:
            return False

        # Delete from storage
        try:
            await self._backend.delete(artifact.storage_key)
        except Exception as e:
            logger.warning(f"Failed to delete artifact file {artifact.storage_key}: {e}")

        # Delete from database
        if self.database and self.database._pool:
            async with self.database._pool.acquire() as conn:
                await conn.execute(
                    "DELETE FROM assistant.artifacts WHERE artifact_id = $1",
                    artifact_id,
                )

        logger.info(f"Deleted artifact {artifact_id}")
        return True

    async def delete_session_artifacts(self, session_id: str) -> int:
        """Delete all artifacts for a session."""
        artifacts = await self.get_session_artifacts(session_id)

        count = 0
        for artifact in artifacts:
            if await self.delete_artifact(artifact.artifact_id):
                count += 1

        logger.info(f"Deleted {count} artifacts for session {session_id}")
        return count

    def get_download_url(
        self,
        artifact: ArtifactInfo,
        expiry_seconds: int = 3600,
    ) -> str:
        """Get download URL for artifact."""
        return self._backend.get_url(artifact.storage_key, expiry_seconds)

    async def get_presigned_download_url(
        self,
        artifact: ArtifactInfo,
        expiry_seconds: int = 3600,
    ) -> str | None:
        """Get presigned download URL (for S3)."""
        if isinstance(self._backend, S3StorageBackend):
            return await self._backend.generate_presigned_download_url(
                artifact.storage_key,
                expiry_seconds,
                artifact.filename,
            )
        return self.get_download_url(artifact, expiry_seconds)

    async def download_artifact(self, artifact_id: str) -> bytes | None:
        """Download artifact content."""
        artifact = await self.get_artifact(artifact_id)
        if not artifact:
            return None

        return await self._backend.download(artifact.storage_key)

    def _row_to_artifact(self, row) -> ArtifactInfo:
        """Convert database row to ArtifactInfo.

        Tolerates rows that pre-date the 001_image_session_artifacts migration:
        new columns are read with ``.get(...)``-style defaults so a legacy DB
        still produces valid ArtifactInfo objects.
        """
        import json

        metadata = row["metadata"]
        if isinstance(metadata, str):
            metadata = json.loads(metadata)

        # asyncpg.Record supports dict-style indexing but not .get(); convert
        # to dict so we can default-handle missing columns.
        try:
            row_dict = dict(row)
        except Exception:
            row_dict = row  # type: ignore[assignment]

        return ArtifactInfo(
            artifact_id=row_dict["artifact_id"],
            session_id=row_dict["session_id"],
            tenant_id=row_dict["tenant_id"],
            user_id=row_dict["user_id"],
            type=row_dict["type"],
            format=row_dict["format"],
            title=row_dict["title"],
            filename=row_dict["filename"],
            storage_key=row_dict["storage_key"],
            size_bytes=row_dict["size_bytes"],
            mime_type=row_dict["mime_type"],
            source=row_dict["source"],
            message_id=row_dict["message_id"],
            metadata=metadata,
            created_at=row_dict["created_at"],
            updated_at=row_dict["updated_at"],
            variant=row_dict.get("variant") or "raw",
            parent_artifact_id=row_dict.get("parent_artifact_id"),
            turn_id=row_dict.get("turn_id"),
            owner_scope=row_dict.get("owner_scope"),
            width=row_dict.get("width"),
            height=row_dict.get("height"),
            provider=row_dict.get("provider"),
            model_id=row_dict.get("model_id"),
            prompt=row_dict.get("prompt"),
        )

    async def close(self) -> None:
        """Close storage backend."""
        if self._backend:
            await self._backend.close()


# Global instance
_artifact_storage: ArtifactStorageService | None = None


def get_artifact_storage() -> ArtifactStorageService | None:
    """Get global artifact storage instance."""
    return _artifact_storage


def init_artifact_storage(
    config: StorageConfig,
    database: DatabaseStorage | None = None,
) -> ArtifactStorageService:
    """Initialize global artifact storage."""
    global _artifact_storage
    _artifact_storage = ArtifactStorageService(config, database)
    logger.info(f"Initialized artifact storage with backend: {config.backend}")
    return _artifact_storage
