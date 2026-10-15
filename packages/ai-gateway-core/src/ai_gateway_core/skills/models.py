"""Skill models for dynamic skill registry."""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime, timezone
from enum import Enum
from typing import Any


class SkillSource(str, Enum):
    """Where the skill came from."""
    BUILTIN = "builtin"
    USER = "user"
    MARKETPLACE = "marketplace"


@dataclass
class TriggerConfig:
    """Auto-trigger configuration for a skill."""
    patterns: list[str] = field(default_factory=list)  # Regex patterns
    auto: bool = False  # Auto-trigger when patterns match (vs explicit /command only)


@dataclass
class SkillManifest:
    """Skill manifest — defines a skill's identity, behavior, and execution config."""

    # === Core (existing) ===
    name: str
    title: str
    description: str
    entrypoint: str
    summary: str = ""
    version: str = "1.0.0"
    tags: list[str] = field(default_factory=list)
    permissions: list[str] = field(default_factory=list)
    enabled: bool = True

    # Server-owned artifact identity.  Tenant uploads receive these values
    # only after the database transaction has created an immutable version.
    skill_id: str | None = None
    version_id: str | None = None
    content_hash: str | None = None
    artifact_type: str = "bundled"

    # === Extended (new) ===
    instructions: str = ""              # Full markdown instructions (L2, loaded on demand)
    trigger: TriggerConfig | None = None  # Auto-trigger patterns
    config: dict = field(default_factory=dict)  # Skill-specific configuration
    source: SkillSource = SkillSource.BUILTIN
    tool_schema: dict | None = None     # JSON Schema for function calling
    max_context_tokens: int = 2000      # Token budget for instructions
    author: str = ""
    generated: bool = False
    lifecycle_status: str = "active"
    review: dict[str, Any] = field(default_factory=dict)
    evaluation: dict[str, Any] = field(default_factory=dict)
    rollback: dict[str, Any] = field(default_factory=dict)

    def activation_requirements(self) -> dict[str, bool]:
        """Return generated-skill gates required before enablement."""
        review_verdict = str(
            self.review.get("verdict") or self.review.get("critic_verdict") or ""
        ).lower()
        independent_critic = bool(
            self.review.get("critic_artifact")
            or self.review.get("critic_report")
            or self.review.get("artifact")
        ) and (
            not review_verdict
            or review_verdict in {"approved", "approve", "pass", "passed"}
        )
        eval_evidence = bool(
            self.evaluation.get("evidence")
            or self.evaluation.get("eval_artifact")
            or self.evaluation.get("test_command")
            or self.evaluation.get("passed") is True
        )
        rollback_metadata = bool(
            self.rollback.get("previous_version")
            or self.rollback.get("strategy")
            or self.rollback.get("rollback_artifact")
            or self.rollback.get("restore_from")
        )
        return {
            "independent_critic": independent_critic,
            "eval_evidence": eval_evidence,
            "rollback_metadata": rollback_metadata,
        }

    def activation_requirements_met(self) -> bool:
        """Generated skills can be enabled only after all gates are present."""
        if not self.generated:
            return True
        return all(self.activation_requirements().values())

    def review_required(self) -> bool:
        """Whether this skill must stay proposed/disabled before execution."""
        return self.generated and not self.activation_requirements_met()

    def validate(self) -> list[str]:
        errors: list[str] = []
        if not self.name.strip():
            errors.append("name is required")
        if not self.entrypoint.strip():
            errors.append("entrypoint is required")
        if not self.description.strip():
            errors.append("description is required")
        return errors

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "entrypoint": self.entrypoint,
            "summary": self.summary,
            "version": self.version,
            "tags": self.tags,
            "permissions": self.permissions,
            "enabled": self.enabled,
            "skill_id": self.skill_id,
            "version_id": self.version_id,
            "content_hash": self.content_hash,
            "artifact_type": self.artifact_type,
            "instructions": self.instructions,
            "trigger": {"patterns": self.trigger.patterns, "auto": self.trigger.auto} if self.trigger else None,
            "config": self.config,
            "source": self.source.value,
            "tool_schema": self.tool_schema,
            "max_context_tokens": self.max_context_tokens,
            "author": self.author,
            "generated": self.generated,
            "lifecycle_status": self.lifecycle_status,
            "review": self.review,
            "evaluation": self.evaluation,
            "rollback": self.rollback,
        }


@dataclass
class SkillRunRecord:
    """Runtime metrics for a skill execution."""

    skill_name: str
    version: str
    latency_ms: float
    success: bool
    error: str | None = None
    created_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
