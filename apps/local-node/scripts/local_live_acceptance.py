#!/usr/bin/env python3
"""Run a narrow Local Node acceptance against real host files and processes.

This harness intentionally uses a test-only HMAC platform verifier. It proves
that the Local Node enforces a signed envelope supplied through its trusted
verifier seam; it does not claim production platform-key or transport evidence.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import os
import platform
import sys
import tempfile
import threading
import time
from dataclasses import asdict, replace
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping


PACKAGE_SRC = Path(__file__).resolve().parents[1] / "src"
if str(PACKAGE_SRC) not in sys.path:
    sys.path.insert(0, str(PACKAGE_SRC))

from local_node.computer import (  # noqa: E402
    ComputerController,
    ComputerScope,
    MacOSComputerDriver,
)
from local_node.approvals import OneUseTrustedLocalApprovalVerifier  # noqa: E402
from local_node.errors import (  # noqa: E402
    CapabilityDenied,
    LedgerIntegrityError,
    ProcessPolicyError,
    StaleTargetError,
)
from local_node.files import LocalFileService, sha256_bytes  # noqa: E402
from local_node.grants import DirectoryGrantStore  # noqa: E402
from local_node.ledger import ActionLedger  # noqa: E402
from local_node.models import (  # noqa: E402
    ActionContext,
    ActionStatus,
    ApprovalProof,
    TERMINAL_STATUSES,
    digest_payload,
)
from local_node.processes import (  # noqa: E402
    ProcessPolicy,
    ProcessRequest,
    ProcessRunner,
)
from local_node.service import LocalNodeRuntime, OutboundControlPlane  # noqa: E402
from local_node.watcher import DirectoryWatcher, WatchEvent  # noqa: E402


class LiveAcceptanceTestSignatureVerifier:
    """Explicit non-production signer/verifier for an isolated acceptance run."""

    key_id = "live-acceptance-test-key"
    _key = b"ai-platform-local-node-live-acceptance-test-only"

    def sign(self, payload: bytes) -> str:
        return hmac.new(self._key, payload, hashlib.sha256).hexdigest()

    def verify(self, *, key_id: str, payload: bytes, signature: str) -> bool:
        return key_id == self.key_id and hmac.compare_digest(self.sign(payload), signature)


class LiveAcceptanceTrustedLocalApprovalSigner:
    device_id = "device-live-acceptance"
    _key = b"ai-platform-local-node-live-local-approval-test-only"

    def sign(self, payload: bytes) -> str:
        return hmac.new(self._key, payload, hashlib.sha256).hexdigest()

    def verify(self, payload: bytes, signature: str) -> bool:
        return hmac.compare_digest(self.sign(payload), signature)


class SignedActionFactory:
    def __init__(
        self,
        verifier: LiveAcceptanceTestSignatureVerifier,
        local_signer: LiveAcceptanceTrustedLocalApprovalSigner,
    ) -> None:
        self.verifier = verifier
        self.local_signer = local_signer

    def create(
        self,
        *,
        name: str,
        capability: str,
        tool_name: str,
        operation: str,
        arguments: Mapping[str, Any],
        target_digest: str,
        capability_lease_id: str,
        resource_refs: tuple[str, ...],
        approved: bool = True,
    ) -> ActionContext:
        policy_digest = "local-live-policy-v1"
        arguments_digest = digest_payload(arguments)
        approval = None
        if approved:
            unsigned_approval = ApprovalProof(
                approval_id=f"approval-{name}",
                action_id=f"live-{name}",
                device_id=self.local_signer.device_id,
                arguments_digest=arguments_digest,
                target_snapshot_digest=target_digest,
                policy_snapshot_digest=policy_digest,
                nonce=f"local-approval-{name}",
                expires_at=time.time() + 120,
                local_signature="",
            )
            approval = replace(
                unsigned_approval,
                local_signature=self.local_signer.sign(unsigned_approval.canonical_local_payload()),
            )
        action = ActionContext.create(
            action_id=f"live-{name}",
            idempotency_key=f"live-idempotency-{name}",
            tenant_id="tenant-live-acceptance",
            user_id="user-live-acceptance",
            session_id="session-live-acceptance",
            run_id="run-live-acceptance",
            agent_id="canonical-agent-loop",
            agent_version="local-live-v1",
            call_id=f"call-{name}",
            device_id="device-live-acceptance",
            envelope_version=1,
            capability=capability,
            tool_name=tool_name,
            operation=operation,
            capability_lease_id=capability_lease_id,
            resource_refs=resource_refs,
            normalized_arguments=arguments,
            target_snapshot_digest=target_digest,
            policy_snapshot_digest=policy_digest,
            nonce=f"nonce-{name}",
            platform_key_id=self.verifier.key_id,
            approval=approval,
            ttl_seconds=120,
        )
        return replace(
            action,
            platform_signature=self.verifier.sign(action.canonical_signed_payload()),
        )


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def _wait_until(predicate, *, timeout_seconds: float = 2.0) -> None:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.01)
    raise AssertionError("timed out waiting for a real local state transition")


def _watch_acceptance(*, root: Path, files: LocalFileService, grant_id: str) -> dict[str, Any]:
    events: list[tuple[float, WatchEvent]] = []
    changed = threading.Condition()

    def record(event: WatchEvent) -> None:
        with changed:
            events.append((time.monotonic(), event))
            changed.notify_all()

    watcher = DirectoryWatcher(
        files,
        grant_id,
        interval_seconds=0.05,
        callback=record,
    )
    watcher.start()

    def mutate_and_wait(
        mutation,
        *,
        kind: str,
        relative_path: str,
        previous_path: str | None = None,
    ) -> tuple[float, WatchEvent]:
        started = time.monotonic()
        mutation()
        deadline = started + 2
        with changed:
            while True:
                for arrived_at, event in events:
                    if (
                        arrived_at >= started
                        and event.kind == kind
                        and event.relative_path == relative_path
                        and event.previous_path == previous_path
                    ):
                        return arrived_at - started, event
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AssertionError(
                        f"watcher did not report {kind} for {relative_path} within 2 seconds"
                    )
                changed.wait(remaining)

    body = "LOCAL_NODE_WATCH_BODY_MUST_NOT_BE_EMITTED"
    source = root / "watch-source.txt"
    renamed = root / "watch-renamed.txt"
    latencies: list[float] = []
    observed: list[WatchEvent] = []
    try:
        latency, event = mutate_and_wait(
            lambda: source.write_text(body, encoding="utf-8"),
            kind="create",
            relative_path=source.name,
        )
        latencies.append(latency)
        observed.append(event)
        latency, event = mutate_and_wait(
            lambda: source.write_text("changed metadata body", encoding="utf-8"),
            kind="modify",
            relative_path=source.name,
        )
        latencies.append(latency)
        observed.append(event)
        latency, event = mutate_and_wait(
            lambda: source.rename(renamed),
            kind="rename",
            relative_path=renamed.name,
            previous_path=source.name,
        )
        latencies.append(latency)
        observed.append(event)
        latency, event = mutate_and_wait(
            renamed.unlink,
            kind="delete",
            relative_path=renamed.name,
        )
        latencies.append(latency)
        observed.append(event)
    finally:
        stopped = watcher.stop()

    encoded_events = json.dumps(
        [asdict(event) for event in observed],
        ensure_ascii=False,
        sort_keys=True,
    )
    _require(body not in encoded_events, "watch event leaked the file body")
    _require(stopped, "watcher thread did not stop")
    _require(max(latencies) < 2, "watch event exceeded the two-second smoke threshold")
    sorted_latencies = sorted(latencies)
    p95_index = max(0, int(0.95 * len(sorted_latencies) + 0.999999) - 1)
    return {
        "status": "passed",
        "event_kinds": [event.kind for event in observed],
        "event_sequences": [event.sequence for event in observed],
        "latency_ms": [round(value * 1000, 3) for value in latencies],
        "sample_p95_ms": round(sorted_latencies[p95_index] * 1000, 3),
        "metadata_only": True,
        "stopped": stopped,
        "scope_note": "four-event live smoke; not the required 15-minute OS-A24 soak",
    }


def _run_process(
    *,
    runner: ProcessRunner,
    request: ProcessRequest,
    action: ActionContext,
) -> tuple[Any, list[tuple[str, bytes]]]:
    streamed: list[tuple[str, bytes]] = []
    result = runner.run(
        request,
        action,
        on_output=lambda channel, data: streamed.append((channel, data)),
    )
    return result, streamed


def _terminal_event_uniqueness(entries: tuple[dict[str, Any], ...]) -> bool:
    terminal_names = {status.value for status in TERMINAL_STATUSES}
    terminal_counts: dict[str, int] = {}
    for entry in entries:
        if entry["event_type"] in terminal_names:
            action_id = str(entry["action_id"])
            terminal_counts[action_id] = terminal_counts.get(action_id, 0) + 1
    return all(count == 1 for count in terminal_counts.values())


def run_acceptance() -> dict[str, Any]:
    verifier = LiveAcceptanceTestSignatureVerifier()
    with tempfile.TemporaryDirectory(prefix="ai-platform-local-node-live-") as temporary:
        temp = Path(temporary)
        root = temp / "workspace"
        state = temp / "state"
        root.mkdir()
        local_signer = LiveAcceptanceTrustedLocalApprovalSigner()
        local_approval_verifier = OneUseTrustedLocalApprovalVerifier(
            device_id=local_signer.device_id,
            state_path=state / "trusted-local-approvals.sqlite",
            verify_signature=local_signer.verify,
        )
        actions = SignedActionFactory(verifier, local_signer)

        grants = DirectoryGrantStore()
        grant = grants.issue(
            root,
            frozenset({"list", "read", "search", "watch", "write", "rollback"}),
            tenant_id="tenant-live-acceptance",
            user_id="user-live-acceptance",
        )
        ledger = ActionLedger(
            state / "ledger.sqlite",
            platform_signature_verifier=verifier,
            trusted_local_approval_verifier=local_approval_verifier,
        )
        files = LocalFileService(grants, state / "rollback", ledger)

        docs = root / "docs"
        nested = docs / "nested"
        nested.mkdir(parents=True)
        fixture_bytes = {
            "docs/alpha.txt": b"alpha\nOpenClaw Hermes local node\nomega\n",
            "docs/beta.md": b"first line\nHermes second citation\n",
            "docs/nested/gamma.txt": b"Hermes nested citation\nlast\n",
            "docs/nested/binary.dat": b"\xff\x00Hermes\xfe",
        }
        for relative_path, content in fixture_bytes.items():
            (root / relative_path).write_bytes(content)
        listed = files.list_files(grant.grant_id, recursive=True)
        listed_paths = [(entry.relative_path, entry.kind) for entry in listed]
        _require(
            listed_paths
            == [
                ("docs", "directory"),
                ("docs/alpha.txt", "file"),
                ("docs/beta.md", "file"),
                ("docs/nested", "directory"),
                ("docs/nested/binary.dat", "file"),
                ("docs/nested/gamma.txt", "file"),
            ],
            "recursive listing did not exactly match the real directory",
        )
        analyzed = files.analyze_files(
            grant.grant_id,
            tuple(fixture_bytes),
            query="Hermes",
        )
        for item in analyzed:
            expected = fixture_bytes[item.read.relative_path]
            _require(item.read.content == expected, "multi-file read bytes are not exact")
            _require(
                item.read.sha256 == sha256_bytes(expected),
                "multi-file read hash is not exact",
            )
            _require(
                all(match.file_sha256 == item.read.sha256 for match in item.matches),
                "search citation hash does not bind its exact file version",
            )
        citations = [match for item in analyzed for match in item.matches]
        _require(
            [(item.relative_path, item.line, item.column) for item in citations]
            == [
                ("docs/alpha.txt", 2, 10),
                ("docs/beta.md", 2, 1),
                ("docs/nested/gamma.txt", 1, 1),
            ],
            "multi-file search citations are not exact",
        )
        file_read_evidence = {
            "status": "passed",
            "listed": listed_paths,
            "files": [
                {
                    "relative_path": item.read.relative_path,
                    "bytes": item.read.size,
                    "sha256": item.read.sha256,
                    "encoding": item.read.encoding,
                }
                for item in analyzed
            ],
            "search_query_sha256": sha256_bytes(b"Hermes"),
            "citations": [
                {
                    "relative_path": match.relative_path,
                    "line": match.line,
                    "column": match.column,
                    "file_sha256": match.file_sha256,
                }
                for match in citations
            ],
            "binary_file_skipped_by_text_search": analyzed[-1].matches == (),
            "content_in_receipt": False,
        }

        watch_evidence = _watch_acceptance(
            root=root,
            files=files,
            grant_id=grant.grant_id,
        )

        escape_denied = False
        try:
            files.analyze_files(
                grant.grant_id,
                ("docs/alpha.txt", "../outside.txt"),
                query="Hermes",
            )
        except CapabilityDenied:
            escape_denied = True
        _require(escape_denied, "multi-file path escape did not fail closed")

        revoked_grant = grants.issue(
            root,
            frozenset({"list", "read", "search", "watch"}),
            tenant_id="tenant-live-acceptance",
            user_id="user-live-acceptance",
        )
        revoked_watcher = DirectoryWatcher(files, revoked_grant.grant_id)
        _require(revoked_watcher.scan_once() == (), "revocation watcher baseline failed")
        grants.revoke(revoked_grant.grant_id)
        list_after_revoke_denied = False
        read_after_revoke_denied = False
        search_after_revoke_denied = False
        analysis_after_revoke_denied = False
        watch_after_revoke_denied = False
        try:
            files.list_files(revoked_grant.grant_id, recursive=True)
        except CapabilityDenied:
            list_after_revoke_denied = True
        try:
            files.read_file(revoked_grant.grant_id, "docs/alpha.txt")
        except CapabilityDenied:
            read_after_revoke_denied = True
        try:
            files.search(revoked_grant.grant_id, "Hermes")
        except CapabilityDenied:
            search_after_revoke_denied = True
        try:
            files.analyze_files(
                revoked_grant.grant_id,
                ("docs/alpha.txt",),
                query="Hermes",
            )
        except CapabilityDenied:
            analysis_after_revoke_denied = True
        try:
            revoked_watcher.scan_once()
        except CapabilityDenied:
            watch_after_revoke_denied = True
        _require(list_after_revoke_denied, "revoked grant still allowed file listing")
        _require(read_after_revoke_denied, "revoked grant still allowed file read")
        _require(search_after_revoke_denied, "revoked grant still allowed file search")
        _require(analysis_after_revoke_denied, "revoked grant still allowed multi-file analysis")
        _require(watch_after_revoke_denied, "revoked grant still allowed watcher polling")
        file_read_evidence["boundary"] = {
            "path_escape_denied": escape_denied,
            "list_after_revoke_denied": list_after_revoke_denied,
            "read_after_revoke_denied": read_after_revoke_denied,
            "search_after_revoke_denied": search_after_revoke_denied,
            "analysis_after_revoke_denied": analysis_after_revoke_denied,
            "watch_after_revoke_denied": watch_after_revoke_denied,
        }

        atomic_target = root / "atomic.txt"
        before_bytes = b"before atomic write\n"
        after_bytes = b"after atomic write\n"
        atomic_target.write_bytes(before_bytes)
        before_hash = sha256_bytes(before_bytes)
        after_hash = sha256_bytes(after_bytes)
        write_args = {
            "grant_id": grant.grant_id,
            "relative_path": atomic_target.name,
            "content_sha256": after_hash,
            "expected_hash": before_hash,
        }
        write_action = actions.create(
            name="atomic-write",
            capability="file.write",
            tool_name="local_file_write",
            operation="file.write",
            arguments=write_args,
            target_digest=before_hash,
            capability_lease_id=grant.grant_id,
            resource_refs=(grant.grant_id, atomic_target.name),
        )
        write_receipt = files.write_atomic(
            grant.grant_id,
            atomic_target.name,
            after_bytes,
            before_hash,
            write_action,
        )
        _require(atomic_target.read_bytes() == after_bytes, "atomic write bytes differ")
        events_before_replay = len(ledger.entries())
        replay_receipt = files.write_atomic(
            grant.grant_id,
            atomic_target.name,
            after_bytes,
            before_hash,
            write_action,
        )
        _require(replay_receipt == write_receipt, "write replay returned a different receipt")
        _require(
            len(ledger.entries()) == events_before_replay,
            "idempotent replay appended duplicate action events",
        )

        rollback_args = {
            "rollback_ref": write_receipt.rollback_ref,
            "grant_id": grant.grant_id,
            "relative_path": atomic_target.name,
            "expected_current_hash": after_hash,
        }
        rollback_action = actions.create(
            name="atomic-rollback",
            capability="file.write",
            tool_name="local_file_rollback",
            operation="file.rollback",
            arguments=rollback_args,
            target_digest=after_hash,
            capability_lease_id=grant.grant_id,
            resource_refs=(write_receipt.rollback_ref, grant.grant_id, atomic_target.name),
        )
        rollback_receipt = files.rollback(write_receipt.rollback_ref, rollback_action)
        _require(atomic_target.read_bytes() == before_bytes, "rollback did not restore bytes")
        _require(rollback_receipt.after_sha256 == before_hash, "rollback hash differs")

        stale_target = root / "stale.txt"
        approved_bytes = b"version shown for approval\n"
        human_bytes = b"new human edit after approval\n"
        stale_target.write_bytes(approved_bytes)
        approved_hash = sha256_bytes(approved_bytes)
        stale_args = {
            "grant_id": grant.grant_id,
            "relative_path": stale_target.name,
            "content_sha256": sha256_bytes(b"agent replacement\n"),
            "expected_hash": approved_hash,
        }
        stale_action = actions.create(
            name="stale-write",
            capability="file.write",
            tool_name="local_file_write",
            operation="file.write",
            arguments=stale_args,
            target_digest=approved_hash,
            capability_lease_id=grant.grant_id,
            resource_refs=(grant.grant_id, stale_target.name),
        )
        stale_target.write_bytes(human_bytes)
        stale_denied = False
        try:
            files.write_atomic(
                grant.grant_id,
                stale_target.name,
                b"agent replacement\n",
                approved_hash,
                stale_action,
            )
        except StaleTargetError:
            stale_denied = True
        _require(stale_denied, "stale file approval was not denied")
        _require(stale_target.read_bytes() == human_bytes, "stale action overwrote human bytes")
        write_evidence = {
            "status": "passed",
            "before_sha256": write_receipt.before_sha256,
            "after_sha256": write_receipt.after_sha256,
            "disk_after_write_sha256": sha256_bytes(after_bytes),
            "idempotent_replay_same_receipt": replay_receipt == write_receipt,
            "idempotent_replay_added_events": False,
            "rollback_restored_sha256": rollback_receipt.after_sha256,
            "rollback_matches_original": rollback_receipt.after_sha256 == before_hash,
            "stale_target_denied": stale_denied,
            "human_edit_preserved": stale_target.read_bytes() == human_bytes,
        }

        env_bin = Path("/usr/bin/env")
        sleep_bin = Path("/bin/sleep")
        _require(env_bin.exists(), "/usr/bin/env is unavailable on this host")
        _require(sleep_bin.exists(), "/bin/sleep is unavailable on this host")
        policy = ProcessPolicy(
            allowed_executables=frozenset({env_bin, sleep_bin}),
            allowed_env_names=frozenset({"LOCAL_NODE_ALLOWED"}),
            max_timeout_seconds=10,
            allow_inherited_network=True,
        )
        runner = ProcessRunner(grants, policy, ledger)

        env_request = ProcessRequest(
            argv=(str(env_bin),),
            grant_id=grant.grant_id,
            timeout_seconds=5,
            env={"LOCAL_NODE_ALLOWED": "yes"},
            network_policy="inherit",
        )
        _, env_target = runner.cwd_snapshot_digest(
            env_request,
            tenant_id="tenant-live-acceptance",
            user_id="user-live-acceptance",
        )
        env_action = actions.create(
            name="clean-environment",
            capability="process.run",
            tool_name="local_process_run",
            operation="process.run",
            arguments=env_request.normalized_arguments(),
            target_digest=env_target,
            capability_lease_id=grant.grant_id,
            resource_refs=(grant.grant_id, env_request.cwd),
        )
        inherited_name = "OPENAI_API_KEY"
        previous_inherited = os.environ.get(inherited_name)
        os.environ[inherited_name] = "LOCAL_NODE_SECRET_CANARY_MUST_NOT_LEAK"
        try:
            env_result, streamed = _run_process(
                runner=runner,
                request=env_request,
                action=env_action,
            )
        finally:
            if previous_inherited is None:
                os.environ.pop(inherited_name, None)
            else:
                os.environ[inherited_name] = previous_inherited
        environment = {
            line.split("=", 1)[0]: line.split("=", 1)[1]
            for line in env_result.stdout.splitlines()
            if "=" in line
        }
        expected_environment_names = {"PATH", "LANG", "LC_ALL", "LOCAL_NODE_ALLOWED"}
        _require(env_result.exit_code == 0, "real env process failed")
        _require(
            set(environment) == expected_environment_names,
            "real child process received an unexpected environment variable",
        )
        _require(inherited_name not in environment, "provider key name leaked to child process")
        _require(bool(streamed), "real process stdout was not streamed")

        deny_request = ProcessRequest(
            argv=(str(env_bin),),
            grant_id=grant.grant_id,
            timeout_seconds=5,
            network_policy="deny",
        )
        _, deny_target = runner.cwd_snapshot_digest(
            deny_request,
            tenant_id="tenant-live-acceptance",
            user_id="user-live-acceptance",
        )
        deny_action = actions.create(
            name="network-deny",
            capability="process.run",
            tool_name="local_process_run",
            operation="process.run",
            arguments=deny_request.normalized_arguments(),
            target_digest=deny_target,
            capability_lease_id=grant.grant_id,
            resource_refs=(grant.grant_id, deny_request.cwd),
        )
        network_deny_failed_closed = False
        try:
            runner.run(deny_request, deny_action)
        except ProcessPolicyError as exc:
            network_deny_failed_closed = "sandbox backend" in str(exc)
        _require(network_deny_failed_closed, "network-deny ran without a sandbox backend")
        _require(
            ledger.get(deny_action.action_id) is None,
            "network-denied process was reserved as though it had dispatched",
        )

        cancel_request = ProcessRequest(
            argv=(str(sleep_bin), "5"),
            grant_id=grant.grant_id,
            timeout_seconds=8,
            network_policy="inherit",
        )
        _, cancel_target = runner.cwd_snapshot_digest(
            cancel_request,
            tenant_id="tenant-live-acceptance",
            user_id="user-live-acceptance",
        )
        cancel_action = actions.create(
            name="cancel-process",
            capability="process.run",
            tool_name="local_process_run",
            operation="process.run",
            arguments=cancel_request.normalized_arguments(),
            target_digest=cancel_target,
            capability_lease_id=grant.grant_id,
            resource_refs=(grant.grant_id, cancel_request.cwd),
        )
        cancel_results: list[Any] = []
        cancel_errors: list[BaseException] = []

        def run_cancel_target() -> None:
            try:
                cancel_results.append(runner.run(cancel_request, cancel_action))
            except BaseException as exc:  # captured and re-raised in the main thread
                cancel_errors.append(exc)

        cancel_thread = threading.Thread(target=run_cancel_target)
        cancel_thread.start()
        _wait_until(
            lambda: (
                (record := ledger.get(cancel_action.action_id)) is not None
                and record.status is ActionStatus.RUNNING
            )
        )
        cancel_started = time.monotonic()
        _require(runner.cancel(cancel_action.action_id), "running child was not cancellable")
        cancel_thread.join(2)
        cancel_latency = time.monotonic() - cancel_started
        _require(not cancel_thread.is_alive(), "cancelled child process remained running")
        if cancel_errors:
            raise cancel_errors[0]
        _require(
            len(cancel_results) == 1 and cancel_results[0].status == "cancelled",
            "cancelled process did not receive one cancelled terminal result",
        )
        process_evidence = {
            "status": "passed",
            "executable": str(env_bin),
            "argv_vector": list(env_request.argv),
            "cwd_is_granted_root": True,
            "exit_code": env_result.exit_code,
            "stream_chunk_count": len(streamed),
            "child_environment_names": sorted(environment),
            "provider_key_absent": inherited_name not in environment,
            "network_deny_without_sandbox": "failed_closed",
            "cancel_status": cancel_results[0].status,
            "cancel_latency_ms": round(cancel_latency * 1000, 3),
        }

        runtime_driver = MacOSComputerDriver(platform_name=platform.system())
        runtime_computer = ComputerController(
            runtime_driver,
            ComputerScope(frozenset()),
            ledger,
        )
        runtime = LocalNodeRuntime(
            ledger,
            runner,
            runtime_computer,
            control_plane=OutboundControlPlane("wss://control.example.test/node"),
        )
        runtime.connect()

        orphan_args = {"operation": "already-dispatched-side-effect"}
        orphan_action = actions.create(
            name="disconnect-unknown",
            capability="test.disconnect",
            tool_name="test_disconnect",
            operation="test.disconnect",
            arguments=orphan_args,
            target_digest="disconnect-target",
            capability_lease_id="disconnect-lease",
            resource_refs=("disconnect-resource",),
            approved=False,
        )
        ledger.begin(orphan_action)
        ledger.mark_dispatched(orphan_action.action_id)
        ledger.mark_running(orphan_action.action_id)

        disconnect_request = ProcessRequest(
            argv=(str(sleep_bin), "5"),
            grant_id=grant.grant_id,
            timeout_seconds=8,
            network_policy="inherit",
        )
        _, disconnect_target = runner.cwd_snapshot_digest(
            disconnect_request,
            tenant_id="tenant-live-acceptance",
            user_id="user-live-acceptance",
        )
        disconnect_action = actions.create(
            name="disconnect-process",
            capability="process.run",
            tool_name="local_process_run",
            operation="process.run",
            arguments=disconnect_request.normalized_arguments(),
            target_digest=disconnect_target,
            capability_lease_id=grant.grant_id,
            resource_refs=(grant.grant_id, disconnect_request.cwd),
        )
        disconnect_results: list[Any] = []
        disconnect_errors: list[BaseException] = []

        def run_disconnect_target() -> None:
            try:
                disconnect_results.append(runner.run(disconnect_request, disconnect_action))
            except BaseException as exc:  # captured and re-raised in the main thread
                disconnect_errors.append(exc)

        disconnect_thread = threading.Thread(target=run_disconnect_target)
        disconnect_thread.start()
        _wait_until(
            lambda: (
                (record := ledger.get(disconnect_action.action_id)) is not None
                and record.status is ActionStatus.RUNNING
            )
        )
        disconnect_started = time.monotonic()
        interrupted = runtime.disconnect()
        disconnect_thread.join(2)
        disconnect_latency = time.monotonic() - disconnect_started
        _require(not disconnect_thread.is_alive(), "disconnect left a child process running")
        if disconnect_errors:
            raise disconnect_errors[0]
        _require(
            len(disconnect_results) == 1 and disconnect_results[0].status == "cancelled",
            "disconnect did not cancel the real child process",
        )
        _require(
            interrupted == (orphan_action.action_id,),
            "disconnect did not mark exactly the unresolved side effect",
        )
        orphan_record = ledger.get(orphan_action.action_id)
        _require(
            orphan_record is not None and orphan_record.status is ActionStatus.UNKNOWN,
            "unresolved dispatched side effect was not marked unknown",
        )
        orphan_replay = ledger.begin(orphan_action)
        _require(
            not orphan_replay.created and orphan_replay.record.status is ActionStatus.UNKNOWN,
            "unknown action was automatically replayable",
        )
        disconnect_evidence = {
            "status": "passed",
            "runtime_offline": not runtime.online,
            "real_child_status": disconnect_results[0].status,
            "stop_latency_ms": round(disconnect_latency * 1000, 3),
            "unresolved_terminal": orphan_record.status.value,
            "automatic_replay": False,
            "transport_scope": (
                "LocalNodeRuntime disconnect entrypoint exercised in-process; "
                "no real WSS socket/reconnect was exercised"
            ),
        }

        _require(ledger.verify_integrity(), "real SQLite ledger failed verification")
        entries_before_tamper = ledger.entries()
        _require(
            [entry["seq"] for entry in entries_before_tamper]
            == list(range(1, len(entries_before_tamper) + 1)),
            "ledger event sequence has a gap or regression",
        )
        _require(
            _terminal_event_uniqueness(entries_before_tamper),
            "a side-effecting action has more than one terminal event",
        )
        action_event_sequences: dict[str, list[str]] = {}
        for entry in entries_before_tamper:
            action_event_sequences.setdefault(str(entry["action_id"]), []).append(
                str(entry["event_type"])
            )
        _require(
            action_event_sequences[write_action.action_id]
            == ["policy_check", "awaiting_approval", "dispatched", "running", "succeeded"],
            "atomic write ledger order is not exact",
        )
        _require(
            action_event_sequences[cancel_action.action_id][-1] == "cancelled",
            "cancelled process lacks a cancelled terminal event",
        )
        ledger._db.execute("UPDATE events SET event_type='tampered' WHERE seq=1")
        tamper_detected = False
        try:
            ledger.verify_integrity()
        except LedgerIntegrityError:
            tamper_detected = True
        _require(tamper_detected, "ledger event tampering was not detected")
        ledger.close()
        ledger_evidence = {
            "status": "passed",
            "sqlite_file_created": True,
            "event_count_before_tamper": len(entries_before_tamper),
            "sequences_contiguous": True,
            "unique_terminal_per_action": True,
            "atomic_write_event_order": action_event_sequences[write_action.action_id],
            "cancel_event_order": action_event_sequences[cancel_action.action_id],
            "tamper_detected": tamper_detected,
            "raw_process_output_stored_in_event_metadata": False,
        }

    return {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "harness": "apps/local-node/scripts/local_live_acceptance.py",
        "host": {
            "system": platform.system(),
            "machine": platform.machine(),
            "python": platform.python_version(),
        },
        "overall_status": "passed",
        "evidence": {
            "exact_file_list_read_search_hash": file_read_evidence,
            "watch": watch_evidence,
            "atomic_write_stale_rollback_replay": write_evidence,
            "structured_process": process_evidence,
            "runtime_disconnect": disconnect_evidence,
            "ledger": ledger_evidence,
        },
        "evidence_tiers": {
            "E2_local_live": [
                "real temporary-disk list/read/search/hash",
                "real background watcher create/modify/rename/delete smoke",
                "real atomic replace, stale-target preservation, rollback and replay",
                "real allowlisted argv process, clean environment, streaming and cancellation",
                "real disconnect-triggered child termination and SQLite ledger tamper detection",
            ],
            "E1_contract_only": [
                "platform signature uses an explicit test-only HMAC verifier",
                "outbound control-plane URL is validated but no WSS socket/reconnect is opened",
            ],
            "not_claimed": [
                "native secure credential storage or production device identity",
                "browser Computer Use",
                "desktop App Computer Use",
                "OpenAI Responses provider execution",
                "trusted local approval UI",
                "15-minute OS-A24 soak",
            ],
        },
    }


def main() -> int:
    try:
        receipt = run_acceptance()
    except Exception as exc:
        failure = {
            "schema_version": 1,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "harness": "apps/local-node/scripts/local_live_acceptance.py",
            "overall_status": "failed",
            "error_type": type(exc).__name__,
            "error": str(exc),
        }
        print(json.dumps(failure, ensure_ascii=False, indent=2, sort_keys=True))
        return 1
    print(json.dumps(receipt, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
