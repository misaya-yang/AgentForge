#!/usr/bin/env python3
"""Explicit interactive Local Node macOS Computer Use acceptance.

The script performs no action until the native helper shows the exact-intent
approval prompt and the local user clicks "Approve Once". It operates a new
Calculator window, reads back the result through Accessibility, verifies stop,
and writes only redaction-safe artifacts beneath the requested report folder.
"""

from __future__ import annotations

import argparse
import hashlib
import hmac
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import replace
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


PACKAGE_SRC = Path(__file__).resolve().parents[1] / "src"
if str(PACKAGE_SRC) not in sys.path:
    sys.path.insert(0, str(PACKAGE_SRC))

from local_node.approvals import OneUseTrustedLocalApprovalVerifier  # noqa: E402
from local_node.computer import (  # noqa: E402
    ComputerAction,
    ComputerController,
    ComputerScope,
    MacOSComputerDriver,
)
from local_node.ledger import ActionLedger  # noqa: E402
from local_node.macos_native import (  # noqa: E402
    MacOSKeychainApprovalSigner,
    MacOSNativeComputerBackend,
    NativeApprovalIntent,
)
from local_node.errors import CapabilityDenied  # noqa: E402
from local_node.models import ActionContext, ApprovalProof, digest_payload  # noqa: E402


APP_ID = "com.apple.calculator"
SESSION_ID = "session-macos-native-live-acceptance"
RUN_ID = "run-macos-native-live-acceptance"
DEVICE_ID = "device-macos-native-live-acceptance"
PLATFORM_KEY_ID = "macos-live-acceptance-platform-test-key"
PLATFORM_TEST_KEY = b"local-node-macos-live-platform-signature-test-only"


class AcceptancePlatformVerifier:
    def sign(self, payload: bytes) -> str:
        return hmac.new(PLATFORM_TEST_KEY, payload, hashlib.sha256).hexdigest()

    def verify(self, *, key_id: str, payload: bytes, signature: str) -> bool:
        return key_id == PLATFORM_KEY_ID and hmac.compare_digest(
            self.sign(payload), signature
        )


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise RuntimeError(message)


def _build_helper(build_dir: Path) -> Path:
    source = (
        Path(__file__).resolve().parents[1]
        / "native"
        / "macos"
        / "LocalNodeMacOSHelper.swift"
    )
    helper = build_dir / "local-node-macos-helper"
    completed = subprocess.run(
        (
            "/usr/bin/xcrun",
            "swiftc",
            "-O",
            "-framework",
            "AppKit",
            "-framework",
            "ApplicationServices",
            "-framework",
            "CoreGraphics",
            "-framework",
            "Security",
            str(source),
            "-o",
            str(helper),
        ),
        check=False,
        capture_output=True,
        timeout=60,
        env={"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "C", "LC_ALL": "C"},
    )
    _require(completed.returncode == 0, "native helper compilation failed")
    helper.chmod(0o700)
    return helper


def _calculator_value(lines: tuple[str, ...]) -> str | None:
    for line in lines:
        if "AXStaticText" not in line and "AXTextField" not in line:
            continue
        for candidate in reversed([item.strip() for item in line.split("|")]):
            if candidate in {"AXStaticText", "AXTextField", "AXValue"}:
                continue
            normalized = candidate.replace(",", "")
            try:
                float(normalized)
            except ValueError:
                continue
            return candidate
    return None


_FRAME = re.compile(
    r"frame=(-?\d+(?:\.\d+)?),(-?\d+(?:\.\d+)?),(\d+(?:\.\d+)?),(\d+(?:\.\d+)?)"
)


def _find_button_center(
    lines: tuple[str, ...],
    *,
    label: str,
    window_x: float,
    window_y: float,
) -> tuple[float, float] | None:
    for line in lines:
        fields = [item.strip() for item in line.split("|")]
        if not fields or fields[0] != "AXButton" or label not in fields[:-1]:
            continue
        match = _FRAME.search(line)
        if match is None:
            continue
        x, y, width, height = (float(item) for item in match.groups())
        return x + width / 2 - window_x, y + height / 2 - window_y
    return None


def _approval_and_action(
    *,
    name: str,
    primitive: ComputerAction,
    controller: ComputerController,
    lease,
    before,
    signer: MacOSKeychainApprovalSigner,
    verifier: AcceptancePlatformVerifier,
) -> tuple[Any, ActionContext, dict[str, Any]]:
    normalized_arguments = {
        "lease_id": lease.lease_id,
        "kind": primitive.kind,
        "observation_id": primitive.observation_id,
        "arguments": dict(primitive.arguments),
    }
    policy_digest = "macos-native-live-policy-v1"
    action_id = f"macos-live-{name}"
    expires_at = time.time() + 180
    unsigned_approval = ApprovalProof(
        approval_id=f"macos-local-approval-{name}",
        action_id=action_id,
        device_id=DEVICE_ID,
        arguments_digest=digest_payload(normalized_arguments),
        target_snapshot_digest=before.digest,
        policy_snapshot_digest=policy_digest,
        nonce=f"macos-local-approval-nonce-{name}",
        expires_at=expires_at,
        local_signature="",
    )
    details = controller.driver.backend.last_details  # type: ignore[attr-defined]
    _require(details is not None, "native observation details are unavailable")
    intent = NativeApprovalIntent(
        action_id=action_id,
        run_id=RUN_ID,
        device_id=DEVICE_ID,
        app_id=before.app_id,
        window_id=before.window_id,
        origin=before.origin,
        operation=f"app.control ({primitive.kind})",
        arguments_digest=digest_payload(normalized_arguments),
        target_snapshot_digest=before.digest,
        risk_reason="native mouse input to the exact freshly observed Calculator window",
    )
    signature = signer.prompt_and_sign(
        payload=unsigned_approval.canonical_local_payload(),
        intent=intent,
        timeout_seconds=120,
    )
    if signature is None:
        raise RuntimeError("trusted local user denied or timed out")
    approval = replace(unsigned_approval, local_signature=signature)
    action = ActionContext.create(
        action_id=action_id,
        idempotency_key=f"macos-live-idempotency-{name}",
        tenant_id="tenant-macos-live-acceptance",
        user_id="user-macos-live-acceptance",
        session_id=SESSION_ID,
        run_id=RUN_ID,
        agent_id="canonical-agent-loop",
        agent_version="macos-live-v1",
        call_id=f"call-macos-live-{name}",
        device_id=DEVICE_ID,
        envelope_version=1,
        capability="app.control",
        tool_name="local_app_control",
        operation="app.control",
        capability_lease_id=lease.lease_id,
        resource_refs=(lease.lease_id, before.app_id, before.window_id),
        normalized_arguments=normalized_arguments,
        target_snapshot_digest=before.digest,
        policy_snapshot_digest=policy_digest,
        nonce=f"macos-platform-action-nonce-{name}",
        platform_key_id=PLATFORM_KEY_ID,
        approval=approval,
        ttl_seconds=180,
    )
    signed_action = replace(
        action,
        platform_signature=verifier.sign(action.canonical_signed_payload()),
    )
    result = controller.execute(lease.token, primitive, signed_action)
    record = controller.ledger.get(action_id)
    _require(record is not None, "action ledger record is unavailable")
    return result, signed_action, {
        "action_id": action_id,
        "primitive": primitive.kind,
        "arguments_digest": action.arguments_digest,
        "target_snapshot_digest": before.digest,
        "approval_device_id": DEVICE_ID,
        "approval_nonce": approval.nonce,
        "terminal": record.status.value,
    }


def run(report_dir: Path) -> dict[str, Any]:
    _require(platform.system() == "Darwin", "native live acceptance is macOS-only")
    report_dir.mkdir(parents=True, exist_ok=True, mode=0o700)
    os.chmod(report_dir, 0o700)
    with tempfile.TemporaryDirectory(prefix="ai-platform-macos-native-live-") as raw_temp:
        temp = Path(raw_temp)
        helper = _build_helper(temp)
        platform_verifier = AcceptancePlatformVerifier()
        local_signer = MacOSKeychainApprovalSigner(
            device_id=DEVICE_ID,
            helper_path=helper,
        )
        local_verifier = OneUseTrustedLocalApprovalVerifier(
            device_id=DEVICE_ID,
            state_path=temp / "trusted-local-approvals.sqlite",
            verify_signature=local_signer.verify,
        )
        ledger = ActionLedger(
            temp / "action-ledger.sqlite",
            platform_signature_verifier=platform_verifier,
            trusted_local_approval_verifier=local_verifier,
        )
        backend = MacOSNativeComputerBackend(
            helper_path=helper,
            session_id=SESSION_ID,
            allowed_apps=frozenset({APP_ID}),
            screenshot_dir=temp / "screenshots",
            screen_observe=True,
            screen_share=True,
        )
        driver = MacOSComputerDriver(
            backend,
            accessibility_probe=backend.accessibility_ready,
            screen_recording_probe=backend.screen_recording_ready,
            platform_name="Darwin",
        )
        controller = ComputerController(driver, ComputerScope(frozenset({APP_ID})), ledger)
        subprocess.run(
            ("/usr/bin/open", "-na", "Calculator"),
            check=True,
            timeout=10,
            env={"PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "C", "LC_ALL": "C"},
        )
        time.sleep(0.5)
        lease = controller.acquire(APP_ID, session_id=SESSION_ID)
        before = controller.observe(lease.token)
        before_details = backend.last_details
        _require(before_details is not None, "native pre-action details are unavailable")
        _require(before_details.frontmost, "Calculator is not the frontmost bound window")
        _require(not before_details.risk_flags, "sensitive/modal Calculator UI is present")
        point = _find_button_center(
            before_details.accessibility_lines,
            label="7",
            window_x=before_details.window_x,
            window_y=before_details.window_y,
        )
        _require(point is not None, "Calculator 7 target is unavailable")
        primitive = ComputerAction(
            "click",
            before.observation_id,
            {"x": point[0], "y": point[1]},
        )
        result, signed_action, action_evidence = _approval_and_action(
            name="calculator-seven",
            primitive=primitive,
            controller=controller,
            lease=lease,
            before=before,
            signer=local_signer,
            verifier=platform_verifier,
        )
        after_details = backend.last_details
        _require(after_details is not None, "native post-action details are unavailable")
        readback = _calculator_value(after_details.accessibility_lines)
        _require(readback == "7", f"Calculator read-back mismatch: {readback!r}")
        _require(result.status == "succeeded", "Calculator action did not succeed")
        _require(after_details.screenshot_path is not None, "post-action screenshot is missing")
        artifact = report_dir / "calculator-local-node-native-live.png"
        shutil.copy2(after_details.screenshot_path, artifact)
        os.chmod(artifact, 0o600)
        artifact_hash = hashlib.sha256(artifact.read_bytes()).hexdigest()
        stop_started = time.monotonic()
        _require(controller.emergency_stop(), "trusted local emergency stop did not close lease")
        stop_latency_ms = (time.monotonic() - stop_started) * 1000
        post_stop_denied = False
        post_stop_denial_code: str | None = None
        try:
            controller.execute(lease.token, primitive, signed_action)
        except CapabilityDenied as exc:
            post_stop_denied = True
            post_stop_denial_code = exc.code
        _require(post_stop_denied, "post-stop input was not denied")
        _require(
            post_stop_denial_code == "capability_denied",
            "post-stop denial did not come from the invalidated Computer Use lease",
        )
        _require(stop_latency_ms < 2000, "native emergency stop exceeded two seconds")
        _require(ledger.verify_integrity(), "native action ledger failed integrity verification")
        action_events = [
            entry["event_type"]
            for entry in ledger.entries()
            if entry["action_id"] == action_evidence["action_id"]
        ]
        _require(
            action_events
            == [
                "policy_check",
                "awaiting_approval",
                "dispatched",
                "running",
                "observed",
                "succeeded",
            ],
            "native action ledger order is not exact",
        )
        receipt = {
            "schema_version": 1,
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "overall_status": "passed",
            "host": {
                "system": platform.system(),
                "machine": platform.machine(),
                "python": platform.python_version(),
            },
            "driver": backend.name,
            "permissions": {
                "accessibility": True,
                "screen_recording": True,
            },
            "scope": {
                "session_id": SESSION_ID,
                "app_id": APP_ID,
                "window_id": before.window_id,
                "origin": before.origin,
            },
            "action": {
                **action_evidence,
                "ledger_events": action_events,
                "before_observation_id": result.before_observation_id,
                "after_observation_id": result.after_observation_id,
                "readback": readback,
            },
            "screenshot": {
                "path": str(artifact),
                "sha256": artifact_hash,
            },
            "takeover_stop": {
                "latency_ms": round(stop_latency_ms, 3),
                "post_stop_input_denied": post_stop_denied,
                "post_stop_denial_code": post_stop_denial_code,
            },
            "evidence_tier": {
                "E2_local_live": [
                    "real Local Node Swift Accessibility/CoreGraphics helper",
                    "real Calculator window screenshot/action/accessibility read-back",
                    "native Keychain-backed exact-intent local approval",
                    "session/window/latest-observation binding and emergency stop",
                ],
                "E1_only": ["test-only platform action-envelope HMAC signer"],
                "not_claimed": [
                    "browser Computer Use",
                    "TextEdit file save",
                    "OpenAI provider path",
                ],
            },
        }
        ledger.close()
        local_verifier.close()
        return receipt


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report-dir", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        result = run(arguments.report_dir.resolve())
    except Exception as exc:
        print(
            json.dumps(
                {
                    "schema_version": 1,
                    "overall_status": "failed",
                    "error_type": type(exc).__name__,
                    "error": str(exc),
                },
                ensure_ascii=False,
                indent=2,
                sort_keys=True,
            )
        )
        return 1
    print(json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
