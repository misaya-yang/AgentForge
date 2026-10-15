"""Deterministic long-running sub-agent protocol acceptance scenarios.

Passing this file proves the fault-injection harness and target protocol are
self-consistent.  It is *not* evidence that the production Assistant is wired
to the durable store; production evidence lives in the separate contract file.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import time

import pytest

from .durable_subagent_harness import (
    DurableProtocolOracle,
    EffectPolicy,
    FaultInjector,
    HostToolEvidence,
    IdempotencyConflict,
    InjectedCrash,
    InvalidTransition,
    JobStatus,
    LeaseLost,
    OperationKind,
    RecordingPublisher,
    Scope,
    ScopeViolation,
    TerminalConflict,
    Usage,
    VirtualClock,
    make_spec,
)


async def _enqueue_and_claim(
    store: DurableProtocolOracle,
    *,
    task_id: str,
    worker_id: str = "worker-a",
    lease_seconds: float = 60,
    **spec_values: object,
):
    spec = make_spec(task_id=task_id, **spec_values)
    await store.create_or_reuse(spec)
    claims = await store.claim_next(
        tenant_id=spec.scope.tenant_id,
        worker_id=worker_id,
        lease_seconds=lease_seconds,
    )
    assert len(claims) == 1
    return spec, claims[0]


@pytest.mark.asyncio
async def test_two_workers_atomically_claim_same_durable_task_once() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec = make_spec(task_id="legal-analysis-1")
    await store.create_or_reuse(spec)

    winners = await asyncio.gather(
        store.claim_next(tenant_id="tenant-a", worker_id="worker-a", lease_seconds=60),
        store.claim_next(tenant_id="tenant-a", worker_id="worker-b", lease_seconds=60),
    )

    claims = [claim for result in winners for claim in result]
    assert len(claims) == 1
    snapshot = await store.get(spec.scope, spec.task_id)
    assert snapshot["status"] is JobStatus.RUNNING
    assert snapshot["attempt_number"] == 1
    assert snapshot["claim"].worker_id in {"worker-a", "worker-b"}


@pytest.mark.asyncio
async def test_read_only_crash_is_reclaimed_with_new_fencing_epoch() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec, first = await _enqueue_and_claim(
        store,
        task_id="sec-filings-read-only",
        lease_seconds=30,
        effect_policy=EffectPolicy.UNKNOWN,
    )
    first, _ = await store.record_host_tool_start(
        first,
        expected_version=first.version,
        evidence=HostToolEvidence(
            tool_call_id="read-sec-10q",
            tool_name="read_source_packet",
            operation_kind=OperationKind.READ,
            definition_sha256="a" * 64,
        ),
    )

    clock.advance(seconds=31)
    assert await store.reclaim_stale() == [(spec.task_id, JobStatus.QUEUED)]
    second = (await store.claim_next(tenant_id="tenant-a", worker_id="worker-b", lease_seconds=30))[
        0
    ]

    assert second.attempt_id != first.attempt_id
    assert second.fencing_epoch == first.fencing_epoch + 1
    with pytest.raises(LeaseLost):
        await store.complete_terminal(
            first,
            expected_version=first.version,
            status=JobStatus.COMPLETED,
            result={"stale": True},
            usage=Usage(),
        )
    events = await store.fetch_events(spec.scope, spec.task_id, after_cursor=0)
    assert [event.event_type for event in events].count("claimed") == 2
    assert any(event.event_type == "lease_expired_requeued" for event in events)


@pytest.mark.parametrize(
    ("operation_kind", "expected_recovery"),
    [
        (OperationKind.WRITE, "read_back_before_retry"),
        (OperationKind.UNKNOWN, "manual_resolution"),
    ],
)
@pytest.mark.asyncio
async def test_write_or_unknown_crash_is_blocked_and_never_blindly_replayed(
    operation_kind: OperationKind,
    expected_recovery: str,
) -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec, claim = await _enqueue_and_claim(
        store,
        task_id=f"external-effect-{operation_kind.value}",
        lease_seconds=20,
        effect_policy=EffectPolicy.READ_ONLY,
    )
    claim, _ = await store.record_host_tool_start(
        claim,
        expected_version=claim.version,
        evidence=HostToolEvidence(
            tool_call_id="external-call",
            tool_name="external_operation",
            operation_kind=operation_kind,
            definition_sha256="b" * 64,
        ),
    )

    clock.advance(seconds=21)
    assert await store.reclaim_stale() == [(spec.task_id, JobStatus.BLOCKED)]
    assert (
        await store.claim_next(tenant_id="tenant-a", worker_id="worker-b", lease_seconds=20) == []
    )
    snapshot = await store.get(spec.scope, spec.task_id)
    assert snapshot["terminal"] is None
    assert snapshot["recovery"].side_effect_state == "unknown"
    assert snapshot["recovery"].replay_allowed is False
    assert snapshot["recovery"].recovery_action == expected_recovery
    events = await store.fetch_events(spec.scope, spec.task_id, after_cursor=0)
    assert events[-1].event_type == "recovery_blocked"


@pytest.mark.asyncio
async def test_model_cannot_forge_read_only_evidence_to_enable_replay() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec, claim = await _enqueue_and_claim(
        store,
        task_id="forged-read-claim",
        lease_seconds=10,
        effect_policy=EffectPolicy.READ_ONLY,
    )
    claim, _ = await store.append_event(
        claim,
        expected_version=claim.version,
        event_id="model-event-1",
        event_type="tool_started_untrusted",
        payload={"operation_kind": "read", "host_attested": True},
    )

    clock.advance(seconds=11)
    await store.reclaim_stale()
    snapshot = await store.get(spec.scope, spec.task_id)
    assert snapshot["status"] is JobStatus.BLOCKED
    assert snapshot["terminal"] is None
    assert snapshot["recovery"].recovery_action == "manual_resolution"


@pytest.mark.asyncio
async def test_event_and_terminal_writes_are_exactly_once_under_retries_and_races() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec, claim = await _enqueue_and_claim(store, task_id="terminal-cas")
    stale_before_append = claim
    claim, first_event = await store.append_event(
        claim,
        expected_version=claim.version,
        event_id="model-chunk-1",
        event_type="progress",
        payload={"section": "risk analysis"},
    )
    same_claim, same_event = await store.append_event(
        stale_before_append,
        expected_version=stale_before_append.version,
        event_id="model-chunk-1",
        event_type="progress",
        payload={"section": "risk analysis"},
    )
    assert (same_event.seq, same_event.digest) == (first_event.seq, first_event.digest)
    assert same_claim.version == claim.version
    with pytest.raises(IdempotencyConflict):
        await store.append_event(
            claim,
            expected_version=claim.version,
            event_id="model-chunk-1",
            event_type="progress",
            payload={"section": "tampered"},
        )

    async def finish(result: dict[str, object]):
        return await store.complete_terminal(
            claim,
            expected_version=claim.version,
            status=JobStatus.COMPLETED,
            result=result,
            usage=Usage(input_tokens=100, output_tokens=40, usd_micros=500),
        )

    raced = await asyncio.gather(
        finish({"answer": "version-a"}),
        finish({"answer": "version-b"}),
        return_exceptions=True,
    )
    terminals = [value for value in raced if not isinstance(value, BaseException)]
    conflicts = [value for value in raced if isinstance(value, TerminalConflict)]
    assert len(terminals) == len(conflicts) == 1
    winning = terminals[0]
    assert await finish(winning.result) == winning
    snapshot = await store.get(spec.scope, spec.task_id)
    assert len(snapshot["completion_outbox"]) == 1
    events = await store.fetch_events(spec.scope, spec.task_id, after_cursor=0)
    assert [event.event_type for event in events].count("terminal") == 1


@pytest.mark.asyncio
async def test_blocked_recovery_checkpoint_cannot_masquerade_as_terminal() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    _spec, claim = await _enqueue_and_claim(store, task_id="recovery-is-not-terminal")

    with pytest.raises(InvalidTransition, match="not terminal"):
        await store.complete_terminal(
            claim,
            expected_version=claim.version,
            status=JobStatus.BLOCKED,
            result={"recovery_action": "read_back_before_retry"},
            usage=Usage(),
        )


@pytest.mark.asyncio
async def test_terminal_commit_survives_crash_before_sse_and_replays_from_cursor() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec, claim = await _enqueue_and_claim(store, task_id="sse-outbox")
    before_terminal = (await store.fetch_events(spec.scope, spec.task_id, after_cursor=0))[-1].seq
    terminal = await store.complete_terminal(
        claim,
        expected_version=claim.version,
        status=JobStatus.COMPLETED,
        result={"memo": "persisted before delivery"},
        usage=Usage(input_tokens=80, output_tokens=20, usd_micros=400),
    )
    persisted = (await store.fetch_events(spec.scope, spec.task_id, after_cursor=before_terminal))[
        0
    ]
    assert persisted.seq == terminal.event_seq

    publisher = RecordingPublisher(FaultInjector("after_terminal_commit_before_sse_publish"))
    with pytest.raises(InjectedCrash):
        await publisher.publish(persisted)
    assert publisher.events == []

    replay = await store.fetch_events(spec.scope, spec.task_id, after_cursor=before_terminal)
    assert replay == [persisted]
    await publisher.publish(replay[0])
    assert publisher.events == [persisted]


@pytest.mark.asyncio
async def test_client_disconnect_only_detaches_and_does_not_cancel_worker() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec, claim = await _enqueue_and_claim(store, task_id="disconnect-detach")
    before = await store.get(spec.scope, spec.task_id)

    await store.detach(spec.scope, spec.task_id)

    assert await store.get(spec.scope, spec.task_id) == before
    claim = await store.heartbeat(
        claim,
        expected_version=claim.version,
        lease_seconds=60,
    )
    terminal = await store.complete_terminal(
        claim,
        expected_version=claim.version,
        status=JobStatus.COMPLETED,
        result={"continued": True},
        usage=Usage(input_tokens=40, output_tokens=10, usd_micros=100),
    )
    assert terminal.status is JobStatus.COMPLETED


@pytest.mark.asyncio
async def test_cancel_is_durable_but_ack_is_not_a_fake_terminal() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec, original_claim = await _enqueue_and_claim(store, task_id="durable-cancel")
    control = await store.request_cancel(
        spec.scope,
        spec.task_id,
        control_id="cancel-1",
        reason="operator stop",
    )
    assert (
        await store.request_cancel(
            spec.scope,
            spec.task_id,
            control_id="cancel-1",
            reason="operator stop",
        )
        == control
    )

    with pytest.raises(LeaseLost):
        await store.heartbeat(
            original_claim,
            expected_version=original_claim.version,
            lease_seconds=60,
        )
    refreshed, controls = await store.poll_controls(original_claim, after_seq=0)
    assert controls == [control]
    refreshed = await store.acknowledge_control(
        refreshed,
        expected_version=refreshed.version,
        control_seq=control.seq,
        disposition="observed_cancel_requested",
    )
    assert (
        await store.acknowledge_control(
            original_claim,
            expected_version=original_claim.version,
            control_seq=control.seq,
            disposition="observed_cancel_requested",
        )
        == refreshed
    )
    assert (await store.get(spec.scope, spec.task_id))["status"] is JobStatus.RUNNING

    terminal = await store.complete_terminal(
        refreshed,
        expected_version=refreshed.version,
        status=JobStatus.CANCELLED,
        result={"reason": "operator stop", "safe_boundary": True},
        usage=Usage(input_tokens=20, output_tokens=0, usd_micros=50),
    )
    assert terminal.status is JobStatus.CANCELLED
    events = await store.fetch_events(spec.scope, spec.task_id, after_cursor=0)
    assert any(event.event_type == "control_acknowledged" for event in events)
    assert events[-1].event_type == "terminal"


@pytest.mark.asyncio
async def test_steer_survives_worker_restart_and_is_scoped_for_followup() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec, first = await _enqueue_and_claim(
        store,
        task_id="durable-steer",
        lease_seconds=20,
    )
    steer = await store.request_steer(
        spec.scope,
        spec.task_id,
        control_id="steer-1",
        instruction="replace the valuation section with a downside case",
    )
    with pytest.raises(ScopeViolation):
        await store.request_steer(
            Scope("tenant-b", spec.scope.session_id, spec.scope.run_id),
            spec.task_id,
            control_id="steer-attack",
            instruction="cross-tenant overwrite",
        )

    clock.advance(seconds=21)
    await store.reclaim_stale()
    second = (await store.claim_next(tenant_id="tenant-a", worker_id="worker-b", lease_seconds=20))[
        0
    ]
    refreshed, controls = await store.poll_controls(second, after_seq=0)
    assert controls == [steer]
    refreshed = await store.acknowledge_control(
        refreshed,
        expected_version=refreshed.version,
        control_seq=steer.seq,
        disposition="accepted_for_followup",
    )
    terminal = await store.complete_terminal(
        refreshed,
        expected_version=refreshed.version,
        status=JobStatus.CANCELLED,
        result={"successor_required": True, "steer_control_seq": steer.seq},
        usage=Usage(),
    )
    assert terminal.result["successor_required"] is True
    assert second.fencing_epoch == first.fencing_epoch + 1


@pytest.mark.asyncio
async def test_two_hour_task_uses_virtual_heartbeats_without_real_wait() -> None:
    wall_start = time.monotonic()
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock)
    spec, claim = await _enqueue_and_claim(
        store,
        task_id="two-hour-regulatory-research",
        lease_seconds=360,
    )

    for _ in range(24):
        clock.advance(minutes=5)
        claim = await store.heartbeat(
            claim,
            expected_version=claim.version,
            lease_seconds=360,
        )
        assert (
            await store.claim_next(tenant_id="tenant-a", worker_id="worker-b", lease_seconds=360)
            == []
        )

    assert clock.now == 120 * 60
    assert time.monotonic() - wall_start < 1.0
    assert (await store.get(spec.scope, spec.task_id))["status"] is JobStatus.RUNNING


@pytest.mark.asyncio
async def test_tenant_scope_and_store_wide_concurrency_hold_across_workers() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(
        clock=clock,
        tenant_concurrency=2,
        session_concurrency=1,
    )
    specs = [
        make_spec(task_id="a-1", session_id="session-a"),
        make_spec(task_id="a-2", session_id="session-b"),
        make_spec(task_id="a-3", session_id="session-c"),
        make_spec(task_id="b-1", tenant_id="tenant-b", session_id="session-a"),
    ]
    for spec in specs:
        await store.create_or_reuse(spec)

    claims_a = await asyncio.gather(
        store.claim_next(tenant_id="tenant-a", worker_id="worker-a", lease_seconds=60, limit=2),
        store.claim_next(tenant_id="tenant-a", worker_id="worker-b", lease_seconds=60, limit=2),
    )
    assert len([claim for group in claims_a for claim in group]) == 2
    assert (
        await store.claim_next(tenant_id="tenant-a", worker_id="worker-c", lease_seconds=60) == []
    )
    assert (
        len(await store.claim_next(tenant_id="tenant-b", worker_id="worker-c", lease_seconds=60))
        == 1
    )
    with pytest.raises(ScopeViolation):
        await store.get(
            Scope("tenant-b", "session-a", "run-a"),
            "a-1",
        )


@pytest.mark.asyncio
async def test_token_and_usd_reservation_and_settlement_are_atomic() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock, tenant_concurrency=5, session_concurrency=5)
    await store.configure_budget(
        "tenant-a",
        token_limit=1_000,
        usd_limit_micros=30_000,
    )
    for task_id in ("budget-a", "budget-b"):
        await store.create_or_reuse(
            make_spec(
                task_id=task_id,
                reserve_tokens=700,
                reserve_usd_micros=20_000,
            )
        )

    races = await asyncio.gather(
        store.claim_next(tenant_id="tenant-a", worker_id="worker-a", lease_seconds=60),
        store.claim_next(tenant_id="tenant-a", worker_id="worker-b", lease_seconds=60),
    )
    claims = [claim for group in races for claim in group]
    assert len(claims) == 1
    reserved = await store.budget_snapshot("tenant-a")
    assert (reserved.reserved_tokens, reserved.reserved_usd_micros) == (700, 20_000)

    claim = claims[0]
    terminal = await store.complete_terminal(
        claim,
        expected_version=claim.version,
        status=JobStatus.COMPLETED,
        result={"answer": "within budget"},
        usage=Usage(input_tokens=350, output_tokens=150, usd_micros=15_000),
    )
    settled = await store.budget_snapshot("tenant-a")
    assert (settled.reserved_tokens, settled.reserved_usd_micros) == (0, 0)
    assert (settled.spent_tokens, settled.spent_usd_micros) == (500, 15_000)

    # A duplicate terminal acknowledgement is idempotent and cannot double-charge.
    assert (
        await store.complete_terminal(
            claim,
            expected_version=claim.version,
            status=JobStatus.COMPLETED,
            result={"answer": "within budget"},
            usage=Usage(input_tokens=350, output_tokens=150, usd_micros=15_000),
        )
        == terminal
    )
    assert await store.budget_snapshot("tenant-a") == settled
    assert (
        await store.claim_next(tenant_id="tenant-a", worker_id="worker-c", lease_seconds=60) == []
    )


@pytest.mark.asyncio
async def test_child_usage_is_aggregated_transitively_without_parent_self_usage() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock, tenant_concurrency=4, session_concurrency=4)
    scope = Scope("tenant-a", "session-a", "run-a")
    specs = [
        make_spec(task_id="child-legal", parent_task_id="parent"),
        make_spec(task_id="child-finance", parent_task_id="parent"),
        make_spec(task_id="grandchild-check", parent_task_id="child-finance"),
    ]
    for spec in specs:
        await store.create_or_reuse(spec)
    claims = await store.claim_next(
        tenant_id="tenant-a", worker_id="worker-a", lease_seconds=60, limit=3
    )
    usages = {
        "child-legal": Usage(input_tokens=100, output_tokens=30, usd_micros=1_000),
        "child-finance": Usage(input_tokens=200, output_tokens=50, usd_micros=2_000),
        "grandchild-check": Usage(input_tokens=80, output_tokens=20, usd_micros=700),
    }
    for claim in claims:
        task_id = claim.key[-1]
        await store.complete_terminal(
            claim,
            expected_version=claim.version,
            status=JobStatus.COMPLETED,
            result={"task_id": task_id},
            usage=usages[task_id],
        )

    total = await store.aggregate_child_usage(scope, "parent")
    assert total == Usage(input_tokens=380, output_tokens=100, usd_micros=3_700)


@pytest.mark.asyncio
async def test_large_terminal_result_spills_to_tenant_scoped_artifact() -> None:
    clock = VirtualClock()
    store = DurableProtocolOracle(clock=clock, artifact_inline_limit=1_024)
    spec, claim = await _enqueue_and_claim(store, task_id="large-due-diligence-result")
    large_result = {"report": "risk-factor\n" * 10_000}
    terminal = await store.complete_terminal(
        claim,
        expected_version=claim.version,
        status=JobStatus.COMPLETED,
        result=large_result,
        usage=Usage(input_tokens=500, output_tokens=400, usd_micros=5_000),
    )

    reference = terminal.result["artifact_ref"]
    assert set(reference) == {"artifact_id", "sha256", "size_bytes", "media_type"}
    assert len(json.dumps(terminal.result)) < 1_024
    artifact = await store.get_artifact("tenant-a", reference["artifact_id"])
    assert hashlib.sha256(artifact.content).hexdigest() == reference["sha256"]
    assert json.loads(artifact.content) == large_result
    with pytest.raises(ScopeViolation):
        await store.get_artifact("tenant-b", reference["artifact_id"])
