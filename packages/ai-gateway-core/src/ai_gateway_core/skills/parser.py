"""
SKILL.md Parser — parse YAML frontmatter + markdown body into SkillManifest.

Format:
    ---
    name: my-skill
    title: My Skill
    description: Does something useful
    ...
    ---

    # My Skill
    ## Instructions
    ...
"""

from __future__ import annotations

import logging
import re
from dataclasses import dataclass
from typing import Any, Final

import yaml

from .models import SkillManifest, SkillSource, TriggerConfig

logger = logging.getLogger(__name__)


MAX_SKILL_MD_SIZE = 50_000  # 50KB max
MAX_INSTRUCTIONS_TOKENS = 5000

_SERVER_OWNED_FIELDS: Final = frozenset(
    {
        "artifacttype",
        "builtin",
        "command",
        "entrypoint",
        "executable",
        "handler",
        "module",
        "path",
        "script",
        "skillid",
        "source",
        "url",
        "versionid",
    }
)


@dataclass(frozen=True)
class UserSkillPolicyError(ValueError):
    """Field-level rejection for a tenant-controlled Skill artifact."""

    field: str
    code: str
    message: str

    def __str__(self) -> str:
        return self.message


def normalize_skill_md(content: str) -> str:
    """Return the canonical text persisted and hashed for a Skill version."""

    if "\x00" in content:
        raise UserSkillPolicyError(
            field="file",
            code="SKILL_CONTENT_INVALID",
            message="SKILL.md must not contain NUL bytes",
        )
    normalized = content.replace("\r\n", "\n").replace("\r", "\n")
    return normalized.rstrip() + "\n"


def serialize_user_skill_md(manifest: SkillManifest) -> str:
    """Serialize a complete tenant manifest without server-owned identity."""

    metadata: dict[str, Any] = {
        "name": manifest.name,
        "title": manifest.title,
        "description": manifest.description,
        "summary": manifest.summary,
        "version": manifest.version,
        "tags": list(manifest.tags),
        "permissions": list(manifest.permissions),
        "enabled": manifest.enabled,
        "generated": manifest.generated,
        "lifecycle_status": manifest.lifecycle_status,
        "config": dict(manifest.config),
        "max_context_tokens": manifest.max_context_tokens,
        "author": manifest.author,
        "review": dict(manifest.review),
        "evaluation": dict(manifest.evaluation),
        "rollback": dict(manifest.rollback),
    }
    if manifest.trigger is not None:
        metadata["trigger"] = {
            "patterns": list(manifest.trigger.patterns),
            "auto": manifest.trigger.auto,
        }
    if manifest.tool_schema is not None:
        metadata["tool_schema"] = dict(manifest.tool_schema)
    frontmatter = yaml.safe_dump(
        metadata,
        allow_unicode=True,
        sort_keys=False,
    ).rstrip()
    return normalize_skill_md(
        f"---\n{frontmatter}\n---\n{manifest.instructions.rstrip()}\n"
    )


def parse_user_skill_md(
    content: str,
    *,
    reserved_names: frozenset[str] = frozenset(),
) -> tuple[str, SkillManifest]:
    """Parse an instruction-only tenant upload without trusting artifact identity.

    Entrypoint/source and executable aliases are rejected when present instead
    of being silently ignored.  The caller replaces the pending entrypoint
    with ``db://<skill_id>/<version_id>`` inside the persistence transaction.
    """

    normalized = normalize_skill_md(content)
    frontmatter, _body = _split_frontmatter(normalized)
    if not frontmatter:
        raise UserSkillPolicyError(
            field="file",
            code="SKILL_FRONTMATTER_REQUIRED",
            message="SKILL.md must start with YAML frontmatter",
        )
    metadata = yaml.safe_load(frontmatter)
    if not isinstance(metadata, dict):
        raise UserSkillPolicyError(
            field="file",
            code="SKILL_FRONTMATTER_INVALID",
            message="YAML frontmatter must be a mapping",
    )
    for raw_key in metadata:
        field = str(raw_key).strip().lower().replace("-", "_")
        canonical_key = re.sub(r"[^a-z0-9]", "", field)
        if canonical_key in _SERVER_OWNED_FIELDS:
            raise UserSkillPolicyError(
                field=field,
                code="SKILL_ARTIFACT_FIELD_FORBIDDEN",
                message=f"'{field}' is server-owned for tenant Skills",
            )

    manifest = parse_skill_md(normalized)
    if manifest.name in reserved_names:
        raise UserSkillPolicyError(
            field="name",
            code="SKILL_NAME_RESERVED",
            message="Skill name is reserved by a platform artifact",
        )
    manifest.source = SkillSource.USER
    manifest.entrypoint = "db://pending/pending"
    manifest.artifact_type = "tenant_instruction"
    manifest.skill_id = None
    manifest.version_id = None
    manifest.content_hash = None
    return normalized, manifest


def parse_skill_md(content: str) -> SkillManifest:
    """Parse a SKILL.md file (YAML frontmatter + markdown body) into a SkillManifest."""
    if len(content) > MAX_SKILL_MD_SIZE:
        raise ValueError(f"SKILL.md too large ({len(content)} bytes, max {MAX_SKILL_MD_SIZE})")

    frontmatter, body = _split_frontmatter(content)
    if not frontmatter:
        raise ValueError("SKILL.md must start with YAML frontmatter (--- ... ---)")

    meta = yaml.safe_load(frontmatter)
    if not isinstance(meta, dict):
        raise ValueError("YAML frontmatter must be a mapping")

    # Required fields
    name = str(meta.get("name", "")).strip()
    title = str(meta.get("title", name)).strip()
    description = str(meta.get("description", "")).strip()

    if not name:
        raise ValueError("'name' is required in frontmatter")
    if not description:
        raise ValueError("'description' is required in frontmatter")

    # Validate name format (kebab-case, 3-50 chars)
    if not re.match(r"^[a-z][a-z0-9\-]{2,49}$", name):
        raise ValueError("name must be kebab-case (a-z, 0-9, hyphens), 3-50 chars")

    # Trigger config
    trigger = None
    trigger_raw = meta.get("trigger")
    if isinstance(trigger_raw, dict):
        trigger = TriggerConfig(
            patterns=trigger_raw.get("patterns", []),
            auto=bool(trigger_raw.get("auto", False)),
        )

    # Source
    source_str = str(meta.get("source", "user")).lower()
    try:
        source = SkillSource(source_str)
    except ValueError:
        source = SkillSource.USER

    generated = bool(meta.get("generated", source is SkillSource.USER))
    lifecycle_status = str(
        meta.get("lifecycle_status")
        or meta.get("status")
        or ("proposed" if generated else "active")
    )
    enabled_raw = meta.get("enabled")

    manifest = SkillManifest(
        name=name,
        title=title,
        description=description,
        entrypoint=str(meta.get("entrypoint", f"md://{name}")),
        summary=str(meta.get("summary", description[:180])),
        version=str(meta.get("version", "1.0.0")),
        tags=meta.get("tags", []),
        permissions=meta.get("permissions", []),
        enabled=bool(enabled_raw) if enabled_raw is not None else lifecycle_status == "active",
        instructions=body.strip(),
        trigger=trigger,
        config=meta.get("config", {}),
        source=source,
        tool_schema=meta.get("tool_schema"),
        max_context_tokens=int(meta.get("max_context_tokens", 2000)),
        author=str(meta.get("author", "")),
        generated=generated,
        lifecycle_status=lifecycle_status,
        review=meta.get("review", {}) or {},
        evaluation=meta.get("evaluation", {}) or {},
        rollback=meta.get("rollback", {}) or {},
    )
    if manifest.review_required():
        manifest.enabled = False
        manifest.lifecycle_status = "proposed"
    return manifest


def _split_frontmatter(content: str) -> tuple[str, str]:
    """Split --- YAML --- from markdown body."""
    match = re.match(r"^---\s*\n(.*?)\n---\s*\n?(.*)", content, re.DOTALL)
    if match:
        return match.group(1), match.group(2)
    return "", content
