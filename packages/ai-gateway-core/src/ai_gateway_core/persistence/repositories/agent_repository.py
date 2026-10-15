"""Tenant-scoped persistence for the Agent Studio domain.

UUIDs identify records but never authorize them. Every public repository
operation requires a tenant and caller, and every object query constrains the
tenant explicitly before applying Agent ACLs.
"""

from __future__ import annotations

import base64
import copy
import hashlib
import json
import os
import re
import secrets
import uuid
from collections.abc import AsyncIterator, Awaitable, Callable
from contextlib import asynccontextmanager
from datetime import datetime, timedelta, timezone
from typing import Any, Final

from ai_gateway_contracts.agent_runtime import agent_memory_principal

from ai_gateway_core.agents import spec as _agent_spec
from ai_gateway_core.agents.deletion import (
    build_runtime_cleanup_plan,
    cleanup_receipt_completed,
    validate_runtime_cleanup_inventory,
    validate_runtime_cleanup_plan,
    validate_runtime_cleanup_receipt,
)
from ai_gateway_core.eval.agent_version_candidate import (
    build_model_authorization_evidence,
    structured_agent_release_diff,
)
from ai_gateway_core.eval.dataset_manifest import build_eval_dataset_manifest
from ai_gateway_core.skills.artifact_repository import (
    SkillArtifactUnavailableError,
    manifest_from_artifact,
)

from .agent_resource_resolver import AgentKnowledgeResolver, authorized_dataset_ids
from .base import BaseRepository

AGENT_SPEC_SCHEMA_VERSION: Final = _agent_spec.AGENT_SPEC_SCHEMA_VERSION
ROLE_RANK: Final = {"viewer": 1, "editor": 2, "owner": 3}
_DATA_DELETION_EXECUTION_SCHEMA: Final = "agent-data-deletion-execution/v1"


def _agent_data_deletion_lock_key(*, tenant_id: str, agent_id: str, deletion_id: str) -> int:
    digest = hashlib.sha256(
        f"agent-data-deletion:{tenant_id}:{agent_id}:{deletion_id}".encode()
    ).digest()
    return int.from_bytes(digest[:8], byteorder="big", signed=True)


def _agent_data_deletion_claim_digest(
    *, deletion_id: str, generation: int, claim_token: str
) -> str:
    return hashlib.sha256(f"{deletion_id}\x00{generation}\x00{claim_token}".encode()).hexdigest()


class AgentRepositoryError(RuntimeError):
    """Base class for stable Agent repository failures."""


class AgentNotFoundError(AgentRepositoryError):
    """Agent is absent or deliberately hidden from the caller."""


class AgentDraftConflictError(AgentRepositoryError):
    def __init__(self, current_revision: int):
        super().__init__("AGENT_DRAFT_CONFLICT")
        self.current_revision = current_revision


class AgentLastOwnerError(AgentRepositoryError):
    """A mutation would leave an Agent without an Owner."""


class AgentPrincipalNotFoundError(AgentRepositoryError):
    """The requested principal is not available in the Agent tenant."""


class AgentValidationError(AgentRepositoryError):
    def __init__(self, errors: list[dict[str, str]]):
        super().__init__("AGENT_SPEC_INVALID")
        self.errors = errors


class AgentArchivedError(AgentRepositoryError):
    """Archived/deleted Agents cannot accept mutable Draft operations."""


class AgentRuntimeUnavailableError(AgentRepositoryError):
    """A requested Draft/Version/Publication cannot be executed safely."""

    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


class AgentReleaseEvaluationNotFoundError(AgentRepositoryError):
    """Release evaluation is absent or deliberately hidden from the caller."""


class AgentReleaseEvaluationStaleError(AgentRepositoryError):
    def __init__(self, current_revision: int):
        self.current_revision = current_revision
        super().__init__("AGENT_EVAL_STALE")


class AgentReleaseEvaluationTerminalError(AgentRepositoryError):
    """A terminal release evaluation cannot be executed or cancelled again."""


class AgentReleaseGateError(AgentRepositoryError):
    def __init__(self, code: str, findings: list[dict[str, Any]] | None = None):
        self.code = code
        self.findings = findings or []
        super().__init__(code)


class AgentReleaseIdempotencyConflictError(AgentRepositoryError):
    """An idempotency key was reused for a different release request."""


class AgentPublicationNotFoundError(AgentRepositoryError):
    """Publication is absent or deliberately hidden from the caller."""


def require_public_release_expiry(
    auth_mode: str, policy: dict[str, Any], *, now: datetime | None = None
) -> datetime | None:
    """Require a future, timezone-aware deadline for every new public release."""

    if auth_mode != "public":
        return None
    raw = policy.get("expires_at")
    try:
        expiry = raw if isinstance(raw, datetime) else datetime.fromisoformat(str(raw).replace("Z", "+00:00"))
    except ValueError as exc:
        raise AgentReleaseGateError("AGENT_PUBLIC_EXPIRY_REQUIRED") from exc
    if expiry.tzinfo is None or expiry.utcoffset() is None:
        raise AgentReleaseGateError("AGENT_PUBLIC_EXPIRY_REQUIRED")
    if expiry <= (now or datetime.now(timezone.utc)):
        raise AgentReleaseGateError("AGENT_PUBLIC_EXPIRY_EXPIRED")
    return expiry


def _require_active_publication(publication: Any) -> None:
    if not publication or publication["status"] != "active":
        raise AgentRuntimeUnavailableError("PUBLICATION_DISABLED")
    if _publication_expired(publication):
        raise AgentRuntimeUnavailableError("PUBLICATION_EXPIRED")


def _publication_expired(publication: Any) -> bool:
    expires_at = publication.get("expires_at")
    return expires_at is not None and expires_at <= datetime.now(timezone.utc)


def canonical_spec(spec: dict[str, Any]) -> str:
    """Return the deterministic JSON representation used by Draft/Version hashes."""

    return _agent_spec.canonical_spec(spec)


def hash_agent_spec(spec: dict[str, Any]) -> str:
    return hashlib.sha256(canonical_spec(spec).encode("utf-8")).hexdigest()


def _canonical_hash(value: Any) -> str:
    return _agent_spec._canonical_hash(value)


def unsafe_agent_spec_paths(value: Any, location: str = "spec") -> list[str]:
    """Return credential-shaped key paths using case/separator-insensitive matching."""

    return _agent_spec.unsafe_agent_spec_paths(value, location)


def agent_spec_safety_errors(spec: dict[str, Any]) -> list[dict[str, str]]:
    """Validate the closed public shape and the no-Secret invariant."""

    return _agent_spec.agent_spec_safety_errors(spec)


def _redact_unstructured_spec_value(value: Any) -> Any:
    return _agent_spec._redact_unstructured_spec_value(value)


def redact_agent_spec_for_read(spec: dict[str, Any]) -> dict[str, Any]:
    """Return only the closed public shape and redact unsafe legacy keys."""

    return _agent_spec.redact_agent_spec_for_read(spec)


def sanitize_agent_copy_spec(spec: dict[str, Any]) -> dict[str, Any]:
    """Allowlist presentation/instruction/model settings for a new Draft.

    AS-01 has no resource-authorization resolver, so it cannot prove that a
    source binding is accessible to the new Agent. Capabilities, Knowledge,
    memory, arbitrary legacy containers and credential-shaped fields are all
    dropped instead of being interpreted.
    """

    return _agent_spec.sanitize_agent_copy_spec(spec)


def validate_agent_spec(spec: dict[str, Any]) -> list[dict[str, str]]:
    """Validate the AS-01 structural subset without resolving runtime resources."""

    return _agent_spec.validate_agent_spec(spec)


def _row_to_dict(row: Any) -> dict[str, Any]:
    if not row:
        return {}
    result = dict(row)
    for key, value in list(result.items()):
        if isinstance(value, (datetime, uuid.UUID)):
            result[key] = value.isoformat() if isinstance(value, datetime) else str(value)
        elif isinstance(value, str) and key in {
            "spec",
            "resolved_spec",
            "policy",
            "retrieval_config",
            "config",
            "runtime_fingerprint",
            "channel_policy",
            "validation_snapshot",
            "gate_snapshot",
            "summary",
            "request_summary",
            "response_summary",
            "redaction_state",
            "object_keys",
            "deleted_counts",
        }:
            try:
                result[key] = json.loads(value)
            except (TypeError, json.JSONDecodeError):
                result[key] = {}
    return result


def _slugify(value: str) -> str:
    slug = re.sub(r"[^a-z0-9]+", "-", value.strip().lower()).strip("-")
    return slug[:128] or "agent"


def _encode_cursor(updated_at: datetime, agent_id: uuid.UUID) -> str:
    raw = f"{updated_at.isoformat()}|{agent_id}".encode()
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


def _decode_cursor(cursor: str) -> tuple[datetime, uuid.UUID]:
    try:
        padded = cursor + "=" * (-len(cursor) % 4)
        raw = base64.urlsafe_b64decode(padded.encode("ascii")).decode("utf-8")
        timestamp, raw_id = raw.rsplit("|", 1)
        return datetime.fromisoformat(timestamp), uuid.UUID(raw_id)
    except (ValueError, UnicodeDecodeError) as exc:
        raise AgentRepositoryError("AGENT_CURSOR_INVALID") from exc


class DatabaseAgentRepository(BaseRepository):
    """Async PostgreSQL Agent repository with tenant and ACL enforcement."""

    def __init__(
        self,
        pool_holder: Any,
        *,
        knowledge_resolver: AgentKnowledgeResolver | None = None,
    ) -> None:
        super().__init__(pool_holder)
        self._knowledge_resolver = knowledge_resolver

    def _require_enabled(self) -> None:
        if not self.enabled:
            raise AgentRepositoryError("AGENT_STORAGE_UNAVAILABLE")

    @staticmethod
    def _assert_role(actual_role: str, required_role: str) -> None:
        if ROLE_RANK.get(actual_role, 0) < ROLE_RANK[required_role]:
            # Deliberately collapse ACL denial into not-found at object paths.
            raise AgentNotFoundError("AGENT_NOT_FOUND")

    async def _authorized_agent(
        self,
        conn: Any,
        *,
        tenant_id: str,
        agent_id: str | uuid.UUID,
        user_id: str,
        required_role: str,
        is_tenant_admin: bool,
        for_update: bool = False,
    ) -> tuple[dict[str, Any], str]:
        lock = " FOR UPDATE OF a" if for_update else ""
        row = await conn.fetchrow(
            f"""
            SELECT a.*, m.role AS caller_role
            FROM agents a
            LEFT JOIN agent_members m
              ON m.tenant_id = a.tenant_id
             AND m.agent_id = a.agent_id
             AND m.principal_type = 'user'
             AND m.principal_id = $3
            WHERE a.tenant_id = $1
              AND a.agent_id = $2
              AND a.deleted_at IS NULL
              AND ($4::boolean OR m.principal_id IS NOT NULL)
            {lock}
            """,
            tenant_id,
            uuid.UUID(str(agent_id)),
            user_id,
            is_tenant_admin,
        )
        if not row:
            raise AgentNotFoundError("AGENT_NOT_FOUND")
        role = "owner" if is_tenant_admin else str(row["caller_role"])
        self._assert_role(role, required_role)
        return dict(row), role

    @staticmethod
    async def _audit(
        conn: Any,
        *,
        tenant_id: str,
        user_id: str,
        agent_id: str | uuid.UUID,
        action: str,
        summary: dict[str, Any] | None = None,
        agent_version_id: str | uuid.UUID | None = None,
        publication_id: str | uuid.UUID | None = None,
        channel: str | None = None,
    ) -> None:
        safe_summary = _redact_unstructured_spec_value(summary or {})
        if agent_version_id:
            safe_summary["agent_version_id"] = str(agent_version_id)
        if publication_id:
            safe_summary["publication_id"] = str(publication_id)
        if channel:
            safe_summary["channel"] = channel
        await conn.execute(
            """
            INSERT INTO audit_logs (
                event_type, user_id, tenant_id, resource_type, resource_id,
                action, request_summary, status
            ) VALUES ('agent_studio', $1, $2, 'agent', $3, $4, $5::jsonb, 'success')
            """,
            user_id,
            tenant_id,
            str(agent_id),
            action,
            json.dumps(safe_summary, ensure_ascii=False, sort_keys=True),
        )

    @classmethod
    async def _audit_binding_changes(
        cls,
        conn: Any,
        *,
        tenant_id: str,
        user_id: str,
        agent_id: str | uuid.UUID,
        old_spec: dict[str, Any],
        new_spec: dict[str, Any],
        agent_version_id: str | uuid.UUID | None = None,
    ) -> None:
        """Write one safe, dimensioned audit event per binding mutation."""

        def capabilities(spec: dict[str, Any]) -> set[tuple[str, str, str, str]]:
            return {
                (
                    str(binding["capability_type"]),
                    str(binding["resource_id"]),
                    str(binding.get("resource_version") or ""),
                    str(binding.get("schema_hash") or ""),
                )
                for binding in cls._capability_bindings(spec)
                if binding["capability_type"] in {"mcp", "connector", "skill"}
            }

        old_capabilities = capabilities(old_spec)
        new_capabilities = capabilities(new_spec)
        for action, bindings in (
            ("remove", old_capabilities - new_capabilities),
            ("add", new_capabilities - old_capabilities),
        ):
            for capability_type, resource_id, resource_version, schema_hash in sorted(bindings):
                summary = {
                    "capability_type": capability_type,
                    "resource_id": resource_id,
                }
                if resource_version:
                    summary["resource_version"] = resource_version
                if schema_hash:
                    summary["schema_hash"] = schema_hash
                await cls._audit(
                    conn,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    agent_id=agent_id,
                    action=f"{capability_type}_binding_{action}",
                    summary=summary,
                    agent_version_id=agent_version_id,
                )

        old_knowledge = {
            str(binding["dataset_id"]) for binding in cls._knowledge_bindings(old_spec)
        }
        new_knowledge = {
            str(binding["dataset_id"]) for binding in cls._knowledge_bindings(new_spec)
        }
        for action, dataset_ids in (
            ("remove", old_knowledge - new_knowledge),
            ("add", new_knowledge - old_knowledge),
        ):
            for dataset_id in sorted(dataset_ids):
                await cls._audit(
                    conn,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    agent_id=agent_id,
                    action=f"knowledge_binding_{action}",
                    summary={"dataset_id": dataset_id},
                    agent_version_id=agent_version_id,
                )

    @staticmethod
    async def _enforce_tenant_agent_quota(conn: Any, *, tenant_id: str) -> None:
        await conn.fetchval(
            "SELECT pg_advisory_xact_lock(hashtextextended('agent-count:' || $1, 0))",
            tenant_id,
        )
        policy_table = await conn.fetchval(
            """
            SELECT EXISTS (
                SELECT 1
                FROM pg_class table_meta
                JOIN pg_namespace schema_meta
                  ON schema_meta.oid = table_meta.relnamespace
                WHERE schema_meta.nspname = current_schema()
                  AND table_meta.relname = 'agent_governance_policies'
            )
            """
        )
        limit = 100
        if policy_table:
            limit = int(
                await conn.fetchval(
                    """
                    SELECT COALESCE(MIN(max_agents_per_tenant), 100)::int
                    FROM agent_governance_policies
                    WHERE tenant_id = $1
                    """,
                    tenant_id,
                )
                or 100
            )
        current = int(
            await conn.fetchval(
                """
                SELECT COUNT(*)::int
                FROM agents
                WHERE tenant_id = $1 AND deleted_at IS NULL
                """,
                tenant_id,
            )
            or 0
        )
        if current >= limit:
            raise AgentRepositoryError("AGENT_TENANT_AGENT_QUOTA_EXCEEDED")

    @staticmethod
    async def _enforce_active_publication_quota(
        conn: Any,
        *,
        tenant_id: str,
        agent_id: str,
    ) -> None:
        await conn.fetchval(
            "SELECT pg_advisory_xact_lock(hashtextextended('agent-publications:' || $1, 0))",
            tenant_id,
        )
        policy_table = await conn.fetchval(
            """
            SELECT EXISTS (
                SELECT 1
                FROM pg_class table_meta
                JOIN pg_namespace schema_meta
                  ON schema_meta.oid = table_meta.relnamespace
                WHERE schema_meta.nspname = current_schema()
                  AND table_meta.relname = 'agent_governance_policies'
            )
            """
        )
        limit = 10
        if policy_table:
            limit = int(
                await conn.fetchval(
                    """
                    SELECT COALESCE(max_active_publications, 10)::int
                    FROM agent_governance_policies
                    WHERE tenant_id = $1 AND agent_id = $2::uuid
                    """,
                    tenant_id,
                    agent_id,
                )
                or 10
            )
        current = int(
            await conn.fetchval(
                """
                SELECT COUNT(*)::int
                FROM agent_publications
                WHERE tenant_id = $1 AND status = 'active'
                """,
                tenant_id,
            )
            or 0
        )
        if current >= limit:
            raise AgentRepositoryError("AGENT_ACTIVE_PUBLICATION_QUOTA_EXCEEDED")

    async def create_agent(
        self,
        *,
        tenant_id: str,
        user_id: str,
        name: str,
        slug: str | None,
        description: str,
        spec: dict[str, Any],
        is_tenant_admin: bool = False,
    ) -> dict[str, Any]:
        self._require_enabled()
        safety_errors = agent_spec_safety_errors(spec)
        if safety_errors:
            raise AgentValidationError(safety_errors)
        agent_id = uuid.uuid4()
        draft_id = uuid.uuid4()
        resolved_slug = _slugify(slug or name)
        draft_hash = hash_agent_spec(spec)
        async with self._pool.acquire() as conn, conn.transaction():
            principal_exists = await conn.fetchval(
                """
                SELECT EXISTS(
                    SELECT 1 FROM users
                    WHERE tenant_id = $1 AND user_id = $2 AND status = 'active'
                )
                """,
                tenant_id,
                user_id,
            )
            if not principal_exists:
                raise AgentPrincipalNotFoundError("AGENT_PRINCIPAL_NOT_FOUND")
            await self._enforce_tenant_agent_quota(conn, tenant_id=tenant_id)
            await conn.execute(
                """
                INSERT INTO agents (
                    tenant_id, agent_id, slug, name, description, owner_id,
                    created_by, updated_by
                ) VALUES ($1, $2, $3, $4, $5, $6, $6, $6)
                """,
                tenant_id,
                agent_id,
                resolved_slug,
                name,
                description,
                user_id,
            )
            await conn.execute(
                """
                INSERT INTO agent_drafts (
                    tenant_id, draft_id, agent_id, revision, schema_version,
                    spec, spec_hash, updated_by
                ) VALUES ($1, $2, $3, 1, $4, $5::jsonb, $6, $7)
                """,
                tenant_id,
                draft_id,
                agent_id,
                AGENT_SPEC_SCHEMA_VERSION,
                canonical_spec(spec),
                draft_hash,
                user_id,
            )
            await self._replace_draft_knowledge(
                conn,
                tenant_id=tenant_id,
                draft_id=draft_id,
                spec=spec,
                user_id=user_id,
                is_tenant_admin=is_tenant_admin,
            )
            await self._replace_draft_skills(
                conn,
                tenant_id=tenant_id,
                draft_id=draft_id,
                spec=spec,
                user_id=user_id,
                is_tenant_admin=is_tenant_admin,
            )
            await conn.execute(
                """
                INSERT INTO agent_members (
                    tenant_id, agent_id, principal_type, principal_id, role, created_by
                ) VALUES ($1, $2, 'user', $3, 'owner', $3)
                """,
                tenant_id,
                agent_id,
                user_id,
            )
            await conn.execute(
                """
                UPDATE agents
                SET current_draft_id = $3, updated_by = $4
                WHERE tenant_id = $1 AND agent_id = $2
                """,
                tenant_id,
                agent_id,
                draft_id,
                user_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="create",
                summary={"draft_revision": 1},
            )
            await self._audit_binding_changes(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                old_spec={},
                new_spec=spec,
            )
            row = await conn.fetchrow(
                """
                SELECT a.*, 'owner'::text AS caller_role, d.revision AS draft_revision
                FROM agents a
                JOIN agent_drafts d
                  ON d.tenant_id = a.tenant_id AND d.draft_id = a.current_draft_id
                WHERE a.tenant_id = $1 AND a.agent_id = $2
                """,
                tenant_id,
                agent_id,
            )
        return _row_to_dict(row)

    async def list_agents(
        self,
        *,
        tenant_id: str,
        user_id: str,
        is_tenant_admin: bool,
        limit: int,
        cursor: str | None = None,
        status: str | None = None,
        owner_id: str | None = None,
        search: str | None = None,
        channel: str | None = None,
    ) -> dict[str, Any]:
        self._require_enabled()
        params: list[Any] = [tenant_id, user_id, is_tenant_admin]
        conditions = [
            "a.tenant_id = $1",
            "a.deleted_at IS NULL",
            "($3::boolean OR self_member.principal_id IS NOT NULL)",
        ]
        if status:
            params.append(status)
            conditions.append(f"a.status = ${len(params)}")
        if owner_id:
            params.append(owner_id)
            conditions.append(f"a.owner_id = ${len(params)}")
        if search:
            params.append(f"%{search}%")
            conditions.append(f"(a.name ILIKE ${len(params)} OR a.slug ILIKE ${len(params)})")
        if channel:
            params.append(channel)
            conditions.append(
                "EXISTS (SELECT 1 FROM agent_publications p "
                "WHERE p.tenant_id = a.tenant_id AND p.agent_id = a.agent_id "
                f"AND p.channel = ${len(params)} AND p.status = 'active')"
            )
        if cursor:
            cursor_time, cursor_id = _decode_cursor(cursor)
            params.extend([cursor_time, cursor_id])
            conditions.append(f"(a.updated_at, a.agent_id) < (${len(params) - 1}, ${len(params)})")
        params.append(limit + 1)
        rows = await self.fetch(
            f"""
            SELECT a.*, COALESCE(self_member.role, 'owner') AS caller_role,
                   d.revision AS draft_revision,
                   ARRAY(
                       SELECT DISTINCT publication.channel
                       FROM agent_publications publication
                       WHERE publication.tenant_id = a.tenant_id
                         AND publication.agent_id = a.agent_id
                         AND publication.status = 'active'
                       ORDER BY publication.channel
                   ) AS published_channels
            FROM agents a
            LEFT JOIN agent_members self_member
              ON self_member.tenant_id = a.tenant_id
             AND self_member.agent_id = a.agent_id
             AND self_member.principal_type = 'user'
             AND self_member.principal_id = $2
            JOIN agent_drafts d
              ON d.tenant_id = a.tenant_id AND d.draft_id = a.current_draft_id
            WHERE {" AND ".join(conditions)}
            ORDER BY a.updated_at DESC, a.agent_id DESC
            LIMIT ${len(params)}
            """,
            *params,
        )
        has_more = len(rows) > limit
        page = rows[:limit]
        next_cursor = None
        if has_more and page:
            last = page[-1]
            next_cursor = _encode_cursor(last["updated_at"], last["agent_id"])
        return {"items": [_row_to_dict(row) for row in page], "next_cursor": next_cursor}

    async def get_dataset_impact(
        self,
        *,
        tenant_id: str,
        dataset_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        """Count Agent references to a Dataset, revealing names only under Agent ACL."""
        self._require_enabled()
        rows = await self.fetch(
            """
            WITH draft_refs AS (
                SELECT a.agent_id
                FROM agent_draft_knowledge_bindings b
                JOIN agents a
                  ON a.tenant_id = b.tenant_id
                 AND a.current_draft_id = b.draft_id
                WHERE b.tenant_id = $1 AND b.dataset_id = $2
            ),
            version_refs AS (
                SELECT v.agent_id,
                       BOOL_OR(p.publication_id IS NOT NULL) AS active_publication,
                       COUNT(DISTINCT v.agent_version_id) FILTER (
                           WHERE p.publication_id IS NULL
                       ) AS historical_version_count
                FROM agent_version_knowledge_bindings b
                JOIN agent_versions v
                  ON v.tenant_id = b.tenant_id
                 AND v.agent_version_id = b.agent_version_id
                LEFT JOIN agent_publications p
                  ON p.tenant_id = v.tenant_id
                 AND p.agent_id = v.agent_id
                 AND p.version_id = v.agent_version_id
                 AND p.status = 'active'
                WHERE b.tenant_id = $1 AND b.dataset_id = $2
                GROUP BY v.agent_id
            ),
            referenced_agents AS (
                SELECT agent_id FROM draft_refs
                UNION
                SELECT agent_id FROM version_refs
            )
            SELECT a.agent_id, a.name,
                   a.deleted_at IS NULL
                     AND current_draft.draft_id IS NOT NULL
                     AND ($4::boolean OR self_member.principal_id IS NOT NULL) AS visible,
                   d.agent_id IS NOT NULL AS current_draft,
                   COALESCE(v.active_publication, FALSE) AS active_publication,
                   COALESCE(v.historical_version_count, 0) AS historical_version_count
            FROM referenced_agents r
            JOIN agents a ON a.tenant_id = $1 AND a.agent_id = r.agent_id
            LEFT JOIN agent_drafts current_draft
              ON current_draft.tenant_id = a.tenant_id
             AND current_draft.draft_id = a.current_draft_id
            LEFT JOIN draft_refs d ON d.agent_id = a.agent_id
            LEFT JOIN version_refs v ON v.agent_id = a.agent_id
            LEFT JOIN agent_members self_member
              ON self_member.tenant_id = a.tenant_id
             AND self_member.agent_id = a.agent_id
             AND self_member.principal_type = 'user'
             AND self_member.principal_id = $3
            ORDER BY a.name, a.agent_id
            """,
            tenant_id,
            dataset_id,
            user_id,
            is_tenant_admin,
        )
        counts = {
            category: {"visible": 0, "hidden": 0}
            for category in ("current_draft", "active_publication", "historical_version")
        }
        visible_agents: list[dict[str, Any]] = []
        hidden_agent_count = 0
        for row in rows:
            visible = bool(row["visible"])
            bucket = "visible" if visible else "hidden"
            current_draft = bool(row["current_draft"])
            active_publication = bool(row["active_publication"])
            historical_version_count = int(row["historical_version_count"])
            if current_draft:
                counts["current_draft"][bucket] += 1
            if active_publication:
                counts["active_publication"][bucket] += 1
            if historical_version_count:
                counts["historical_version"][bucket] += 1
            if visible:
                visible_agents.append(
                    {
                        "agent_id": str(row["agent_id"]),
                        "name": str(row["name"]),
                        "current_draft": current_draft,
                        "active_publication": active_publication,
                        "historical_version_count": historical_version_count,
                    }
                )
            else:
                hidden_agent_count += 1
        return {
            "dataset_id": dataset_id,
            "visible_agents": visible_agents,
            "hidden_agent_count": hidden_agent_count,
            "counts": counts,
        }

    async def get_agent(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            row, role = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            draft = await conn.fetchrow(
                """
                SELECT revision, schema_version, spec_hash, updated_at
                FROM agent_drafts
                WHERE tenant_id = $1 AND agent_id = $2
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
        result = _row_to_dict(row)
        result["caller_role"] = role
        result["draft"] = _row_to_dict(draft)
        return result

    async def update_agent(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        changes: dict[str, Any],
    ) -> dict[str, Any]:
        self._require_enabled()
        allowed = {key: changes[key] for key in ("name", "description", "slug") if key in changes}
        if not allowed:
            return await self.get_agent(
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                is_tenant_admin=is_tenant_admin,
            )
        if "slug" in allowed:
            allowed["slug"] = _slugify(str(allowed["slug"]))
        async with self._pool.acquire() as conn, conn.transaction():
            _, role = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="editor",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            params: list[Any] = [tenant_id, uuid.UUID(agent_id), user_id]
            assignments = ["updated_by = $3", "updated_at = NOW()"]
            for field, value in allowed.items():
                params.append(value)
                assignments.append(f"{field} = ${len(params)}")
            row = await conn.fetchrow(
                f"""
                UPDATE agents SET {", ".join(assignments)}
                WHERE tenant_id = $1 AND agent_id = $2 AND deleted_at IS NULL
                RETURNING *
                """,
                *params,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="update",
                summary={"fields": sorted(allowed)},
            )
        result = _row_to_dict(row)
        result["caller_role"] = role
        return result

    async def get_draft(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            row = await conn.fetchrow(
                """
                SELECT * FROM agent_drafts
                WHERE tenant_id = $1 AND agent_id = $2
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
        if not row:
            raise AgentNotFoundError("AGENT_NOT_FOUND")
        result = _row_to_dict(row)
        result["spec"] = redact_agent_spec_for_read(result.get("spec", {}))
        return result

    @staticmethod
    def _knowledge_bindings(spec: dict[str, Any]) -> list[dict[str, Any]]:
        raw = spec.get("knowledge")
        if not isinstance(raw, list):
            return []
        bindings: list[dict[str, Any]] = []
        seen: set[str] = set()
        for item in raw:
            if not isinstance(item, dict):
                continue
            dataset_id = item.get("dataset_id")
            if not isinstance(dataset_id, str) or not dataset_id or dataset_id in seen:
                continue
            seen.add(dataset_id)
            config = item.get("retrieval_config")
            bindings.append(
                {
                    "dataset_id": dataset_id,
                    "retrieval_config": config if isinstance(config, dict) else {},
                }
            )
        return bindings

    async def _authorized_knowledge_dataset_ids(
        self,
        conn: Any,
        *,
        tenant_id: str,
        user_id: str,
        dataset_ids: list[str],
        is_tenant_admin: bool,
    ) -> set[str]:
        """Ask KS for the effective Dataset ACL using signed actor roles."""

        if not dataset_ids:
            return set()
        raw_roles = await conn.fetchval(
            """
            SELECT roles
            FROM users
            WHERE tenant_id = $1 AND user_id = $2 AND status = 'active'
            """,
            tenant_id,
            user_id,
        )
        roles = (
            [str(role) for role in raw_roles if str(role)]
            if isinstance(raw_roles, (list, tuple))
            else []
        )
        return await authorized_dataset_ids(
            self._knowledge_resolver,
            tenant_id=tenant_id,
            user_id=user_id,
            dataset_ids=dataset_ids,
            is_tenant_admin=is_tenant_admin,
            roles=roles,
        )

    async def _replace_draft_knowledge(
        self,
        conn: Any,
        *,
        tenant_id: str,
        draft_id: uuid.UUID,
        spec: dict[str, Any],
        user_id: str,
        is_tenant_admin: bool,
    ) -> None:
        bindings = self._knowledge_bindings(spec)
        if bindings:
            dataset_ids = [item["dataset_id"] for item in bindings]
            visible = await self._authorized_knowledge_dataset_ids(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                dataset_ids=dataset_ids,
                is_tenant_admin=is_tenant_admin,
            )
            if visible != set(dataset_ids):
                raise AgentValidationError(
                    [
                        {
                            "field": "knowledge",
                            "code": "AGENT_RESOURCE_NOT_FOUND",
                            "message": "one or more Dataset bindings are unavailable",
                        }
                    ]
                )
        await conn.execute(
            "DELETE FROM agent_draft_knowledge_bindings WHERE tenant_id = $1 AND draft_id = $2",
            tenant_id,
            draft_id,
        )
        for binding in bindings:
            await conn.execute(
                """
                INSERT INTO agent_draft_knowledge_bindings (
                    tenant_id, draft_id, dataset_id, retrieval_config,
                    bound_by, authorization_checked_at
                ) VALUES ($1, $2, $3, $4::jsonb, $5, NOW())
                """,
                tenant_id,
                draft_id,
                binding["dataset_id"],
                canonical_spec(binding["retrieval_config"]),
                user_id,
            )

    @staticmethod
    def _skill_bindings(spec: dict[str, Any]) -> list[dict[str, Any]]:
        return [
            binding
            for binding in DatabaseAgentRepository._capability_bindings(spec)
            if binding["capability_type"] == "skill"
        ]

    async def _authorized_skill_versions(
        self,
        conn: Any,
        *,
        tenant_id: str,
        user_id: str,
        bindings: list[dict[str, Any]],
        is_tenant_admin: bool,
    ) -> dict[str, dict[str, Any]]:
        del is_tenant_admin
        if not bindings:
            return {}
        version_ids: list[uuid.UUID] = []
        for binding in bindings:
            try:
                version_ids.append(uuid.UUID(str(binding["resource_id"])))
            except (TypeError, ValueError) as exc:
                raise AgentValidationError(
                    [
                        {
                            "field": "capabilities",
                            "code": "AGENT_SKILL_VERSION_INVALID",
                            "message": "Skill bindings require an exact skill_version_id",
                        }
                    ]
                ) from exc
        rows = await conn.fetch(
            """
            SELECT skill.skill_id, skill.tenant_id, skill.user_id, skill.name,
                   skill.title, skill.description, skill.tags, skill.permissions,
                   skill.enabled, skill.status AS skill_status,
                   skill.disabled_at, skill.deleted_at,
                   version.version_id, version.version, version.revision,
                   version.manifest, version.entrypoint, version.content,
                   version.content_hash, version.status,
                   version.artifact_type, version.created_at,
                   EXISTS (
                       SELECT 1 FROM assistant_skill_version_revocations revoked
                       WHERE revoked.tenant_id = version.tenant_id
                         AND revoked.version_id = version.version_id
                   ) AS revoked
            FROM assistant_skill_versions AS version
            JOIN assistant_skills AS skill
              ON skill.tenant_id = version.tenant_id
             AND skill.skill_id = version.skill_id
            WHERE version.tenant_id = $1
              AND version.version_id = ANY($2::uuid[])
              AND skill.user_id = $3
              AND skill.enabled = TRUE
              AND skill.status = 'active'
              AND skill.deleted_at IS NULL
              AND version.status = 'active'
              AND version.artifact_type = 'tenant_instruction'
              AND NOT EXISTS (
                  SELECT 1 FROM assistant_skill_version_revocations revoked
                  WHERE revoked.tenant_id = version.tenant_id
                    AND revoked.version_id = version.version_id
              )
            """,
            tenant_id,
            version_ids,
            user_id,
        )
        resolved: dict[str, dict[str, Any]] = {}
        try:
            for row in rows:
                record = dict(row)
                manifest = manifest_from_artifact(record)
                resolved[str(record["version_id"])] = {
                    "skill_id": str(record["skill_id"]),
                    "skill_version_id": str(record["version_id"]),
                    "skill_name": manifest.name,
                    "content_hash": manifest.content_hash,
                }
        except SkillArtifactUnavailableError as exc:
            raise AgentValidationError(
                [
                    {
                        "field": "capabilities",
                        "code": exc.code,
                        "message": "Skill version content is unavailable",
                    }
                ]
            ) from exc
        if set(resolved) != {str(value) for value in version_ids}:
            raise AgentValidationError(
                [
                    {
                        "field": "capabilities",
                        "code": "AGENT_SKILL_VERSION_UNAVAILABLE",
                        "message": "one or more Skill versions are unavailable",
                    }
                ]
            )
        names = [artifact["skill_name"] for artifact in resolved.values()]
        if len(names) != len(set(names)):
            raise AgentValidationError(
                [
                    {
                        "field": "capabilities",
                        "code": "AGENT_SKILL_NAME_CONFLICT",
                        "message": "an Agent cannot bind multiple versions of one Skill",
                    }
                ]
            )
        return resolved

    async def _replace_draft_skills(
        self,
        conn: Any,
        *,
        tenant_id: str,
        draft_id: uuid.UUID,
        spec: dict[str, Any],
        user_id: str,
        is_tenant_admin: bool,
    ) -> None:
        bindings = self._skill_bindings(spec)
        table_exists = await conn.fetchval("SELECT TO_REGCLASS('agent_draft_skill_bindings')")
        if not table_exists:
            if bindings:
                raise AgentValidationError(
                    [
                        {
                            "field": "capabilities",
                            "code": "AGENT_SKILL_STORAGE_UNAVAILABLE",
                            "message": "Skill version binding storage is unavailable",
                        }
                    ]
                )
            return
        resolved = await self._authorized_skill_versions(
            conn,
            tenant_id=tenant_id,
            user_id=user_id,
            bindings=bindings,
            is_tenant_admin=is_tenant_admin,
        )
        await conn.execute(
            "DELETE FROM agent_draft_skill_bindings WHERE tenant_id = $1 AND draft_id = $2",
            tenant_id,
            draft_id,
        )
        for version_id, artifact in resolved.items():
            await conn.execute(
                """
                INSERT INTO agent_draft_skill_bindings (
                    tenant_id, draft_id, skill_id, skill_version_id,
                    skill_name, bound_by
                ) VALUES ($1, $2, $3, $4, $5, $6)
                """,
                tenant_id,
                draft_id,
                uuid.UUID(artifact["skill_id"]),
                uuid.UUID(version_id),
                artifact["skill_name"],
                user_id,
            )

    async def update_draft(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        expected_revision: int,
        spec: dict[str, Any],
        agent_changes: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        self._require_enabled()
        allowed_agent_changes = {
            key: agent_changes[key]
            for key in ("name", "description")
            if agent_changes is not None and key in agent_changes
        }
        async with self._pool.acquire() as conn, conn.transaction():
            agent, _ = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="editor",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            if agent["status"] in {"archived", "deleted"}:
                raise AgentArchivedError("AGENT_ARCHIVED")
            safety_errors = agent_spec_safety_errors(spec)
            if safety_errors:
                raise AgentValidationError(safety_errors)
            draft = await conn.fetchrow(
                """
                SELECT * FROM agent_drafts
                WHERE tenant_id = $1 AND agent_id = $2
                FOR UPDATE
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
            if not draft:
                raise AgentNotFoundError("AGENT_NOT_FOUND")
            if int(draft["revision"]) != expected_revision:
                raise AgentDraftConflictError(int(draft["revision"]))
            old_spec = (
                dict(draft["spec"])
                if isinstance(draft["spec"], dict)
                else json.loads(draft["spec"])
            )
            next_revision = expected_revision + 1
            spec_hash = hash_agent_spec(spec)
            await self._replace_draft_knowledge(
                conn,
                tenant_id=tenant_id,
                draft_id=draft["draft_id"],
                spec=spec,
                user_id=user_id,
                is_tenant_admin=is_tenant_admin,
            )
            await self._replace_draft_skills(
                conn,
                tenant_id=tenant_id,
                draft_id=draft["draft_id"],
                spec=spec,
                user_id=user_id,
                is_tenant_admin=is_tenant_admin,
            )
            if allowed_agent_changes:
                params: list[Any] = [tenant_id, uuid.UUID(agent_id), user_id]
                assignments = ["updated_by = $3", "updated_at = NOW()"]
                for field, value in allowed_agent_changes.items():
                    params.append(value)
                    assignments.append(f"{field} = ${len(params)}")
                await conn.execute(
                    f"""
                    UPDATE agents SET {", ".join(assignments)}
                    WHERE tenant_id = $1 AND agent_id = $2 AND deleted_at IS NULL
                    """,
                    *params,
                )
            row = await conn.fetchrow(
                """
                UPDATE agent_drafts
                SET revision = $4, schema_version = $5, spec = $6::jsonb,
                    spec_hash = $7, updated_by = $8, updated_at = NOW()
                WHERE tenant_id = $1 AND agent_id = $2 AND draft_id = $3
                RETURNING *
                """,
                tenant_id,
                uuid.UUID(agent_id),
                draft["draft_id"],
                next_revision,
                AGENT_SPEC_SCHEMA_VERSION,
                canonical_spec(spec),
                spec_hash,
                user_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="draft_update",
                summary={
                    "from_revision": expected_revision,
                    "to_revision": next_revision,
                    "agent_fields": sorted(allowed_agent_changes),
                },
            )
            await self._audit_binding_changes(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                old_spec=old_spec,
                new_spec=spec,
            )
        return _row_to_dict(row)

    async def validate_draft(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        draft = await self.get_draft(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=user_id,
            is_tenant_admin=is_tenant_admin,
        )
        errors = validate_agent_spec(draft["spec"])
        return {
            "valid": not errors,
            "revision": draft["revision"],
            "spec_hash": draft["spec_hash"],
            "errors": errors,
        }

    @staticmethod
    def _capability_bindings(spec: dict[str, Any]) -> list[dict[str, Any]]:
        raw = spec.get("capabilities")
        if not isinstance(raw, list):
            return []
        bindings: list[dict[str, Any]] = []
        seen: set[tuple[str, str]] = set()
        for item in raw:
            if not isinstance(item, dict):
                continue
            capability_type = item.get("type")
            resource_id = item.get("resource_id", "")
            if not isinstance(capability_type, str) or not isinstance(resource_id, str):
                continue
            key = (capability_type, resource_id)
            if key in seen:
                continue
            seen.add(key)
            config = item.get("config") if isinstance(item.get("config"), dict) else {}
            bindings.append(
                {
                    "capability_type": capability_type,
                    "resource_id": resource_id,
                    "resource_version": item.get("resource_version"),
                    "schema_hash": item.get("schema_hash"),
                    "config": config,
                }
            )
        return bindings

    async def _resolve_version_material(
        self,
        conn: Any,
        *,
        tenant_id: str,
        user_id: str,
        is_tenant_admin: bool,
        draft: Any,
        spec: dict[str, Any],
    ) -> tuple[list[dict[str, Any]], dict[str, dict[str, Any]], list[Any]]:
        """Reauthorize and normalize every resource used by a new Version."""

        errors = validate_agent_spec(spec)
        if errors:
            raise AgentValidationError(errors)
        capability_bindings = self._capability_bindings(spec)
        skill_bindings = [
            binding for binding in capability_bindings if binding["capability_type"] == "skill"
        ]
        skill_versions = await self._authorized_skill_versions(
            conn,
            tenant_id=tenant_id,
            user_id=user_id,
            bindings=skill_bindings,
            is_tenant_admin=is_tenant_admin,
        )
        sealed_capability_bindings: list[dict[str, Any]] = []
        for binding in capability_bindings:
            if binding["capability_type"] != "skill":
                sealed_capability_bindings.append(binding)
                continue
            artifact = skill_versions[binding["resource_id"]]
            sealed_capability_bindings.append(
                {
                    **binding,
                    "resource_id": artifact["skill_name"],
                    "resource_version": artifact["skill_version_id"],
                    "schema_hash": artifact["content_hash"],
                }
            )
        if any(
            binding["capability_type"] in {"mcp", "connector"} for binding in capability_bindings
        ):
            from .mcp_repository import DatabaseMCPRepository, MCPValidationError

            mcp_repository = DatabaseMCPRepository(self._holder)
            binding_errors: list[dict[str, str]] = []
            for index, binding in enumerate(capability_bindings):
                if binding["capability_type"] not in {"mcp", "connector"}:
                    continue
                try:
                    await mcp_repository.validate_version_binding(
                        tenant_id=tenant_id,
                        capability_type=binding["capability_type"],
                        resource_id=binding["resource_id"],
                        schema_hash=binding["schema_hash"],
                        risk_level=binding["config"].get("risk"),
                        config=binding["config"],
                        connection=conn,
                    )
                except MCPValidationError as exc:
                    binding_errors.append(
                        {
                            "field": f"capabilities[{index}]",
                            "code": exc.code,
                            "message": "Capability binding is unavailable or changed",
                        }
                    )
            if binding_errors:
                raise AgentValidationError(binding_errors)
        knowledge = await conn.fetch(
            """
            SELECT dataset_id, retrieval_config
            FROM agent_draft_knowledge_bindings
            WHERE tenant_id = $1 AND draft_id = $2
            ORDER BY dataset_id
            """,
            tenant_id,
            draft["draft_id"],
        )
        knowledge_ids = [str(binding["dataset_id"]) for binding in knowledge]
        if knowledge_ids:
            allowed_datasets = await self._authorized_knowledge_dataset_ids(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                dataset_ids=knowledge_ids,
                is_tenant_admin=is_tenant_admin,
            )
            if allowed_datasets != set(knowledge_ids):
                raise AgentValidationError(
                    [
                        {
                            "field": "knowledge",
                            "code": "AGENT_KNOWLEDGE_UNAVAILABLE",
                            "message": "one or more Dataset bindings are unavailable",
                        }
                    ]
                )
        return sealed_capability_bindings, skill_versions, list(knowledge)

    async def _insert_version_from_material(
        self,
        conn: Any,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        expected_revision: int,
        draft: Any,
        spec: dict[str, Any],
        sealed_capability_bindings: list[dict[str, Any]],
        skill_versions: dict[str, dict[str, Any]],
        knowledge: list[Any],
        release_evaluation_id: str | None = None,
        release_identity_hash: str | None = None,
    ) -> dict[str, Any]:
        """Insert and seal a Version inside the caller's transaction."""

        version_number = await conn.fetchval(
            """
            SELECT COALESCE(MAX(version_number), 0) + 1
            FROM agent_versions
            WHERE tenant_id = $1 AND agent_id = $2
            """,
            tenant_id,
            uuid.UUID(agent_id),
        )
        version_id = uuid.uuid4()
        if release_identity_hash is None:
            row = await conn.fetchrow(
                """
                INSERT INTO agent_versions (
                    tenant_id, agent_version_id, agent_id, version_number,
                    schema_version, resolved_spec, spec_hash, source_draft_id,
                    source_draft_revision, created_by
                ) VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, $8, $9, $10)
                RETURNING *
                """,
                tenant_id,
                version_id,
                uuid.UUID(agent_id),
                version_number,
                AGENT_SPEC_SCHEMA_VERSION,
                canonical_spec(spec),
                hash_agent_spec(spec),
                draft["draft_id"],
                expected_revision,
                user_id,
            )
        else:
            row = await conn.fetchrow(
                """
                INSERT INTO agent_versions (
                    tenant_id, agent_version_id, agent_id, version_number,
                    schema_version, resolved_spec, spec_hash, source_draft_id,
                    source_draft_revision, created_by, release_evaluation_id,
                    release_identity_hash
                ) VALUES (
                    $1, $2, $3, $4, $5, $6::jsonb, $7, $8, $9, $10, $11, $12
                )
                RETURNING *
                """,
                tenant_id,
                version_id,
                uuid.UUID(agent_id),
                version_number,
                AGENT_SPEC_SCHEMA_VERSION,
                canonical_spec(spec),
                hash_agent_spec(spec),
                draft["draft_id"],
                expected_revision,
                user_id,
                uuid.UUID(str(release_evaluation_id)),
                release_identity_hash,
            )
        for binding in sealed_capability_bindings:
            await conn.execute(
                """
                INSERT INTO agent_version_capabilities (
                    tenant_id, agent_version_id, capability_type, resource_id,
                    resource_version, schema_hash, config
                ) VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)
                """,
                tenant_id,
                version_id,
                binding["capability_type"],
                binding["resource_id"],
                binding["resource_version"],
                binding["schema_hash"],
                canonical_spec(binding["config"]),
            )
        for artifact in skill_versions.values():
            await conn.execute(
                """
                INSERT INTO agent_version_skill_bindings (
                    tenant_id, agent_version_id, skill_id, skill_version_id,
                    skill_name, content_hash
                ) VALUES ($1, $2, $3, $4, $5, $6)
                """,
                tenant_id,
                version_id,
                uuid.UUID(artifact["skill_id"]),
                uuid.UUID(artifact["skill_version_id"]),
                artifact["skill_name"],
                artifact["content_hash"],
            )
        for binding in knowledge:
            normalized_binding = _row_to_dict(binding)
            await conn.execute(
                """
                INSERT INTO agent_version_knowledge_bindings (
                    tenant_id, agent_version_id, dataset_id, retrieval_config,
                    bound_by, authorization_checked_at,
                    content_mode, historical_replayable
                ) VALUES ($1, $2, $3, $4::jsonb, $5, NOW(), 'live_latest', FALSE)
                """,
                tenant_id,
                version_id,
                binding["dataset_id"],
                canonical_spec(normalized_binding["retrieval_config"]),
                user_id,
            )
        row = await conn.fetchrow(
            """
            UPDATE agent_versions
            SET bindings_sealed = TRUE
            WHERE tenant_id = $1 AND agent_version_id = $2
            RETURNING *
            """,
            tenant_id,
            version_id,
        )
        return _row_to_dict(row)

    async def create_version(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        expected_revision: int,
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            agent, _ = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            if agent["status"] in {"archived", "deleted"}:
                raise AgentArchivedError("AGENT_ARCHIVED")
            draft = await conn.fetchrow(
                """
                SELECT * FROM agent_drafts
                WHERE tenant_id = $1 AND agent_id = $2
                FOR UPDATE
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
            if not draft:
                raise AgentNotFoundError("AGENT_NOT_FOUND")
            if int(draft["revision"]) != expected_revision:
                raise AgentDraftConflictError(int(draft["revision"]))
            spec = draft["spec"] if isinstance(draft["spec"], dict) else json.loads(draft["spec"])
            errors = validate_agent_spec(spec)
            if errors:
                raise AgentValidationError(errors)
            capability_bindings = self._capability_bindings(spec)
            skill_bindings = [
                binding for binding in capability_bindings if binding["capability_type"] == "skill"
            ]
            skill_versions = await self._authorized_skill_versions(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                bindings=skill_bindings,
                is_tenant_admin=is_tenant_admin,
            )
            sealed_capability_bindings: list[dict[str, Any]] = []
            for binding in capability_bindings:
                if binding["capability_type"] != "skill":
                    sealed_capability_bindings.append(binding)
                    continue
                artifact = skill_versions[binding["resource_id"]]
                sealed_capability_bindings.append(
                    {
                        **binding,
                        "resource_id": artifact["skill_name"],
                        "resource_version": artifact["skill_version_id"],
                        "schema_hash": artifact["content_hash"],
                    }
                )
            if any(
                binding["capability_type"] in {"mcp", "connector"}
                for binding in capability_bindings
            ):
                from .mcp_repository import (
                    DatabaseMCPRepository,
                    MCPValidationError,
                )

                mcp_repository = DatabaseMCPRepository(self._holder)
                binding_errors: list[dict[str, str]] = []
                for index, binding in enumerate(capability_bindings):
                    if binding["capability_type"] not in {"mcp", "connector"}:
                        continue
                    try:
                        await mcp_repository.validate_version_binding(
                            tenant_id=tenant_id,
                            capability_type=binding["capability_type"],
                            resource_id=binding["resource_id"],
                            schema_hash=binding["schema_hash"],
                            risk_level=binding["config"].get("risk"),
                            config=binding["config"],
                            connection=conn,
                        )
                    except MCPValidationError as exc:
                        binding_errors.append(
                            {
                                "field": f"capabilities[{index}]",
                                "code": exc.code,
                                "message": "Capability binding is unavailable or changed",
                            }
                        )
                if binding_errors:
                    raise AgentValidationError(binding_errors)
            knowledge = await conn.fetch(
                """
                SELECT dataset_id, retrieval_config
                FROM agent_draft_knowledge_bindings
                WHERE tenant_id = $1 AND draft_id = $2
                """,
                tenant_id,
                draft["draft_id"],
            )
            knowledge_ids = [str(binding["dataset_id"]) for binding in knowledge]
            if knowledge_ids:
                allowed_datasets = await self._authorized_knowledge_dataset_ids(
                    conn,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    dataset_ids=knowledge_ids,
                    is_tenant_admin=is_tenant_admin,
                )
                if allowed_datasets != set(knowledge_ids):
                    raise AgentValidationError(
                        [
                            {
                                "field": "knowledge",
                                "code": "AGENT_KNOWLEDGE_UNAVAILABLE",
                                "message": "one or more Dataset bindings are unavailable",
                            }
                        ]
                    )
            version_number = await conn.fetchval(
                """
                SELECT COALESCE(MAX(version_number), 0) + 1
                FROM agent_versions
                WHERE tenant_id = $1 AND agent_id = $2
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
            version_id = uuid.uuid4()
            row = await conn.fetchrow(
                """
                INSERT INTO agent_versions (
                    tenant_id, agent_version_id, agent_id, version_number,
                    schema_version, resolved_spec, spec_hash, source_draft_id,
                    source_draft_revision, created_by
                ) VALUES ($1, $2, $3, $4, $5, $6::jsonb, $7, $8, $9, $10)
                RETURNING *
                """,
                tenant_id,
                version_id,
                uuid.UUID(agent_id),
                version_number,
                AGENT_SPEC_SCHEMA_VERSION,
                canonical_spec(spec),
                hash_agent_spec(spec),
                draft["draft_id"],
                expected_revision,
                user_id,
            )
            for binding in sealed_capability_bindings:
                await conn.execute(
                    """
                    INSERT INTO agent_version_capabilities (
                        tenant_id, agent_version_id, capability_type, resource_id,
                        resource_version, schema_hash, config
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7::jsonb)
                    """,
                    tenant_id,
                    version_id,
                    binding["capability_type"],
                    binding["resource_id"],
                    binding["resource_version"],
                    binding["schema_hash"],
                    canonical_spec(binding["config"]),
                )
            for artifact in skill_versions.values():
                await conn.execute(
                    """
                    INSERT INTO agent_version_skill_bindings (
                        tenant_id, agent_version_id, skill_id, skill_version_id,
                        skill_name, content_hash
                    ) VALUES ($1, $2, $3, $4, $5, $6)
                    """,
                    tenant_id,
                    version_id,
                    uuid.UUID(artifact["skill_id"]),
                    uuid.UUID(artifact["skill_version_id"]),
                    artifact["skill_name"],
                    artifact["content_hash"],
                )
            for binding in knowledge:
                normalized_binding = _row_to_dict(binding)
                await conn.execute(
                    """
                    INSERT INTO agent_version_knowledge_bindings (
                        tenant_id, agent_version_id, dataset_id, retrieval_config,
                        bound_by, authorization_checked_at,
                        content_mode, historical_replayable
                    ) VALUES ($1, $2, $3, $4::jsonb, $5, NOW(), 'live_latest', FALSE)
                    """,
                    tenant_id,
                    version_id,
                    binding["dataset_id"],
                    canonical_spec(normalized_binding["retrieval_config"]),
                    user_id,
                )
            row = await conn.fetchrow(
                """
                UPDATE agent_versions
                SET bindings_sealed = TRUE
                WHERE tenant_id = $1 AND agent_version_id = $2
                RETURNING *
                """,
                tenant_id,
                version_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="version_create",
                summary={"version_number": version_number, "draft_revision": expected_revision},
            )
            await self._audit_binding_changes(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                old_spec={},
                new_spec=spec,
                agent_version_id=version_id,
            )
        return _row_to_dict(row)

    @staticmethod
    def _json_mapping(value: Any) -> dict[str, Any]:
        if isinstance(value, dict):
            return dict(value)
        if isinstance(value, str):
            try:
                decoded = json.loads(value)
            except (TypeError, json.JSONDecodeError):
                return {}
            return decoded if isinstance(decoded, dict) else {}
        return {}

    async def _eval_dataset_snapshot_from_conn(
        self,
        conn: Any,
        *,
        tenant_id: str,
        dataset_id: str,
        lock: bool = False,
    ) -> dict[str, Any]:
        dataset_lock_clause = "FOR UPDATE" if lock else ""
        dataset = await conn.fetchrow(
            f"""
            SELECT dataset_id, tenant_id, name, description, version,
                   schema, metadata, updated_at
            FROM eval_datasets
            WHERE tenant_id = $1 AND dataset_id = $2
            {dataset_lock_clause}
            """,
            tenant_id,
            uuid.UUID(dataset_id),
        )
        if not dataset:
            raise AgentReleaseGateError("AGENT_EVAL_DATASET_NOT_FOUND")
        examples_lock_clause = "FOR SHARE" if lock else ""
        examples = await conn.fetch(
            f"""
            SELECT example_id, split, input, expected_output, metadata,
                   source_trace_id, source_span_id
            FROM eval_examples
            WHERE tenant_id = $1 AND dataset_id = $2
            ORDER BY COALESCE(metadata->>'case_id', example_id::text), example_id
            {examples_lock_clause}
            """,
            tenant_id,
            uuid.UUID(dataset_id),
        )
        manifest = build_eval_dataset_manifest(dataset, examples)
        return {
            "dataset_id": str(dataset["dataset_id"]),
            "tenant_id": str(dataset["tenant_id"]),
            "version": str(dataset.get("version") or "v1"),
            "manifest_hash": _canonical_hash(manifest),
            "example_count": len(manifest["examples"]),
        }

    async def resolve_eval_dataset_snapshot(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        dataset_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        """Resolve one tenant Eval Dataset to a content-bound, prompt-free identity."""

        self._require_enabled()
        async with (
            self._pool.acquire() as conn,
            conn.transaction(isolation="repeatable_read", readonly=True),
        ):
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            return await self._eval_dataset_snapshot_from_conn(
                conn,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )

    @staticmethod
    async def _lock_release_idempotency_key(
        conn: Any,
        *,
        tenant_id: str,
        operation: str,
        idempotency_key_hash: str,
    ) -> None:
        await conn.execute(
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            f"agent-release:{tenant_id}:{operation}:{idempotency_key_hash}",
        )

    async def _validate_model_authorization(
        self,
        conn: Any,
        *,
        tenant_id: str,
        spec: dict[str, Any],
        model_authorization: dict[str, Any] | None,
        actor_model_access_levels: set[str] | None,
        is_tenant_admin: bool,
        model_authorization_revalidator: (Callable[[], Awaitable[dict[str, Any]]] | None) = None,
    ) -> None:
        requested = spec.get("model") if isinstance(spec.get("model"), dict) else {}
        model_id = str(requested.get("model_id") or "")
        requested_provider = str(requested.get("provider_id") or "")
        proof = model_authorization if isinstance(model_authorization, dict) else {}
        provider_id = str(proof.get("provider_id") or requested_provider)
        if (
            not model_id
            or not provider_id
            or str(proof.get("model_id") or "") != model_id
            or (requested_provider and requested_provider != provider_id)
            or not bool(proof.get("runtime_provider_configured"))
        ):
            raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_UNAVAILABLE")

        source = str(proof.get("source") or "")
        if source == "database":
            row = await conn.fetchrow(
                """
                SELECT model.model_id, model.provider_id, model.access_level,
                       model.is_enabled AS model_enabled,
                       model.updated_at AS model_updated_at,
                       provider.is_enabled AS provider_enabled,
                       provider.updated_at AS provider_updated_at
                FROM llm_models AS model
                JOIN llm_providers AS provider
                  ON provider.tenant_id = model.tenant_id
                 AND provider.provider_id = model.provider_id
                WHERE model.tenant_id = $1
                  AND model.provider_id = $2
                  AND model.model_id = $3
                FOR SHARE OF model, provider
                """,
                tenant_id,
                provider_id,
                model_id,
            )
            if not row or not bool(row["model_enabled"]) or not bool(row["provider_enabled"]):
                raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_UNAVAILABLE")
            allowed = actor_model_access_levels or (
                {"public", "premium", "admin"} if is_tenant_admin else {"public"}
            )
            if str(row["access_level"] or "public") not in allowed:
                raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_FORBIDDEN")
            current = build_model_authorization_evidence(
                source="database",
                model_id=str(row["model_id"]),
                provider_id=str(row["provider_id"]),
                access_level=str(row["access_level"] or "public"),
                model_enabled=bool(row["model_enabled"]),
                provider_enabled=bool(row["provider_enabled"]),
                runtime_provider_configured=True,
                model_updated_at=row["model_updated_at"],
                provider_updated_at=row["provider_updated_at"],
            )
            if current != proof:
                raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_AUTHORIZATION_STALE")
            if model_authorization_revalidator is None:
                raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_AUTHORIZATION_UNVERIFIABLE")
            current = await model_authorization_revalidator()
            if current != proof:
                raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_AUTHORIZATION_STALE")
            return

        if source == "e2e_stub":
            stub_enabled = os.getenv("ASSISTANT_E2E_STUB_LLM", "").strip().lower() in {
                "1",
                "true",
                "yes",
                "on",
            }
            if not stub_enabled:
                raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_AUTHORIZATION_STALE")
            return

        if model_authorization_revalidator is None:
            raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_AUTHORIZATION_UNVERIFIABLE")
        current = await model_authorization_revalidator()
        if current != proof:
            raise AgentReleaseGateError("AGENT_RUNTIME_MODEL_AUTHORIZATION_STALE")

    async def _release_evaluation_result(
        self,
        conn: Any,
        *,
        tenant_id: str,
        row: Any,
    ) -> dict[str, Any]:
        events = await conn.fetch(
            """
            SELECT * FROM agent_release_evaluation_events
            WHERE tenant_id = $1 AND evaluation_id = $2
            ORDER BY sequence
            """,
            tenant_id,
            row["evaluation_id"],
        )
        result = _row_to_dict(row)
        result["events"] = [_row_to_dict(event) for event in events]
        result["stale"] = False
        return result

    async def create_release_evaluation(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        candidate: dict[str, Any],
        profile: dict[str, Any],
        actor_model_access_levels: set[str] | None = None,
        model_authorization_revalidator: (Callable[[], Awaitable[dict[str, Any]]] | None) = None,
    ) -> dict[str, Any]:
        """Persist a durable queued evaluation after current resource checks."""

        self._require_enabled()
        if (
            str(candidate.get("tenant_id") or "") != tenant_id
            or str(candidate.get("agent_id") or "") != agent_id
        ):
            raise AgentReleaseGateError("AGENT_EVAL_TARGET_MISMATCH")
        require_public_release_expiry(
            str(candidate.get("auth_mode") or ""),
            dict(candidate.get("channel_policy") or {}),
        )
        async with self._pool.acquire() as conn, conn.transaction():
            agent, _ = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            if agent["status"] in {"archived", "deleted"}:
                raise AgentArchivedError("AGENT_ARCHIVED")
            draft = await conn.fetchrow(
                """
                SELECT * FROM agent_drafts
                WHERE tenant_id = $1 AND agent_id = $2
                FOR UPDATE
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
            if not draft:
                raise AgentNotFoundError("AGENT_NOT_FOUND")
            if (
                int(draft["revision"]) != int(candidate.get("draft_revision") or 0)
                or str(draft["draft_id"]) != str(candidate.get("draft_id") or "")
                or str(draft["spec_hash"]) != str(candidate.get("spec_hash") or "")
            ):
                raise AgentReleaseEvaluationStaleError(int(draft["revision"]))
            spec = draft["spec"] if isinstance(draft["spec"], dict) else json.loads(draft["spec"])
            errors = validate_agent_spec(spec)
            if errors:
                raise AgentValidationError(errors)
            await self._resolve_version_material(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                is_tenant_admin=is_tenant_admin,
                draft=draft,
                spec=spec,
            )
            await self._validate_model_authorization(
                conn,
                tenant_id=tenant_id,
                spec=spec,
                model_authorization=candidate.get("model_authorization"),
                actor_model_access_levels=actor_model_access_levels,
                is_tenant_admin=is_tenant_admin,
                model_authorization_revalidator=model_authorization_revalidator,
            )
            dataset_id = candidate.get("dataset_id")
            if dataset_id:
                dataset = await self._eval_dataset_snapshot_from_conn(
                    conn,
                    tenant_id=tenant_id,
                    dataset_id=str(dataset_id),
                    lock=True,
                )
                if dataset["version"] != str(candidate.get("dataset_version") or "") or dataset[
                    "manifest_hash"
                ] != str(candidate.get("dataset_manifest_hash") or ""):
                    raise AgentReleaseGateError("AGENT_EVAL_DATASET_STALE")
            evaluation_id = uuid.uuid4()
            validation_snapshot = {
                "schema_version": "agent-release-validation/v1",
                "draft_revision": int(draft["revision"]),
                "spec_hash": str(draft["spec_hash"]),
                "runtime_fingerprint_hash": str(candidate["runtime_fingerprint_hash"]),
                "release_identity_hash": str(candidate["release_identity_hash"]),
                "evaluation_identity_hash": str(
                    candidate.get("evaluation_identity_hash") or candidate["release_identity_hash"]
                ),
                "dataset_manifest_hash": candidate.get("dataset_manifest_hash"),
                "resource_authorization_rechecked": True,
                "secret_safety_rechecked": True,
                "lifecycle": "queued",
            }
            gate_snapshot = {
                "schema_version": "agent-release-gate/v1",
                "status": "queued",
                "profile_id": str(profile["profile_id"]),
                "profile_version": str(profile["profile_version"]),
                "execution_scope": str(profile.get("execution_scope") or ""),
                "model_quality_evaluated": bool(profile.get("model_quality_evaluated", False)),
                "blocking_findings": [],
                "non_blocking_findings": [],
            }
            row = await conn.fetchrow(
                """
                INSERT INTO agent_release_evaluations (
                    tenant_id, evaluation_id, agent_id, draft_id, draft_revision,
                    spec_hash, runtime_fingerprint, runtime_fingerprint_hash,
                    release_identity_hash, evaluation_identity_hash,
                    profile_id, profile_version, dataset_id, dataset_version,
                    dataset_manifest_hash, channel, auth_mode, channel_policy,
                    channel_policy_hash, status, validation_snapshot,
                    gate_snapshot, created_by, completed_at
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7::jsonb, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18::jsonb,
                    $19, 'queued', $20::jsonb, $21::jsonb, $22, NULL
                )
                RETURNING *
                """,
                tenant_id,
                evaluation_id,
                uuid.UUID(agent_id),
                draft["draft_id"],
                int(draft["revision"]),
                str(draft["spec_hash"]),
                canonical_spec(candidate["runtime_fingerprint"]),
                str(candidate["runtime_fingerprint_hash"]),
                str(candidate["release_identity_hash"]),
                str(
                    candidate.get("evaluation_identity_hash") or candidate["release_identity_hash"]
                ),
                str(profile["profile_id"]),
                str(profile["profile_version"]),
                uuid.UUID(str(dataset_id)) if dataset_id else None,
                str(candidate.get("dataset_version") or "") or None,
                str(candidate.get("dataset_manifest_hash") or "") or None,
                str(candidate["channel"]),
                str(candidate["auth_mode"]),
                canonical_spec(candidate["channel_policy"]),
                str(candidate["channel_policy_hash"]),
                canonical_spec(validation_snapshot),
                canonical_spec(gate_snapshot),
                user_id,
            )
            await conn.execute(
                """
                INSERT INTO agent_release_evaluation_events (
                    tenant_id, evaluation_id, sequence, status, summary
                ) VALUES ($1, $2, 1, 'queued', $3::jsonb)
                """,
                tenant_id,
                evaluation_id,
                canonical_spec(
                    {
                        "profile_id": str(profile["profile_id"]),
                        "profile_version": str(profile["profile_version"]),
                    }
                ),
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="release_evaluation_queued",
                summary={
                    "evaluation_id": str(evaluation_id),
                    "draft_revision": int(draft["revision"]),
                    "profile_id": str(profile["profile_id"]),
                },
            )
            return await self._release_evaluation_result(conn, tenant_id=tenant_id, row=row)

    async def start_release_evaluation(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        evaluation_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        """Claim a queued evaluation; only the caller that transitions it executes."""

        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
            )
            row = await conn.fetchrow(
                """
                SELECT * FROM agent_release_evaluations
                WHERE tenant_id = $1 AND agent_id = $2 AND evaluation_id = $3
                FOR UPDATE
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
            )
            if not row:
                raise AgentReleaseEvaluationNotFoundError("AGENT_EVAL_NOT_FOUND")
            if str(row["status"]) != "queued":
                result = await self._release_evaluation_result(conn, tenant_id=tenant_id, row=row)
                result["execution_claimed"] = False
                return result
            validation = _row_to_dict(row).get("validation_snapshot") or {}
            validation["lifecycle"] = "running"
            gate = _row_to_dict(row).get("gate_snapshot") or {}
            gate["status"] = "running"
            row = await conn.fetchrow(
                """
                UPDATE agent_release_evaluations
                SET status = 'running', started_at = NOW(),
                    validation_snapshot = $4::jsonb, gate_snapshot = $5::jsonb
                WHERE tenant_id = $1 AND agent_id = $2 AND evaluation_id = $3
                RETURNING *
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
                canonical_spec(validation),
                canonical_spec(gate),
            )
            await conn.execute(
                """
                INSERT INTO agent_release_evaluation_events (
                    tenant_id, evaluation_id, sequence, status, summary
                ) VALUES ($1, $2, 2, 'running', $3::jsonb)
                """,
                tenant_id,
                uuid.UUID(evaluation_id),
                canonical_spec({"execution_scope": gate.get("execution_scope") or ""}),
            )
            result = await self._release_evaluation_result(conn, tenant_id=tenant_id, row=row)
            result["execution_claimed"] = True
            return result

    async def complete_release_evaluation(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        evaluation_id: str,
        user_id: str,
        is_tenant_admin: bool,
        candidate: dict[str, Any],
        gate: dict[str, Any],
        actor_model_access_levels: set[str] | None = None,
        model_authorization_revalidator: (Callable[[], Awaitable[dict[str, Any]]] | None) = None,
    ) -> dict[str, Any]:
        """Write one terminal result unless cancellation won the race."""

        status = str(gate.get("status") or "")
        if status not in {"passed", "failed"}:
            raise AgentReleaseGateError("AGENT_EVAL_STATUS_INVALID")
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
            )
            row = await conn.fetchrow(
                """
                SELECT * FROM agent_release_evaluations
                WHERE tenant_id = $1 AND agent_id = $2 AND evaluation_id = $3
                FOR UPDATE
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
            )
            if not row:
                raise AgentReleaseEvaluationNotFoundError("AGENT_EVAL_NOT_FOUND")
            if str(row["status"]) == "cancelled":
                return await self._release_evaluation_result(conn, tenant_id=tenant_id, row=row)
            if str(row["status"]) in {"passed", "failed"}:
                return await self._release_evaluation_result(conn, tenant_id=tenant_id, row=row)
            if str(row["status"]) != "running":
                raise AgentReleaseGateError("AGENT_EVAL_LIFECYCLE_INVALID")
            candidate_checks = {
                "draft_id": str(row["draft_id"]),
                "draft_revision": int(row["draft_revision"]),
                "spec_hash": str(row["spec_hash"]),
                "runtime_fingerprint_hash": str(row["runtime_fingerprint_hash"]),
                "release_identity_hash": str(row["release_identity_hash"]),
                "evaluation_identity_hash": str(
                    row["evaluation_identity_hash"] or row["release_identity_hash"]
                ),
                "channel_policy_hash": str(row["channel_policy_hash"]),
            }
            if status == "passed" and any(
                candidate.get(key) != value for key, value in candidate_checks.items()
            ):
                raise AgentReleaseEvaluationStaleError(int(row["draft_revision"]))
            if status == "passed":
                draft = await conn.fetchrow(
                    """
                    SELECT * FROM agent_drafts
                    WHERE tenant_id = $1 AND agent_id = $2
                    FOR UPDATE
                    """,
                    tenant_id,
                    uuid.UUID(agent_id),
                )
                if (
                    not draft
                    or int(draft["revision"]) != int(row["draft_revision"])
                    or str(draft["draft_id"]) != str(row["draft_id"])
                    or str(draft["spec_hash"]) != str(row["spec_hash"])
                ):
                    raise AgentReleaseEvaluationStaleError(int(draft["revision"]) if draft else 0)
                spec = (
                    draft["spec"] if isinstance(draft["spec"], dict) else json.loads(draft["spec"])
                )
                await self._resolve_version_material(
                    conn,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    is_tenant_admin=is_tenant_admin,
                    draft=draft,
                    spec=spec,
                )
                await self._validate_model_authorization(
                    conn,
                    tenant_id=tenant_id,
                    spec=spec,
                    model_authorization=candidate.get("model_authorization"),
                    actor_model_access_levels=actor_model_access_levels,
                    is_tenant_admin=is_tenant_admin,
                    model_authorization_revalidator=model_authorization_revalidator,
                )
                if row["dataset_id"]:
                    dataset = await self._eval_dataset_snapshot_from_conn(
                        conn,
                        tenant_id=tenant_id,
                        dataset_id=str(row["dataset_id"]),
                        lock=True,
                    )
                    if dataset["version"] != str(row["dataset_version"] or "") or dataset[
                        "manifest_hash"
                    ] != str(row["dataset_manifest_hash"] or ""):
                        raise AgentReleaseGateError("AGENT_EVAL_DATASET_STALE")
            blocking = gate.get("blocking_findings")
            blocking = blocking if isinstance(blocking, list) else []
            non_blocking = gate.get("non_blocking_findings")
            non_blocking = non_blocking if isinstance(non_blocking, list) else []
            validation = _row_to_dict(row).get("validation_snapshot") or {}
            validation.update(
                {
                    "lifecycle": status,
                    "resource_authorization_rechecked": status == "passed",
                    "blocking_finding_count": len(blocking),
                    "non_blocking_finding_count": len(non_blocking),
                }
            )
            row = await conn.fetchrow(
                """
                UPDATE agent_release_evaluations
                SET status = $4, validation_snapshot = $5::jsonb,
                    gate_snapshot = $6::jsonb, completed_at = NOW()
                WHERE tenant_id = $1 AND agent_id = $2 AND evaluation_id = $3
                RETURNING *
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
                status,
                canonical_spec(validation),
                canonical_spec(gate),
            )
            sequence = await conn.fetchval(
                """
                SELECT COALESCE(MAX(sequence), 0) + 1
                FROM agent_release_evaluation_events
                WHERE tenant_id = $1 AND evaluation_id = $2
                """,
                tenant_id,
                uuid.UUID(evaluation_id),
            )
            await conn.execute(
                """
                INSERT INTO agent_release_evaluation_events (
                    tenant_id, evaluation_id, sequence, status, summary
                ) VALUES ($1, $2, $3, $4, $5::jsonb)
                """,
                tenant_id,
                uuid.UUID(evaluation_id),
                int(sequence),
                status,
                canonical_spec(
                    {
                        "blocking_finding_count": len(blocking),
                        "non_blocking_finding_count": len(non_blocking),
                    }
                ),
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="release_evaluation_completed",
                summary={"evaluation_id": evaluation_id, "status": status},
            )
            return await self._release_evaluation_result(conn, tenant_id=tenant_id, row=row)

    async def cancel_release_evaluation(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        evaluation_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        """Cancel queued/running work; terminal evidence remains immutable."""

        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
            )
            row = await conn.fetchrow(
                """
                SELECT * FROM agent_release_evaluations
                WHERE tenant_id = $1 AND agent_id = $2 AND evaluation_id = $3
                FOR UPDATE
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
            )
            if not row:
                raise AgentReleaseEvaluationNotFoundError("AGENT_EVAL_NOT_FOUND")
            current_status = str(row["status"])
            if current_status == "cancelled":
                return await self._release_evaluation_result(conn, tenant_id=tenant_id, row=row)
            if current_status in {"passed", "failed"}:
                raise AgentReleaseEvaluationTerminalError("AGENT_EVAL_TERMINAL")
            validation = _row_to_dict(row).get("validation_snapshot") or {}
            validation.update({"lifecycle": "cancelled", "cancelled_from": current_status})
            gate = _row_to_dict(row).get("gate_snapshot") or {}
            gate.update(
                {
                    "status": "cancelled",
                    "blocking_findings": [],
                    "non_blocking_findings": [],
                }
            )
            row = await conn.fetchrow(
                """
                UPDATE agent_release_evaluations
                SET status = 'cancelled', validation_snapshot = $4::jsonb,
                    gate_snapshot = $5::jsonb, completed_at = NOW()
                WHERE tenant_id = $1 AND agent_id = $2 AND evaluation_id = $3
                RETURNING *
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
                canonical_spec(validation),
                canonical_spec(gate),
            )
            sequence = await conn.fetchval(
                """
                SELECT COALESCE(MAX(sequence), 0) + 1
                FROM agent_release_evaluation_events
                WHERE tenant_id = $1 AND evaluation_id = $2
                """,
                tenant_id,
                uuid.UUID(evaluation_id),
            )
            await conn.execute(
                """
                INSERT INTO agent_release_evaluation_events (
                    tenant_id, evaluation_id, sequence, status, summary
                ) VALUES ($1, $2, $3, 'cancelled', $4::jsonb)
                """,
                tenant_id,
                uuid.UUID(evaluation_id),
                int(sequence),
                canonical_spec({"cancelled_from": current_status}),
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="release_evaluation_cancelled",
                summary={"evaluation_id": evaluation_id, "cancelled_from": current_status},
            )
            return await self._release_evaluation_result(conn, tenant_id=tenant_id, row=row)

    async def record_release_evaluation(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        candidate: dict[str, Any],
        gate: dict[str, Any],
        actor_model_access_levels: set[str] | None = None,
        model_authorization_revalidator: (Callable[[], Awaitable[dict[str, Any]]] | None) = None,
    ) -> dict[str, Any]:
        """Compatibility helper that drives the same durable lifecycle."""

        queued = await self.create_release_evaluation(
            tenant_id=tenant_id,
            agent_id=agent_id,
            user_id=user_id,
            is_tenant_admin=is_tenant_admin,
            candidate=candidate,
            profile={
                "profile_id": gate["profile_id"],
                "profile_version": gate["profile_version"],
                "execution_scope": gate.get("execution_scope") or "",
                "model_quality_evaluated": gate.get("model_quality_evaluated", False),
            },
            actor_model_access_levels=actor_model_access_levels,
            model_authorization_revalidator=model_authorization_revalidator,
        )
        evaluation_id = str(queued["evaluation_id"])
        if str(gate.get("status") or "") == "cancelled":
            return await self.cancel_release_evaluation(
                tenant_id=tenant_id,
                agent_id=agent_id,
                evaluation_id=evaluation_id,
                user_id=user_id,
                is_tenant_admin=is_tenant_admin,
            )
        await self.start_release_evaluation(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=evaluation_id,
            user_id=user_id,
            is_tenant_admin=is_tenant_admin,
        )
        return await self.complete_release_evaluation(
            tenant_id=tenant_id,
            agent_id=agent_id,
            evaluation_id=evaluation_id,
            user_id=user_id,
            is_tenant_admin=is_tenant_admin,
            candidate=candidate,
            gate=gate,
            actor_model_access_levels=actor_model_access_levels,
            model_authorization_revalidator=model_authorization_revalidator,
        )

    async def list_release_evaluations(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> list[dict[str, Any]]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            rows = await conn.fetch(
                """
                SELECT evaluation.*,
                       CASE
                           WHEN evaluation.status = 'passed'
                            AND (
                                draft.revision <> evaluation.draft_revision
                                OR draft.spec_hash <> evaluation.spec_hash
                            )
                           THEN TRUE ELSE FALSE
                       END AS stale
                FROM agent_release_evaluations AS evaluation
                JOIN agent_drafts AS draft
                  ON draft.tenant_id = evaluation.tenant_id
                 AND draft.agent_id = evaluation.agent_id
                 AND draft.draft_id = evaluation.draft_id
                WHERE evaluation.tenant_id = $1 AND evaluation.agent_id = $2
                ORDER BY evaluation.created_at DESC, evaluation.evaluation_id DESC
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
            events = (
                await conn.fetch(
                    """
                SELECT * FROM agent_release_evaluation_events
                WHERE tenant_id = $1
                  AND evaluation_id = ANY($2::uuid[])
                ORDER BY evaluation_id, sequence
                """,
                    tenant_id,
                    [row["evaluation_id"] for row in rows],
                )
                if rows
                else []
            )
            events_by_evaluation: dict[str, list[dict[str, Any]]] = {}
            for event in events:
                events_by_evaluation.setdefault(str(event["evaluation_id"]), []).append(
                    _row_to_dict(event)
                )
            results: list[dict[str, Any]] = []
            for raw in rows:
                result = _row_to_dict(raw)
                if result.get("stale"):
                    result["status"] = "stale"
                    result["stale_reasons"] = ["draft_changed"]
                else:
                    result["stale_reasons"] = []
                result["events"] = events_by_evaluation.get(str(result["evaluation_id"]), [])
                results.append(result)
            return results

    async def get_release_evaluation(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        evaluation_id: str,
        user_id: str,
        is_tenant_admin: bool,
        required_role: str = "viewer",
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role=required_role,
                is_tenant_admin=is_tenant_admin,
            )
            row = await conn.fetchrow(
                """
                SELECT evaluation.*,
                       CASE
                           WHEN evaluation.status = 'passed'
                            AND (
                                draft.revision <> evaluation.draft_revision
                                OR draft.spec_hash <> evaluation.spec_hash
                            )
                           THEN TRUE ELSE FALSE
                       END AS stale
                FROM agent_release_evaluations AS evaluation
                JOIN agent_drafts AS draft
                  ON draft.tenant_id = evaluation.tenant_id
                 AND draft.agent_id = evaluation.agent_id
                 AND draft.draft_id = evaluation.draft_id
                WHERE evaluation.tenant_id = $1
                  AND evaluation.agent_id = $2
                  AND evaluation.evaluation_id = $3
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
            )
            if not row:
                raise AgentReleaseEvaluationNotFoundError("AGENT_EVAL_NOT_FOUND")
            events = await conn.fetch(
                """
                SELECT * FROM agent_release_evaluation_events
                WHERE tenant_id = $1 AND evaluation_id = $2
                ORDER BY sequence
                """,
                tenant_id,
                uuid.UUID(evaluation_id),
            )
        result = _row_to_dict(row)
        if result.get("stale"):
            result["status"] = "stale"
            result["stale_reasons"] = ["draft_changed"]
        else:
            result["stale_reasons"] = []
        result["events"] = [_row_to_dict(event) for event in events]
        return result

    async def get_release_diff(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        evaluation_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            evaluation = await conn.fetchrow(
                """
                SELECT * FROM agent_release_evaluations
                WHERE tenant_id = $1 AND agent_id = $2 AND evaluation_id = $3
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
            )
            if not evaluation:
                raise AgentReleaseEvaluationNotFoundError("AGENT_EVAL_NOT_FOUND")
            draft = await conn.fetchrow(
                """
                SELECT * FROM agent_drafts
                WHERE tenant_id = $1 AND agent_id = $2
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
            if (
                not draft
                or int(draft["revision"]) != int(evaluation["draft_revision"])
                or str(draft["spec_hash"]) != str(evaluation["spec_hash"])
            ):
                current_revision = int(draft["revision"]) if draft else 0
                raise AgentReleaseEvaluationStaleError(current_revision)
            publication = await conn.fetchrow(
                """
                SELECT publication.*, version.resolved_spec AS current_resolved_spec,
                       version.version_number AS current_version_number
                FROM agent_publications AS publication
                LEFT JOIN agent_versions AS version
                  ON version.tenant_id = publication.tenant_id
                 AND version.agent_id = publication.agent_id
                 AND version.agent_version_id = publication.version_id
                WHERE publication.tenant_id = $1
                  AND publication.agent_id = $2
                  AND publication.channel = $3
                """,
                tenant_id,
                uuid.UUID(agent_id),
                evaluation["channel"],
            )
        before_spec: dict[str, Any] = {}
        if publication and publication["current_resolved_spec"]:
            before_spec = (
                publication["current_resolved_spec"]
                if isinstance(publication["current_resolved_spec"], dict)
                else json.loads(publication["current_resolved_spec"])
            )
        after_spec = draft["spec"] if isinstance(draft["spec"], dict) else json.loads(draft["spec"])
        return {
            "evaluation_id": str(evaluation["evaluation_id"]),
            "draft_revision": int(evaluation["draft_revision"]),
            "publication_id": str(publication["publication_id"]) if publication else None,
            "current_version_id": str(publication["version_id"]) if publication else None,
            "current_version_number": (
                int(publication["current_version_number"])
                if publication and publication["current_version_number"] is not None
                else None
            ),
            "diff": structured_agent_release_diff(before_spec, after_spec),
        }

    @staticmethod
    async def _release_result_from_request(
        conn: Any,
        *,
        tenant_id: str,
        request_row: Any,
        replayed: bool,
    ) -> dict[str, Any]:
        version = await conn.fetchrow(
            """
            SELECT * FROM agent_versions
            WHERE tenant_id = $1 AND agent_version_id = $2
            """,
            tenant_id,
            request_row["result_version_id"],
        )
        publication = await conn.fetchrow(
            """
            SELECT * FROM agent_publications
            WHERE tenant_id = $1 AND publication_id = $2
            """,
            tenant_id,
            request_row["result_publication_id"],
        )
        event = await conn.fetchrow(
            """
            SELECT * FROM agent_publish_events
            WHERE tenant_id = $1 AND event_id = $2
            """,
            tenant_id,
            request_row["result_event_id"],
        )
        if not version or not publication or not event:
            raise AgentRepositoryError("AGENT_RELEASE_RESULT_UNAVAILABLE")
        return {
            "version": _row_to_dict(version),
            "publication": _row_to_dict(publication),
            "event": _row_to_dict(event),
            "idempotent_replay": replayed,
        }

    async def replay_release_request(
        self,
        *,
        tenant_id: str,
        operation: str,
        idempotency_key: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        reason: str,
        evaluation_id: str | None = None,
        experiment_run_id: str | None = None,
        publication_id: str | None = None,
        target_version_id: str | None = None,
    ) -> dict[str, Any] | None:
        """Replay a committed release before resolving mutable runtime state."""

        self._require_enabled()
        if operation not in {"promote", "rollback"}:
            raise AgentRepositoryError("AGENT_RELEASE_OPERATION_INVALID")
        idempotency_key_hash = hashlib.sha256(idempotency_key.encode("utf-8")).hexdigest()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
            )
            request_row = await conn.fetchrow(
                """
                SELECT * FROM agent_release_requests
                WHERE tenant_id = $1
                  AND operation = $2
                  AND idempotency_key_hash = $3
                """,
                tenant_id,
                operation,
                idempotency_key_hash,
            )
            if not request_row:
                return None
            event = await conn.fetchrow(
                """
                SELECT operation, reason, validation_snapshot
                FROM agent_publish_events
                WHERE tenant_id = $1 AND event_id = $2
                """,
                tenant_id,
                request_row["result_event_id"],
            )
            matches = (
                str(request_row["agent_id"]) == agent_id
                and event is not None
                and str(event["operation"]) == operation
                and str(event["reason"] or "") == reason
            )
            if operation == "promote":
                snapshot = _row_to_dict(event).get("validation_snapshot") if event else {}
                recorded_run_id = (
                    str(snapshot.get("experiment_run_id"))
                    if isinstance(snapshot, dict) and snapshot.get("experiment_run_id")
                    else None
                )
                matches = (
                    matches
                    and evaluation_id is not None
                    and (str(request_row["evaluation_id"]) == evaluation_id)
                    and recorded_run_id == experiment_run_id
                )
            else:
                matches = (
                    matches
                    and publication_id is not None
                    and target_version_id is not None
                    and str(request_row["result_publication_id"]) == publication_id
                    and str(request_row["result_version_id"]) == target_version_id
                )
            if not matches:
                raise AgentReleaseIdempotencyConflictError("AGENT_RELEASE_IDEMPOTENCY_CONFLICT")
            return await self._release_result_from_request(
                conn,
                tenant_id=tenant_id,
                request_row=request_row,
                replayed=True,
            )

    async def publish_agent(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        evaluation_id: str,
        user_id: str,
        is_tenant_admin: bool,
        idempotency_key: str,
        reason: str,
        current_candidate: dict[str, Any],
        experiment_run_id: str | None = None,
        selected_version_id: str | None = None,
        selected_version_snapshot_hash: str | None = None,
        actor_model_access_levels: set[str] | None = None,
        model_authorization_revalidator: (Callable[[], Awaitable[dict[str, Any]]] | None) = None,
    ) -> dict[str, Any]:
        """Create/reuse an immutable Version and atomically promote its channel."""

        self._require_enabled()
        idempotency_key_hash = hashlib.sha256(idempotency_key.encode("utf-8")).hexdigest()
        request_identity = {
            "operation": "promote",
            "agent_id": agent_id,
            "evaluation_id": evaluation_id,
            "draft_id": current_candidate.get("draft_id"),
            "draft_revision": current_candidate.get("draft_revision"),
            "spec_hash": current_candidate.get("spec_hash"),
            "runtime_fingerprint_hash": current_candidate.get("runtime_fingerprint_hash"),
            "release_identity_hash": current_candidate.get("release_identity_hash"),
            "channel": current_candidate.get("channel"),
            "channel_policy_hash": current_candidate.get("channel_policy_hash"),
            "reason": reason,
            "experiment_run_id": experiment_run_id,
            "selected_version_id": selected_version_id,
            "selected_version_snapshot_hash": selected_version_snapshot_hash,
        }
        request_hash = hashlib.sha256(canonical_spec(request_identity).encode("utf-8")).hexdigest()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._lock_release_idempotency_key(
                conn,
                tenant_id=tenant_id,
                operation="promote",
                idempotency_key_hash=idempotency_key_hash,
            )
            agent, _ = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            if agent["status"] in {"archived", "deleted"}:
                raise AgentArchivedError("AGENT_ARCHIVED")
            existing_request = await conn.fetchrow(
                """
                SELECT * FROM agent_release_requests
                WHERE tenant_id = $1
                  AND operation = 'promote'
                  AND idempotency_key_hash = $2
                FOR UPDATE
                """,
                tenant_id,
                idempotency_key_hash,
            )
            if existing_request:
                if (
                    str(existing_request["request_hash"]) != request_hash
                    or str(existing_request["agent_id"]) != agent_id
                ):
                    raise AgentReleaseIdempotencyConflictError("AGENT_RELEASE_IDEMPOTENCY_CONFLICT")
                return await self._release_result_from_request(
                    conn,
                    tenant_id=tenant_id,
                    request_row=existing_request,
                    replayed=True,
                )
            evaluation = await conn.fetchrow(
                """
                SELECT * FROM agent_release_evaluations
                WHERE tenant_id = $1 AND agent_id = $2 AND evaluation_id = $3
                FOR SHARE
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
            )
            if not evaluation:
                raise AgentReleaseEvaluationNotFoundError("AGENT_EVAL_NOT_FOUND")
            if str(evaluation["status"]) != "passed":
                gate = _row_to_dict(evaluation).get("gate_snapshot") or {}
                findings = gate.get("blocking_findings") if isinstance(gate, dict) else []
                raise AgentReleaseGateError("AGENT_EVAL_NOT_PASSED", findings)
            release_policy = _row_to_dict(evaluation)["channel_policy"]
            expires_at = require_public_release_expiry(
                str(evaluation["auth_mode"]), release_policy
            )
            draft = await conn.fetchrow(
                """
                SELECT * FROM agent_drafts
                WHERE tenant_id = $1 AND agent_id = $2
                FOR UPDATE
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
            if not draft:
                raise AgentNotFoundError("AGENT_NOT_FOUND")
            if (
                int(draft["revision"]) != int(evaluation["draft_revision"])
                or str(draft["draft_id"]) != str(evaluation["draft_id"])
                or str(draft["spec_hash"]) != str(evaluation["spec_hash"])
            ):
                raise AgentReleaseEvaluationStaleError(int(draft["revision"]))
            candidate_checks = {
                "draft_id": str(evaluation["draft_id"]),
                "draft_revision": int(evaluation["draft_revision"]),
                "spec_hash": str(evaluation["spec_hash"]),
                "runtime_fingerprint_hash": str(evaluation["runtime_fingerprint_hash"]),
                "release_identity_hash": str(evaluation["release_identity_hash"]),
                "evaluation_identity_hash": str(
                    evaluation["evaluation_identity_hash"] or evaluation["release_identity_hash"]
                ),
                "channel": str(evaluation["channel"]),
                "auth_mode": str(evaluation["auth_mode"]),
                "channel_policy_hash": str(evaluation["channel_policy_hash"]),
                "dataset_id": (str(evaluation["dataset_id"]) if evaluation["dataset_id"] else None),
                "dataset_version": (
                    str(evaluation["dataset_version"]) if evaluation["dataset_version"] else None
                ),
                "dataset_manifest_hash": (
                    str(evaluation["dataset_manifest_hash"])
                    if evaluation["dataset_manifest_hash"]
                    else None
                ),
            }
            mismatched = [
                key
                for key, value in candidate_checks.items()
                if current_candidate.get(key) != value
            ]
            if mismatched:
                raise AgentReleaseEvaluationStaleError(int(draft["revision"]))
            spec = draft["spec"] if isinstance(draft["spec"], dict) else json.loads(draft["spec"])
            await self._validate_model_authorization(
                conn,
                tenant_id=tenant_id,
                spec=spec,
                model_authorization=current_candidate.get("model_authorization"),
                actor_model_access_levels=actor_model_access_levels,
                is_tenant_admin=is_tenant_admin,
                model_authorization_revalidator=model_authorization_revalidator,
            )
            if evaluation["dataset_id"]:
                dataset = await self._eval_dataset_snapshot_from_conn(
                    conn,
                    tenant_id=tenant_id,
                    dataset_id=str(evaluation["dataset_id"]),
                    lock=True,
                )
                if dataset["version"] != str(evaluation["dataset_version"] or "") or dataset[
                    "manifest_hash"
                ] != str(evaluation["dataset_manifest_hash"] or ""):
                    raise AgentReleaseEvaluationStaleError(int(draft["revision"]))
            selected_version = None
            run_evidence: dict[str, Any] = {}
            if experiment_run_id is not None:
                if (
                    str(evaluation["auth_mode"]) not in {"private", "tenant"}
                    or not selected_version_id
                    or not selected_version_snapshot_hash
                ):
                    raise AgentReleaseGateError("AGENT_EVAL_RUN_AUDIENCE_UNSUPPORTED")
                run = await conn.fetchrow(
                    """
                    SELECT * FROM eval_experiment_runs
                    WHERE tenant_id = $1 AND run_id = $2
                    FOR SHARE
                    """,
                    tenant_id,
                    uuid.UUID(experiment_run_id),
                )
                if not run or run["status"] != "succeeded" or run["run_mode"] != "live_candidate":
                    raise AgentReleaseGateError("AGENT_EVAL_RUN_UNAVAILABLE")
                if (
                    (str(run["dataset_id"]) if run["dataset_id"] else None)
                    != (str(evaluation["dataset_id"]) if evaluation["dataset_id"] else None)
                    or str(run["dataset_manifest_hash"] or "")
                    != str(evaluation["dataset_manifest_hash"] or "")
                ):
                    raise AgentReleaseGateError("AGENT_EVAL_RUN_DATASET_MISMATCH")
                target = self._json_mapping(run["target_snapshot"])
                metrics = self._json_mapping(run["metrics"])
                summary = self._json_mapping(run["score_summary"])
                actual = metrics.get("actual_fingerprint")
                if not isinstance(actual, dict):
                    raise AgentReleaseGateError("AGENT_EVAL_RUN_IDENTITY_MISMATCH")
                expected_identity = {
                    "agent_id": agent_id,
                    "agent_version_id": selected_version_id,
                    "agent_spec_hash": str(draft["spec_hash"]),
                    "agent_runtime_snapshot_hash": selected_version_snapshot_hash,
                }
                if (
                    target.get("candidate_type") != "agent_version"
                    or any(
                        target.get(key) != value or actual.get(key) != value
                        for key, value in expected_identity.items()
                    )
                ):
                    raise AgentReleaseGateError("AGENT_EVAL_RUN_IDENTITY_MISMATCH")
                counts = {
                    key: metrics.get(key)
                    for key in (
                        "attempted_trials", "completed_trials", "total_trials",
                        "failed_trials", "execution_failed_trials", "judge_failed_trials",
                        "unscored_trials", "unknown_side_effect_trial_count",
                    )
                }
                run_gate = metrics.get("gate")
                if (
                    any(isinstance(value, bool) or not isinstance(value, int) for value in counts.values())
                    or counts["total_trials"] <= 0
                    or counts["attempted_trials"] != counts["total_trials"]
                    or counts["completed_trials"] != counts["total_trials"]
                    or any(
                        counts[key] != 0
                        for key in (
                            "failed_trials", "execution_failed_trials", "judge_failed_trials",
                            "unscored_trials", "unknown_side_effect_trial_count",
                        )
                    )
                    or summary.get("critical_failed_count") != 0
                    or summary.get("excluded_case_count") != 0
                    or not isinstance(run_gate, dict)
                    or run_gate.get("profile") != "agent_version_task_suite"
                    or not (
                        run_gate.get("status") == "pass"
                        or (run_gate.get("status") == "warning"
                            and run_gate.get("reason") == "fixed_sample_only")
                    )
                ):
                    raise AgentReleaseGateError("AGENT_EVAL_RUN_HARD_FAILURE")
                selected_version = await conn.fetchrow(
                    """
                    SELECT version.*,
                           EXISTS (
                               SELECT 1 FROM agent_version_revocations AS revoked
                               WHERE revoked.tenant_id = version.tenant_id
                                 AND revoked.agent_version_id = version.agent_version_id
                           ) AS revoked
                    FROM agent_versions AS version
                    WHERE version.tenant_id = $1
                      AND version.agent_id = $2
                      AND version.agent_version_id = $3
                    FOR SHARE OF version
                    """,
                    tenant_id,
                    uuid.UUID(agent_id),
                    uuid.UUID(selected_version_id),
                )
                if (
                    not selected_version
                    or selected_version["revoked"]
                    or not selected_version["bindings_sealed"]
                    or str(selected_version["source_draft_id"]) != str(draft["draft_id"])
                    or int(selected_version["source_draft_revision"]) != int(draft["revision"])
                    or str(selected_version["spec_hash"]) != str(draft["spec_hash"])
                ):
                    raise AgentReleaseGateError("AGENT_EVAL_RUN_VERSION_STALE")
                await self._validate_existing_version_resources(
                    conn,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    is_tenant_admin=is_tenant_admin,
                    version=selected_version,
                )
                run_evidence = {
                    "experiment_run_id": experiment_run_id,
                    "dataset_manifest_hash": str(run["dataset_manifest_hash"] or ""),
                    "evaluator_suite_hash": str(run["evaluator_suite_hash"] or ""),
                    "actual_fingerprint": expected_identity,
                    "quality_gate_status": run_gate["status"],
                    "quality_evidence_scope": "fixed_sample_only",
                    "model_quality_evaluated": True,
                    "general_quality_proven": False,
                }
            material = await self._resolve_version_material(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                is_tenant_admin=is_tenant_admin,
                draft=draft,
                spec=spec,
            )
            version = selected_version
            if version is None:
                version = await conn.fetchrow(
                    """
                    SELECT * FROM agent_versions
                    WHERE tenant_id = $1 AND agent_id = $2 AND release_identity_hash = $3
                    """,
                    tenant_id,
                    uuid.UUID(agent_id),
                    str(evaluation["release_identity_hash"]),
                )
            if version:
                version_result = _row_to_dict(version)
            else:
                version_result = await self._insert_version_from_material(
                    conn,
                    tenant_id=tenant_id,
                    agent_id=agent_id,
                    user_id=user_id,
                    expected_revision=int(draft["revision"]),
                    draft=draft,
                    spec=spec,
                    sealed_capability_bindings=material[0],
                    skill_versions=material[1],
                    knowledge=material[2],
                    release_evaluation_id=evaluation_id,
                    release_identity_hash=str(evaluation["release_identity_hash"]),
                )
            publication = await conn.fetchrow(
                """
                SELECT * FROM agent_publications
                WHERE tenant_id = $1 AND agent_id = $2 AND channel = $3
                FOR UPDATE
                """,
                tenant_id,
                uuid.UUID(agent_id),
                evaluation["channel"],
            )
            if not publication or publication["status"] != "active":
                await self._enforce_active_publication_quota(
                    conn,
                    tenant_id=tenant_id,
                    agent_id=agent_id,
                )
            from_version_id = publication["version_id"] if publication else None
            before_spec: dict[str, Any] = {}
            if from_version_id:
                previous = await conn.fetchrow(
                    """
                    SELECT resolved_spec FROM agent_versions
                    WHERE tenant_id = $1 AND agent_id = $2 AND agent_version_id = $3
                    """,
                    tenant_id,
                    uuid.UUID(agent_id),
                    from_version_id,
                )
                if previous:
                    before_spec = (
                        previous["resolved_spec"]
                        if isinstance(previous["resolved_spec"], dict)
                        else json.loads(previous["resolved_spec"])
                    )
            if publication:
                publication = await conn.fetchrow(
                    """
                    UPDATE agent_publications
                    SET version_id = $4, auth_mode = $5, policy = $6::jsonb,
                        expires_at = $8, status = 'active', updated_by = $7,
                        updated_at = NOW()
                    WHERE tenant_id = $1 AND publication_id = $2 AND agent_id = $3
                    RETURNING *
                    """,
                    tenant_id,
                    publication["publication_id"],
                    uuid.UUID(agent_id),
                    uuid.UUID(str(version_result["agent_version_id"])),
                    evaluation["auth_mode"],
                    canonical_spec(release_policy),
                    user_id,
                    expires_at,
                )
            else:
                publication = await conn.fetchrow(
                    """
                    INSERT INTO agent_publications (
                        tenant_id, agent_id, channel, version_id, auth_mode,
                        policy, status, created_by, updated_by, expires_at
                    ) VALUES ($1, $2, $3, $4, $5, $6::jsonb, 'active', $7, $7, $8)
                    RETURNING *
                    """,
                    tenant_id,
                    uuid.UUID(agent_id),
                    evaluation["channel"],
                    uuid.UUID(str(version_result["agent_version_id"])),
                    evaluation["auth_mode"],
                    canonical_spec(release_policy),
                    user_id,
                    expires_at,
                )
            release_diff = structured_agent_release_diff(before_spec, spec)
            validation_snapshot = {
                "schema_version": "agent-publish-validation/v1",
                "evaluation_id": evaluation_id,
                "profile_id": str(evaluation["profile_id"]),
                "profile_version": str(evaluation["profile_version"]),
                "draft_revision": int(evaluation["draft_revision"]),
                "spec_hash": str(evaluation["spec_hash"]),
                "runtime_fingerprint_hash": str(evaluation["runtime_fingerprint_hash"]),
                "release_identity_hash": str(evaluation["release_identity_hash"]),
                "resource_authorization_rechecked": True,
                "session_pinning": "existing_sessions_keep_version_new_sessions_use_pointer",
                "diff": release_diff,
                **run_evidence,
            }
            event = await conn.fetchrow(
                """
                INSERT INTO agent_publish_events (
                    tenant_id, publication_id, agent_id, from_version_id,
                    to_version_id, actor_id, reason, validation_snapshot,
                    operation, release_evaluation_id, request_hash
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8::jsonb, 'promote', $9, $10
                )
                RETURNING *
                """,
                tenant_id,
                publication["publication_id"],
                uuid.UUID(agent_id),
                from_version_id,
                uuid.UUID(str(version_result["agent_version_id"])),
                user_id,
                reason,
                canonical_spec(validation_snapshot),
                uuid.UUID(evaluation_id),
                request_hash,
            )
            request_row = await conn.fetchrow(
                """
                INSERT INTO agent_release_requests (
                    tenant_id, operation, idempotency_key_hash, request_hash,
                    agent_id, evaluation_id, result_version_id,
                    result_publication_id, result_event_id, created_by
                ) VALUES ($1, 'promote', $2, $3, $4, $5, $6, $7, $8, $9)
                RETURNING *
                """,
                tenant_id,
                idempotency_key_hash,
                request_hash,
                uuid.UUID(agent_id),
                uuid.UUID(evaluation_id),
                uuid.UUID(str(version_result["agent_version_id"])),
                publication["publication_id"],
                event["event_id"],
                user_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="publication_promote",
                summary={
                    "evaluation_id": evaluation_id,
                    "publication_id": str(publication["publication_id"]),
                    "from_version_id": str(from_version_id) if from_version_id else None,
                    "to_version_id": str(version_result["agent_version_id"]),
                },
                agent_version_id=version_result["agent_version_id"],
                publication_id=publication["publication_id"],
                channel=str(publication["channel"]),
            )
            return await self._release_result_from_request(
                conn,
                tenant_id=tenant_id,
                request_row=request_row,
                replayed=False,
            )

    async def list_publications(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> list[dict[str, Any]]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            rows = await conn.fetch(
                """
                SELECT publication.*, version.version_number,
                       version.spec_hash AS version_spec_hash
                FROM agent_publications AS publication
                LEFT JOIN agent_versions AS version
                  ON version.tenant_id = publication.tenant_id
                 AND version.agent_id = publication.agent_id
                 AND version.agent_version_id = publication.version_id
                WHERE publication.tenant_id = $1 AND publication.agent_id = $2
                ORDER BY publication.channel
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
        return [_row_to_dict(row) for row in rows]

    async def get_publication(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        user_id: str,
        is_tenant_admin: bool,
        required_role: str = "viewer",
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            probe = await conn.fetchrow(
                """
                SELECT * FROM agent_publications
                WHERE tenant_id = $1 AND publication_id = $2
                """,
                tenant_id,
                uuid.UUID(publication_id),
            )
            if not probe:
                raise AgentPublicationNotFoundError("AGENT_PUBLICATION_NOT_FOUND")
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=probe["agent_id"],
                user_id=user_id,
                required_role=required_role,
                is_tenant_admin=is_tenant_admin,
            )
        return _row_to_dict(probe)

    async def list_publish_events(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> list[dict[str, Any]]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            rows = await conn.fetch(
                """
                SELECT event.* FROM agent_publish_events AS event
                WHERE event.tenant_id = $1 AND event.agent_id = $2
                ORDER BY event.created_at DESC, event.event_id DESC
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
        return [_row_to_dict(row) for row in rows]

    async def _validate_existing_version_resources(
        self,
        conn: Any,
        *,
        tenant_id: str,
        user_id: str,
        is_tenant_admin: bool,
        version: Any,
    ) -> dict[str, Any]:
        """Recheck current authorization/readiness without mutating a Version."""

        spec = (
            version["resolved_spec"]
            if isinstance(version["resolved_spec"], dict)
            else json.loads(version["resolved_spec"])
        )
        errors = validate_agent_spec(spec)
        if errors:
            raise AgentValidationError(errors)
        capability_rows = await conn.fetch(
            """
            SELECT capability_type, resource_id, resource_version, schema_hash, config
            FROM agent_version_capabilities
            WHERE tenant_id = $1 AND agent_version_id = $2
            ORDER BY capability_type, resource_id
            """,
            tenant_id,
            version["agent_version_id"],
        )
        if any(row["capability_type"] in {"mcp", "connector"} for row in capability_rows):
            from .mcp_repository import DatabaseMCPRepository, MCPValidationError

            mcp_repository = DatabaseMCPRepository(self._holder)
            binding_errors: list[dict[str, str]] = []
            for index, raw in enumerate(capability_rows):
                if raw["capability_type"] not in {"mcp", "connector"}:
                    continue
                binding = _row_to_dict(raw)
                try:
                    await mcp_repository.validate_version_binding(
                        tenant_id=tenant_id,
                        capability_type=str(binding["capability_type"]),
                        resource_id=str(binding["resource_id"]),
                        schema_hash=binding.get("schema_hash"),
                        risk_level=(binding.get("config") or {}).get("risk"),
                        config=binding.get("config") or {},
                        connection=conn,
                    )
                except MCPValidationError as exc:
                    binding_errors.append(
                        {
                            "field": f"capabilities[{index}]",
                            "code": exc.code,
                            "message": "Capability binding is unavailable or changed",
                        }
                    )
            if binding_errors:
                raise AgentValidationError(binding_errors)
        skill_count = sum(row["capability_type"] == "skill" for row in capability_rows)
        if skill_count:
            active_skill_count = await conn.fetchval(
                """
                SELECT COUNT(*)
                FROM agent_version_skill_bindings AS binding
                JOIN assistant_skill_versions AS version
                  ON version.tenant_id = binding.tenant_id
                 AND version.version_id = binding.skill_version_id
                JOIN assistant_skills AS skill
                  ON skill.tenant_id = version.tenant_id
                 AND skill.skill_id = version.skill_id
                WHERE binding.tenant_id = $1
                  AND binding.agent_version_id = $2
                  AND skill.user_id = $3
                  AND skill.enabled = TRUE
                  AND skill.status = 'active'
                  AND skill.deleted_at IS NULL
                  AND version.status = 'active'
                  AND version.artifact_type = 'tenant_instruction'
                  AND NOT EXISTS (
                      SELECT 1 FROM assistant_skill_version_revocations AS revoked
                      WHERE revoked.tenant_id = version.tenant_id
                        AND revoked.version_id = version.version_id
                  )
                """,
                tenant_id,
                version["agent_version_id"],
                user_id,
            )
            if int(active_skill_count or 0) != skill_count:
                raise AgentValidationError(
                    [
                        {
                            "field": "capabilities",
                            "code": "AGENT_SKILL_VERSION_UNAVAILABLE",
                            "message": "one or more Skill versions are unavailable",
                        }
                    ]
                )
        knowledge_rows = await conn.fetch(
            """
            SELECT dataset_id FROM agent_version_knowledge_bindings
            WHERE tenant_id = $1 AND agent_version_id = $2
            ORDER BY dataset_id
            """,
            tenant_id,
            version["agent_version_id"],
        )
        dataset_ids = [str(row["dataset_id"]) for row in knowledge_rows]
        if dataset_ids:
            allowed = await self._authorized_knowledge_dataset_ids(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                dataset_ids=dataset_ids,
                is_tenant_admin=is_tenant_admin,
            )
            if allowed != set(dataset_ids):
                raise AgentValidationError(
                    [
                        {
                            "field": "knowledge",
                            "code": "AGENT_KNOWLEDGE_UNAVAILABLE",
                            "message": "one or more Dataset bindings are unavailable",
                        }
                    ]
                )
        return spec

    async def rollback_publication(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        target_version_id: str,
        user_id: str,
        is_tenant_admin: bool,
        idempotency_key: str,
        reason: str,
        runtime_snapshot_hash: str,
        runtime_spec_hash: str,
        model_authorization: dict[str, Any] | None = None,
        actor_model_access_levels: set[str] | None = None,
        model_authorization_revalidator: (Callable[[], Awaitable[dict[str, Any]]] | None) = None,
    ) -> dict[str, Any]:
        """Atomically repoint one Publication after current resource rechecks."""

        self._require_enabled()
        idempotency_key_hash = hashlib.sha256(idempotency_key.encode("utf-8")).hexdigest()
        request_identity = {
            "operation": "rollback",
            "publication_id": publication_id,
            "target_version_id": target_version_id,
            "runtime_snapshot_hash": runtime_snapshot_hash,
            "runtime_spec_hash": runtime_spec_hash,
            "model_authorization_hash": _canonical_hash(model_authorization or {}),
            "reason": reason,
        }
        request_hash = hashlib.sha256(canonical_spec(request_identity).encode("utf-8")).hexdigest()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._lock_release_idempotency_key(
                conn,
                tenant_id=tenant_id,
                operation="rollback",
                idempotency_key_hash=idempotency_key_hash,
            )
            publication_probe = await conn.fetchrow(
                """
                SELECT agent_id FROM agent_publications
                WHERE tenant_id = $1 AND publication_id = $2
                """,
                tenant_id,
                uuid.UUID(publication_id),
            )
            if not publication_probe:
                raise AgentPublicationNotFoundError("AGENT_PUBLICATION_NOT_FOUND")
            agent_id = str(publication_probe["agent_id"])
            agent, _ = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            if agent["status"] in {"archived", "deleted"}:
                raise AgentArchivedError("AGENT_ARCHIVED")
            existing_request = await conn.fetchrow(
                """
                SELECT * FROM agent_release_requests
                WHERE tenant_id = $1
                  AND operation = 'rollback'
                  AND idempotency_key_hash = $2
                FOR UPDATE
                """,
                tenant_id,
                idempotency_key_hash,
            )
            if existing_request:
                if (
                    str(existing_request["request_hash"]) != request_hash
                    or str(existing_request["agent_id"]) != agent_id
                ):
                    raise AgentReleaseIdempotencyConflictError("AGENT_RELEASE_IDEMPOTENCY_CONFLICT")
                return await self._release_result_from_request(
                    conn,
                    tenant_id=tenant_id,
                    request_row=existing_request,
                    replayed=True,
                )
            publication = await conn.fetchrow(
                """
                SELECT * FROM agent_publications
                WHERE tenant_id = $1 AND publication_id = $2 AND agent_id = $3
                FOR UPDATE
                """,
                tenant_id,
                uuid.UUID(publication_id),
                uuid.UUID(agent_id),
            )
            if not publication:
                raise AgentPublicationNotFoundError("AGENT_PUBLICATION_NOT_FOUND")
            if _publication_expired(publication):
                raise AgentReleaseGateError("AGENT_PUBLIC_EXPIRY_EXPIRED")
            if publication["status"] != "active":
                await self._enforce_active_publication_quota(
                    conn,
                    tenant_id=tenant_id,
                    agent_id=agent_id,
                )
            if not publication["version_id"]:
                raise AgentReleaseGateError("AGENT_ROLLBACK_PUBLICATION_EMPTY")
            if str(publication["version_id"]) == target_version_id:
                raise AgentReleaseGateError("AGENT_ROLLBACK_TARGET_CURRENT")
            target_version = await conn.fetchrow(
                """
                SELECT version.*,
                       EXISTS (
                           SELECT 1 FROM agent_version_revocations AS revoked
                           WHERE revoked.tenant_id = version.tenant_id
                             AND revoked.agent_version_id = version.agent_version_id
                       ) AS revoked
                FROM agent_versions AS version
                WHERE version.tenant_id = $1
                  AND version.agent_id = $2
                  AND version.agent_version_id = $3
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(target_version_id),
            )
            if not target_version or bool(target_version["revoked"]):
                raise AgentReleaseGateError("AGENT_ROLLBACK_VERSION_UNAVAILABLE")
            target_was_channel_history = await conn.fetchval(
                """
                SELECT EXISTS (
                    SELECT 1
                    FROM agent_publish_events
                    WHERE tenant_id = $1
                      AND publication_id = $2
                      AND agent_id = $3
                      AND (from_version_id = $4 OR to_version_id = $4)
                )
                """,
                tenant_id,
                uuid.UUID(publication_id),
                uuid.UUID(agent_id),
                uuid.UUID(target_version_id),
            )
            if not target_was_channel_history:
                raise AgentReleaseGateError("AGENT_ROLLBACK_TARGET_NOT_HISTORICAL")
            if str(target_version["spec_hash"]) != runtime_spec_hash:
                raise AgentReleaseGateError("AGENT_ROLLBACK_FINGERPRINT_STALE")
            if not re.fullmatch(r"[0-9a-f]{64}", runtime_snapshot_hash):
                raise AgentReleaseGateError("AGENT_ROLLBACK_FINGERPRINT_INVALID")
            target_spec = await self._validate_existing_version_resources(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                is_tenant_admin=is_tenant_admin,
                version=target_version,
            )
            await self._validate_model_authorization(
                conn,
                tenant_id=tenant_id,
                spec=target_spec,
                model_authorization=model_authorization,
                actor_model_access_levels=actor_model_access_levels,
                is_tenant_admin=is_tenant_admin,
                model_authorization_revalidator=model_authorization_revalidator,
            )
            current_version = await conn.fetchrow(
                """
                SELECT * FROM agent_versions
                WHERE tenant_id = $1 AND agent_id = $2 AND agent_version_id = $3
                """,
                tenant_id,
                uuid.UUID(agent_id),
                publication["version_id"],
            )
            if not current_version:
                raise AgentReleaseGateError("AGENT_ROLLBACK_CURRENT_VERSION_UNAVAILABLE")
            current_spec = (
                current_version["resolved_spec"]
                if isinstance(current_version["resolved_spec"], dict)
                else json.loads(current_version["resolved_spec"])
            )
            publication = await conn.fetchrow(
                """
                UPDATE agent_publications
                SET version_id = $4, status = 'active', updated_by = $5, updated_at = NOW()
                WHERE tenant_id = $1 AND publication_id = $2 AND agent_id = $3
                RETURNING *
                """,
                tenant_id,
                uuid.UUID(publication_id),
                uuid.UUID(agent_id),
                uuid.UUID(target_version_id),
                user_id,
            )
            validation_snapshot = {
                "schema_version": "agent-rollback-validation/v1",
                "runtime_snapshot_hash": runtime_snapshot_hash,
                "spec_hash": runtime_spec_hash,
                "resource_authorization_rechecked": True,
                "target_version_revoked": False,
                "prior_version_recoverable": True,
                "session_pinning": "existing_sessions_keep_version_new_sessions_use_pointer",
                "diff": structured_agent_release_diff(current_spec, target_spec),
            }
            event = await conn.fetchrow(
                """
                INSERT INTO agent_publish_events (
                    tenant_id, publication_id, agent_id, from_version_id,
                    to_version_id, actor_id, reason, validation_snapshot,
                    operation, request_hash
                ) VALUES (
                    $1, $2, $3, $4, $5, $6, $7, $8::jsonb, 'rollback', $9
                )
                RETURNING *
                """,
                tenant_id,
                uuid.UUID(publication_id),
                uuid.UUID(agent_id),
                current_version["agent_version_id"],
                target_version["agent_version_id"],
                user_id,
                reason,
                canonical_spec(validation_snapshot),
                request_hash,
            )
            request_row = await conn.fetchrow(
                """
                INSERT INTO agent_release_requests (
                    tenant_id, operation, idempotency_key_hash, request_hash,
                    agent_id, result_version_id, result_publication_id,
                    result_event_id, created_by
                ) VALUES ($1, 'rollback', $2, $3, $4, $5, $6, $7, $8)
                RETURNING *
                """,
                tenant_id,
                idempotency_key_hash,
                request_hash,
                uuid.UUID(agent_id),
                target_version["agent_version_id"],
                uuid.UUID(publication_id),
                event["event_id"],
                user_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="publication_rollback",
                summary={
                    "publication_id": publication_id,
                    "from_version_id": str(current_version["agent_version_id"]),
                    "to_version_id": target_version_id,
                },
                agent_version_id=target_version_id,
                publication_id=publication_id,
                channel=str(publication["channel"]),
            )
            return await self._release_result_from_request(
                conn,
                tenant_id=tenant_id,
                request_row=request_row,
                replayed=False,
            )

    async def list_versions(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> list[dict[str, Any]]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            rows = await conn.fetch(
                """
                SELECT tenant_id, agent_version_id, agent_id, version_number,
                       schema_version, resolved_spec, spec_hash, source_draft_id,
                       source_draft_revision, created_by, created_at
                FROM agent_versions
                WHERE tenant_id = $1 AND agent_id = $2
                ORDER BY version_number DESC
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
        return [_row_to_dict(row) for row in rows]

    async def resolve_preview_runtime(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        draft_revision: int,
    ) -> dict[str, Any]:
        """Resolve one exact authorized current Draft for Preview execution."""

        self._require_enabled()
        async with self._pool.acquire() as conn:
            agent, role = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            if agent["status"] in {"archived", "deleted"}:
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_AGENT_UNAVAILABLE")
            draft = await conn.fetchrow(
                """
                SELECT tenant_id, draft_id, agent_id, revision, schema_version,
                       spec, spec_hash, updated_at
                FROM agent_drafts
                WHERE tenant_id = $1
                  AND agent_id = $2
                  AND revision = $3
                """,
                tenant_id,
                uuid.UUID(agent_id),
                int(draft_revision),
            )
            if not draft:
                raise AgentRuntimeUnavailableError("AGENT_PREVIEW_REVISION_STALE")
            knowledge_rows = await conn.fetch(
                """
                SELECT dataset_id, retrieval_config
                FROM agent_draft_knowledge_bindings
                WHERE tenant_id = $1 AND draft_id = $2
                ORDER BY dataset_id
                """,
                tenant_id,
                draft["draft_id"],
            )
            preview_spec = (
                draft["spec"] if isinstance(draft["spec"], dict) else json.loads(draft["spec"])
            )
            preview_capabilities = self._capability_bindings(preview_spec)
            if any(binding["capability_type"] == "skill" for binding in preview_capabilities):
                skill_rows = await conn.fetch(
                    """
                    SELECT binding.skill_name, binding.skill_version_id,
                           version.content_hash
                    FROM agent_draft_skill_bindings AS binding
                    JOIN assistant_skill_versions AS version
                      ON version.tenant_id = binding.tenant_id
                     AND version.version_id = binding.skill_version_id
                    WHERE binding.tenant_id = $1 AND binding.draft_id = $2
                    """,
                    tenant_id,
                    draft["draft_id"],
                )
                skills_by_version = {str(row["skill_version_id"]): dict(row) for row in skill_rows}
                normalized: list[dict[str, Any]] = []
                for binding in preview_capabilities:
                    if binding["capability_type"] != "skill":
                        normalized.append(binding)
                        continue
                    artifact = skills_by_version.get(binding["resource_id"])
                    if artifact is None:
                        raise AgentRuntimeUnavailableError("AGENT_SKILL_UNAVAILABLE")
                    normalized.append(
                        {
                            **binding,
                            "resource_id": str(artifact["skill_name"]),
                            "resource_version": str(artifact["skill_version_id"]),
                            "schema_hash": str(artifact["content_hash"]),
                        }
                    )
                preview_capabilities = normalized

        resolved_draft = _row_to_dict(draft)
        spec = resolved_draft.get("spec")
        if not isinstance(spec, dict):
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_SPEC_INVALID")
        errors = validate_agent_spec(spec)
        if errors:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_SPEC_INVALID")
        return {
            "agent": _row_to_dict(agent),
            "caller_role": role,
            "draft": resolved_draft,
            "spec": copy.deepcopy(spec),
            "capabilities": preview_capabilities,
            "knowledge": [_row_to_dict(row) for row in knowledge_rows],
            "publication": None,
        }

    async def resolve_version_runtime(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        agent_version_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        """Resolve one exact authorized immutable Version for isolated Preview."""

        self._require_enabled()
        async with self._pool.acquire() as conn:
            agent, role = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            if agent["status"] in {"archived", "deleted"}:
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_AGENT_UNAVAILABLE")
            version = await conn.fetchrow(
                """
                SELECT version.*,
                       EXISTS (
                           SELECT 1
                           FROM agent_version_revocations AS revoked
                           WHERE revoked.tenant_id = version.tenant_id
                             AND revoked.agent_version_id = version.agent_version_id
                       ) AS revoked
                FROM agent_versions AS version
                WHERE version.tenant_id = $1
                  AND version.agent_id = $2
                  AND version.agent_version_id = $3
                """,
                tenant_id,
                uuid.UUID(agent_id),
                uuid.UUID(agent_version_id),
            )
            if not version or bool(version["revoked"]):
                raise AgentRuntimeUnavailableError("AGENT_VERSION_REVOKED")
            capability_rows = await conn.fetch(
                """
                SELECT capability_type, resource_id, resource_version,
                       schema_hash, config
                FROM agent_version_capabilities
                WHERE tenant_id = $1 AND agent_version_id = $2
                ORDER BY capability_type, resource_id
                """,
                tenant_id,
                version["agent_version_id"],
            )
            knowledge_rows = await conn.fetch(
                """
                SELECT dataset_id, retrieval_config
                FROM agent_version_knowledge_bindings
                WHERE tenant_id = $1 AND agent_version_id = $2
                ORDER BY dataset_id
                """,
                tenant_id,
                version["agent_version_id"],
            )

        resolved_version = _row_to_dict(version)
        spec = resolved_version.get("resolved_spec")
        if not isinstance(spec, dict):
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_SPEC_INVALID")
        errors = validate_agent_spec(spec)
        if errors:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_SPEC_INVALID")
        return {
            "agent": _row_to_dict(agent),
            "caller_role": role,
            "version": resolved_version,
            "spec": copy.deepcopy(spec),
            "capabilities": [_row_to_dict(row) for row in capability_rows],
            "knowledge": [_row_to_dict(row) for row in knowledge_rows],
            "publication": None,
        }

    async def resolve_publication_runtime(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        user_id: str,
        is_tenant_admin: bool,
        pinned_version_id: str | None = None,
    ) -> dict[str, Any]:
        """Resolve an active Publication or one already-pinned immutable Version."""

        self._require_enabled()
        async with self._pool.acquire() as conn:
            publication = await conn.fetchrow(
                """
                SELECT publication.*
                FROM agent_publications AS publication
                JOIN agents AS agent
                  ON agent.tenant_id = publication.tenant_id
                 AND agent.agent_id = publication.agent_id
                WHERE publication.tenant_id = $1
                  AND publication.publication_id = $2
                  AND (
                      $4::boolean
                      OR agent.owner_id = $3
                      OR EXISTS (
                          SELECT 1
                          FROM agent_members AS member
                          WHERE member.tenant_id = publication.tenant_id
                            AND member.agent_id = publication.agent_id
                            AND member.principal_type = 'user'
                            AND member.principal_id = $3
                            AND member.role IN ('owner', 'editor', 'viewer')
                      )
                  )
                """,
                tenant_id,
                uuid.UUID(publication_id),
                user_id,
                is_tenant_admin,
            )
            _require_active_publication(publication)
            agent, role = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=publication["agent_id"],
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            if agent["status"] in {"archived", "deleted"}:
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_AGENT_UNAVAILABLE")
            version_id = pinned_version_id or publication["version_id"]
            if not version_id:
                raise AgentRuntimeUnavailableError("PUBLICATION_VERSION_UNAVAILABLE")
            version = await conn.fetchrow(
                """
                SELECT v.*,
                       EXISTS (
                           SELECT 1
                           FROM agent_version_revocations r
                           WHERE r.tenant_id = v.tenant_id
                             AND r.agent_version_id = v.agent_version_id
                       ) AS revoked
                FROM agent_versions v
                WHERE v.tenant_id = $1
                  AND v.agent_id = $2
                  AND v.agent_version_id = $3
                """,
                tenant_id,
                publication["agent_id"],
                uuid.UUID(str(version_id)),
            )
            if not version or bool(version["revoked"]):
                raise AgentRuntimeUnavailableError("AGENT_VERSION_REVOKED")
            capability_rows = await conn.fetch(
                """
                SELECT capability_type, resource_id, resource_version,
                       schema_hash, config
                FROM agent_version_capabilities
                WHERE tenant_id = $1 AND agent_version_id = $2
                ORDER BY capability_type, resource_id
                """,
                tenant_id,
                version["agent_version_id"],
            )
            knowledge_rows = await conn.fetch(
                """
                SELECT dataset_id, retrieval_config
                FROM agent_version_knowledge_bindings
                WHERE tenant_id = $1 AND agent_version_id = $2
                ORDER BY dataset_id
                """,
                tenant_id,
                version["agent_version_id"],
            )

        resolved_version = _row_to_dict(version)
        spec = resolved_version.get("resolved_spec")
        if not isinstance(spec, dict):
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_SPEC_INVALID")
        errors = validate_agent_spec(spec)
        if errors:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_SPEC_INVALID")
        return {
            "agent": _row_to_dict(agent),
            "caller_role": role,
            "version": resolved_version,
            "spec": copy.deepcopy(spec),
            "capabilities": [_row_to_dict(row) for row in capability_rows],
            "knowledge": [_row_to_dict(row) for row in knowledge_rows],
            "publication": _row_to_dict(publication),
        }

    async def _load_channel_runtime_material(
        self,
        conn: Any,
        *,
        publication: Any,
        pinned_version_id: str | None,
        caller_role: str,
    ) -> dict[str, Any]:
        """Load only immutable material owned by an already-authorized Publication."""

        _require_active_publication(publication)
        agent = await conn.fetchrow(
            """
            SELECT *
            FROM agents
            WHERE tenant_id = $1 AND agent_id = $2 AND deleted_at IS NULL
            """,
            publication["tenant_id"],
            publication["agent_id"],
        )
        if not agent or agent["status"] in {"archived", "deleted"}:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_AGENT_UNAVAILABLE")
        version_id = pinned_version_id or publication["version_id"]
        if not version_id:
            raise AgentRuntimeUnavailableError("PUBLICATION_VERSION_UNAVAILABLE")
        version = await conn.fetchrow(
            """
            SELECT version.*,
                   EXISTS (
                       SELECT 1 FROM agent_version_revocations AS revoked
                       WHERE revoked.tenant_id = version.tenant_id
                         AND revoked.agent_version_id = version.agent_version_id
                   ) AS revoked
            FROM agent_versions AS version
            WHERE version.tenant_id = $1
              AND version.agent_id = $2
              AND version.agent_version_id = $3
            """,
            publication["tenant_id"],
            publication["agent_id"],
            uuid.UUID(str(version_id)),
        )
        if not version or bool(version["revoked"]):
            raise AgentRuntimeUnavailableError("AGENT_VERSION_REVOKED")
        capability_rows = await conn.fetch(
            """
            SELECT capability_type, resource_id, resource_version, schema_hash, config
            FROM agent_version_capabilities
            WHERE tenant_id = $1 AND agent_version_id = $2
            ORDER BY capability_type, resource_id
            """,
            publication["tenant_id"],
            version["agent_version_id"],
        )
        knowledge_rows = await conn.fetch(
            """
            SELECT dataset_id, retrieval_config
            FROM agent_version_knowledge_bindings
            WHERE tenant_id = $1 AND agent_version_id = $2
            ORDER BY dataset_id
            """,
            publication["tenant_id"],
            version["agent_version_id"],
        )
        resolved_version = _row_to_dict(version)
        spec = resolved_version.get("resolved_spec")
        if not isinstance(spec, dict) or validate_agent_spec(spec):
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_SPEC_INVALID")
        return {
            "agent": _row_to_dict(agent),
            "caller_role": caller_role,
            "version": resolved_version,
            "spec": copy.deepcopy(spec),
            "capabilities": [_row_to_dict(row) for row in capability_rows],
            "knowledge": [_row_to_dict(row) for row in knowledge_rows],
            "publication": _row_to_dict(publication),
        }

    async def resolve_public_channel_runtime(
        self,
        *,
        public_id: str,
        channel: str,
        caller_tenant_id: str,
        user_id: str,
        authenticated: bool,
        is_tenant_admin: bool,
        pinned_version_id: str | None = None,
        browser_token_authorized: bool = False,
    ) -> dict[str, Any]:
        """Resolve Hosted/Embed by stable public ID without trusting client tenant data."""

        self._require_enabled()
        if channel not in {"hosted", "embed"}:
            raise AgentRuntimeUnavailableError("PUBLICATION_CHANNEL_MISMATCH")
        async with self._pool.acquire() as conn:
            publication = await conn.fetchrow(
                """
                SELECT publication.*
                FROM agent_publications AS publication
                WHERE publication.public_id = $1
                """,
                uuid.UUID(public_id),
            )
            _require_active_publication(publication)
            if str(publication["channel"]) != channel:
                raise AgentRuntimeUnavailableError("PUBLICATION_CHANNEL_MISMATCH")

            tenant_matches = authenticated and caller_tenant_id == publication["tenant_id"]
            auth_mode = str(publication["auth_mode"])
            if auth_mode in {"private", "tenant"} and not authenticated:
                raise AgentRuntimeUnavailableError("PUBLICATION_AUTHENTICATION_REQUIRED")
            caller_role = "viewer"
            allowed = auth_mode == "public"
            if auth_mode == "tenant":
                allowed = bool(tenant_matches)
            elif auth_mode == "private" and tenant_matches:
                member = await conn.fetchrow(
                    """
                    SELECT role
                    FROM agent_members
                    WHERE tenant_id = $1 AND agent_id = $2
                      AND principal_type = 'user' AND principal_id = $3
                    """,
                    publication["tenant_id"],
                    publication["agent_id"],
                    user_id,
                )
                allowed = bool(is_tenant_admin or member)
                caller_role = "owner" if is_tenant_admin else str(member["role"] if member else "")
            elif auth_mode == "token":
                # Browser tokens are short-lived HMAC grants issued for one exact
                # Publication/origin. They are not reusable Runtime API tokens.
                allowed = channel == "embed" and browser_token_authorized
            if not allowed:
                raise AgentRuntimeUnavailableError("PUBLICATION_ACCESS_DENIED")
            return await self._load_channel_runtime_material(
                conn,
                publication=publication,
                pinned_version_id=pinned_version_id,
                caller_role=caller_role,
            )

    async def get_publication_channel(self, *, public_id: str) -> dict[str, Any]:
        """Return the minimal public delivery descriptor; never return Version spec."""

        self._require_enabled()
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                """
                SELECT publication.tenant_id, publication.publication_id,
                       publication.agent_id, publication.public_id,
                       publication.channel, publication.auth_mode,
                       publication.policy, publication.status,
                       publication.expires_at,
                       agent.name, agent.description,
                       version.resolved_spec->'identity' AS identity
                FROM agent_publications AS publication
                JOIN agents AS agent
                  ON agent.tenant_id = publication.tenant_id
                 AND agent.agent_id = publication.agent_id
                 AND agent.deleted_at IS NULL
                LEFT JOIN agent_versions AS version
                  ON version.tenant_id = publication.tenant_id
                 AND version.agent_version_id = publication.version_id
                WHERE publication.public_id = $1
                """,
                uuid.UUID(public_id),
            )
        _require_active_publication(row)
        return _row_to_dict(row)

    async def resolve_api_token_runtime(
        self,
        *,
        raw_token: str,
        publication_id: str,
        required_scopes: list[str],
        pinned_version_id: str | None = None,
    ) -> dict[str, Any]:
        """Authenticate a one-way token hash and resolve its exact API Publication."""

        self._require_enabled()
        if not raw_token.startswith("agt_") or len(raw_token) > 256:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_TOKEN_INVALID")
        token_hash = hashlib.sha256(raw_token.encode("utf-8")).hexdigest()
        async with self._pool.acquire() as conn, conn.transaction():
            token = await conn.fetchrow(
                """
                SELECT token.*, publication.agent_id, publication.channel,
                       publication.auth_mode, publication.policy,
                       publication.public_id, publication.version_id,
                       publication.expires_at AS publication_expires_at,
                       publication.status AS publication_status
                FROM agent_api_tokens AS token
                JOIN agent_publications AS publication
                  ON publication.tenant_id = token.tenant_id
                 AND publication.publication_id = token.publication_id
                WHERE token.token_hash = $1
                  AND token.publication_id = $2
                FOR UPDATE OF token
                """,
                token_hash,
                uuid.UUID(publication_id),
            )
            if (
                not token
                or token["revoked_at"] is not None
                or (
                    token["expires_at"] is not None
                    and token["expires_at"] <= datetime.now(token["expires_at"].tzinfo)
                )
            ):
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_TOKEN_INVALID")
            if token["publication_status"] != "active":
                raise AgentRuntimeUnavailableError("PUBLICATION_DISABLED")
            if token["channel"] != "api":
                raise AgentRuntimeUnavailableError("PUBLICATION_CHANNEL_MISMATCH")
            scopes = {str(scope) for scope in token["scopes"]}
            missing = sorted(set(required_scopes) - scopes)
            if missing:
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_TOKEN_SCOPE_FORBIDDEN")
            await conn.execute(
                "UPDATE agent_api_tokens SET last_used_at = NOW() WHERE tenant_id = $1 AND token_id = $2",
                token["tenant_id"],
                token["token_id"],
            )
            publication = {
                "tenant_id": token["tenant_id"],
                "publication_id": token["publication_id"],
                "agent_id": token["agent_id"],
                "channel": token["channel"],
                "auth_mode": token["auth_mode"],
                "policy": token["policy"],
                "public_id": token["public_id"],
                "version_id": token["version_id"],
                "expires_at": token["publication_expires_at"],
                "status": token["publication_status"],
            }
            result = await self._load_channel_runtime_material(
                conn,
                publication=publication,
                pinned_version_id=pinned_version_id,
                caller_role="runtime_token",
            )
            result["api_token"] = {
                "token_id": str(token["token_id"]),
                "scopes": sorted(scopes),
            }
            return result

    async def list_members(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> list[dict[str, Any]]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            rows = await conn.fetch(
                """
                SELECT tenant_id, agent_id, principal_type, principal_id, role,
                       created_by, created_at, updated_at
                FROM agent_members
                WHERE tenant_id = $1 AND agent_id = $2
                ORDER BY role, principal_type, principal_id
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
        return [_row_to_dict(row) for row in rows]

    async def upsert_member(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        principal_type: str,
        principal_id: str,
        role: str,
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            if principal_type != "user":
                raise AgentPrincipalNotFoundError("AGENT_PRINCIPAL_NOT_FOUND")
            principal_exists = await conn.fetchval(
                """
                SELECT EXISTS(
                    SELECT 1 FROM users
                    WHERE tenant_id = $1 AND user_id = $2 AND status = 'active'
                )
                """,
                tenant_id,
                principal_id,
            )
            if not principal_exists:
                raise AgentPrincipalNotFoundError("AGENT_PRINCIPAL_NOT_FOUND")
            try:
                row = await conn.fetchrow(
                    """
                    INSERT INTO agent_members (
                        tenant_id, agent_id, principal_type, principal_id, role, created_by
                    ) VALUES ($1, $2, $3, $4, $5, $6)
                    ON CONFLICT (tenant_id, agent_id, principal_type, principal_id)
                    DO UPDATE SET role = EXCLUDED.role, updated_at = NOW()
                    RETURNING *
                    """,
                    tenant_id,
                    uuid.UUID(agent_id),
                    principal_type,
                    principal_id,
                    role,
                    user_id,
                )
            except Exception as exc:
                if "AGENT_LAST_OWNER" in str(exc):
                    raise AgentLastOwnerError("AGENT_LAST_OWNER") from exc
                raise
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="member_upsert",
                summary={
                    "principal_type": principal_type,
                    "principal_id": principal_id,
                    "role": role,
                },
            )
        return _row_to_dict(row)

    async def remove_member(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        principal_type: str,
        principal_id: str,
    ) -> None:
        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            try:
                result = await conn.execute(
                    """
                    DELETE FROM agent_members
                    WHERE tenant_id = $1 AND agent_id = $2
                      AND principal_type = $3 AND principal_id = $4
                    """,
                    tenant_id,
                    uuid.UUID(agent_id),
                    principal_type,
                    principal_id,
                )
            except Exception as exc:
                if "AGENT_LAST_OWNER" in str(exc):
                    raise AgentLastOwnerError("AGENT_LAST_OWNER") from exc
                raise
            if result != "DELETE 1":
                raise AgentNotFoundError("AGENT_MEMBER_NOT_FOUND")
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="member_remove",
                summary={"principal_type": principal_type, "principal_id": principal_id},
            )

    async def copy_agent(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        name: str | None,
        slug: str | None,
    ) -> dict[str, Any]:
        self._require_enabled()
        new_agent_id = uuid.uuid4()
        new_draft_id = uuid.uuid4()
        async with self._pool.acquire() as conn, conn.transaction():
            source, _ = await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
            )
            await self._enforce_tenant_agent_quota(conn, tenant_id=tenant_id)
            source_draft = await conn.fetchrow(
                """
                SELECT spec FROM agent_drafts
                WHERE tenant_id = $1 AND agent_id = $2
                """,
                tenant_id,
                uuid.UUID(agent_id),
            )
            source_spec = source_draft["spec"]
            if not isinstance(source_spec, dict):
                source_spec = json.loads(source_spec)
            copied_spec = sanitize_agent_copy_spec(source_spec)
            copied_name = name or f"{source['name']} Copy"
            copied_slug = _slugify(slug or f"{source['slug']}-copy-{str(new_agent_id)[:8]}")
            await conn.execute(
                """
                INSERT INTO agents (
                    tenant_id, agent_id, slug, name, description, owner_id,
                    created_by, updated_by
                ) VALUES ($1, $2, $3, $4, $5, $6, $6, $6)
                """,
                tenant_id,
                new_agent_id,
                copied_slug,
                copied_name,
                source["description"],
                user_id,
            )
            await conn.execute(
                """
                INSERT INTO agent_drafts (
                    tenant_id, draft_id, agent_id, revision, schema_version,
                    spec, spec_hash, updated_by
                ) VALUES ($1, $2, $3, 1, $4, $5::jsonb, $6, $7)
                """,
                tenant_id,
                new_draft_id,
                new_agent_id,
                AGENT_SPEC_SCHEMA_VERSION,
                canonical_spec(copied_spec),
                hash_agent_spec(copied_spec),
                user_id,
            )
            await conn.execute(
                """
                INSERT INTO agent_members (
                    tenant_id, agent_id, principal_type, principal_id, role, created_by
                ) VALUES ($1, $2, 'user', $3, 'owner', $3)
                """,
                tenant_id,
                new_agent_id,
                user_id,
            )
            row = await conn.fetchrow(
                """
                UPDATE agents
                SET current_draft_id = $3, updated_by = $4
                WHERE tenant_id = $1 AND agent_id = $2
                RETURNING *, 'owner'::text AS caller_role
                """,
                tenant_id,
                new_agent_id,
                new_draft_id,
                user_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=new_agent_id,
                action="copy",
                summary={"source_agent_id": str(agent_id)},
            )
        result = _row_to_dict(row)
        result["draft_revision"] = 1
        return result

    async def archive_agent(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        disable_publications: bool,
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            row = await conn.fetchrow(
                """
                UPDATE agents
                SET status = 'archived', archived_at = NOW(), updated_by = $3
                WHERE tenant_id = $1 AND agent_id = $2 AND deleted_at IS NULL
                RETURNING *
                """,
                tenant_id,
                uuid.UUID(agent_id),
                user_id,
            )
            if disable_publications:
                await conn.execute(
                    """
                    UPDATE agent_publications
                    SET status = 'disabled', updated_by = $3
                    WHERE tenant_id = $1 AND agent_id = $2
                    """,
                    tenant_id,
                    uuid.UUID(agent_id),
                    user_id,
                )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="archive",
                summary={"disable_publications": disable_publications},
            )
        return _row_to_dict(row)

    async def soft_delete_agent(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> None:
        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            await conn.execute(
                """
                UPDATE agents
                SET status = 'deleted', deleted_at = NOW(), updated_by = $3
                WHERE tenant_id = $1 AND agent_id = $2 AND deleted_at IS NULL
                """,
                tenant_id,
                uuid.UUID(agent_id),
                user_id,
            )
            await conn.execute(
                """
                UPDATE agent_publications
                SET status = 'disabled', updated_by = $3
                WHERE tenant_id = $1 AND agent_id = $2
                """,
                tenant_id,
                uuid.UUID(agent_id),
                user_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="soft_delete",
            )

    @staticmethod
    def _default_governance_policy(
        *, tenant_id: str, agent_id: str, updated_by: str
    ) -> dict[str, Any]:
        return {
            "tenant_id": tenant_id,
            "agent_id": agent_id,
            "trace_retention_days": 90,
            "runtime_retention_days": 30,
            "attachment_retention_days": 1,
            "legal_hold": False,
            "principal_requests_per_minute": 30,
            "principal_requests_per_day": 1000,
            "ip_requests_per_minute": 60,
            "ip_requests_per_day": 2000,
            "publication_requests_per_minute": 300,
            "publication_requests_per_day": 10000,
            "max_agents_per_tenant": 100,
            "max_active_publications": 10,
            "max_concurrent_runs": 25,
            "max_daily_tokens": 10_000_000,
            "max_daily_mcp_calls": 100_000,
            "max_storage_bytes": 10_737_418_240,
            "alert_threshold_percent": 90,
            "cache_epoch": 0,
            "updated_by": updated_by,
            "created_at": None,
            "updated_at": None,
        }

    async def get_governance_policy(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="viewer",
                is_tenant_admin=is_tenant_admin,
            )
            row = await conn.fetchrow(
                """
                SELECT * FROM agent_governance_policies
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                """,
                tenant_id,
                agent_id,
            )
        if row:
            return _row_to_dict(row)
        return self._default_governance_policy(
            tenant_id=tenant_id, agent_id=agent_id, updated_by=user_id
        )

    async def update_governance_policy(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        changes: dict[str, Any],
    ) -> dict[str, Any]:
        self._require_enabled()
        allowed = {
            "trace_retention_days",
            "runtime_retention_days",
            "attachment_retention_days",
            "legal_hold",
            "principal_requests_per_minute",
            "principal_requests_per_day",
            "ip_requests_per_minute",
            "ip_requests_per_day",
            "publication_requests_per_minute",
            "publication_requests_per_day",
            "max_agents_per_tenant",
            "max_active_publications",
            "max_concurrent_runs",
            "max_daily_tokens",
            "max_daily_mcp_calls",
            "max_storage_bytes",
            "alert_threshold_percent",
        }
        if not changes or set(changes) - allowed:
            raise AgentRepositoryError("AGENT_GOVERNANCE_POLICY_INVALID")
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            current_row = await conn.fetchrow(
                """
                SELECT * FROM agent_governance_policies
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                FOR UPDATE
                """,
                tenant_id,
                agent_id,
            )
            current = (
                _row_to_dict(current_row)
                if current_row
                else self._default_governance_policy(
                    tenant_id=tenant_id, agent_id=agent_id, updated_by=user_id
                )
            )
            merged = {**current, **changes}
            if bool(changes.get("legal_hold")) and not bool(current.get("legal_hold")):
                active_cleanups = await conn.fetch(
                    """
                    SELECT deletion_id, status, deleted_counts, attempt_count
                    FROM agent_data_deletion_requests
                    WHERE tenant_id = $1 AND agent_id = $2::uuid
                      AND status IN ('pending', 'failed')
                    ORDER BY requested_at, deletion_id
                    FOR UPDATE
                    """,
                    tenant_id,
                    agent_id,
                )
                for cleanup_row in active_cleanups:
                    cleanup = _row_to_dict(cleanup_row)
                    counts = cleanup.get("deleted_counts") or {}
                    if isinstance(counts, str):
                        try:
                            counts = json.loads(counts)
                        except json.JSONDecodeError as exc:
                            raise AgentRepositoryError(
                                "AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"
                            ) from exc
                    if not isinstance(counts, dict):
                        raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_STATE_INVALID")
                    execution = counts.get("cleanup_execution")
                    if execution is not None and not isinstance(execution, dict):
                        raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_STATE_INVALID")
                    execution = dict(execution or {})
                    execution_state = str(execution.get("state") or "")
                    if execution_state == "claimed":
                        # A released DB session lock does not prove the remote deletion
                        # stopped: the provider may have accepted it before disconnect.
                        # Only an idempotent recovery may advance a claimed execution.
                        raise AgentRepositoryError("AGENT_LEGAL_HOLD_CLEANUP_ACTIVE")
                    elif (
                        str(cleanup.get("status")) == "pending"
                        and int(cleanup.get("attempt_count") or 0) > 0
                        and not execution_state
                    ):
                        # A pre-fence in-flight request has no safe liveness proof.
                        raise AgentRepositoryError("AGENT_LEGAL_HOLD_CLEANUP_ACTIVE")

                    interrupted = bool(execution) or int(cleanup.get("attempt_count") or 0) > 0
                    execution.pop("claim_digest", None)
                    execution.update(
                        {
                            "schema_version": _DATA_DELETION_EXECUTION_SCHEMA,
                            "state": "blocked",
                            "blocked_after_execution_started": interrupted,
                        }
                    )
                    counts["cleanup_execution"] = execution
                    error_code = (
                        "AGENT_LEGAL_HOLD_ACTIVE_AFTER_INTERRUPTED_CLEANUP"
                        if interrupted
                        else "AGENT_LEGAL_HOLD_ACTIVE"
                    )
                    await conn.execute(
                        """
                        UPDATE agent_data_deletion_requests
                        SET status = 'blocked', error_code = $2,
                            deleted_counts = $3::jsonb, completed_at = NOW()
                        WHERE deletion_id = $1::uuid
                        """,
                        cleanup["deletion_id"],
                        error_code,
                        json.dumps(counts, sort_keys=True),
                    )
                    await self._audit(
                        conn,
                        tenant_id=tenant_id,
                        user_id=user_id,
                        agent_id=agent_id,
                        action="data_deletion_blocked",
                        summary={
                            "deletion_id": str(cleanup["deletion_id"]),
                            "stage": "legal_hold_activation",
                            "interrupted": interrupted,
                        },
                    )
            row = await conn.fetchrow(
                """
                INSERT INTO agent_governance_policies (
                    tenant_id, agent_id, trace_retention_days,
                    runtime_retention_days, attachment_retention_days, legal_hold,
                    principal_requests_per_minute, principal_requests_per_day,
                    ip_requests_per_minute, ip_requests_per_day,
                    publication_requests_per_minute, publication_requests_per_day,
                    alert_threshold_percent, max_agents_per_tenant,
                    max_active_publications, max_concurrent_runs,
                    max_daily_tokens, max_daily_mcp_calls, max_storage_bytes,
                    updated_by
                ) VALUES (
                    $1, $2::uuid, $3, $4, $5, $6, $7, $8, $9, $10,
                    $11, $12, $13, $14, $15, $16, $17, $18, $19, $20
                )
                ON CONFLICT (tenant_id, agent_id) DO UPDATE SET
                    trace_retention_days = EXCLUDED.trace_retention_days,
                    runtime_retention_days = EXCLUDED.runtime_retention_days,
                    attachment_retention_days = EXCLUDED.attachment_retention_days,
                    legal_hold = EXCLUDED.legal_hold,
                    principal_requests_per_minute = EXCLUDED.principal_requests_per_minute,
                    principal_requests_per_day = EXCLUDED.principal_requests_per_day,
                    ip_requests_per_minute = EXCLUDED.ip_requests_per_minute,
                    ip_requests_per_day = EXCLUDED.ip_requests_per_day,
                    publication_requests_per_minute = EXCLUDED.publication_requests_per_minute,
                    publication_requests_per_day = EXCLUDED.publication_requests_per_day,
                    alert_threshold_percent = EXCLUDED.alert_threshold_percent,
                    max_agents_per_tenant = EXCLUDED.max_agents_per_tenant,
                    max_active_publications = EXCLUDED.max_active_publications,
                    max_concurrent_runs = EXCLUDED.max_concurrent_runs,
                    max_daily_tokens = EXCLUDED.max_daily_tokens,
                    max_daily_mcp_calls = EXCLUDED.max_daily_mcp_calls,
                    max_storage_bytes = EXCLUDED.max_storage_bytes,
                    updated_by = EXCLUDED.updated_by,
                    updated_at = NOW()
                RETURNING *
                """,
                tenant_id,
                agent_id,
                merged["trace_retention_days"],
                merged["runtime_retention_days"],
                merged["attachment_retention_days"],
                merged["legal_hold"],
                merged["principal_requests_per_minute"],
                merged["principal_requests_per_day"],
                merged["ip_requests_per_minute"],
                merged["ip_requests_per_day"],
                merged["publication_requests_per_minute"],
                merged["publication_requests_per_day"],
                merged["alert_threshold_percent"],
                merged["max_agents_per_tenant"],
                merged["max_active_publications"],
                merged["max_concurrent_runs"],
                merged["max_daily_tokens"],
                merged["max_daily_mcp_calls"],
                merged["max_storage_bytes"],
                user_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="governance_policy_update",
                summary={"fields": sorted(changes), "after": changes},
            )
        return _row_to_dict(row)

    @classmethod
    async def _write_governance_quota_alert(
        cls,
        conn: Any,
        *,
        tenant_id: str,
        agent_id: str,
        publication_id: str,
        quota: str,
        usage: int,
        limit: int,
        threshold_percent: int,
    ) -> None:
        action = "quota_exceeded" if usage >= limit else "quota_threshold_reached"
        summary = {
            "publication_id": publication_id,
            "quota": quota,
            "usage": usage,
            "limit": limit,
            "threshold_percent": threshold_percent,
        }
        await conn.execute(
            """
            INSERT INTO audit_logs (
                event_type, user_id, tenant_id, resource_type, resource_id,
                action, request_summary, status
            )
            SELECT 'agent_studio', 'system:agent-governance', $1::varchar,
                   'agent', $2::varchar, $3::varchar, $4::jsonb, 'success'
            WHERE NOT EXISTS (
                SELECT 1 FROM audit_logs
                WHERE tenant_id = $1::varchar AND agent_id = $2::uuid
                  AND action = $3::varchar AND request_summary->>'quota' = $5::text
                  AND created_at >= date_trunc('day', NOW())
            )
            """,
            tenant_id,
            agent_id,
            action,
            canonical_spec(summary),
            quota,
        )

    async def get_runtime_governance_usage(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        publication_id: str,
    ) -> dict[str, Any]:
        """Load authoritative cross-worker usage and emit threshold alerts."""

        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            publication_exists = await conn.fetchval(
                """
                SELECT EXISTS (
                    SELECT 1 FROM agent_publications
                    WHERE tenant_id = $1 AND agent_id = $2::uuid
                      AND publication_id = $3::uuid AND status = 'active'
                )
                """,
                tenant_id,
                agent_id,
                publication_id,
            )
            if not publication_exists:
                raise AgentRuntimeUnavailableError("PUBLICATION_DISABLED")
            policy_row = await conn.fetchrow(
                """
                SELECT * FROM agent_governance_policies
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                """,
                tenant_id,
                agent_id,
            )
            policy = {
                **self._default_governance_policy(
                    tenant_id=tenant_id,
                    agent_id=agent_id,
                    updated_by="system:agent-governance",
                ),
                **(_row_to_dict(policy_row) if policy_row else {}),
            }
            usage_row = await conn.fetchrow(
                """
                SELECT
                    (
                        SELECT COUNT(*)::bigint
                        FROM assistant_runs
                        WHERE tenant_id = $1 AND agent_id = $2::uuid
                          AND status IN ('queued', 'running', 'awaiting_approval')
                    ) AS concurrent_runs,
                    (
                        SELECT COALESCE(SUM(total_tokens), 0)::bigint
                        FROM agent_traces
                        WHERE tenant_id = $1 AND agent_id = $2::uuid
                          AND created_at >= date_trunc('day', NOW())
                    ) AS daily_tokens,
                    (
                        SELECT COUNT(*)::bigint
                        FROM agent_trace_spans span
                        JOIN agent_traces trace ON trace.trace_id = span.trace_id
                        WHERE trace.tenant_id = $1 AND trace.agent_id = $2::uuid
                          AND trace.created_at >= date_trunc('day', NOW())
                          AND span.span_kind = 'tool_execution'
                          AND (
                              span.name LIKE 'tool:mcp_%'
                              OR span.attributes->>'capability_type' = 'mcp'
                          )
                    ) AS daily_mcp_calls,
                    (
                        SELECT COALESCE(SUM(attachment.size_bytes), 0)::bigint
                        FROM agent_runtime_attachments attachment
                        JOIN agent_publications publication
                          ON publication.tenant_id = attachment.tenant_id
                         AND publication.publication_id = attachment.publication_id
                        WHERE publication.tenant_id = $1
                          AND publication.agent_id = $2::uuid
                          AND attachment.deleted_at IS NULL
                    ) AS storage_bytes
                """,
                tenant_id,
                agent_id,
            )
            usage = {
                key: int((usage_row or {}).get(key) or 0)
                for key in (
                    "concurrent_runs",
                    "daily_tokens",
                    "daily_mcp_calls",
                    "storage_bytes",
                )
            }
            quota_fields = {
                "concurrent_runs": "max_concurrent_runs",
                "daily_tokens": "max_daily_tokens",
                "daily_mcp_calls": "max_daily_mcp_calls",
                "storage_bytes": "max_storage_bytes",
            }
            exceeded: list[str] = []
            codes = {
                "concurrent_runs": "AGENT_RUNTIME_CONCURRENCY_QUOTA_EXCEEDED",
                "daily_tokens": "AGENT_RUNTIME_TOKEN_QUOTA_EXCEEDED",
                "daily_mcp_calls": "AGENT_RUNTIME_MCP_QUOTA_EXCEEDED",
                "storage_bytes": "AGENT_RUNTIME_STORAGE_QUOTA_EXCEEDED",
            }
            threshold = int(policy["alert_threshold_percent"])
            for quota, policy_field in quota_fields.items():
                limit = int(policy[policy_field])
                if usage[quota] * 100 >= limit * threshold:
                    await self._write_governance_quota_alert(
                        conn,
                        tenant_id=tenant_id,
                        agent_id=agent_id,
                        publication_id=publication_id,
                        quota=quota,
                        usage=usage[quota],
                        limit=limit,
                        threshold_percent=threshold,
                    )
                if usage[quota] >= limit:
                    exceeded.append(codes[quota])
        return {"policy": policy, "usage": usage, "exceeded": exceeded}

    async def invalidate_agent_caches(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            row = await conn.fetchrow(
                """
                INSERT INTO agent_governance_policies (
                    tenant_id, agent_id, updated_by, cache_epoch
                ) VALUES ($1, $2::uuid, $3, 1)
                ON CONFLICT (tenant_id, agent_id) DO UPDATE SET
                    cache_epoch = agent_governance_policies.cache_epoch + 1,
                    updated_by = EXCLUDED.updated_by,
                    updated_at = NOW()
                RETURNING *
                """,
                tenant_id,
                agent_id,
                user_id,
            )
            deleted = await conn.fetchval(
                """
                WITH doomed AS (
                    DELETE FROM semantic_cache
                    WHERE metadata->>'tenant_id' = $1
                      AND metadata->>'agent_id' = $2
                    RETURNING 1
                ) SELECT COUNT(*)::int FROM doomed
                """,
                tenant_id,
                agent_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="cache_invalidate",
                summary={"cache_epoch": int(row["cache_epoch"]), "rows": int(deleted or 0)},
            )
        result = _row_to_dict(row)
        result["deleted_cache_rows"] = int(deleted or 0)
        return result

    async def list_agent_audit_events(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        agent_version_id: str | None = None,
        publication_id: str | None = None,
        channel: str | None = None,
        action: str | None = None,
        started_after: Any | None = None,
        started_before: Any | None = None,
        limit: int = 50,
        offset: int = 0,
    ) -> tuple[list[dict[str, Any]], int]:
        self._require_enabled()
        async with self._pool.acquire() as conn:
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
            )
            params: list[Any] = [tenant_id, agent_id]
            filters = ["tenant_id = $1", "agent_id = $2::uuid", "event_type = 'agent_studio'"]
            for value, expression in (
                (agent_version_id, "agent_version_id"),
                (publication_id, "publication_id"),
            ):
                if value:
                    params.append(value)
                    filters.append(f"{expression} = ${len(params)}::uuid")
            if channel:
                params.append(channel)
                filters.append(f"channel = ${len(params)}")
            if action:
                params.append(action)
                filters.append(f"action = ${len(params)}")
            if started_after is not None:
                params.append(started_after)
                filters.append(f"created_at >= ${len(params)}")
            if started_before is not None:
                params.append(started_before)
                filters.append(f"created_at <= ${len(params)}")
            where_clause = " AND ".join(filters)
            count = await conn.fetchval(
                f"SELECT COUNT(*)::int FROM audit_logs WHERE {where_clause}", *params
            )
            rows = await conn.fetch(
                f"""
                SELECT id, event_type, user_id, tenant_id, resource_type, resource_id,
                       action, request_summary, response_summary, status, created_at,
                       agent_id, agent_version_id, publication_id, channel, redaction_state
                FROM audit_logs
                WHERE {where_clause}
                ORDER BY created_at DESC, id DESC
                LIMIT ${len(params) + 1} OFFSET ${len(params) + 2}
                """,
                *params,
                limit,
                offset,
            )
        decoded: list[dict[str, Any]] = []
        for source in rows:
            item = _row_to_dict(source)
            for field in ("request_summary", "response_summary"):
                value = item.get(field)
                if isinstance(value, str):
                    try:
                        value = json.loads(value)
                    except json.JSONDecodeError:
                        value = {}
                item[field] = _redact_unstructured_spec_value(value or {})
            decoded.append(item)
        return decoded, int(count or 0)

    async def revoke_agent_credentials(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> dict[str, int]:
        """Revoke Agent-specific tokens and bound grants without deleting history."""

        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            token_result = await conn.execute(
                """
                UPDATE agent_api_tokens t
                SET revoked_at = COALESCE(t.revoked_at, NOW())
                FROM agent_publications p
                WHERE p.tenant_id = $1 AND p.agent_id = $2::uuid
                  AND t.tenant_id = p.tenant_id AND t.publication_id = p.publication_id
                  AND t.revoked_at IS NULL
                """,
                tenant_id,
                agent_id,
            )
            mcp_result = await conn.execute(
                """
                UPDATE mcp_channel_grants g
                SET enabled = FALSE, updated_at = NOW()
                WHERE g.tenant_id = $1 AND g.enabled = TRUE
                  AND g.tool_id::text IN (
                      SELECT c.resource_id
                      FROM agent_version_capabilities c
                      JOIN agent_versions v
                        ON v.tenant_id = c.tenant_id
                       AND v.agent_version_id = c.agent_version_id
                      WHERE v.tenant_id = $1 AND v.agent_id = $2::uuid
                        AND c.capability_type = 'mcp'
                  )
                """,
                tenant_id,
                agent_id,
            )
            connector_result = await conn.execute(
                """
                UPDATE connector_credential_principals g
                SET enabled = FALSE, revoked_at = COALESCE(revoked_at, NOW()),
                    updated_by = $3, updated_at = NOW()
                WHERE g.tenant_id = $1 AND g.enabled = TRUE
                  AND g.grant_id::text IN (
                      SELECT c.resource_id
                      FROM agent_version_capabilities c
                      JOIN agent_versions v
                        ON v.tenant_id = c.tenant_id
                       AND v.agent_version_id = c.agent_version_id
                      WHERE v.tenant_id = $1 AND v.agent_id = $2::uuid
                        AND c.capability_type = 'connector'
                  )
                """,
                tenant_id,
                agent_id,
                user_id,
            )

            def affected(command: str) -> int:
                try:
                    return int(command.rsplit(" ", 1)[-1])
                except (TypeError, ValueError):
                    return 0

            counts = {
                "api_tokens": affected(token_result),
                "mcp_channel_grants": affected(mcp_result),
                "connector_grants": affected(connector_result),
            }
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="credentials_revoke",
                summary=counts,
            )
        return counts

    async def _frozen_agent_memory_principals(
        self,
        conn: Any,
        *,
        tenant_id: str,
        agent_id: str,
        scope: str,
        subject_user_id: str | None,
        cutoff_at: datetime,
    ) -> list[str]:
        """Resolve opaque Agent memory principals at the prepare cutoff."""

        session_params: list[Any] = [tenant_id, agent_id, cutoff_at]
        timestamp_column = "updated_at" if scope == "retention" else "created_at"
        session_condition = f" AND {timestamp_column} <= $3"
        if scope == "user":
            session_params.append(subject_user_id)
            session_condition += f" AND user_id = ${len(session_params)}"
        session_rows = await conn.fetch(
            f"""
            SELECT user_id, agent_version_id, agent_draft_revision
            FROM assistant.sessions
            WHERE tenant_id = $1 AND agent_id = $2::uuid{session_condition}
            """,
            *session_params,
        )
        principals: set[str] = set()

        def add_principal(caller: str, version_scope: str) -> None:
            digest = hashlib.sha256(f"{caller}:{agent_id}:{version_scope}".encode()).hexdigest()
            principals.add(agent_memory_principal(caller, agent_id, version_scope))
            principals.add(f"agent-memory:{digest}")

        for item in session_rows:
            caller = str(item.get("user_id") or "")
            version_id = item.get("agent_version_id")
            draft_revision = item.get("agent_draft_revision")
            if not caller or (version_id is None and draft_revision is None):
                continue
            version_scope = (
                f"version:{version_id}" if version_id is not None else f"draft:{draft_revision}"
            )
            add_principal(caller, version_scope)

        if scope in {"user", "tenant"}:
            if scope == "user":
                callers = {str(subject_user_id)}
            else:
                caller_rows = await conn.fetch(
                    "SELECT user_id FROM users WHERE tenant_id = $1",
                    tenant_id,
                )
                callers = {str(item["user_id"]) for item in caller_rows if item.get("user_id")}
            version_rows = await conn.fetch(
                """
                SELECT agent_version_id
                FROM agent_versions
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                  AND created_at <= $3
                """,
                tenant_id,
                agent_id,
                cutoff_at,
            )
            version_scopes = {f"version:{item['agent_version_id']}" for item in version_rows}
            draft_revision = await conn.fetchval(
                """
                SELECT revision
                FROM agent_drafts
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                  AND created_at <= $3
                """,
                tenant_id,
                agent_id,
                cutoff_at,
            )
            if draft_revision:
                version_scopes.update(
                    f"draft:{revision}" for revision in range(1, int(draft_revision) + 1)
                )
            for caller in callers:
                for version_scope in version_scopes:
                    add_principal(caller, version_scope)
        return sorted(principals)

    async def prepare_agent_data_deletion(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        user_id: str,
        is_tenant_admin: bool,
        scope: str,
        subject_user_id: str | None,
        idempotency_key: str,
    ) -> dict[str, Any]:
        """Create a durable cleanup request and freeze its external object set."""

        self._require_enabled()
        if scope not in {"retention", "user", "tenant"}:
            raise AgentRepositoryError("AGENT_DATA_DELETION_SCOPE_INVALID")
        if (scope == "user") != bool(subject_user_id):
            raise AgentRepositoryError("AGENT_DATA_DELETION_SUBJECT_INVALID")
        async with self._pool.acquire() as conn, conn.transaction():
            receipt_sql = """
                SELECT * FROM agent_data_deletion_requests
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                  AND scope = $3 AND idempotency_key = $4
            """
            existing = await conn.fetchrow(
                receipt_sql,
                tenant_id,
                agent_id,
                scope,
                idempotency_key,
            )
            if existing:
                existing_data = _row_to_dict(existing)
                if not is_tenant_admin and existing_data.get("requested_by") != user_id:
                    raise AgentNotFoundError("AGENT_NOT_FOUND")
                if existing_data.get("status") in {"completed", "blocked"}:
                    return existing_data

            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            existing = await conn.fetchrow(
                receipt_sql + " FOR UPDATE",
                tenant_id,
                agent_id,
                scope,
                idempotency_key,
            )
            if existing:
                existing_data = _row_to_dict(existing)
                if not is_tenant_admin and existing_data.get("requested_by") != user_id:
                    raise AgentNotFoundError("AGENT_NOT_FOUND")
                return existing_data
            policy = await conn.fetchrow(
                """
                SELECT * FROM agent_governance_policies
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                FOR UPDATE
                """,
                tenant_id,
                agent_id,
            )
            defaults = self._default_governance_policy(
                tenant_id=tenant_id, agent_id=agent_id, updated_by=user_id
            )
            policy_data = {**defaults, **(_row_to_dict(policy) if policy else {})}
            if bool(policy_data["legal_hold"]):
                row = await conn.fetchrow(
                    """
                    INSERT INTO agent_data_deletion_requests (
                        tenant_id, agent_id, scope, subject_user_id,
                        idempotency_key, status, error_code, requested_by, completed_at
                    ) VALUES ($1, $2::uuid, $3, $4, $5, 'blocked',
                              'AGENT_LEGAL_HOLD_ACTIVE', $6, NOW())
                    RETURNING *
                    """,
                    tenant_id,
                    agent_id,
                    scope,
                    subject_user_id,
                    idempotency_key,
                    user_id,
                )
                await self._audit(
                    conn,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    agent_id=agent_id,
                    action="data_deletion_blocked",
                    summary={"scope": scope, "subject_user_id": subject_user_id},
                )
                return _row_to_dict(row)

            attachment_params: list[Any] = [tenant_id, agent_id]
            attachment_filters = [
                "a.tenant_id = $1",
                "p.agent_id = $2::uuid",
                "a.deleted_at IS NULL",
            ]
            if scope == "user":
                attachment_params.append(subject_user_id)
                attachment_filters.append(f"a.principal_id = ${len(attachment_params)}")
            elif scope == "retention":
                attachment_params.append(int(policy_data["attachment_retention_days"]))
                attachment_filters.append(
                    f"(a.expires_at < NOW() OR a.created_at < NOW() - make_interval(days => ${len(attachment_params)}::int))"
                )
            object_rows = await conn.fetch(
                f"""
                SELECT DISTINCT a.storage_key
                FROM agent_runtime_attachments a
                JOIN agent_publications p
                  ON p.tenant_id = a.tenant_id AND p.publication_id = a.publication_id
                WHERE {" AND ".join(attachment_filters)}
                ORDER BY a.storage_key
                """,
                *attachment_params,
            )
            object_keys = [str(item["storage_key"]) for item in object_rows]
            policy_snapshot = {
                "trace_retention_days": policy_data["trace_retention_days"],
                "runtime_retention_days": policy_data["runtime_retention_days"],
                "attachment_retention_days": policy_data["attachment_retention_days"],
            }
            row = await conn.fetchrow(
                """
                INSERT INTO agent_data_deletion_requests (
                    tenant_id, agent_id, scope, subject_user_id,
                    idempotency_key, object_keys, deleted_counts, requested_by
                ) VALUES ($1, $2::uuid, $3, $4, $5, $6::jsonb, $7::jsonb, $8)
                RETURNING *
                """,
                tenant_id,
                agent_id,
                scope,
                subject_user_id,
                idempotency_key,
                json.dumps(object_keys),
                json.dumps({"policy": policy_snapshot}),
                user_id,
            )
            requested_at = row["requested_at"]
            cutoff_at = requested_at
            if scope == "retention":
                cutoff_at = requested_at - timedelta(
                    days=int(policy_snapshot["runtime_retention_days"])
                )
            principal_handles = await self._frozen_agent_memory_principals(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                scope=scope,
                subject_user_id=subject_user_id,
                cutoff_at=cutoff_at,
            )
            cleanup_plan = build_runtime_cleanup_plan(
                deletion_id=str(row["deletion_id"]),
                tenant_id=tenant_id,
                agent_id=agent_id,
                scope=scope,
                subject_user_id=subject_user_id,
                cutoff_at=cutoff_at.isoformat(),
                principal_handles=principal_handles,
            )
            row = await conn.fetchrow(
                """
                UPDATE agent_data_deletion_requests
                SET deleted_counts = $2::jsonb
                WHERE deletion_id = $1::uuid
                RETURNING *
                """,
                row["deletion_id"],
                json.dumps(
                    {
                        "policy": policy_snapshot,
                        "runtime_cleanup_plan": cleanup_plan,
                    },
                    sort_keys=True,
                ),
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="data_deletion_requested",
                summary={
                    "deletion_id": str(row["deletion_id"]),
                    "scope": scope,
                    "subject_user_id": subject_user_id,
                    "object_count": len(object_keys),
                },
            )
        return _row_to_dict(row)

    @asynccontextmanager
    async def _agent_data_deletion_connection(
        self,
        bound_connection: Any | None,
    ) -> AsyncIterator[Any]:
        """Reuse a claimed session-lock connection or acquire one for legacy callers."""

        if bound_connection is not None:
            if bound_connection.is_closed():
                raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_FENCE_LOST")
            yield bound_connection
            return
        async with self._pool.acquire() as conn:
            yield conn

    async def freeze_agent_runtime_cleanup_inventory(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        deletion_id: str,
        user_id: str,
        is_tenant_admin: bool,
        inventory: dict[str, Any],
        _execution_connection: Any | None = None,
        _execution_generation: int | None = None,
        _execution_claim_digest: str | None = None,
    ) -> dict[str, Any]:
        """Durably freeze Assistant's source handles before deletion starts."""

        self._require_enabled()
        async with (
            self._agent_data_deletion_connection(_execution_connection) as conn,
            conn.transaction(),
        ):
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            request_row = await conn.fetchrow(
                """
                SELECT * FROM agent_data_deletion_requests
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                  AND deletion_id = $3::uuid
                FOR UPDATE
                """,
                tenant_id,
                agent_id,
                deletion_id,
            )
            if not request_row:
                raise AgentRepositoryError("AGENT_DATA_DELETION_NOT_FOUND")
            request_data = _row_to_dict(request_row)
            if _execution_connection is not None:
                counts_snapshot = request_data.get("deleted_counts") or {}
                if isinstance(counts_snapshot, str):
                    try:
                        counts_snapshot = json.loads(counts_snapshot)
                    except json.JSONDecodeError as exc:
                        raise AgentRepositoryError(
                            "AGENT_DATA_DELETION_EXECUTION_FENCE_LOST"
                        ) from exc
                execution = (
                    counts_snapshot.get("cleanup_execution")
                    if isinstance(counts_snapshot, dict)
                    else None
                )
                try:
                    stored_generation = int((execution or {}).get("generation") or 0)
                    expected_generation = int(_execution_generation or 0)
                except (TypeError, ValueError) as exc:
                    raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_FENCE_LOST") from exc
                if (
                    request_data["status"] != "pending"
                    or not isinstance(execution, dict)
                    or execution.get("schema_version") != _DATA_DELETION_EXECUTION_SCHEMA
                    or execution.get("state") != "claimed"
                    or stored_generation < 1
                    or stored_generation != expected_generation
                    or not _execution_claim_digest
                    or not secrets.compare_digest(
                        str(execution.get("claim_digest") or ""),
                        _execution_claim_digest,
                    )
                ):
                    raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_FENCE_LOST")
            if request_data["status"] not in {"pending", "failed"}:
                return request_data
            deleted_counts = request_data.get("deleted_counts") or {}
            if isinstance(deleted_counts, str):
                deleted_counts = json.loads(deleted_counts)
            try:
                plan = validate_runtime_cleanup_plan(deleted_counts.get("runtime_cleanup_plan"))
                frozen_inventory = validate_runtime_cleanup_inventory(
                    inventory,
                    plan=plan,
                )
            except (TypeError, ValueError) as exc:
                raise AgentRepositoryError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID") from exc
            existing_inventory = deleted_counts.get("runtime_cleanup_inventory")
            if existing_inventory is not None:
                try:
                    existing_inventory = validate_runtime_cleanup_inventory(
                        existing_inventory,
                        plan=plan,
                    )
                except (TypeError, ValueError) as exc:
                    raise AgentRepositoryError("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID") from exc
                if existing_inventory["inventory_digest"] != frozen_inventory["inventory_digest"]:
                    raise AgentRepositoryError("AGENT_RUNTIME_CLEANUP_INVENTORY_CONFLICT")
                return request_data
            deleted_counts = {
                **deleted_counts,
                "runtime_cleanup_inventory": frozen_inventory,
            }
            row = await conn.fetchrow(
                """
                UPDATE agent_data_deletion_requests
                SET deleted_counts = $2::jsonb
                WHERE deletion_id = $1::uuid
                RETURNING *
                """,
                deletion_id,
                json.dumps(deleted_counts, sort_keys=True),
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="data_deletion_inventory_frozen",
                summary={
                    "deletion_id": deletion_id,
                    "principal_count": len(frozen_inventory["principals"]),
                    "source_count": sum(
                        int(item.get("source_count") or 0)
                        for item in frozen_inventory["principals"]
                    ),
                    "inventory_digest": frozen_inventory["inventory_digest"],
                },
            )
        return _row_to_dict(row)

    @asynccontextmanager
    async def claim_agent_data_deletion_execution(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        deletion_id: str,
        user_id: str,
        is_tenant_admin: bool,
    ) -> AsyncIterator[dict[str, Any]]:
        """Fence one external cleanup execution with a durable generation claim."""

        self._require_enabled()
        lock_key = _agent_data_deletion_lock_key(
            tenant_id=tenant_id,
            agent_id=agent_id,
            deletion_id=deletion_id,
        )
        async with self._pool.acquire() as conn:
            lock_acquired = bool(
                await conn.fetchval(
                    "SELECT pg_try_advisory_lock($1::bigint)",
                    lock_key,
                )
            )
            if not lock_acquired:
                await self._authorized_agent(
                    conn,
                    tenant_id=tenant_id,
                    agent_id=agent_id,
                    user_id=user_id,
                    required_role="owner",
                    is_tenant_admin=is_tenant_admin,
                )
                request_row = await conn.fetchrow(
                    """
                    SELECT * FROM agent_data_deletion_requests
                    WHERE tenant_id = $1 AND agent_id = $2::uuid
                      AND deletion_id = $3::uuid
                    """,
                    tenant_id,
                    agent_id,
                    deletion_id,
                )
                if not request_row:
                    raise AgentRepositoryError("AGENT_DATA_DELETION_NOT_FOUND")
                result = _row_to_dict(request_row)
                result["execution_claimed"] = False
                yield result
                return

            try:
                claim_token = secrets.token_urlsafe(32)
                claim_generation = 0
                claim_digest = ""
                execution_claimed = False
                async with conn.transaction():
                    await self._authorized_agent(
                        conn,
                        tenant_id=tenant_id,
                        agent_id=agent_id,
                        user_id=user_id,
                        required_role="owner",
                        is_tenant_admin=is_tenant_admin,
                        for_update=True,
                    )
                    request_row = await conn.fetchrow(
                        """
                        SELECT * FROM agent_data_deletion_requests
                        WHERE tenant_id = $1 AND agent_id = $2::uuid
                          AND deletion_id = $3::uuid
                        FOR UPDATE
                        """,
                        tenant_id,
                        agent_id,
                        deletion_id,
                    )
                    if not request_row:
                        raise AgentRepositoryError("AGENT_DATA_DELETION_NOT_FOUND")
                    request_data = _row_to_dict(request_row)
                    if request_data["status"] not in {"pending", "failed"}:
                        result = request_data
                    else:
                        policy_row = await conn.fetchrow(
                            """
                            SELECT legal_hold
                            FROM agent_governance_policies
                            WHERE tenant_id = $1 AND agent_id = $2::uuid
                            FOR UPDATE
                            """,
                            tenant_id,
                            agent_id,
                        )
                        if bool((policy_row or {}).get("legal_hold")):
                            result_row = await conn.fetchrow(
                                """
                                UPDATE agent_data_deletion_requests
                                SET status = 'blocked',
                                    error_code = 'AGENT_LEGAL_HOLD_ACTIVE',
                                    completed_at = NOW()
                                WHERE deletion_id = $1::uuid
                                RETURNING *
                                """,
                                deletion_id,
                            )
                            await self._audit(
                                conn,
                                tenant_id=tenant_id,
                                user_id=user_id,
                                agent_id=agent_id,
                                action="data_deletion_blocked",
                                summary={
                                    "deletion_id": deletion_id,
                                    "scope": request_data["scope"],
                                    "stage": "execution_claim",
                                },
                            )
                            result = _row_to_dict(result_row)
                        else:
                            counts = request_data.get("deleted_counts") or {}
                            if isinstance(counts, str):
                                try:
                                    counts = json.loads(counts)
                                except json.JSONDecodeError as exc:
                                    raise AgentRepositoryError(
                                        "AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"
                                    ) from exc
                            if not isinstance(counts, dict):
                                raise AgentRepositoryError(
                                    "AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"
                                )
                            prior_execution = counts.get("cleanup_execution")
                            if prior_execution is not None and not isinstance(
                                prior_execution, dict
                            ):
                                raise AgentRepositoryError(
                                    "AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"
                                )
                            prior_execution = dict(prior_execution or {})
                            try:
                                prior_generation = int(prior_execution.get("generation") or 0)
                            except (TypeError, ValueError) as exc:
                                raise AgentRepositoryError(
                                    "AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"
                                ) from exc
                            if prior_generation < 0:
                                raise AgentRepositoryError(
                                    "AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"
                                )
                            claim_generation = prior_generation + 1
                            claim_digest = _agent_data_deletion_claim_digest(
                                deletion_id=deletion_id,
                                generation=claim_generation,
                                claim_token=claim_token,
                            )
                            claimed_at = await conn.fetchval("SELECT NOW()")
                            counts["cleanup_execution"] = {
                                "schema_version": _DATA_DELETION_EXECUTION_SCHEMA,
                                "state": "claimed",
                                "generation": claim_generation,
                                "claim_digest": claim_digest,
                                "claimed_at": claimed_at.isoformat(),
                                "recovered": bool(prior_execution),
                            }
                            result_row = await conn.fetchrow(
                                """
                                UPDATE agent_data_deletion_requests
                                SET status = 'pending', deleted_counts = $2::jsonb,
                                    error_code = 'AGENT_DATA_DELETION_EXECUTION_IN_PROGRESS',
                                    attempt_count = attempt_count + 1,
                                    last_attempt_at = NOW(), completed_at = NULL
                                WHERE deletion_id = $1::uuid
                                RETURNING *
                                """,
                                deletion_id,
                                json.dumps(counts, sort_keys=True),
                            )
                            await self._audit(
                                conn,
                                tenant_id=tenant_id,
                                user_id=user_id,
                                agent_id=agent_id,
                                action="data_deletion_execution_claimed",
                                summary={
                                    "deletion_id": deletion_id,
                                    "generation": claim_generation,
                                    "recovered": bool(prior_execution),
                                },
                            )
                            result = _row_to_dict(result_row)
                            execution_claimed = True

                result["execution_claimed"] = execution_claimed
                if not execution_claimed:
                    yield result
                    return

                async def assert_execution_fence() -> None:
                    if conn.is_closed():
                        raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_FENCE_LOST")
                    current_row = await conn.fetchrow(
                        """
                        SELECT status, deleted_counts
                        FROM agent_data_deletion_requests
                        WHERE tenant_id = $1 AND agent_id = $2::uuid
                          AND deletion_id = $3::uuid
                        """,
                        tenant_id,
                        agent_id,
                        deletion_id,
                    )
                    if not current_row or str(current_row["status"]) != "pending":
                        raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_FENCE_LOST")
                    current_counts = current_row["deleted_counts"] or {}
                    if isinstance(current_counts, str):
                        try:
                            current_counts = json.loads(current_counts)
                        except json.JSONDecodeError as exc:
                            raise AgentRepositoryError(
                                "AGENT_DATA_DELETION_EXECUTION_FENCE_LOST"
                            ) from exc
                    execution = (
                        current_counts.get("cleanup_execution")
                        if isinstance(current_counts, dict)
                        else None
                    )
                    if not isinstance(execution, dict) or (
                        execution.get("state") != "claimed"
                        or execution.get("generation") != claim_generation
                        or not secrets.compare_digest(
                            str(execution.get("claim_digest") or ""),
                            claim_digest,
                        )
                    ):
                        raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_FENCE_LOST")

                async def freeze_execution_inventory(
                    *, inventory: dict[str, Any]
                ) -> dict[str, Any]:
                    await assert_execution_fence()
                    return await self.freeze_agent_runtime_cleanup_inventory(
                        tenant_id=tenant_id,
                        agent_id=agent_id,
                        deletion_id=deletion_id,
                        user_id=user_id,
                        is_tenant_admin=is_tenant_admin,
                        inventory=inventory,
                        _execution_connection=conn,
                        _execution_generation=claim_generation,
                        _execution_claim_digest=claim_digest,
                    )

                async def finish_execution(
                    *,
                    storage_cleanup_succeeded: bool,
                    runtime_cleanup_receipt: dict[str, Any] | None = None,
                ) -> dict[str, Any]:
                    await assert_execution_fence()
                    return await self.finish_agent_data_deletion(
                        tenant_id=tenant_id,
                        agent_id=agent_id,
                        deletion_id=deletion_id,
                        user_id=user_id,
                        is_tenant_admin=is_tenant_admin,
                        storage_cleanup_succeeded=storage_cleanup_succeeded,
                        runtime_cleanup_receipt=runtime_cleanup_receipt,
                        execution_claim_token=claim_token,
                        execution_generation=claim_generation,
                        _execution_connection=conn,
                    )

                result["_execution_claim_token"] = claim_token
                result["_execution_generation"] = claim_generation
                result["_execution_guard"] = assert_execution_fence
                result["_execution_freeze_inventory"] = freeze_execution_inventory
                result["_execution_finish"] = finish_execution
                yield result
            finally:
                if lock_acquired and not conn.is_closed():
                    await conn.fetchval(
                        "SELECT pg_advisory_unlock($1::bigint)",
                        lock_key,
                    )

    async def finish_agent_data_deletion(
        self,
        *,
        tenant_id: str,
        agent_id: str,
        deletion_id: str,
        user_id: str,
        is_tenant_admin: bool,
        storage_cleanup_succeeded: bool,
        runtime_cleanup_receipt: dict[str, Any] | None = None,
        execution_claim_token: str | None = None,
        execution_generation: int | None = None,
        _execution_connection: Any | None = None,
    ) -> dict[str, Any]:
        """Commit DB cleanup only after frozen object/runtime receipts complete."""

        self._require_enabled()
        async with (
            self._agent_data_deletion_connection(_execution_connection) as conn,
            conn.transaction(),
        ):
            await self._authorized_agent(
                conn,
                tenant_id=tenant_id,
                agent_id=agent_id,
                user_id=user_id,
                required_role="owner",
                is_tenant_admin=is_tenant_admin,
                for_update=True,
            )
            request_row = await conn.fetchrow(
                """
                SELECT * FROM agent_data_deletion_requests
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                  AND deletion_id = $3::uuid
                FOR UPDATE
                """,
                tenant_id,
                agent_id,
                deletion_id,
            )
            if not request_row:
                raise AgentRepositoryError("AGENT_DATA_DELETION_NOT_FOUND")
            request_data = _row_to_dict(request_row)
            if request_data["status"] not in {"pending", "failed"}:
                return request_data
            counts_snapshot = request_data.get("deleted_counts") or {}
            if isinstance(counts_snapshot, str):
                try:
                    counts_snapshot = json.loads(counts_snapshot)
                except json.JSONDecodeError as exc:
                    raise AgentRepositoryError(
                        "AGENT_DATA_DELETION_EXECUTION_STATE_INVALID"
                    ) from exc
            if not isinstance(counts_snapshot, dict):
                raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_STATE_INVALID")
            execution = counts_snapshot.get("cleanup_execution")
            if not isinstance(execution, dict):
                raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_CLAIM_INVALID")
            try:
                stored_generation = int(execution.get("generation") or 0)
                supplied_generation = int(execution_generation or 0)
            except (TypeError, ValueError) as exc:
                raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_CLAIM_INVALID") from exc
            supplied_token = str(execution_claim_token or "")
            supplied_digest = _agent_data_deletion_claim_digest(
                deletion_id=deletion_id,
                generation=supplied_generation,
                claim_token=supplied_token,
            )
            if (
                request_data["status"] != "pending"
                or execution.get("schema_version") != _DATA_DELETION_EXECUTION_SCHEMA
                or execution.get("state") != "claimed"
                or stored_generation < 1
                or supplied_generation != stored_generation
                or not supplied_token
                or not secrets.compare_digest(
                    str(execution.get("claim_digest") or ""), supplied_digest
                )
            ):
                raise AgentRepositoryError("AGENT_DATA_DELETION_EXECUTION_CLAIM_INVALID")
            policy_row = await conn.fetchrow(
                """
                SELECT legal_hold
                FROM agent_governance_policies
                WHERE tenant_id = $1 AND agent_id = $2::uuid
                FOR UPDATE
                """,
                tenant_id,
                agent_id,
            )
            if bool((policy_row or {}).get("legal_hold")):
                interrupted_counts = dict(counts_snapshot)
                interrupted_execution = dict(execution)
                interrupted_execution.pop("claim_digest", None)
                interrupted_execution.update(
                    {
                        "state": "interrupted",
                        "reason": "legal_hold_race_detected",
                    }
                )
                interrupted_counts["cleanup_execution"] = interrupted_execution
                row = await conn.fetchrow(
                    """
                    UPDATE agent_data_deletion_requests
                    SET status = 'failed',
                        error_code = 'AGENT_LEGAL_HOLD_RACE_DETECTED',
                        deleted_counts = $2::jsonb, completed_at = NOW()
                    WHERE deletion_id = $1::uuid
                    RETURNING *
                    """,
                    deletion_id,
                    json.dumps(interrupted_counts, sort_keys=True),
                )
                await self._audit(
                    conn,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    agent_id=agent_id,
                    action="data_deletion_failed",
                    summary={
                        "deletion_id": deletion_id,
                        "scope": request_data["scope"],
                        "stage": "commit_race_detected",
                    },
                )
                return _row_to_dict(row)
            if not storage_cleanup_succeeded:
                retry_counts = dict(counts_snapshot)
                retry_execution = dict(execution)
                retry_execution.pop("claim_digest", None)
                retry_execution.update({"state": "retryable", "reason": "storage_cleanup_failed"})
                retry_counts["cleanup_execution"] = retry_execution
                row = await conn.fetchrow(
                    """
                    UPDATE agent_data_deletion_requests
                    SET status = 'failed', error_code = 'AGENT_STORAGE_CLEANUP_FAILED',
                        deleted_counts = $2::jsonb, completed_at = NULL
                    WHERE deletion_id = $1::uuid
                    RETURNING *
                    """,
                    deletion_id,
                    json.dumps(retry_counts, sort_keys=True),
                )
                await self._audit(
                    conn,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    agent_id=agent_id,
                    action="data_deletion_failed",
                    summary={"deletion_id": deletion_id, "stage": "storage"},
                )
                return _row_to_dict(row)

            runtime_receipt: dict[str, Any] | None = None
            try:
                cleanup_plan = validate_runtime_cleanup_plan(
                    counts_snapshot.get("runtime_cleanup_plan")
                )
                cleanup_inventory = validate_runtime_cleanup_inventory(
                    counts_snapshot.get("runtime_cleanup_inventory"),
                    plan=cleanup_plan,
                )
                runtime_receipt = validate_runtime_cleanup_receipt(
                    runtime_cleanup_receipt,
                    plan=cleanup_plan,
                    inventory=cleanup_inventory,
                )
                runtime_cleanup_succeeded = cleanup_receipt_completed(runtime_receipt)
            except (TypeError, ValueError):
                runtime_cleanup_succeeded = False
            if not runtime_cleanup_succeeded:
                retry_counts = dict(counts_snapshot)
                retry_execution = dict(execution)
                retry_execution.pop("claim_digest", None)
                retry_execution.update(
                    {"state": "retryable", "reason": "runtime_cleanup_incomplete"}
                )
                retry_counts["cleanup_execution"] = retry_execution
                row = await conn.fetchrow(
                    """
                    UPDATE agent_data_deletion_requests
                    SET status = 'failed',
                        error_code = 'AGENT_RUNTIME_CLEANUP_FAILED',
                        deleted_counts = $2::jsonb, completed_at = NULL
                    WHERE deletion_id = $1::uuid
                    RETURNING *
                    """,
                    deletion_id,
                    json.dumps(retry_counts, sort_keys=True),
                )
                await self._audit(
                    conn,
                    tenant_id=tenant_id,
                    user_id=user_id,
                    agent_id=agent_id,
                    action="data_deletion_failed",
                    summary={"deletion_id": deletion_id, "stage": "runtime_memory"},
                )
                return _row_to_dict(row)

            scope = str(request_data["scope"])
            subject = request_data.get("subject_user_id")
            policy = counts_snapshot.get("policy") or {}
            trace_days = int(policy.get("trace_retention_days") or 90)
            keys = request_data.get("object_keys") or []
            if isinstance(keys, str):
                keys = json.loads(keys)

            cutoff_at = datetime.fromisoformat(str(cleanup_plan["cutoff_at"]))
            condition_params: list[Any] = [tenant_id, agent_id, cutoff_at]
            condition = " AND created_at <= $3"
            if scope == "user":
                condition_params.append(subject)
                condition += f" AND user_id = ${len(condition_params)}"

            session_condition = (
                condition.replace("created_at", "updated_at") if scope == "retention" else condition
            )
            session_rows = await conn.fetch(
                f"""
                SELECT session_id, user_id, agent_version_id, agent_draft_revision
                FROM assistant.sessions
                WHERE tenant_id = $1 AND agent_id = $2::uuid{session_condition}
                """,
                *condition_params,
            )
            session_ids = [str(item["session_id"]) for item in session_rows]
            memory_principals = set(cleanup_plan["principal_handles"])

            async def delete_count(sql: str, *params: Any) -> int:
                rows = await conn.fetch(sql, *params)
                return len(rows)

            deleted: dict[str, Any] = {}
            deleted["checkpoints"] = await delete_count(
                f"""
                DELETE FROM assistant_run_checkpoints
                WHERE tenant_id = $1 AND agent_id = $2::uuid{condition}
                RETURNING checkpoint_id
                """,
                *condition_params,
            )
            deleted["runs"] = await delete_count(
                f"""
                DELETE FROM assistant_runs
                WHERE tenant_id = $1 AND agent_id = $2::uuid{condition}
                RETURNING run_id
                """,
                *condition_params,
            )
            if session_ids:
                deleted["session_memory"] = await delete_count(
                    """
                    DELETE FROM session_memory
                    WHERE tenant_id = $1 AND session_id = ANY($2::varchar[])
                    RETURNING session_id
                    """,
                    tenant_id,
                    session_ids,
                )
            else:
                deleted["session_memory"] = 0
            if memory_principals:
                memory_ids = sorted(memory_principals)
                deleted["user_memory"] = await delete_count(
                    """
                    DELETE FROM user_memory
                    WHERE tenant_id = $1 AND user_id = ANY($2::varchar[])
                      AND updated_at <= $3
                    RETURNING key
                    """,
                    tenant_id,
                    memory_ids,
                    cutoff_at,
                )
                deleted["memory_sources"] = sum(
                    int(item.get("deleted_source_count") or 0)
                    for item in (runtime_receipt or {}).get("principals", [])
                )
                deleted["memory_reflections"] = await delete_count(
                    """
                    DELETE FROM assistant_memory_reflections
                    WHERE tenant_id = $1 AND user_id = ANY($2::varchar[])
                      AND updated_at <= $3
                    RETURNING reflection_id
                    """,
                    tenant_id,
                    memory_ids,
                    cutoff_at,
                )
            else:
                deleted["user_memory"] = 0
                deleted["memory_sources"] = 0
                deleted["memory_reflections"] = 0
            deleted["memory_vectors"] = sum(
                int(item.get("deleted_vector_count") or 0)
                for item in (runtime_receipt or {}).get("principals", [])
            )
            deleted["runtime_cleanup"] = {
                "status": "completed",
                "principal_count": len((runtime_receipt or {}).get("principals", [])),
                "plan_digest": cleanup_plan["plan_digest"],
                "inventory_digest": cleanup_inventory["inventory_digest"],
                "receipt_digest": (runtime_receipt or {}).get("receipt_digest"),
            }
            deleted["sessions"] = await delete_count(
                f"""
                DELETE FROM assistant.sessions
                WHERE tenant_id = $1 AND agent_id = $2::uuid{session_condition}
                RETURNING session_id
                """,
                *condition_params,
            )

            requested_at = request_data["requested_at"]
            if isinstance(requested_at, str):
                requested_at = datetime.fromisoformat(requested_at)
            trace_cutoff = requested_at
            if scope == "retention":
                trace_cutoff = requested_at - timedelta(days=trace_days)
            trace_params = [tenant_id, agent_id, trace_cutoff]
            trace_condition = " AND created_at <= $3"
            if scope == "user":
                trace_params.append(subject)
                trace_condition += f" AND user_id = ${len(trace_params)}"
            deleted["traces"] = await delete_count(
                f"""
                DELETE FROM agent_traces
                WHERE tenant_id = $1 AND agent_id = $2::uuid{trace_condition}
                RETURNING trace_id
                """,
                *trace_params,
            )

            principal_params: list[Any] = [tenant_id, agent_id, cutoff_at]
            principal_filter = " AND r.created_at <= $3"
            if scope == "user":
                principal_params.append(subject)
                principal_filter += f" AND r.principal_id = ${len(principal_params)}"
            for table, id_column in (
                ("agent_runtime_idempotency", "idempotency_key"),
                ("agent_runtime_feedback", "feedback_id"),
            ):
                deleted[table] = await delete_count(
                    f"""
                    DELETE FROM {table} r
                    USING agent_publications p
                    WHERE p.tenant_id = $1 AND p.agent_id = $2::uuid
                      AND r.tenant_id = p.tenant_id
                      AND r.publication_id = p.publication_id{principal_filter}
                    RETURNING r.{id_column}
                    """,
                    *principal_params,
                )
            if keys:
                deleted["attachments"] = await delete_count(
                    """
                    DELETE FROM agent_runtime_attachments a
                    USING agent_publications p
                    WHERE p.tenant_id = $1 AND p.agent_id = $2::uuid
                      AND a.tenant_id = p.tenant_id
                      AND a.publication_id = p.publication_id
                      AND a.storage_key = ANY($3::text[])
                    RETURNING a.attachment_id
                    """,
                    tenant_id,
                    agent_id,
                    [str(key) for key in keys],
                )
            else:
                deleted["attachments"] = 0

            if scope == "user":
                token_result = await conn.execute(
                    """
                    UPDATE agent_api_tokens t SET revoked_at = COALESCE(revoked_at, NOW())
                    FROM agent_publications p
                    WHERE p.tenant_id = $1 AND p.agent_id = $2::uuid
                      AND t.tenant_id = p.tenant_id AND t.publication_id = p.publication_id
                      AND t.created_by = $3 AND t.revoked_at IS NULL
                    """,
                    tenant_id,
                    agent_id,
                    subject,
                )
                await conn.execute(
                    """
                    UPDATE mcp_connections SET enabled = FALSE,
                        revoked_at = COALESCE(revoked_at, NOW()), updated_by = $3,
                        updated_at = NOW()
                    WHERE tenant_id = $1 AND owner_user_id = $2
                      AND enabled = TRUE
                    """,
                    tenant_id,
                    subject,
                    user_id,
                )
                await conn.execute(
                    """
                    UPDATE connector_credential_principals SET enabled = FALSE,
                        revoked_at = COALESCE(revoked_at, NOW()), updated_by = $3,
                        updated_at = NOW()
                    WHERE tenant_id = $1 AND owner_user_id = $2
                      AND enabled = TRUE
                    """,
                    tenant_id,
                    subject,
                    user_id,
                )
            elif scope == "tenant":
                token_result = await conn.execute(
                    """
                    UPDATE agent_api_tokens t SET revoked_at = COALESCE(revoked_at, NOW())
                    FROM agent_publications p
                    WHERE p.tenant_id = $1 AND p.agent_id = $2::uuid
                      AND t.tenant_id = p.tenant_id AND t.publication_id = p.publication_id
                      AND t.revoked_at IS NULL
                    """,
                    tenant_id,
                    agent_id,
                )
            else:
                token_result = "UPDATE 0"
            try:
                deleted["api_tokens_revoked"] = int(token_result.rsplit(" ", 1)[-1])
            except (TypeError, ValueError):
                deleted["api_tokens_revoked"] = 0

            cache_params: list[Any] = [tenant_id, agent_id, cutoff_at]
            cache_condition = " AND created_at <= $3"
            if scope == "user":
                cache_params.append(subject)
                cache_condition += (
                    f" AND (metadata->>'user_id' = ${len(cache_params)}"
                    f" OR metadata->>'principal_id' = ${len(cache_params)}"
                    f" OR metadata->>'caller_principal' = ${len(cache_params)})"
                )
            deleted["semantic_cache"] = await delete_count(
                f"""
                DELETE FROM semantic_cache
                WHERE metadata->>'tenant_id' = $1 AND metadata->>'agent_id' = $2
                  {cache_condition}
                RETURNING id
                """,
                *cache_params,
            )
            if scope == "tenant":
                deleted["publications_disabled"] = await delete_count(
                    """
                    UPDATE agent_publications
                    SET status = 'disabled', updated_by = $3, updated_at = NOW()
                    WHERE tenant_id = $1 AND agent_id = $2::uuid
                      AND status <> 'disabled'
                    RETURNING publication_id
                    """,
                    tenant_id,
                    agent_id,
                    user_id,
                )
                deleted["mcp_channel_grants_revoked"] = await delete_count(
                    """
                    UPDATE mcp_channel_grants g
                    SET enabled = FALSE, updated_at = NOW()
                    WHERE g.tenant_id = $1 AND g.enabled = TRUE
                      AND g.tool_id::text IN (
                          SELECT c.resource_id
                          FROM agent_version_capabilities c
                          JOIN agent_versions v
                            ON v.tenant_id = c.tenant_id
                           AND v.agent_version_id = c.agent_version_id
                          WHERE v.tenant_id = $1 AND v.agent_id = $2::uuid
                            AND c.capability_type = 'mcp'
                      )
                    RETURNING g.tool_id
                    """,
                    tenant_id,
                    agent_id,
                )
                deleted["connector_grants_revoked"] = await delete_count(
                    """
                    UPDATE connector_credential_principals g
                    SET enabled = FALSE, revoked_at = COALESCE(g.revoked_at, NOW()),
                        updated_by = $3, updated_at = NOW()
                    WHERE g.tenant_id = $1 AND g.enabled = TRUE
                      AND g.grant_id::text IN (
                          SELECT c.resource_id
                          FROM agent_version_capabilities c
                          JOIN agent_versions v
                            ON v.tenant_id = c.tenant_id
                           AND v.agent_version_id = c.agent_version_id
                          WHERE v.tenant_id = $1 AND v.agent_id = $2::uuid
                            AND c.capability_type = 'connector'
                      )
                    RETURNING g.grant_id
                    """,
                    tenant_id,
                    agent_id,
                    user_id,
                )
                deleted["draft_knowledge_bindings"] = await delete_count(
                    """
                    DELETE FROM agent_draft_knowledge_bindings b
                    USING agent_drafts d
                    WHERE d.tenant_id = $1 AND d.agent_id = $2::uuid
                      AND b.tenant_id = d.tenant_id AND b.draft_id = d.draft_id
                    RETURNING b.dataset_id
                    """,
                    tenant_id,
                    agent_id,
                )
                deleted["draft_skill_bindings"] = await delete_count(
                    """
                    DELETE FROM agent_draft_skill_bindings b
                    USING agent_drafts d
                    WHERE d.tenant_id = $1 AND d.agent_id = $2::uuid
                      AND b.tenant_id = d.tenant_id AND b.draft_id = d.draft_id
                    RETURNING b.skill_version_id
                    """,
                    tenant_id,
                    agent_id,
                )
                deleted["drafts_scrubbed"] = await delete_count(
                    """
                    UPDATE agent_drafts
                    SET spec = $3::jsonb, spec_hash = $4,
                        updated_by = $5, updated_at = NOW()
                    WHERE tenant_id = $1 AND agent_id = $2::uuid
                    RETURNING draft_id
                    """,
                    tenant_id,
                    agent_id,
                    canonical_spec({"deleted": True}),
                    hash_agent_spec({"deleted": True}),
                    user_id,
                )
                deleted["agents_disabled"] = await delete_count(
                    """
                    UPDATE agents
                    SET current_draft_id = NULL, status = 'deleted',
                        deleted_at = COALESCE(deleted_at, NOW()),
                        updated_by = $3, updated_at = NOW()
                    WHERE tenant_id = $1 AND agent_id = $2::uuid
                    RETURNING agent_id
                    """,
                    tenant_id,
                    agent_id,
                    user_id,
                )
                deleted["acl_members"] = await delete_count(
                    """
                    DELETE FROM agent_members
                    WHERE tenant_id = $1 AND agent_id = $2::uuid
                    RETURNING principal_id
                    """,
                    tenant_id,
                    agent_id,
                )
            deleted["cleanup_execution"] = {
                "schema_version": _DATA_DELETION_EXECUTION_SCHEMA,
                "state": "completed",
                "generation": stored_generation,
            }
            row = await conn.fetchrow(
                """
                UPDATE agent_data_deletion_requests
                SET status = 'completed', deleted_counts = $2::jsonb,
                    error_code = NULL, completed_at = NOW()
                WHERE deletion_id = $1::uuid
                RETURNING *
                """,
                deletion_id,
                json.dumps(deleted, sort_keys=True),
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=agent_id,
                action="data_deletion_completed",
                summary={"deletion_id": deletion_id, "scope": scope, "counts": deleted},
            )
        return _row_to_dict(row)

    async def create_api_token(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        user_id: str,
        name: str,
        scopes: list[str],
        expires_at: datetime | None,
    ) -> tuple[str, dict[str, Any]]:
        """Persist only a token hash and return raw material exactly once."""

        self._require_enabled()
        allowed_scopes = {
            "chat:write",
            "sessions:write",
            "attachments:write",
            "feedback:write",
        }
        scopes = sorted({str(scope) for scope in scopes})
        if not scopes or set(scopes) - allowed_scopes:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_TOKEN_SCOPE_INVALID")
        if expires_at is not None and expires_at <= datetime.now(expires_at.tzinfo):
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_TOKEN_EXPIRY_INVALID")
        raw_token = f"agt_{secrets.token_urlsafe(32)}"
        token_hash = hashlib.sha256(raw_token.encode("utf-8")).hexdigest()
        async with self._pool.acquire() as conn, conn.transaction():
            publication = await conn.fetchrow(
                """
                SELECT p.publication_id, p.agent_id, p.channel, p.status
                FROM agent_publications p
                JOIN agent_members m
                  ON m.tenant_id = p.tenant_id AND m.agent_id = p.agent_id
                WHERE p.tenant_id = $1 AND p.publication_id = $2
                  AND m.principal_type = 'user' AND m.principal_id = $3
                  AND m.role = 'owner'
                """,
                tenant_id,
                uuid.UUID(publication_id),
                user_id,
            )
            if not publication:
                raise AgentNotFoundError("AGENT_PUBLICATION_NOT_FOUND")
            if publication["channel"] != "api" or publication["status"] != "active":
                raise AgentRuntimeUnavailableError("PUBLICATION_CHANNEL_MISMATCH")
            row = await conn.fetchrow(
                """
                INSERT INTO agent_api_tokens (
                    tenant_id, publication_id, token_hash, name, scopes,
                    expires_at, created_by
                ) VALUES ($1, $2, $3, $4, $5, $6, $7)
                RETURNING tenant_id, token_id, publication_id, name, scopes,
                          expires_at, revoked_at, created_by, created_at
                """,
                tenant_id,
                uuid.UUID(publication_id),
                token_hash,
                name,
                scopes,
                expires_at,
                user_id,
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=publication["agent_id"],
                action="api_token_create",
                summary={
                    "publication_id": str(publication["publication_id"]),
                    "token_id": str(row["token_id"]),
                    "name": name,
                    "scopes": sorted(scopes),
                    "expires_at": expires_at.isoformat() if expires_at else None,
                },
                publication_id=publication["publication_id"],
                channel=str(publication["channel"]),
            )
        return raw_token, _row_to_dict(row)

    async def list_api_tokens(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        user_id: str,
    ) -> list[dict[str, Any]]:
        """List redacted token metadata for a Publication owner."""

        self._require_enabled()
        async with self._pool.acquire() as conn:
            owner = await conn.fetchval(
                """
                SELECT EXISTS (
                    SELECT 1
                    FROM agent_publications AS publication
                    JOIN agent_members AS member
                      ON member.tenant_id = publication.tenant_id
                     AND member.agent_id = publication.agent_id
                    WHERE publication.tenant_id = $1
                      AND publication.publication_id = $2
                      AND member.principal_type = 'user'
                      AND member.principal_id = $3
                      AND member.role = 'owner'
                )
                """,
                tenant_id,
                uuid.UUID(publication_id),
                user_id,
            )
            if not owner:
                raise AgentNotFoundError("AGENT_PUBLICATION_NOT_FOUND")
            rows = await conn.fetch(
                """
                SELECT tenant_id, token_id, publication_id, name, scopes,
                       expires_at, revoked_at, last_used_at,
                       rotated_from_token_id, created_by, created_at
                FROM agent_api_tokens
                WHERE tenant_id = $1 AND publication_id = $2
                ORDER BY created_at DESC
                """,
                tenant_id,
                uuid.UUID(publication_id),
            )
        return [_row_to_dict(row) for row in rows]

    async def revoke_api_token(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        token_id: str,
        user_id: str,
    ) -> dict[str, Any]:
        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            row = await conn.fetchrow(
                """
                UPDATE agent_api_tokens AS token
                SET revoked_at = COALESCE(token.revoked_at, NOW())
                FROM agent_publications AS publication
                JOIN agent_members AS member
                  ON member.tenant_id = publication.tenant_id
                 AND member.agent_id = publication.agent_id
                WHERE token.tenant_id = $1
                  AND token.publication_id = $2
                  AND token.token_id = $3
                  AND publication.tenant_id = token.tenant_id
                  AND publication.publication_id = token.publication_id
                  AND member.principal_type = 'user'
                  AND member.principal_id = $4
                  AND member.role = 'owner'
                RETURNING token.tenant_id, token.token_id, token.publication_id,
                          token.name, token.scopes, token.expires_at,
                          token.revoked_at, token.last_used_at,
                          token.rotated_from_token_id, token.created_by,
                          token.created_at, publication.agent_id, publication.channel
                """,
                tenant_id,
                uuid.UUID(publication_id),
                uuid.UUID(token_id),
                user_id,
            )
            if not row:
                raise AgentNotFoundError("AGENT_API_TOKEN_NOT_FOUND")
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=row["agent_id"],
                action="api_token_revoke",
                summary={"publication_id": publication_id, "token_id": token_id},
                publication_id=publication_id,
                channel=str(row["channel"]),
            )
        result = _row_to_dict(row)
        result.pop("agent_id", None)
        result.pop("channel", None)
        return result

    async def rotate_api_token(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        token_id: str,
        user_id: str,
        name: str | None,
        scopes: list[str] | None,
        expires_at: datetime | None,
    ) -> tuple[str, dict[str, Any]]:
        """Atomically revoke the old hash and issue replacement raw material once."""

        self._require_enabled()
        allowed_scopes = {
            "chat:write",
            "sessions:write",
            "attachments:write",
            "feedback:write",
        }
        raw_token = f"agt_{secrets.token_urlsafe(32)}"
        token_hash = hashlib.sha256(raw_token.encode("utf-8")).hexdigest()
        async with self._pool.acquire() as conn, conn.transaction():
            old = await conn.fetchrow(
                """
                SELECT token.*, publication.agent_id, publication.channel
                FROM agent_api_tokens AS token
                JOIN agent_publications AS publication
                  ON publication.tenant_id = token.tenant_id
                 AND publication.publication_id = token.publication_id
                JOIN agent_members AS member
                  ON member.tenant_id = publication.tenant_id
                 AND member.agent_id = publication.agent_id
                WHERE token.tenant_id = $1 AND token.publication_id = $2
                  AND token.token_id = $3
                  AND member.principal_type = 'user'
                  AND member.principal_id = $4 AND member.role = 'owner'
                FOR UPDATE OF token
                """,
                tenant_id,
                uuid.UUID(publication_id),
                uuid.UUID(token_id),
                user_id,
            )
            if not old or old["revoked_at"] is not None:
                raise AgentNotFoundError("AGENT_API_TOKEN_NOT_FOUND")
            replacement_scopes = sorted({str(scope) for scope in (scopes or old["scopes"])})
            if not replacement_scopes or set(replacement_scopes) - allowed_scopes:
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_TOKEN_SCOPE_INVALID")
            replacement_expiry = expires_at if expires_at is not None else old["expires_at"]
            if replacement_expiry is not None and replacement_expiry <= datetime.now(
                replacement_expiry.tzinfo
            ):
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_TOKEN_EXPIRY_INVALID")
            await conn.execute(
                "UPDATE agent_api_tokens SET revoked_at = NOW() WHERE tenant_id = $1 AND token_id = $2",
                tenant_id,
                old["token_id"],
            )
            row = await conn.fetchrow(
                """
                INSERT INTO agent_api_tokens (
                    tenant_id, publication_id, token_hash, name, scopes,
                    expires_at, created_by, rotated_from_token_id
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                RETURNING tenant_id, token_id, publication_id, name, scopes,
                          expires_at, revoked_at, last_used_at,
                          rotated_from_token_id, created_by, created_at
                """,
                tenant_id,
                old["publication_id"],
                token_hash,
                (name or str(old["name"]))[:255],
                replacement_scopes,
                replacement_expiry,
                user_id,
                old["token_id"],
            )
            await self._audit(
                conn,
                tenant_id=tenant_id,
                user_id=user_id,
                agent_id=old["agent_id"],
                action="api_token_rotate",
                summary={
                    "publication_id": publication_id,
                    "old_token_id": token_id,
                    "new_token_id": str(row["token_id"]),
                    "scopes": replacement_scopes,
                },
                publication_id=publication_id,
                channel=str(old["channel"]),
            )
        return raw_token, _row_to_dict(row)

    async def reserve_runtime_idempotency(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        principal_id: str,
        idempotency_key: str,
        request_hash: str,
        session_id: str,
    ) -> dict[str, Any]:
        """Atomically reserve execution or return its persisted terminal state."""

        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            row = await conn.fetchrow(
                """
                INSERT INTO agent_runtime_idempotency (
                    tenant_id, publication_id, principal_id,
                    idempotency_key, request_hash, session_id
                ) VALUES ($1, $2, $3, $4, $5, $6)
                ON CONFLICT (tenant_id, publication_id, principal_id, idempotency_key)
                DO UPDATE SET
                    request_hash = EXCLUDED.request_hash,
                    session_id = EXCLUDED.session_id,
                    status = 'pending',
                    response_body = NULL,
                    response_media_type = NULL,
                    response_status_code = NULL,
                    completed_at = NULL,
                    created_at = NOW(),
                    expires_at = NOW() + INTERVAL '24 hours'
                WHERE agent_runtime_idempotency.expires_at <= NOW()
                RETURNING session_id, status, response_body,
                          response_media_type, response_status_code
                """,
                tenant_id,
                uuid.UUID(publication_id),
                principal_id,
                idempotency_key,
                request_hash,
                session_id,
            )
            if row:
                return {
                    "created": True,
                    "session_id": str(row["session_id"]),
                    "status": "pending",
                    "response_body": None,
                    "response_media_type": None,
                    "response_status_code": None,
                }
            existing = await conn.fetchrow(
                """
                SELECT request_hash, session_id, status, response_body,
                       response_media_type, response_status_code
                FROM agent_runtime_idempotency
                WHERE tenant_id = $1 AND publication_id = $2
                  AND principal_id = $3 AND idempotency_key = $4
                  AND expires_at > NOW()
                """,
                tenant_id,
                uuid.UUID(publication_id),
                principal_id,
                idempotency_key,
            )
            if not existing or existing["request_hash"] != request_hash:
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_IDEMPOTENCY_CONFLICT")
            return {
                "created": False,
                "session_id": str(existing["session_id"]),
                "status": str(existing["status"]),
                "response_body": (
                    bytes(existing["response_body"])
                    if existing["response_body"] is not None
                    else None
                ),
                "response_media_type": existing["response_media_type"],
                "response_status_code": existing["response_status_code"],
            }

    async def complete_runtime_idempotency(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        principal_id: str,
        idempotency_key: str,
        request_hash: str,
        response_body: bytes,
        response_media_type: str,
        response_status_code: int,
    ) -> None:
        """Persist the exact terminal response before a retry may be replayed."""

        self._require_enabled()
        async with self._pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE agent_runtime_idempotency
                SET status = 'completed', response_body = $6,
                    response_media_type = $7, response_status_code = $8,
                    completed_at = NOW()
                WHERE tenant_id = $1 AND publication_id = $2
                  AND principal_id = $3 AND idempotency_key = $4
                  AND request_hash = $5 AND status = 'pending'
                  AND expires_at > NOW()
                """,
                tenant_id,
                uuid.UUID(publication_id),
                principal_id,
                idempotency_key,
                request_hash,
                response_body,
                response_media_type[:255],
                int(response_status_code),
            )
            if result != "UPDATE 1":
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_IDEMPOTENCY_STATE_INVALID")

    async def fail_runtime_idempotency(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        principal_id: str,
        idempotency_key: str,
        request_hash: str,
    ) -> None:
        """Seal an attempted execution so a retry cannot duplicate side effects."""

        self._require_enabled()
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                UPDATE agent_runtime_idempotency
                SET status = 'failed', completed_at = NOW()
                WHERE tenant_id = $1 AND publication_id = $2
                  AND principal_id = $3 AND idempotency_key = $4
                  AND request_hash = $5 AND status = 'pending'
                """,
                tenant_id,
                uuid.UUID(publication_id),
                principal_id,
                idempotency_key,
                request_hash,
            )

    async def create_runtime_attachment(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        principal_id: str,
        channel: str,
        storage_key: str,
        filename: str,
        mime_type: str,
        size_bytes: int,
    ) -> dict[str, Any]:
        """Persist a server-owned attachment handle scoped to one channel caller."""

        self._require_enabled()
        row = None
        storage_exceeded = False
        async with self._pool.acquire() as conn, conn.transaction():
            policy = await conn.fetchrow(
                """
                SELECT publication.agent_id,
                       COALESCE(governance.max_storage_bytes, 10737418240)::bigint
                           AS max_storage_bytes,
                       COALESCE(governance.alert_threshold_percent, 90)::int
                           AS alert_threshold_percent
                FROM agent_publications publication
                JOIN agents agent
                  ON agent.tenant_id = publication.tenant_id
                 AND agent.agent_id = publication.agent_id
                LEFT JOIN agent_governance_policies governance
                  ON governance.tenant_id = publication.tenant_id
                 AND governance.agent_id = publication.agent_id
                WHERE publication.tenant_id = $1
                  AND publication.publication_id = $2::uuid
                  AND publication.channel = $3
                  AND publication.status = 'active'
                  AND agent.deleted_at IS NULL
                FOR UPDATE OF agent
                """,
                tenant_id,
                publication_id,
                channel,
            )
            if not policy:
                raise AgentRuntimeUnavailableError("PUBLICATION_DISABLED")
            current_bytes = int(
                await conn.fetchval(
                    """
                    SELECT COALESCE(SUM(attachment.size_bytes), 0)::bigint
                    FROM agent_runtime_attachments attachment
                    JOIN agent_publications publication
                      ON publication.tenant_id = attachment.tenant_id
                     AND publication.publication_id = attachment.publication_id
                    WHERE publication.tenant_id = $1
                      AND publication.agent_id = $2::uuid
                      AND attachment.deleted_at IS NULL
                    """,
                    tenant_id,
                    policy["agent_id"],
                )
                or 0
            )
            requested_total = current_bytes + int(size_bytes)
            storage_limit = int(policy["max_storage_bytes"])
            threshold = int(policy["alert_threshold_percent"])
            if requested_total * 100 >= storage_limit * threshold:
                await self._write_governance_quota_alert(
                    conn,
                    tenant_id=tenant_id,
                    agent_id=str(policy["agent_id"]),
                    publication_id=publication_id,
                    quota="storage_bytes",
                    usage=requested_total,
                    limit=storage_limit,
                    threshold_percent=threshold,
                )
            storage_exceeded = requested_total > storage_limit
            if not storage_exceeded:
                row = await conn.fetchrow(
                    """
                    INSERT INTO agent_runtime_attachments (
                        tenant_id, publication_id, principal_id, channel,
                        storage_key, filename, mime_type, size_bytes
                    ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                    RETURNING attachment_id, publication_id, channel, filename,
                              mime_type, size_bytes, created_at, expires_at
                    """,
                    tenant_id,
                    uuid.UUID(publication_id),
                    principal_id,
                    channel,
                    storage_key,
                    filename[:255],
                    mime_type[:255],
                    int(size_bytes),
                )
        if storage_exceeded:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_STORAGE_QUOTA_EXCEEDED")
        if row is None:
            raise AgentRepositoryError("AGENT_RUNTIME_ATTACHMENT_PERSIST_FAILED")
        return _row_to_dict(row)

    async def resolve_runtime_attachments(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        principal_id: str,
        channel: str,
        attachment_ids: list[str],
    ) -> list[dict[str, Any]]:
        """Resolve opaque handles without accepting client-authored storage paths."""

        self._require_enabled()
        if not attachment_ids:
            return []
        try:
            parsed_ids = [uuid.UUID(value) for value in attachment_ids]
        except ValueError as exc:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_ATTACHMENT_NOT_FOUND") from exc
        if len(set(parsed_ids)) != len(parsed_ids):
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_ATTACHMENT_DUPLICATE")
        async with self._pool.acquire() as conn:
            rows = await conn.fetch(
                """
                SELECT attachment_id, storage_key, filename, mime_type, size_bytes
                FROM agent_runtime_attachments
                WHERE tenant_id = $1 AND publication_id = $2
                  AND principal_id = $3 AND channel = $4
                  AND attachment_id = ANY($5::uuid[]) AND expires_at > NOW()
                """,
                tenant_id,
                uuid.UUID(publication_id),
                principal_id,
                channel,
                parsed_ids,
            )
        by_id = {str(row["attachment_id"]): row for row in rows}
        if set(by_id) != {str(value) for value in parsed_ids}:
            raise AgentRuntimeUnavailableError("AGENT_RUNTIME_ATTACHMENT_NOT_FOUND")
        return [
            {
                "artifact_id": str(by_id[str(value)]["attachment_id"]),
                "filename": str(by_id[str(value)]["filename"]),
                "mime_type": str(by_id[str(value)]["mime_type"]),
                "file_path": "/" + str(by_id[str(value)]["storage_key"]).lstrip("/"),
            }
            for value in parsed_ids
        ]

    async def record_runtime_feedback(
        self,
        *,
        tenant_id: str,
        publication_id: str,
        agent_version_id: str,
        session_id: str,
        principal_id: str,
        channel: str,
        rating: int,
        comment: str,
    ) -> dict[str, Any]:
        """Persist minimal feedback only after proving the caller owns the pinned session."""

        self._require_enabled()
        async with self._pool.acquire() as conn, conn.transaction():
            owns_session = await conn.fetchval(
                """
                SELECT EXISTS (
                    SELECT 1 FROM assistant.sessions
                    WHERE session_id = $1 AND tenant_id = $2
                      AND publication_id = $3 AND agent_version_id = $4
                      AND user_id = $5 AND channel = $6 AND status = 'active'
                )
                """,
                session_id,
                tenant_id,
                uuid.UUID(publication_id),
                uuid.UUID(agent_version_id),
                principal_id,
                channel,
            )
            if not owns_session:
                raise AgentRuntimeUnavailableError("AGENT_RUNTIME_SESSION_NOT_FOUND")
            row = await conn.fetchrow(
                """
                INSERT INTO agent_runtime_feedback (
                    tenant_id, publication_id, agent_version_id, session_id,
                    principal_id, rating, comment, channel
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
                ON CONFLICT (tenant_id, publication_id, session_id, principal_id)
                DO UPDATE SET rating = EXCLUDED.rating, comment = EXCLUDED.comment
                RETURNING feedback_id, publication_id, agent_version_id,
                          session_id, rating, comment, channel, created_at
                """,
                tenant_id,
                uuid.UUID(publication_id),
                uuid.UUID(agent_version_id),
                session_id,
                principal_id,
                rating,
                comment,
                channel,
            )
        return _row_to_dict(row)


__all__ = [
    "AGENT_SPEC_SCHEMA_VERSION",
    "AgentArchivedError",
    "AgentDraftConflictError",
    "AgentLastOwnerError",
    "AgentNotFoundError",
    "AgentPrincipalNotFoundError",
    "AgentRepositoryError",
    "AgentRuntimeUnavailableError",
    "AgentValidationError",
    "DatabaseAgentRepository",
    "agent_spec_safety_errors",
    "canonical_spec",
    "hash_agent_spec",
    "redact_agent_spec_for_read",
    "sanitize_agent_copy_spec",
    "unsafe_agent_spec_paths",
    "validate_agent_spec",
]
