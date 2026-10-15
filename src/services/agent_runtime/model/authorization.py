"""Lease authorization and immutable snapshot authority."""

from __future__ import annotations

import json
import logging
import math
import re
import uuid
from collections.abc import Mapping
from dataclasses import dataclass
from datetime import datetime
from hashlib import sha256
from typing import Any, Protocol

from ai_gateway_contracts.agent_runtime import canonical_runtime_json
from ai_gateway_contracts.agent_runtime_lease import (
    RuntimeModelLeaseClaims,
    RuntimeModelLeaseError,
)

_TOOL_NAME_RE = re.compile(r"^[A-Za-z0-9_-]{1,64}$")


logger = logging.getLogger(__name__)

KERNEL_TOOL_TRANSCRIPT_NAMES = frozenset(
    {
        "update_plan",
        # Retired Python-loop alias. It may appear in a provider retry before
        # the model selects collaboration.spawn_agent. The Runtime still
        # rejects dispatch; the paired failure must remain replayable.
        "spawn_subagent",
        "spawn_agent",
        "send_input",
        "wait",
        "close_agent",
        "resume_agent",
        "send_message",
        "followup_task",
        "wait_agent",
        "list_agents",
        "interrupt_agent",
    }
)
KERNEL_TOOL_TRANSCRIPT_NAMESPACES = frozenset({"collaboration", "multi_agent_v1"})


def _is_unnamespaced(value: Any) -> bool:
    return value is None or value == ""


def _is_kernel_tool_identity(
    name: Any, namespace: Any = None, *, _helpers: Any = None
) -> bool:
    if not isinstance(name, str):
        return False
    is_unnamespaced = (
        _helpers._is_unnamespaced if _helpers is not None else _is_unnamespaced
    )
    if is_unnamespaced(namespace) and name in KERNEL_TOOL_TRANSCRIPT_NAMES:
        return True
    if (
        isinstance(namespace, str)
        and namespace in KERNEL_TOOL_TRANSCRIPT_NAMESPACES
        and name in KERNEL_TOOL_TRANSCRIPT_NAMES
    ):
        return True
    if not is_unnamespaced(namespace):
        return False
    return any(
        name == f"{prefix}{tool_name}"
        for prefix in KERNEL_TOOL_TRANSCRIPT_NAMESPACES
        for tool_name in KERNEL_TOOL_TRANSCRIPT_NAMES
    )


def _is_allowed_tool_identity(
    name: Any,
    namespace: Any,
    *,
    allowed_tool_names: set[str] | None,
    allowed_namespaced_tools: set[tuple[str, str]] | None,
    _helpers: Any = None,
) -> bool:
    if allowed_tool_names is None:
        return True
    is_kernel = (
        _helpers._is_kernel_tool_identity
        if _helpers is not None
        else _is_kernel_tool_identity
    )
    if is_kernel(name, namespace):
        return True
    if not isinstance(name, str):
        return False
    is_unnamespaced = (
        _helpers._is_unnamespaced if _helpers is not None else _is_unnamespaced
    )
    if is_unnamespaced(namespace):
        return name in allowed_tool_names
    return (
        isinstance(namespace, str)
        and allowed_namespaced_tools is not None
        and (namespace, name) in allowed_namespaced_tools
    )


class _Database(Protocol):
    async def fetchrow(self, query: str, *args): ...

    async def execute(self, query: str, *args): ...


class AgentModelPlaneError(RuntimeError):
    def __init__(self, code: str, *, status_code: int = 400) -> None:
        self.code = code
        self.status_code = status_code
        super().__init__(code)


def _runtime_snapshot(value: Any) -> dict[str, Any]:
    """Normalize asyncpg JSONB codecs without weakening snapshot validation."""

    if isinstance(value, str):
        try:
            value = json.loads(value)
        except json.JSONDecodeError:
            value = None
    if not isinstance(value, dict):
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
    return value


def _snapshot_parameters(snapshot: Mapping[str, Any]) -> dict[str, Any]:
    """Return provider parameters pinned by the immutable control snapshot."""

    raw = snapshot.get("parameters")
    if raw is None:
        return {}
    if not isinstance(raw, Mapping):
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
    temperature = raw.get("temperature")
    if temperature is None:
        return {}
    if (
        isinstance(temperature, bool)
        or not isinstance(temperature, int | float)
        or not math.isfinite(float(temperature))
        or not 0 <= float(temperature) <= 2
    ):
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
    return {"temperature": temperature}


def native_web_search_authorized(snapshot: Mapping[str, Any]) -> bool:
    readonly = snapshot.get("readonly_capabilities")
    if not isinstance(readonly, Mapping):
        return False
    if readonly.get("responses_tool_choice") == "none" or readonly.get("responses_tool_names") is not None:
        return False
    items = readonly.get("items")
    return isinstance(items, list) and any(
        isinstance(item, Mapping) and item.get("source") == "web-search"
        for item in items
    )


def _snapshot_responses_tool_controls(
    snapshot: Mapping[str, Any],
) -> tuple[set[str] | None, str | dict[str, str], bool]:
    """Read the immutable Responses tool policy pinned for this Runtime turn."""

    raw = snapshot.get("readonly_capabilities")
    if raw is None:
        return None, "auto", True
    if not isinstance(raw, Mapping):
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
    names_raw = raw.get("responses_tool_names")
    names: set[str] | None
    if names_raw is None:
        names = None
    elif (
        isinstance(names_raw, list)
        and len(names_raw) <= 128
        and all(isinstance(name, str) and _TOOL_NAME_RE.fullmatch(name) for name in names_raw)
        and len(set(names_raw)) == len(names_raw)
    ):
        names = set(names_raw)
    else:
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
    choice = raw.get("responses_tool_choice", "auto")
    if isinstance(choice, str) and choice in {"auto", "none", "required"}:
        normalized_choice: str | dict[str, str] = choice
    elif (
        isinstance(choice, Mapping)
        and choice.get("type") == "function"
        and isinstance(choice.get("name"), str)
        and _TOOL_NAME_RE.fullmatch(choice["name"])
    ):
        normalized_choice = {"type": "function", "name": choice["name"]}
    else:
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
    if isinstance(normalized_choice, dict) and (
        names is None or normalized_choice["name"] not in names
    ):
        raise AgentModelPlaneError("RUNTIME_TOOL_CHOICE_INVALID", status_code=422)
    parallel = raw.get("responses_parallel_tool_calls", True)
    if not isinstance(parallel, bool):
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_INVALID", status_code=503)
    if normalized_choice == "required" and names == set():
        raise AgentModelPlaneError("RUNTIME_TOOL_CHOICE_INVALID", status_code=422)
    return names, normalized_choice, parallel


@dataclass(frozen=True, slots=True)
class _AuthorizedCall:
    call_id: uuid.UUID
    lease_id: uuid.UUID
    run_id: uuid.UUID
    tenant_id: str
    user_id: str
    session_id: str
    model_id: str
    provider_id: str
    provider_revision: str
    snapshot: dict[str, Any]
    estimated_input_tokens: int
    reserved_output_tokens: int


def _timestamp_ms(value: datetime) -> int:
    if not isinstance(value, datetime) or value.tzinfo is None:
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_TIME_INVALID", status_code=503)
    return int(value.timestamp() * 1000)


def _provider_revision(value: Any) -> str:
    if isinstance(value, datetime):
        return value.isoformat()
    return str(value or "")


def _runtime_scope_sha256(tenant_id: str, user_id: str, session_id: str) -> str:
    digest = sha256()
    for value in (tenant_id, user_id, session_id):
        encoded = value.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "big"))
        digest.update(encoded)
    return digest.hexdigest()


def _estimate_tokens(
    value: Any, *, _canonical_runtime_json: Any = canonical_runtime_json
) -> int:
    encoded = _canonical_runtime_json(value)
    return max(1, math.ceil(len(encoded.encode("utf-8")) / 4))



async def _validate_turn_thread_scope(
    self,
    *,
    claims: RuntimeModelLeaseClaims,
    turn_metadata: Mapping[str, Any],
    _helpers: Any,
) -> None:
    """Bind model calls from root and sub-agent turns to one lease.

    A child Responses turn has its own thread/turn identifiers.  It is
    authorized only when the Runtime supplies the root turn plus the
    immediate parent, and the immutable membership row proves that the
    child belongs to the leased root thread and principal scope.
    """

    thread_id = str(turn_metadata.get("thread_id") or "")
    turn_id = str(turn_metadata.get("turn_id") or "")
    root_turn_id = str(turn_metadata.get("root_turn_id") or "")
    parent_thread_id = str(turn_metadata.get("parent_thread_id") or "")
    parent_turn_id = str(turn_metadata.get("parent_turn_id") or "")
    if thread_id == claims.runtime_thread_id:
        expected_metadata = {
            "thread_id": claims.runtime_thread_id,
            "turn_id": claims.run_id,
            "ai_platform_scope_sha256": _helpers._runtime_scope_sha256(
                claims.tenant_id,
                claims.user_id,
                claims.session_id,
            ),
        }
        if any(
            str(turn_metadata.get(key) or "") != value
            for key, value in expected_metadata.items()
        ):
            raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_SCOPE_MISMATCH", status_code=403)
        if root_turn_id not in {"", claims.run_id} or parent_thread_id or parent_turn_id:
            raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_SCOPE_MISMATCH", status_code=403)
        return

    if (
        root_turn_id != claims.run_id
        or not thread_id
        or not turn_id
        or thread_id == claims.runtime_thread_id
        or not parent_thread_id
        or parent_thread_id == thread_id
    ):
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_SCOPE_MISMATCH", status_code=403)
    if str(
        turn_metadata.get("ai_platform_scope_sha256") or ""
    ) != _helpers._runtime_scope_sha256(
        claims.tenant_id,
        claims.user_id,
        claims.session_id,
    ):
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_SCOPE_MISMATCH", status_code=403)
    try:
        child_thread_uuid = uuid.UUID(thread_id)
        root_thread_uuid = uuid.UUID(claims.runtime_thread_id)
        parent_thread_uuid = uuid.UUID(parent_thread_id)
        uuid.UUID(turn_id)
    except (ValueError, TypeError, AttributeError):
        raise AgentModelPlaneError(
            "RUNTIME_MODEL_LEASE_SCOPE_MISMATCH", status_code=403
        ) from None
    member = await self.database.fetchrow(
        """
        SELECT parent_kernel_thread_id, relation_kind
          FROM assistant_runtime_thread_members
         WHERE kernel_thread_id = $1
           AND runtime_thread_id = $2
           AND tenant_id = $3
           AND user_id = $4
           AND session_id = $5
        """,
        child_thread_uuid,
        root_thread_uuid,
        claims.tenant_id,
        claims.user_id,
        claims.session_id,
    )
    if not member:
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_SCOPE_MISMATCH", status_code=403)
    relation_kind = str(member.get("relation_kind") or "")
    stored_parent = member.get("parent_kernel_thread_id")
    if relation_kind != "subagent" or stored_parent is None:
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_SCOPE_MISMATCH", status_code=403)
    try:
        stored_parent_uuid = uuid.UUID(str(stored_parent))
    except (ValueError, TypeError, AttributeError):
        raise AgentModelPlaneError(
            "RUNTIME_MODEL_LEASE_SCOPE_MISMATCH", status_code=403
        ) from None
    if stored_parent_uuid != parent_thread_uuid:
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_SCOPE_MISMATCH", status_code=403)

async def authorize_and_reserve(
    self,
    *,
    body: dict[str, Any],
    turn_metadata: dict[str, Any],
    _helpers: Any,
) -> _AuthorizedCall:
    lease_id_raw = turn_metadata.get("ai_platform_lease_id")
    signature = turn_metadata.get("ai_platform_lease_signature")
    try:
        lease_id = uuid.UUID(str(lease_id_raw))
    except (ValueError, TypeError, AttributeError):
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_INVALID", status_code=401) from None
    row = await self.database.fetchrow(
        """
        SELECT l.*, s.snapshot, s.snapshot_sha256, o.owner_id AS execution_owner_id, o.fence AS execution_fence
          FROM assistant_runtime_model_leases AS l
          JOIN assistant_runtime_snapshots AS s
            ON s.snapshot_id = l.snapshot_id
           AND s.run_id = l.run_id
           AND s.tenant_id = l.tenant_id
           AND s.user_id = l.user_id
           AND s.session_id = l.session_id
          JOIN assistant_runs AS run
            ON run.run_id = l.run_id
          LEFT JOIN assistant_runtime_execution_owners AS o ON o.run_id = run.run_id
         WHERE l.lease_id = $1
           AND (o.run_id IS NULL OR o.lease_until > NOW())
           AND l.status = 'active'
           AND l.expires_at > NOW()
           AND run.status = 'running'
           AND run.engine = 'agent_runtime'
           AND NOT EXISTS (
               SELECT 1
                 FROM assistant_runtime_snapshot_revocations AS revoked
                WHERE revoked.snapshot_id = l.snapshot_id
           )
        """,
        lease_id,
    )
    if row is None:
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_NOT_FOUND", status_code=401)
    data = dict(row)
    execution_owner = data.get("execution_owner_id")
    execution_fence = data.get("execution_fence")
    if execution_owner is not None and (
        str(turn_metadata.get("ai_platform_execution_owner") or "") != str(execution_owner)
        or str(turn_metadata.get("ai_platform_execution_fence") or "") != str(execution_fence)
    ):
        raise AgentModelPlaneError("RUNTIME_EXECUTION_FENCE_LOST", status_code=403)
    claims = RuntimeModelLeaseClaims(
        schema_version=str(data["schema_version"]),
        lease_id=str(data["lease_id"]),
        snapshot_id=str(data["snapshot_id"]),
        run_id=str(data["run_id"]),
        runtime_thread_id=str(data["runtime_thread_id"]),
        tenant_id=str(data["tenant_id"]),
        user_id=str(data["user_id"]),
        session_id=str(data["session_id"]),
        provider_id=str(data["provider_id"]),
        model_id=str(data["model_id"]),
        capability_revision=int(data["capability_revision"]),
        issued_at_ms=_helpers._timestamp_ms(data["issued_at"]),
        expires_at_ms=_helpers._timestamp_ms(data["expires_at"]),
        nonce_sha256=str(data["nonce_sha256"]),
    )
    try:
        self.lease_signer.verify(str(signature or ""), claims)
    except RuntimeModelLeaseError as exc:
        raise AgentModelPlaneError(exc.code, status_code=401) from None

    await self._validate_turn_thread_scope(claims=claims, turn_metadata=turn_metadata)
    if str(body.get("model") or "") != claims.model_id:
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_MODEL_MISMATCH", status_code=403)
    snapshot = _helpers._runtime_snapshot(data.get("snapshot"))
    snapshot_hash = sha256(_helpers.canonical_runtime_json(snapshot).encode()).hexdigest()
    if snapshot_hash != str(data["snapshot_sha256"]):
        raise AgentModelPlaneError("RUNTIME_MODEL_SNAPSHOT_HASH_MISMATCH", status_code=503)

    checker = getattr(self, "source_access_checker", None)
    if checker is not None:
        try:
            allowed = await checker(
                tenant_id=claims.tenant_id, user_id=claims.user_id,
                session_id=claims.session_id, run_id=claims.run_id,
            )
        except Exception:
            raise AgentModelPlaneError("ASSISTANT_SOURCE_CHECK_UNAVAILABLE", status_code=503) from None
        if not allowed:
            raise AgentModelPlaneError("ASSISTANT_SOURCE_ACCESS_REVOKED", status_code=403)

    estimated_input = _helpers._estimate_tokens(body.get("input"))
    limits = snapshot.get("limits") if isinstance(snapshot.get("limits"), dict) else {}
    requested_output = body.get("max_output_tokens")
    if not isinstance(requested_output, int) or isinstance(requested_output, bool):
        requested_output = int(limits.get("max_output_tokens") or 4096)
    requested_output = max(1, requested_output)
    pricing = snapshot.get("pricing") if isinstance(snapshot.get("pricing"), dict) else {}
    reserved_cost = _helpers._cost_microusd(
        estimated_input,
        requested_output,
        input_price_per_1k=float(pricing.get("input_price_per_1k") or 0),
        output_price_per_1k=float(pricing.get("output_price_per_1k") or 0),
    )
    call_id = uuid.uuid4()
    # A recovery generation can retry an interrupted model request under the
    # original remaining budget. Replays within one generation stay rejected.
    fingerprint = body if execution_owner is None else {"body": body, "execution_fence": execution_fence}
    request_hash = sha256(_helpers.canonical_runtime_json(fingerprint).encode()).hexdigest()
    try:
        if execution_owner is not None:
            await self.database.fetchrow(
                "SELECT reserve_assistant_runtime_model_call($1,$2,$3,$4,$5,$6,$7,$8)",
                call_id, lease_id, request_hash, estimated_input, requested_output,
                reserved_cost, execution_owner, execution_fence,
            )
        else:
            await self.database.fetchrow(
                "SELECT reserve_assistant_runtime_model_call($1, $2, $3, $4, $5, $6)",
                call_id, lease_id, request_hash, estimated_input, requested_output, reserved_cost,
            )
    except Exception as exc:
        code = str(exc)
        if "MODEL_CALL_REPLAYED" in code:
            raise AgentModelPlaneError("RUNTIME_MODEL_CALL_REPLAYED", status_code=409) from None
        if "BUDGET_EXHAUSTED" in code:
            raise AgentModelPlaneError(
                "RUNTIME_MODEL_LEASE_BUDGET_EXHAUSTED", status_code=429
            ) from None
        raise AgentModelPlaneError("RUNTIME_MODEL_LEASE_REJECTED", status_code=403) from None
    return _helpers._AuthorizedCall(
        call_id=call_id,
        lease_id=lease_id,
        run_id=uuid.UUID(claims.run_id),
        tenant_id=claims.tenant_id,
        user_id=claims.user_id,
        session_id=claims.session_id,
        model_id=claims.model_id,
        provider_id=claims.provider_id,
        provider_revision=str(data["provider_revision"]),
        snapshot=snapshot,
        estimated_input_tokens=estimated_input,
        reserved_output_tokens=requested_output,
    )
