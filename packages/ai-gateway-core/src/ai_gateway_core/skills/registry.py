"""In-memory and optional database-backed skill registry."""

from __future__ import annotations

from dataclasses import dataclass, replace
from typing import Any

from .artifact_repository import DatabaseSkillArtifactRepository, manifest_from_artifact
from .models import SkillManifest, SkillSource
from .parser import serialize_user_skill_md


@dataclass
class SkillSelection:
    """Skill selected for a user query."""

    skill: SkillManifest
    score: float


class SkillRegistry:
    """Dynamic skill registry with optional DB persistence."""

    def __init__(self, database: Any | None = None) -> None:
        self.database = database
        self._skills: dict[str, SkillManifest] = {}
        self._scoped_skills: dict[tuple[str, str, str], SkillManifest] = {}

    def register(self, manifest: SkillManifest) -> None:
        manifest = self._safe_manifest_for_storage(manifest)
        errors = manifest.validate()
        if errors:
            raise ValueError(f"Invalid skill manifest: {'; '.join(errors)}")
        self._skills[manifest.name] = manifest

    @staticmethod
    def _safe_manifest_for_storage(manifest: SkillManifest) -> SkillManifest:
        """Keep generated skills proposed/disabled until activation gates pass."""
        if manifest.review_required():
            return replace(manifest, enabled=False, lifecycle_status="proposed")
        if manifest.generated and manifest.enabled:
            return replace(manifest, lifecycle_status="active")
        return manifest

    def unregister(self, skill_name: str) -> bool:
        return self._skills.pop(skill_name, None) is not None

    def get(self, skill_name: str) -> SkillManifest | None:
        return self._skills.get(skill_name)

    def get_scoped(
        self,
        skill_name: str,
        *,
        tenant_id: str,
        user_id: str,
        version_id: str | None = None,
    ) -> SkillManifest | None:
        candidates = [
            skill
            for (tenant, user, version), skill in self._scoped_skills.items()
            if tenant == tenant_id
            and user == user_id
            and skill.name == skill_name
            and (version_id is None or version == version_id)
        ]
        if not candidates:
            if version_id is not None:
                return None
            return self._skills.get(skill_name)
        return sorted(candidates, key=lambda item: str(item.version_id or ""))[-1]

    def fork_runtime_view(self) -> SkillRegistry:
        """Create a per-run view without copying tenant-scoped cache entries.

        Platform Skills are process-owned and safe to share by reference. Exact
        tenant artifacts are loaded into the fresh scoped cache for one run and
        therefore cannot become visible to another tenant or later request.
        """

        runtime = SkillRegistry(database=self.database)
        runtime._skills = dict(self._skills)
        return runtime

    def register_scoped(
        self,
        manifest: SkillManifest,
        *,
        tenant_id: str,
        user_id: str,
    ) -> None:
        """Cache one verified immutable artifact without global name mutation."""

        manifest = self._safe_manifest_for_storage(manifest)
        if manifest.validate() or not manifest.version_id:
            raise ValueError("Scoped Skill requires a valid immutable version")
        self._scoped_skills[(tenant_id, user_id, manifest.version_id)] = manifest

    def list(
        self,
        enabled_only: bool = True,
        *,
        allowed_names: frozenset[str] | None = None,
        scope: tuple[str, str] | None = None,
        allowed_versions: dict[str, str] | None = None,
    ) -> list[SkillManifest]:
        skills_by_identity: dict[tuple[str, str], SkillManifest] = {
            (skill.name, str(skill.version_id or "platform")): skill
            for skill in self._skills.values()
        }
        if scope is not None:
            tenant_id, user_id = scope
            for (tenant, user, version), skill in self._scoped_skills.items():
                if tenant == tenant_id and user == user_id:
                    skills_by_identity[(skill.name, version)] = skill
        skills = list(skills_by_identity.values())
        if enabled_only:
            skills = [skill for skill in skills if skill.enabled]
        if allowed_names is not None:
            skills = [skill for skill in skills if skill.name in allowed_names]
        if allowed_versions is not None:
            skills = [
                skill
                for skill in skills
                if allowed_versions.get(skill.name) == str(skill.version_id or "")
            ]
        return sorted(skills, key=lambda item: (item.name, str(item.version_id or "")))

    def metadata_for_prompt(
        self,
        enabled_only: bool = True,
        *,
        allowed_names: frozenset[str] | None = None,
        scope: tuple[str, str] | None = None,
        allowed_versions: dict[str, str] | None = None,
    ) -> list[dict[str, Any]]:
        """Prompt-safe skill metadata (no implementation payload)."""
        metadata: list[dict[str, Any]] = []
        for skill in self.list(
            enabled_only=enabled_only,
            allowed_names=allowed_names,
            scope=scope,
            allowed_versions=allowed_versions,
        ):
            metadata.append(
                {
                    "name": skill.name,
                    "title": skill.title,
                    "summary": skill.summary or skill.description,
                    "version": skill.version,
                    "tags": skill.tags,
                }
            )
        return metadata

    def select_for_query(
        self,
        query: str,
        max_skills: int | None = 3,
        *,
        allowed_names: frozenset[str] | None = None,
        scope: tuple[str, str] | None = None,
        allowed_versions: dict[str, str] | None = None,
    ) -> list[SkillSelection]:
        """Select likely skills by token overlap with description/tags."""
        normalized = query.lower().strip()
        if not normalized:
            return []

        query_terms = {term for term in normalized.split() if term}
        scored: list[SkillSelection] = []

        for skill in self.list(
            enabled_only=True,
            allowed_names=allowed_names,
            scope=scope,
            allowed_versions=allowed_versions,
        ):
            bag = f"{skill.title} {skill.description} {' '.join(skill.tags)}".lower()
            score = 0.0
            for term in query_terms:
                if term in bag:
                    score += 1.0
            if score > 0:
                scored.append(SkillSelection(skill=skill, score=score))

        scored.sort(key=lambda item: item.score, reverse=True)
        return scored if max_skills is None else scored[: max(0, max_skills)]

    async def load_from_database(
        self,
        tenant_id: str,
        user_id: str,
        *,
        allowed_names: frozenset[str] | None = None,
    ) -> int:
        """Load latest active Skills into a tenant/user scoped cache."""
        if not self.database:
            return 0

        if allowed_names is not None and not allowed_names:
            return 0

        repository = DatabaseSkillArtifactRepository(self.database)
        rows = await repository.list_for_actor(
            tenant_id=tenant_id,
            user_id=user_id,
            enabled_only=True,
        )
        loaded = 0
        for row in rows:
            if allowed_names is not None and str(row.get("name")) not in allowed_names:
                continue
            from .artifact_repository import manifest_from_artifact

            manifest = manifest_from_artifact(row)
            if manifest.version_id:
                self.register_scoped(
                    manifest,
                    tenant_id=tenant_id,
                    user_id=user_id,
                )
                loaded += 1

        return loaded

    async def load_versions_from_database(
        self,
        tenant_id: str,
        user_id: str,
        *,
        allowed_versions: dict[str, str],
    ) -> int:
        """Load only exact immutable versions from a signed Agent Snapshot."""

        if not self.database or not allowed_versions:
            return 0
        repository = DatabaseSkillArtifactRepository(self.database)
        manifests = await repository.load_versions(
            tenant_id=tenant_id,
            user_id=user_id,
            version_ids=frozenset(allowed_versions.values()),
        )
        loaded = 0
        for manifest in manifests:
            expected_version = allowed_versions.get(manifest.name)
            if not expected_version or expected_version != manifest.version_id:
                raise ValueError("Resolved Skill name/version binding mismatch")
            self.register_scoped(
                manifest,
                tenant_id=tenant_id,
                user_id=user_id,
            )
            loaded += 1
        return loaded

    async def save_manifest(
        self,
        *,
        tenant_id: str,
        user_id: str,
        manifest: SkillManifest,
        created_by: str,
    ) -> str:
        """Persist a complete manifest as an immutable tenant artifact."""
        manifest = self._safe_manifest_for_storage(manifest)
        if not self.database:
            self.register(manifest)
            return "in-memory"
        if manifest.source is not SkillSource.USER:
            raise ValueError("Only user Skills can be persisted as tenant artifacts")

        tenant_manifest = replace(
            manifest,
            entrypoint="db://pending/pending",
            source=SkillSource.USER,
            skill_id=None,
            version_id=None,
            content_hash=None,
            artifact_type="tenant_instruction",
        )
        record = await DatabaseSkillArtifactRepository(self.database).create_version(
            tenant_id=tenant_id,
            user_id=user_id,
            content=serialize_user_skill_md(tenant_manifest),
            manifest=tenant_manifest,
            created_by=created_by,
        )
        stored = manifest_from_artifact(record)
        self.register_scoped(stored, tenant_id=tenant_id, user_id=user_id)
        return str(record["skill_id"])
