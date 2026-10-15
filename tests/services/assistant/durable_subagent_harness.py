"""Deterministic protocol oracle for durable sub-agent acceptance tests.

This module is deliberately test-only.  It does not make the production runtime
durable and its passing tests must never be reported as production evidence.
Instead it gives the production store/worker tests a precise state machine,
virtual clock, and crash points to compare against without waiting 30-120 real
minutes or invoking an LLM/provider.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import uuid
from copy import deepcopy
from dataclasses import dataclass, field
from enum import Enum
from typing import Any


class ProtocolError(RuntimeError):
    """Base error for a rejected durable-protocol transition."""


class IdempotencyConflict(ProtocolError):
    pass


class LeaseLost(ProtocolError):
    pass


class TerminalConflict(ProtocolError):
    pass


class ScopeViolation(ProtocolError):
    pass


class InvalidTransition(ProtocolError):
    pass


class EffectPolicy(str, Enum):
    READ_ONLY = "read_only"
    WRITE = "write"
    UNKNOWN = "unknown"


class OperationKind(str, Enum):
    READ = "read"
    WRITE = "write"
    UNKNOWN = "unknown"


class JobStatus(str, Enum):
    QUEUED = "queued"
    RUNNING = "running"
    COMPLETED = "completed"
    FAILED = "failed"
    BLOCKED = "blocked"
    CANCELLED = "cancelled"


TERMINAL_STATUSES = {
    JobStatus.COMPLETED,
    JobStatus.FAILED,
    JobStatus.CANCELLED,
}


@dataclass(frozen=True)
class VirtualClock:
    """Mutable virtual clock; ``advance`` never sleeps."""

    _state: list[float] = field(default_factory=lambda: [0.0])

    @property
    def now(self) -> float:
        return self._state[0]

    def advance(self, *, seconds: float = 0.0, minutes: float = 0.0) -> float:
        delta = seconds + minutes * 60
        if delta < 0:
            raise ValueError("virtual time cannot move backwards")
        self._state[0] += delta
        return self.now


@dataclass(frozen=True)
class Scope:
    tenant_id: str
    session_id: str
    run_id: str


@dataclass(frozen=True)
class Usage:
    input_tokens: int = 0
    output_tokens: int = 0
    usd_micros: int = 0

    @property
    def total_tokens(self) -> int:
        return self.input_tokens + self.output_tokens

    def __add__(self, other: Usage) -> Usage:
        return Usage(
            input_tokens=self.input_tokens + other.input_tokens,
            output_tokens=self.output_tokens + other.output_tokens,
            usd_micros=self.usd_micros + other.usd_micros,
        )


@dataclass(frozen=True)
class JobSpec:
    scope: Scope
    delegation_id: str
    task_id: str
    request_sha256: str
    effect_policy: EffectPolicy
    reserve_tokens: int
    reserve_usd_micros: int
    parent_task_id: str | None = None

    @property
    def key(self) -> tuple[str, str, str, str, str]:
        return (
            self.scope.tenant_id,
            self.scope.session_id,
            self.scope.run_id,
            self.delegation_id,
            self.task_id,
        )


@dataclass(frozen=True)
class Claim:
    key: tuple[str, str, str, str, str]
    worker_id: str
    attempt_id: str
    lease_token: str
    fencing_epoch: int
    version: int
    lease_expires_at: float
    event_cursor: int
    control_cursor: int


@dataclass(frozen=True)
class Event:
    seq: int
    event_id: str
    event_type: str
    payload: dict[str, Any]
    digest: str
    created_at: float


@dataclass(frozen=True)
class Control:
    seq: int
    control_id: str
    kind: str
    payload: dict[str, Any]
    created_at: float


@dataclass(frozen=True)
class Terminal:
    status: JobStatus
    result: dict[str, Any]
    usage: Usage
    digest: str
    event_seq: int


@dataclass(frozen=True)
class RecoveryCheckpoint:
    """Durable pause/read-back fact; deliberately not a completion terminal."""

    side_effect_state: str
    replay_allowed: bool
    recovery_action: str
    digest: str
    event_seq: int


@dataclass(frozen=True)
class Artifact:
    artifact_id: str
    tenant_id: str
    sha256: str
    size_bytes: int
    media_type: str
    content: bytes


@dataclass(frozen=True)
class HostToolEvidence:
    """Tool safety classification resolved by the host, not model/task input."""

    tool_call_id: str
    tool_name: str
    operation_kind: OperationKind
    definition_sha256: str


@dataclass(frozen=True)
class BudgetSnapshot:
    token_limit: int
    usd_limit_micros: int
    reserved_tokens: int
    reserved_usd_micros: int
    spent_tokens: int
    spent_usd_micros: int


@dataclass
class _Budget:
    token_limit: int
    usd_limit_micros: int
    reserved_tokens: int = 0
    reserved_usd_micros: int = 0
    spent_tokens: int = 0
    spent_usd_micros: int = 0


@dataclass
class _Job:
    spec: JobSpec
    status: JobStatus = JobStatus.QUEUED
    version: int = 0
    fencing_epoch: int = 0
    attempt_number: int = 0
    claim: Claim | None = None
    events: list[Event] = field(default_factory=list)
    controls: list[Control] = field(default_factory=list)
    terminal: Terminal | None = None
    recovery: RecoveryCheckpoint | None = None
    reservation_active: bool = False
    usage: Usage = field(default_factory=Usage)
    host_tool_evidence: list[HostToolEvidence] = field(default_factory=list)
    completion_outbox: list[dict[str, Any]] = field(default_factory=list)


def canonical_sha256(value: Any) -> str:
    encoded = json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode()
    return hashlib.sha256(encoded).hexdigest()


def make_spec(
    *,
    task_id: str,
    tenant_id: str = "tenant-a",
    session_id: str = "session-a",
    run_id: str = "run-a",
    delegation_id: str = "delegation-a",
    effect_policy: EffectPolicy = EffectPolicy.READ_ONLY,
    reserve_tokens: int = 1_000,
    reserve_usd_micros: int = 25_000,
    parent_task_id: str | None = None,
) -> JobSpec:
    intent = {
        "tenant_id": tenant_id,
        "session_id": session_id,
        "run_id": run_id,
        "delegation_id": delegation_id,
        "task_id": task_id,
        "effect_policy": effect_policy.value,
        "parent_task_id": parent_task_id,
    }
    return JobSpec(
        scope=Scope(tenant_id, session_id, run_id),
        delegation_id=delegation_id,
        task_id=task_id,
        request_sha256=canonical_sha256(intent),
        effect_policy=effect_policy,
        reserve_tokens=reserve_tokens,
        reserve_usd_micros=reserve_usd_micros,
        parent_task_id=parent_task_id,
    )


class DurableProtocolOracle:
    """Small, atomic reference state machine used only by acceptance tests."""

    def __init__(
        self,
        *,
        clock: VirtualClock,
        tenant_concurrency: int = 4,
        session_concurrency: int = 3,
        artifact_inline_limit: int = 16 * 1024,
    ) -> None:
        self.clock = clock
        self.tenant_concurrency = tenant_concurrency
        self.session_concurrency = session_concurrency
        self.artifact_inline_limit = artifact_inline_limit
        self._lock = asyncio.Lock()
        self._jobs: dict[tuple[str, str, str, str, str], _Job] = {}
        self._budgets: dict[str, _Budget] = {}
        self._artifacts: dict[str, Artifact] = {}

    async def configure_budget(
        self,
        tenant_id: str,
        *,
        token_limit: int,
        usd_limit_micros: int,
    ) -> None:
        async with self._lock:
            self._budgets[tenant_id] = _Budget(token_limit, usd_limit_micros)

    async def create_or_reuse(self, spec: JobSpec) -> tuple[dict[str, Any], bool]:
        async with self._lock:
            existing = self._jobs.get(spec.key)
            if existing is not None:
                if existing.spec.request_sha256 != spec.request_sha256:
                    raise IdempotencyConflict("same durable key has different intent")
                return self._snapshot(existing), False
            job = _Job(spec=spec)
            self._append_event(job, "queued", {"request_sha256": spec.request_sha256})
            self._jobs[spec.key] = job
            return self._snapshot(job), True

    async def claim_next(
        self,
        *,
        tenant_id: str,
        worker_id: str,
        lease_seconds: float,
        limit: int = 1,
    ) -> list[Claim]:
        async with self._lock:
            claimed: list[Claim] = []
            for key in sorted(self._jobs):
                if len(claimed) >= limit:
                    break
                job = self._jobs[key]
                if job.spec.scope.tenant_id != tenant_id or job.status is not JobStatus.QUEUED:
                    continue
                if not self._has_concurrency(job.spec.scope):
                    continue
                if not self._reserve_budget(job):
                    continue
                job.status = JobStatus.RUNNING
                job.attempt_number += 1
                job.fencing_epoch += 1
                job.version += 1
                claim = Claim(
                    key=key,
                    worker_id=worker_id,
                    attempt_id=f"attempt-{job.attempt_number}-{uuid.uuid4().hex}",
                    lease_token=uuid.uuid4().hex,
                    fencing_epoch=job.fencing_epoch,
                    version=job.version,
                    lease_expires_at=self.clock.now + lease_seconds,
                    event_cursor=len(job.events),
                    control_cursor=0,
                )
                job.claim = claim
                self._append_event(
                    job,
                    "claimed",
                    {
                        "attempt_id": claim.attempt_id,
                        "fencing_epoch": claim.fencing_epoch,
                        "worker_id": worker_id,
                    },
                )
                claimed.append(deepcopy(claim))
            return claimed

    async def heartbeat(
        self,
        claim: Claim,
        *,
        expected_version: int,
        lease_seconds: float,
    ) -> Claim:
        async with self._lock:
            job = self._require_claim(claim, expected_version)
            job.version += 1
            refreshed = Claim(
                **{
                    **claim.__dict__,
                    "version": job.version,
                    "lease_expires_at": self.clock.now + lease_seconds,
                    "event_cursor": len(job.events) + 1,
                }
            )
            job.claim = refreshed
            self._append_event(job, "heartbeat", {"attempt_id": claim.attempt_id})
            return deepcopy(refreshed)

    async def append_event(
        self,
        claim: Claim,
        *,
        expected_version: int,
        event_type: str,
        payload: dict[str, Any],
        event_id: str,
    ) -> tuple[Claim, Event]:
        async with self._lock:
            job = self._require_claim_identity(claim)
            digest = canonical_sha256({"event_type": event_type, "payload": payload})
            for existing in job.events:
                if existing.event_id != event_id:
                    continue
                if existing.digest != digest:
                    raise IdempotencyConflict("event id was reused with different content")
                return deepcopy(job.claim), deepcopy(existing)  # type: ignore[arg-type]
            if expected_version != job.version:
                raise LeaseLost("worker CAS version is stale")
            job.version += 1
            event = self._append_event(job, event_type, payload, event_id=event_id)
            refreshed = Claim(
                **{
                    **claim.__dict__,
                    "version": job.version,
                    "event_cursor": event.seq,
                }
            )
            job.claim = refreshed
            return deepcopy(refreshed), deepcopy(event)

    async def record_host_tool_start(
        self,
        claim: Claim,
        *,
        expected_version: int,
        evidence: HostToolEvidence,
    ) -> tuple[Claim, Event]:
        """Persist host-resolved operation evidence used by stale-lease recovery."""

        if len(evidence.definition_sha256) != 64:
            raise ValueError("host tool definition must be bound by a SHA-256 digest")
        async with self._lock:
            job = self._require_claim(claim, expected_version)
            existing = next(
                (
                    item
                    for item in job.host_tool_evidence
                    if item.tool_call_id == evidence.tool_call_id
                ),
                None,
            )
            if existing is not None and existing != evidence:
                raise IdempotencyConflict("tool call evidence changed after persistence")
            if existing is not None:
                event = next(
                    item
                    for item in job.events
                    if item.event_id == f"tool-start:{evidence.tool_call_id}"
                )
                return deepcopy(job.claim), deepcopy(event)  # type: ignore[arg-type]
            job.version += 1
            job.host_tool_evidence.append(evidence)
            event = self._append_event(
                job,
                "tool_started_host_attested",
                {
                    "tool_call_id": evidence.tool_call_id,
                    "tool_name": evidence.tool_name,
                    "operation_kind": evidence.operation_kind.value,
                    "definition_sha256": evidence.definition_sha256,
                },
                event_id=f"tool-start:{evidence.tool_call_id}",
            )
            refreshed = Claim(
                **{
                    **claim.__dict__,
                    "version": job.version,
                    "event_cursor": event.seq,
                }
            )
            job.claim = refreshed
            return deepcopy(refreshed), deepcopy(event)

    async def complete_terminal(
        self,
        claim: Claim,
        *,
        expected_version: int,
        status: JobStatus,
        result: dict[str, Any],
        usage: Usage,
    ) -> Terminal:
        if status not in TERMINAL_STATUSES:
            raise InvalidTransition(f"{status.value} is not terminal")
        async with self._lock:
            job = self._jobs.get(claim.key)
            if job is None:
                raise ScopeViolation("job does not exist in claim scope")
            projected = self._spill_result(job.spec.scope.tenant_id, result)
            digest = canonical_sha256(
                {"status": status.value, "result": projected, "usage": usage.__dict__}
            )
            if job.terminal is not None:
                if job.terminal.digest == digest:
                    return deepcopy(job.terminal)
                raise TerminalConflict("durable task already has a different terminal truth")
            self._require_claim(claim, expected_version)
            self._settle_budget(job, usage)
            job.version += 1
            event = self._append_event(
                job,
                "terminal",
                {"status": status.value, "result": projected, "usage": usage.__dict__},
                event_id=f"terminal:{claim.attempt_id}",
            )
            job.status = status
            job.usage = usage
            job.claim = None
            job.terminal = Terminal(status, projected, usage, digest, event.seq)
            job.completion_outbox.append(
                {
                    "outbox_id": f"complete:{job.spec.task_id}:{event.seq}",
                    "event_seq": event.seq,
                    "terminal_digest": digest,
                    "delivered": False,
                }
            )
            return deepcopy(job.terminal)

    async def reclaim_stale(self) -> list[tuple[str, JobStatus]]:
        async with self._lock:
            transitions: list[tuple[str, JobStatus]] = []
            for job in self._jobs.values():
                claim = job.claim
                if (
                    job.status is not JobStatus.RUNNING
                    or claim is None
                    or claim.lease_expires_at > self.clock.now
                ):
                    continue
                job.version += 1
                stale_attempt = claim.attempt_id
                job.claim = None
                self._settle_budget(job, None, conservative=True)
                observed_tool_starts = [
                    event for event in job.events if event.event_type.startswith("tool_started")
                ]
                replay_safe = not observed_tool_starts or (
                    len(observed_tool_starts) == len(job.host_tool_evidence)
                    and all(
                        event.event_type == "tool_started_host_attested"
                        for event in observed_tool_starts
                    )
                    and all(
                        evidence.operation_kind is OperationKind.READ
                        for evidence in job.host_tool_evidence
                    )
                )
                if replay_safe:
                    job.status = JobStatus.QUEUED
                    self._append_event(
                        job,
                        "lease_expired_requeued",
                        {"attempt_id": stale_attempt, "replay_allowed": True},
                    )
                else:
                    job.status = JobStatus.BLOCKED
                    has_write = any(
                        evidence.operation_kind is OperationKind.WRITE
                        for evidence in job.host_tool_evidence
                    )
                    recovery = "read_back_before_retry" if has_write else "manual_resolution"
                    event = self._append_event(
                        job,
                        "recovery_blocked",
                        {
                            "status": JobStatus.BLOCKED.value,
                            "attempt_id": stale_attempt,
                            "replay_allowed": False,
                            "recovery_action": recovery,
                        },
                    )
                    result = {
                        "side_effect_state": "unknown",
                        "replay_allowed": False,
                        "recovery_action": recovery,
                    }
                    digest = canonical_sha256(result)
                    job.recovery = RecoveryCheckpoint(
                        side_effect_state=result["side_effect_state"],
                        replay_allowed=result["replay_allowed"],
                        recovery_action=result["recovery_action"],
                        digest=digest,
                        event_seq=event.seq,
                    )
                transitions.append((job.spec.task_id, job.status))
            return transitions

    async def request_cancel(
        self,
        scope: Scope,
        task_id: str,
        *,
        control_id: str,
        reason: str,
    ) -> Control:
        async with self._lock:
            job = self._find_scoped(scope, task_id)
            existing = self._find_control(job, control_id, "cancel", {"reason": reason})
            if existing is not None:
                return deepcopy(existing)
            if job.terminal is not None:
                raise InvalidTransition("cannot cancel a terminal task")
            job.version += 1
            self._refresh_active_claim_version(job)
            control = self._append_control(job, control_id, "cancel", {"reason": reason})
            if job.status is JobStatus.QUEUED:
                event = self._append_event(
                    job,
                    "terminal",
                    {"status": JobStatus.CANCELLED.value, "reason": reason},
                )
                result = {"reason": reason, "before_execution": True}
                digest = canonical_sha256(
                    {
                        "status": JobStatus.CANCELLED.value,
                        "result": result,
                        "usage": Usage().__dict__,
                    }
                )
                job.status = JobStatus.CANCELLED
                job.terminal = Terminal(
                    JobStatus.CANCELLED,
                    result,
                    Usage(),
                    digest,
                    event.seq,
                )
            return deepcopy(control)

    async def request_steer(
        self,
        scope: Scope,
        task_id: str,
        *,
        control_id: str,
        instruction: str,
    ) -> Control:
        async with self._lock:
            job = self._find_scoped(scope, task_id)
            existing = self._find_control(
                job,
                control_id,
                "steer",
                {"instruction": instruction},
            )
            if existing is not None:
                return deepcopy(existing)
            if job.terminal is not None:
                raise InvalidTransition("cannot steer a terminal task")
            job.version += 1
            self._refresh_active_claim_version(job)
            return deepcopy(
                self._append_control(
                    job,
                    control_id,
                    "steer",
                    {"instruction": instruction},
                )
            )

    async def poll_controls(
        self,
        claim: Claim,
        *,
        after_seq: int,
    ) -> tuple[Claim, list[Control]]:
        async with self._lock:
            job = self._require_claim_identity(claim)
            active = job.claim
            if active is None:  # pragma: no cover - guarded by _require_claim_identity
                raise LeaseLost("claim disappeared")
            controls = [control for control in job.controls if control.seq > after_seq]
            return deepcopy(active), deepcopy(controls)

    async def acknowledge_control(
        self,
        claim: Claim,
        *,
        expected_version: int,
        control_seq: int,
        disposition: str,
    ) -> Claim:
        async with self._lock:
            job = self._require_claim_identity(claim)
            if not any(control.seq == control_seq for control in job.controls):
                raise InvalidTransition("cannot acknowledge an unknown control")
            existing_ack = next(
                (event for event in job.events if event.event_id == f"control-ack:{control_seq}"),
                None,
            )
            if existing_ack is not None:
                if existing_ack.payload.get("disposition") != disposition:
                    raise IdempotencyConflict("control acknowledgement changed")
                return deepcopy(job.claim)  # type: ignore[arg-type]
            if expected_version != job.version:
                raise LeaseLost("worker CAS version is stale")
            job.version += 1
            event = self._append_event(
                job,
                "control_acknowledged",
                {"control_seq": control_seq, "disposition": disposition},
                event_id=f"control-ack:{control_seq}",
            )
            refreshed = Claim(
                **{
                    **claim.__dict__,
                    "version": job.version,
                    "event_cursor": event.seq,
                    "control_cursor": control_seq,
                }
            )
            job.claim = refreshed
            return deepcopy(refreshed)

    async def fetch_events(
        self,
        scope: Scope,
        task_id: str,
        *,
        after_cursor: int,
    ) -> list[Event]:
        async with self._lock:
            job = self._find_scoped(scope, task_id)
            return deepcopy([event for event in job.events if event.seq > after_cursor])

    async def get(self, scope: Scope, task_id: str) -> dict[str, Any]:
        async with self._lock:
            return self._snapshot(self._find_scoped(scope, task_id))

    async def detach(self, scope: Scope, task_id: str) -> None:
        """A client transport detach must not mutate durable task state."""

        async with self._lock:
            self._find_scoped(scope, task_id)

    async def get_artifact(self, tenant_id: str, artifact_id: str) -> Artifact:
        async with self._lock:
            artifact = self._artifacts.get(artifact_id)
            if artifact is None or artifact.tenant_id != tenant_id:
                raise ScopeViolation("artifact is not visible in this tenant")
            return deepcopy(artifact)

    async def budget_snapshot(self, tenant_id: str) -> BudgetSnapshot:
        async with self._lock:
            budget = self._budgets[tenant_id]
            return BudgetSnapshot(
                token_limit=budget.token_limit,
                usd_limit_micros=budget.usd_limit_micros,
                reserved_tokens=budget.reserved_tokens,
                reserved_usd_micros=budget.reserved_usd_micros,
                spent_tokens=budget.spent_tokens,
                spent_usd_micros=budget.spent_usd_micros,
            )

    async def aggregate_child_usage(self, scope: Scope, parent_task_id: str) -> Usage:
        async with self._lock:
            scoped = [
                job
                for job in self._jobs.values()
                if job.spec.scope == scope and job.spec.parent_task_id == parent_task_id
            ]
            pending = [job.spec.task_id for job in scoped]
            descendants = {job.spec.task_id: job for job in scoped}
            while pending:
                ancestor = pending.pop()
                for job in self._jobs.values():
                    if (
                        job.spec.scope == scope
                        and job.spec.parent_task_id == ancestor
                        and job.spec.task_id not in descendants
                    ):
                        descendants[job.spec.task_id] = job
                        pending.append(job.spec.task_id)
            total = Usage()
            for job in descendants.values():
                total += job.usage
            return total

    def _has_concurrency(self, scope: Scope) -> bool:
        tenant_running = sum(
            job.status is JobStatus.RUNNING and job.spec.scope.tenant_id == scope.tenant_id
            for job in self._jobs.values()
        )
        session_running = sum(
            job.status is JobStatus.RUNNING
            and job.spec.scope.tenant_id == scope.tenant_id
            and job.spec.scope.session_id == scope.session_id
            for job in self._jobs.values()
        )
        return (
            tenant_running < self.tenant_concurrency and session_running < self.session_concurrency
        )

    def _reserve_budget(self, job: _Job) -> bool:
        budget = self._budgets.get(job.spec.scope.tenant_id)
        if budget is None:
            return True
        token_room = budget.token_limit - budget.spent_tokens - budget.reserved_tokens
        usd_room = budget.usd_limit_micros - budget.spent_usd_micros - budget.reserved_usd_micros
        if token_room < job.spec.reserve_tokens or usd_room < job.spec.reserve_usd_micros:
            return False
        budget.reserved_tokens += job.spec.reserve_tokens
        budget.reserved_usd_micros += job.spec.reserve_usd_micros
        job.reservation_active = True
        return True

    def _settle_budget(
        self,
        job: _Job,
        usage: Usage | None,
        *,
        conservative: bool = False,
    ) -> None:
        if not job.reservation_active:
            return
        budget = self._budgets.get(job.spec.scope.tenant_id)
        job.reservation_active = False
        if budget is None:
            return
        if (
            not conservative
            and usage is not None
            and (
                usage.total_tokens > job.spec.reserve_tokens
                or usage.usd_micros > job.spec.reserve_usd_micros
            )
        ):
            job.reservation_active = True
            raise InvalidTransition("actual usage exceeds the atomically reserved ceiling")
        budget.reserved_tokens -= job.spec.reserve_tokens
        budget.reserved_usd_micros -= job.spec.reserve_usd_micros
        if conservative:
            budget.spent_tokens += job.spec.reserve_tokens
            budget.spent_usd_micros += job.spec.reserve_usd_micros
        elif usage is not None:
            budget.spent_tokens += usage.total_tokens
            budget.spent_usd_micros += usage.usd_micros

    def _require_claim(self, claim: Claim, expected_version: int) -> _Job:
        job = self._require_claim_identity(claim)
        if expected_version != job.version:
            raise LeaseLost("worker CAS version is stale")
        return job

    def _require_claim_identity(self, claim: Claim) -> _Job:
        job = self._jobs.get(claim.key)
        if job is None:
            raise ScopeViolation("job does not exist in claim scope")
        active = job.claim
        if (
            job.status is not JobStatus.RUNNING
            or active is None
            or active.attempt_id != claim.attempt_id
            or active.lease_token != claim.lease_token
            or active.fencing_epoch != claim.fencing_epoch
            or active.worker_id != claim.worker_id
            or active.lease_expires_at <= self.clock.now
        ):
            raise LeaseLost("stale or expired worker lease")
        return job

    @staticmethod
    def _refresh_active_claim_version(job: _Job) -> None:
        if job.claim is not None:
            job.claim = Claim(**{**job.claim.__dict__, "version": job.version})

    def _find_scoped(self, scope: Scope, task_id: str) -> _Job:
        matches = [
            job
            for job in self._jobs.values()
            if job.spec.scope == scope and job.spec.task_id == task_id
        ]
        if len(matches) != 1:
            raise ScopeViolation("task is absent or outside the requested scope")
        return matches[0]

    def _append_event(
        self,
        job: _Job,
        event_type: str,
        payload: dict[str, Any],
        *,
        event_id: str | None = None,
    ) -> Event:
        normalized_payload = deepcopy(payload)
        digest = canonical_sha256({"event_type": event_type, "payload": normalized_payload})
        event = Event(
            len(job.events) + 1,
            event_id or f"internal:{len(job.events) + 1}:{digest[:16]}",
            event_type,
            normalized_payload,
            digest,
            self.clock.now,
        )
        job.events.append(event)
        return event

    def _append_control(
        self,
        job: _Job,
        control_id: str,
        kind: str,
        payload: dict[str, Any],
    ) -> Control:
        control = Control(
            len(job.controls) + 1,
            control_id,
            kind,
            deepcopy(payload),
            self.clock.now,
        )
        job.controls.append(control)
        return control

    @staticmethod
    def _find_control(
        job: _Job,
        control_id: str,
        kind: str,
        payload: dict[str, Any],
    ) -> Control | None:
        existing = next(
            (control for control in job.controls if control.control_id == control_id),
            None,
        )
        if existing is not None and (existing.kind != kind or existing.payload != payload):
            raise IdempotencyConflict("control id was reused with different intent")
        return existing

    def _spill_result(self, tenant_id: str, result: dict[str, Any]) -> dict[str, Any]:
        encoded = json.dumps(
            result,
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
        ).encode()
        if len(encoded) <= self.artifact_inline_limit:
            return deepcopy(result)
        digest = hashlib.sha256(encoded).hexdigest()
        artifact_id = "artifact:" + hashlib.sha256(f"{tenant_id}:{digest}".encode()).hexdigest()
        self._artifacts.setdefault(
            artifact_id,
            Artifact(
                artifact_id=artifact_id,
                tenant_id=tenant_id,
                sha256=digest,
                size_bytes=len(encoded),
                media_type="application/json",
                content=encoded,
            ),
        )
        return {
            "artifact_ref": {
                "artifact_id": artifact_id,
                "sha256": digest,
                "size_bytes": len(encoded),
                "media_type": "application/json",
            }
        }

    @staticmethod
    def _snapshot(job: _Job) -> dict[str, Any]:
        return deepcopy(
            {
                "spec": job.spec,
                "status": job.status,
                "version": job.version,
                "fencing_epoch": job.fencing_epoch,
                "attempt_number": job.attempt_number,
                "claim": job.claim,
                "terminal": job.terminal,
                "recovery": job.recovery,
                "host_tool_evidence": job.host_tool_evidence,
                "completion_outbox": job.completion_outbox,
            }
        )


class InjectedCrash(RuntimeError):
    pass


class FaultInjector:
    """One-shot named fault points for deterministic worker crash tests."""

    def __init__(self, *points: str) -> None:
        self._armed = set(points)

    def trip(self, point: str) -> None:
        if point in self._armed:
            self._armed.remove(point)
            raise InjectedCrash(point)


class RecordingPublisher:
    def __init__(self, injector: FaultInjector | None = None) -> None:
        self.events: list[Event] = []
        self.injector = injector or FaultInjector()

    async def publish(self, event: Event) -> None:
        self.injector.trip("after_terminal_commit_before_sse_publish")
        self.events.append(deepcopy(event))
