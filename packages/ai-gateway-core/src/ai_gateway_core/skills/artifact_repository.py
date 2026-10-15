"""Tenant-safe persistence for immutable instruction-only Skill artifacts."""

from __future__ import annotations

import hashlib
import json
import uuid
from dataclasses import replace
from typing import Any

from .models import SkillManifest, SkillSource, TriggerConfig
from .parser import normalize_skill_md, parse_skill_md, serialize_user_skill_md


class SkillArtifactError(RuntimeError):
    """Stable base error for Skill persistence and authorization failures."""

    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


class SkillArtifactNotFoundError(SkillArtifactError):
    """The requested Skill is absent or hidden from the caller."""


class SkillArtifactConflictError(SkillArtifactError):
    """The requested Skill mutation conflicts with an existing artifact."""


class SkillArtifactUnavailableError(SkillArtifactError):
    """The immutable version is disabled, revoked, corrupt, or unavailable."""


def skill_content_hash(content: str) -> str:
    return hashlib.sha256(content.encode("utf-8")).hexdigest()


def _mapping(value: Any) -> dict[str, Any]:
    if isinstance(value, dict):
        return dict(value)
    if isinstance(value, str):
        parsed = json.loads(value)
        return dict(parsed) if isinstance(parsed, dict) else {}
    return {}


def _string_list(value: Any) -> list[str]:
    if isinstance(value, str):
        value = json.loads(value)
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if isinstance(item, str)]


def manifest_from_artifact(record: dict[str, Any]) -> SkillManifest:
    """Reconstruct and verify the full immutable Skill version."""

    content = normalize_skill_md(str(record.get("content") or ""))
    expected_hash = str(record.get("content_hash") or "")
    if not expected_hash or skill_content_hash(content) != expected_hash:
        raise SkillArtifactUnavailableError("SKILL_VERSION_HASH_MISMATCH")
    try:
        parsed = parse_skill_md(content)
    except Exception as exc:
        raise SkillArtifactUnavailableError("SKILL_VERSION_CONTENT_INVALID") from exc

    metadata = _mapping(record.get("manifest"))
    trigger_data = metadata.get("trigger")
    trigger = parsed.trigger
    if isinstance(trigger_data, dict):
        trigger = TriggerConfig(
            patterns=_string_list(trigger_data.get("patterns")),
            auto=bool(trigger_data.get("auto", False)),
        )
    skill_id = str(record.get("skill_id") or metadata.get("skill_id") or "")
    version_id = str(record.get("version_id") or metadata.get("version_id") or "")
    entrypoint = str(record.get("entrypoint") or "")
    if not skill_id or not version_id or entrypoint != f"db://{skill_id}/{version_id}":
        raise SkillArtifactUnavailableError("SKILL_VERSION_ENTRYPOINT_INVALID")

    manifest = replace(
        parsed,
        name=str(metadata.get("name") or record.get("name") or parsed.name),
        title=str(metadata.get("title") or record.get("title") or parsed.title),
        description=str(
            metadata.get("description") or record.get("description") or parsed.description
        ),
        entrypoint=entrypoint,
        summary=str(metadata.get("summary") or parsed.summary),
        version=str(metadata.get("version") or record.get("version") or parsed.version),
        tags=_string_list(metadata.get("tags", record.get("tags"))),
        permissions=_string_list(
            metadata.get("permissions", record.get("permissions"))
        ),
        enabled=bool(record.get("enabled", metadata.get("enabled", True))),
        instructions=parsed.instructions,
        trigger=trigger,
        config=(
            dict(metadata.get("config"))
            if isinstance(metadata.get("config"), dict)
            else parsed.config
        ),
        source=SkillSource.USER,
        tool_schema=(
            dict(metadata.get("tool_schema"))
            if isinstance(metadata.get("tool_schema"), dict)
            else None
        ),
        max_context_tokens=int(
            metadata.get("max_context_tokens") or parsed.max_context_tokens
        ),
        author=str(metadata.get("author") or parsed.author),
        generated=bool(metadata.get("generated", parsed.generated)),
        lifecycle_status=str(record.get("status") or metadata.get("lifecycle_status") or "active"),
        review=(dict(metadata.get("review")) if isinstance(metadata.get("review"), dict) else {}),
        evaluation=(
            dict(metadata.get("evaluation"))
            if isinstance(metadata.get("evaluation"), dict)
            else {}
        ),
        rollback=(
            dict(metadata.get("rollback"))
            if isinstance(metadata.get("rollback"), dict)
            else {}
        ),
        skill_id=skill_id,
        version_id=version_id,
        content_hash=expected_hash,
        artifact_type="tenant_instruction",
    )
    if manifest.validate():
        raise SkillArtifactUnavailableError("SKILL_VERSION_MANIFEST_INVALID")
    return manifest


class DatabaseSkillArtifactRepository:
    """Persist and authorize Skill artifacts through the shared asyncpg pool."""

    def __init__(self, pool_holder: Any):
        self._holder = pool_holder

    @property
    def _pool(self) -> Any:
        return getattr(self._holder, "_pool", None)

    @property
    def enabled(self) -> bool:
        return bool(getattr(self._holder, "enabled", False) and self._pool is not None)

    def _require_enabled(self) -> None:
        if not self.enabled:
            raise SkillArtifactUnavailableError("SKILL_STORAGE_UNAVAILABLE")

    @staticmethod
    def _artifact_select() -> str:
        return """
            SELECT skill.skill_id, skill.tenant_id, skill.user_id, skill.name,
                   skill.title, skill.description, skill.tags, skill.permissions,
                   skill.enabled, skill.status AS skill_status,
                   skill.artifact_type AS skill_artifact_type,
                   skill.disabled_at, skill.deleted_at,
                   version.version_id, version.version, version.revision,
                   version.manifest, version.entrypoint, version.content,
                   version.content_hash, version.status,
                   version.artifact_type, version.created_at,
                   EXISTS (
                       SELECT 1 FROM assistant.assistant_skill_version_revocations revoked
                       WHERE revoked.tenant_id = version.tenant_id
                         AND revoked.version_id = version.version_id
                   ) AS revoked
            FROM assistant.assistant_skill_versions AS version
            JOIN assistant.assistant_skills AS skill
              ON skill.tenant_id = version.tenant_id
             AND skill.skill_id = version.skill_id
        """

    async def create_version(
        self,
        *,
        tenant_id: str,
        user_id: str,
        content: str,
        manifest: SkillManifest,
        created_by: str,
    ) -> dict[str, Any]:
        """Atomically create a Skill and one immutable content version."""

        self._require_enabled()
        normalized = normalize_skill_md(content)
        if manifest.source is not SkillSource.USER or manifest.artifact_type != "tenant_instruction":
            raise SkillArtifactConflictError("SKILL_ARTIFACT_TYPE_FORBIDDEN")
        if manifest.review_required():
            manifest = replace(manifest, enabled=False, lifecycle_status="proposed")
        content_hash = skill_content_hash(normalized)
        async with self._pool.acquire() as connection, connection.transaction():
            skill = await connection.fetchrow(
                """
                SELECT * FROM assistant.assistant_skills
                WHERE tenant_id = $1 AND user_id = $2 AND name = $3
                FOR UPDATE
                """,
                tenant_id,
                user_id,
                manifest.name,
            )
            if skill and (
                skill["artifact_type"] not in {"tenant_instruction", "legacy"}
                or skill["deleted_at"] is not None
                or str(skill["status"]) == "deleted"
            ):
                raise SkillArtifactConflictError("SKILL_NAME_UNAVAILABLE")

            skill_id = uuid.UUID(str(skill["skill_id"])) if skill else uuid.uuid4()
            if not skill:
                await connection.execute(
                    """
                    INSERT INTO assistant.assistant_skills (
                        skill_id, tenant_id, user_id, name, title, description,
                        tags, permissions, enabled, status, artifact_type,
                        created_by, created_at, updated_at
                    ) VALUES (
                        $1, $2, $3, $4, $5, $6,
                        $7::jsonb, $8::jsonb, $9, $10, 'tenant_instruction',
                        $11, NOW(), NOW()
                    )
                    """,
                    skill_id,
                    tenant_id,
                    user_id,
                    manifest.name,
                    manifest.title,
                    manifest.description,
                    json.dumps(manifest.tags),
                    json.dumps(manifest.permissions),
                    manifest.enabled,
                    "active" if manifest.enabled else manifest.lifecycle_status,
                    created_by,
                )

            revision = int(
                await connection.fetchval(
                    """
                    SELECT COALESCE(MAX(revision), 0) + 1
                    FROM assistant.assistant_skill_versions
                    WHERE skill_id = $1
                    """,
                    skill_id,
                )
            )
            version_id = uuid.uuid4()
            entrypoint = f"db://{skill_id}/{version_id}"
            stored = replace(
                manifest,
                entrypoint=entrypoint,
                source=SkillSource.USER,
                skill_id=str(skill_id),
                version_id=str(version_id),
                content_hash=content_hash,
                artifact_type="tenant_instruction",
            )
            version_status = "active" if stored.enabled else stored.lifecycle_status
            await connection.execute(
                """
                INSERT INTO assistant.assistant_skill_versions (
                    version_id, skill_id, tenant_id, user_id, version, revision,
                    manifest, entrypoint, content, content_hash, artifact_type,
                    status, created_by, created_at
                ) VALUES (
                    $1, $2, $3, $4, $5, $6,
                    $7::jsonb, $8, $9, $10, 'tenant_instruction',
                    $11, $12, NOW()
                )
                """,
                version_id,
                skill_id,
                tenant_id,
                user_id,
                stored.version,
                revision,
                json.dumps(stored.to_dict(), ensure_ascii=False, sort_keys=True),
                entrypoint,
                normalized,
                content_hash,
                version_status,
                created_by,
            )
            await connection.execute(
                """
                UPDATE assistant.assistant_skills
                SET title = $4, description = $5, tags = $6::jsonb,
                    permissions = $7::jsonb, enabled = $8, status = $9,
                    artifact_type = 'tenant_instruction',
                    disabled_at = CASE WHEN $8 THEN NULL ELSE NOW() END,
                    updated_at = NOW()
                WHERE tenant_id = $1 AND user_id = $2 AND skill_id = $3
                """,
                tenant_id,
                user_id,
                skill_id,
                stored.title,
                stored.description,
                json.dumps(stored.tags),
                json.dumps(stored.permissions),
                stored.enabled,
                "active" if stored.enabled else stored.lifecycle_status,
            )
            row = await connection.fetchrow(
                self._artifact_select()
                + " WHERE version.tenant_id = $1 AND version.version_id = $2",
                tenant_id,
                version_id,
            )
        return self._public_record(dict(row))

    @staticmethod
    def _public_record(record: dict[str, Any]) -> dict[str, Any]:
        manifest = manifest_from_artifact(record)
        return {
            **manifest.to_dict(),
            "revision": int(record.get("revision") or 0),
            "content": str(record.get("content") or ""),
            "status": str(record.get("skill_status") or record.get("status") or "active"),
            "revoked": bool(record.get("revoked")),
            "created_at": record.get("created_at"),
        }

    async def list_for_actor(
        self,
        *,
        tenant_id: str,
        user_id: str,
        enabled_only: bool,
    ) -> list[dict[str, Any]]:
        self._require_enabled()
        query = (
            self._artifact_select()
            + """
              JOIN LATERAL (
                  SELECT candidate.version_id
                  FROM assistant.assistant_skill_versions AS candidate
                  WHERE candidate.skill_id = skill.skill_id
                  ORDER BY candidate.revision DESC
                  LIMIT 1
              ) AS latest ON latest.version_id = version.version_id
              WHERE skill.tenant_id = $1
                AND skill.user_id = $2
                AND skill.artifact_type = 'tenant_instruction'
                AND version.artifact_type = 'tenant_instruction'
                AND skill.deleted_at IS NULL
                AND skill.status <> 'deleted'
            """
        )
        if enabled_only:
            query += " AND skill.enabled = TRUE AND skill.status = 'active'"
        query += " ORDER BY skill.name, skill.user_id"
        async with self._pool.acquire() as connection:
            rows = await connection.fetch(query, tenant_id, user_id)
        return [self._public_record(dict(row)) for row in rows]

    async def get_for_actor(
        self,
        *,
        tenant_id: str,
        user_id: str,
        name: str,
    ) -> dict[str, Any]:
        rows = await self.list_for_actor(
            tenant_id=tenant_id,
            user_id=user_id,
            enabled_only=False,
        )
        for row in rows:
            if row["name"] == name:
                return row
        raise SkillArtifactNotFoundError("SKILL_NOT_FOUND")

    async def update_metadata(
        self,
        *,
        tenant_id: str,
        user_id: str,
        name: str,
        changes: dict[str, Any],
        updated_by: str,
    ) -> dict[str, Any]:
        current = await self.get_for_actor(
            tenant_id=tenant_id,
            user_id=user_id,
            name=name,
        )
        if current.get("artifact_type") != "tenant_instruction" or current.get("user_id") == "*":
            raise SkillArtifactNotFoundError("SKILL_NOT_FOUND")
        manifest = manifest_from_artifact(current)
        allowed = {"title", "description", "tags", "permissions", "enabled"}
        values = {key: value for key, value in changes.items() if key in allowed}
        manifest = replace(manifest, **values, skill_id=None, version_id=None, content_hash=None)
        if manifest.enabled and manifest.review_required():
            raise SkillArtifactConflictError("SKILL_ACTIVATION_GATES_REQUIRED")
        from .builder import SkillBuilder

        if SkillBuilder().validate_manifest(manifest):
            raise SkillArtifactConflictError("SKILL_MANIFEST_INVALID")
        return await self.create_version(
            tenant_id=tenant_id,
            user_id=user_id,
            content=serialize_user_skill_md(manifest),
            manifest=manifest,
            created_by=updated_by,
        )

    async def set_enabled(
        self,
        *,
        tenant_id: str,
        user_id: str,
        name: str,
        enabled: bool,
    ) -> dict[str, Any]:
        self._require_enabled()
        current = await self.get_for_actor(
            tenant_id=tenant_id,
            user_id=user_id,
            name=name,
        )
        manifest = manifest_from_artifact(current)
        if enabled and manifest.review_required():
            raise SkillArtifactConflictError("SKILL_ACTIVATION_GATES_REQUIRED")
        async with self._pool.acquire() as connection:
            result = await connection.execute(
                """
                UPDATE assistant.assistant_skills
                SET enabled = $4,
                    status = CASE WHEN $4 THEN 'active' ELSE 'disabled' END,
                    disabled_at = CASE WHEN $4 THEN NULL ELSE NOW() END,
                    updated_at = NOW()
                WHERE tenant_id = $1 AND user_id = $2 AND name = $3
                  AND artifact_type = 'tenant_instruction'
                  AND deleted_at IS NULL AND status <> 'deleted'
                """,
                tenant_id,
                user_id,
                name,
                enabled,
            )
        if result != "UPDATE 1":
            raise SkillArtifactNotFoundError("SKILL_NOT_FOUND")
        return await self.get_for_actor(tenant_id=tenant_id, user_id=user_id, name=name)

    async def delete(
        self,
        *,
        tenant_id: str,
        user_id: str,
        name: str,
    ) -> None:
        self._require_enabled()
        async with self._pool.acquire() as connection:
            result = await connection.execute(
                """
                UPDATE assistant.assistant_skills
                SET enabled = FALSE, status = 'deleted', deleted_at = NOW(),
                    disabled_at = COALESCE(disabled_at, NOW()), updated_at = NOW()
                WHERE tenant_id = $1 AND user_id = $2 AND name = $3
                  AND artifact_type = 'tenant_instruction'
                  AND deleted_at IS NULL
                """,
                tenant_id,
                user_id,
                name,
            )
        if result != "UPDATE 1":
            raise SkillArtifactNotFoundError("SKILL_NOT_FOUND")

    async def authorize_version(
        self,
        *,
        tenant_id: str,
        user_id: str,
        version_id: str,
        allow_tenant_admin: bool = False,
    ) -> dict[str, Any]:
        """Load one exact active version after current revocation checks."""

        self._require_enabled()
        try:
            parsed_version = uuid.UUID(version_id)
        except (TypeError, ValueError) as exc:
            raise SkillArtifactUnavailableError("SKILL_VERSION_UNAVAILABLE") from exc
        query = self._artifact_select() + """
            WHERE version.tenant_id = $1 AND version.version_id = $2
              AND ($4::boolean OR skill.user_id = $3 OR skill.user_id = '*')
        """
        async with self._pool.acquire() as connection:
            row = await connection.fetchrow(
                query,
                tenant_id,
                parsed_version,
                user_id,
                allow_tenant_admin,
            )
        if not row:
            raise SkillArtifactUnavailableError("SKILL_VERSION_UNAVAILABLE")
        record = dict(row)
        if (
            not bool(record.get("enabled"))
            or str(record.get("skill_status")) != "active"
            or str(record.get("status")) != "active"
            or record.get("deleted_at") is not None
            or bool(record.get("revoked"))
            or str(record.get("artifact_type")) != "tenant_instruction"
        ):
            raise SkillArtifactUnavailableError("SKILL_VERSION_UNAVAILABLE")
        return self._public_record(record)

    async def load_versions(
        self,
        *,
        tenant_id: str,
        user_id: str,
        version_ids: frozenset[str],
    ) -> list[SkillManifest]:
        manifests: list[SkillManifest] = []
        for version_id in sorted(version_ids):
            record = await self.authorize_version(
                tenant_id=tenant_id,
                user_id=user_id,
                version_id=version_id,
            )
            manifests.append(manifest_from_artifact(record))
        return manifests


__all__ = [
    "DatabaseSkillArtifactRepository",
    "SkillArtifactConflictError",
    "SkillArtifactError",
    "SkillArtifactNotFoundError",
    "SkillArtifactUnavailableError",
    "manifest_from_artifact",
    "skill_content_hash",
]
