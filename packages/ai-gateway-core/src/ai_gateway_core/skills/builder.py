"""Skill builder with validation + approval-first workflow."""

from __future__ import annotations

import json
import uuid
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Any

from .models import SkillManifest


@dataclass
class SkillProposalResult:
    """Result for proposed skill creation."""

    proposal_id: str
    status: str
    reason: str


class SkillBuilder:
    """Create skills via proposal -> validation -> approval pipeline."""

    def __init__(self, database: Any | None = None) -> None:
        self.database = database

    def validate_manifest(self, manifest: SkillManifest) -> list[str]:
        """Validate a skill manifest and detect risky permissions."""
        errors = manifest.validate()
        dangerous_prefixes = ("os:", "exec:", "filesystem:write", "network:raw")
        for perm in manifest.permissions:
            if any(perm.startswith(prefix) for prefix in dangerous_prefixes):
                errors.append(
                    f"permission '{perm}' requires explicit approval via tool approval workflow"
                )
        return errors

    async def propose_skill(
        self,
        *,
        tenant_id: str,
        user_id: str,
        manifest: SkillManifest,
        proposed_by: str,
    ) -> SkillProposalResult:
        """Persist skill proposal as pending approval artifact."""
        errors = self.validate_manifest(manifest)
        if errors:
            return SkillProposalResult(
                proposal_id=str(uuid.uuid4()),
                status="rejected",
                reason="; ".join(errors),
            )

        proposal_id = str(uuid.uuid4())
        if self.database:
            await self.database.execute(
                """
                INSERT INTO assistant_audit_events (
                    event_id, tenant_id, user_id, event_type,
                    actor_id, resource_type, resource_id,
                    payload, created_at
                ) VALUES ($1, $2, $3, 'skill_proposed', $4, 'skill', $5, $6, NOW())
                """,
                str(uuid.uuid4()),
                tenant_id,
                user_id,
                proposed_by,
                proposal_id,
                json.dumps(manifest.to_dict()),
            )

        return SkillProposalResult(
            proposal_id=proposal_id,
            status="pending_approval",
            reason="Skill proposal submitted for approval",
        )

    async def mark_approved(
        self,
        *,
        tenant_id: str,
        user_id: str,
        proposal_id: str,
        approved_by: str,
        reason: str | None = None,
    ) -> None:
        """Emit audit trail after approval."""
        if not self.database:
            return

        await self.database.execute(
            """
            INSERT INTO assistant_audit_events (
                event_id, tenant_id, user_id, event_type,
                actor_id, resource_type, resource_id,
                payload, created_at
            ) VALUES ($1, $2, $3, 'skill_approved', $4, 'skill', $5, $6, NOW())
            """,
            str(uuid.uuid4()),
            tenant_id,
            user_id,
            approved_by,
            proposal_id,
            json.dumps(
                {
                    "reason": reason,
                    "approved_at": datetime.now(timezone.utc).isoformat(),
                }
            ),
        )
