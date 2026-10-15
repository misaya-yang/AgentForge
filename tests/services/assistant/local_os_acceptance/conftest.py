"""Fixtures for the offline Local OS acceptance contract.

The Local Node is deliberately a separately installable application.  These
repository-level contract tests add its source tree explicitly rather than
pretending it is part of the Assistant service package.
"""

from __future__ import annotations

import hashlib
import hmac
import sys
import time
from dataclasses import replace
from pathlib import Path
from typing import Any

import pytest

REPO_ROOT = Path(__file__).resolve().parents[4]
LOCAL_NODE_SRC = REPO_ROOT / "apps" / "local-node" / "src"
if str(LOCAL_NODE_SRC) not in sys.path:
    sys.path.insert(0, str(LOCAL_NODE_SRC))

from local_node.approvals import OneUseTrustedLocalApprovalVerifier  # noqa: E402


class ExplicitTestPlatformSignatureVerifier:
    key_id = "test-platform-key"

    def __init__(self) -> None:
        self._key = b"local-os-acceptance-platform-signature-key"

    def sign(self, payload: bytes) -> str:
        return hmac.new(self._key, payload, hashlib.sha256).hexdigest()

    def verify(self, *, key_id: str, payload: bytes, signature: str) -> bool:
        return key_id == self.key_id and hmac.compare_digest(self.sign(payload), signature)


@pytest.fixture
def platform_signature_verifier() -> ExplicitTestPlatformSignatureVerifier:
    return ExplicitTestPlatformSignatureVerifier()


class ExplicitTestTrustedLocalApprovalSigner:
    device_id = "device-a"

    def __init__(self) -> None:
        self._key = b"local-os-acceptance-trusted-local-approval-key"

    def sign(self, payload: bytes) -> str:
        return hmac.new(self._key, payload, hashlib.sha256).hexdigest()

    def verify(self, payload: bytes, signature: str) -> bool:
        return hmac.compare_digest(self.sign(payload), signature)


@pytest.fixture
def trusted_local_approval(tmp_path):
    signer = ExplicitTestTrustedLocalApprovalSigner()
    verifier = OneUseTrustedLocalApprovalVerifier(
        device_id=signer.device_id,
        state_path=tmp_path / "trusted-local-approvals.sqlite",
        verify_signature=signer.verify,
    )
    yield signer, verifier
    verifier.close()


@pytest.fixture
def trusted_local_approval_verifier(trusted_local_approval):
    return trusted_local_approval[1]


@pytest.fixture
def local_action_factory(platform_signature_verifier, trusted_local_approval):
    from local_node.models import ActionContext, ApprovalProof, digest_payload

    local_signer, _local_verifier = trusted_local_approval

    def _create(**overrides: Any) -> ActionContext:
        normalized_arguments = overrides.pop(
            "normalized_arguments",
            {"path": "notes/result.txt", "content_sha256": "content-v1"},
        )
        explicit_approval = overrides.pop("approval", None)
        approved = bool(overrides.pop("approved", False))
        values: dict[str, Any] = {
            "action_id": "action-001",
            "idempotency_key": "idem-001",
            "tenant_id": "tenant-a",
            "user_id": "user-a",
            "session_id": "session-a",
            "run_id": "run-a",
            "agent_id": "agent-a",
            "agent_version": "agent-version-a",
            "call_id": "call-a",
            "device_id": "device-a",
            "envelope_version": 1,
            "capability": "file.write",
            "tool_name": "local_file_write",
            "operation": "file.write",
            "capability_lease_id": "lease-a",
            "resource_refs": ("resource-a",),
            "normalized_arguments": normalized_arguments,
            "target_snapshot_digest": "target-v1",
            "policy_snapshot_digest": "policy-v1",
            "nonce": "nonce-action-001",
            "platform_key_id": platform_signature_verifier.key_id,
            "ttl_seconds": 60,
            "now": time.time(),
        }
        values.update(overrides)
        approval = explicit_approval
        if approval is None and approved:
            unsigned = ApprovalProof(
                "approval-local-001",
                str(values["action_id"]),
                str(values["device_id"]),
                digest_payload(normalized_arguments),
                str(values["target_snapshot_digest"]),
                str(values["policy_snapshot_digest"]),
                f"local-approval-{values['action_id']}",
                time.time() + 30,
                "",
            )
            approval = replace(
                unsigned,
                local_signature=local_signer.sign(unsigned.canonical_local_payload()),
            )
        values["approval"] = approval
        action = ActionContext.create(**values)
        return replace(
            action,
            platform_signature=platform_signature_verifier.sign(action.canonical_signed_payload()),
        )

    return _create


@pytest.fixture
def path_attack_fixture() -> dict[str, Any]:
    import json

    fixture_path = Path(__file__).with_name("fixtures") / "path_attacks.json"
    return json.loads(fixture_path.read_text(encoding="utf-8"))


@pytest.fixture
def provider_call_fixture() -> dict[str, Any]:
    import json

    fixture_path = Path(__file__).with_name("fixtures") / "provider_computer_calls.json"
    return json.loads(fixture_path.read_text(encoding="utf-8"))
