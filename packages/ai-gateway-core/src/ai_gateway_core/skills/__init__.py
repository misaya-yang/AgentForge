"""Shared skill-management primitives.

Gateway routes and the Runtime capability bridge share these contracts without
depending on an execution service.

Layout:
  - ``models`` — SkillManifest, SkillSource, TriggerConfig, SkillRunRecord
  - ``parser`` — parse_skill_md (Markdown → manifest)
  - ``executor`` — SkillExecutor (runtime dispatch)
  - ``builder`` — SkillBuilder (manifest/template composition)
  - ``registry`` — SkillRegistry (name → manifest lookup)
  - ``builtin`` — shipped skills (skill_create)
"""

from .artifact_repository import (
    DatabaseSkillArtifactRepository,
    SkillArtifactConflictError,
    SkillArtifactError,
    SkillArtifactNotFoundError,
    SkillArtifactUnavailableError,
    manifest_from_artifact,
    skill_content_hash,
)
from .builder import SkillBuilder
from .executor import SkillExecutor, register_builtin
from .models import SkillManifest, SkillRunRecord, SkillSource, TriggerConfig
from .parser import (
    UserSkillPolicyError,
    normalize_skill_md,
    parse_skill_md,
    parse_user_skill_md,
    serialize_user_skill_md,
)
from .registry import SkillRegistry

__all__ = [
    "SkillBuilder",
    "DatabaseSkillArtifactRepository",
    "SkillExecutor",
    "SkillManifest",
    "SkillRegistry",
    "SkillRunRecord",
    "SkillSource",
    "SkillArtifactConflictError",
    "SkillArtifactError",
    "SkillArtifactNotFoundError",
    "SkillArtifactUnavailableError",
    "TriggerConfig",
    "UserSkillPolicyError",
    "normalize_skill_md",
    "manifest_from_artifact",
    "parse_skill_md",
    "parse_user_skill_md",
    "serialize_user_skill_md",
    "register_builtin",
    "skill_content_hash",
]
