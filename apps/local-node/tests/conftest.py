from __future__ import annotations

import hashlib
import hmac
import sys
import time
from dataclasses import replace
from pathlib import Path
from typing import Any, Mapping

import pytest

PACKAGE_SRC = Path(__file__).resolve().parents[1] / "src"
if str(PACKAGE_SRC) not in sys.path:
    sys.path.insert(0, str(PACKAGE_SRC))

from local_node.models import ActionContext, ApprovalProof, digest_payload  # noqa: E402
from local_node.approvals import OneUseTrustedLocalApprovalVerifier  # noqa: E402


class ExplicitTestPlatformSignatureVerifier:
    """Deterministic test fixture, never a production trust configuration."""

    key_id = "test-platform-key"

    def __init__(self) -> None:
        self._key = b"local-node-platform-signature-test-key"

    def sign(self, payload: bytes) -> str:
        return hmac.new(self._key, payload, hashlib.sha256).hexdigest()

    def verify(self, *, key_id: str, payload: bytes, signature: str) -> bool:
        if key_id != self.key_id:
            return False
        return hmac.compare_digest(self.sign(payload), signature)


@pytest.fixture
def platform_signature_verifier() -> ExplicitTestPlatformSignatureVerifier:
    return ExplicitTestPlatformSignatureVerifier()


class ExplicitTestTrustedLocalApprovalSigner:
    device_id = "device-a"

    def __init__(self) -> None:
        self._key = b"local-node-trusted-local-approval-test-key"

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
def action_factory(platform_signature_verifier, trusted_local_approval):
    local_signer, _local_verifier = trusted_local_approval

    def create(
        capability: str,
        arguments: Mapping[str, Any],
        target: str,
        *,
        action_id: str = "action-1",
        idempotency_key: str = "idem-1",
        approved: bool = True,
        capability_lease_id: str | None = None,
        resource_refs: tuple[str, ...] | None = None,
        tool_name: str | None = None,
        operation: str | None = None,
    ) -> ActionContext:
        policy = "policy-v1"
        digest = digest_payload(arguments)
        approval = None
        if approved:
            unsigned_approval = ApprovalProof(
                "approval-1",
                action_id,
                local_signer.device_id,
                digest,
                target,
                policy,
                f"local-approval-{action_id}",
                time.time() + 60,
                "",
            )
            approval = replace(
                unsigned_approval,
                local_signature=local_signer.sign(unsigned_approval.canonical_local_payload()),
            )
        lease_id = capability_lease_id or str(
            arguments.get("grant_id") or arguments.get("lease_id") or "lease-a"
        )
        if resource_refs is None:
            if capability.startswith("file."):
                refs = (lease_id, str(arguments.get("relative_path", "resource-a")))
            elif capability == "process.run":
                refs = (lease_id, str(arguments.get("cwd", ".")))
            else:
                refs = ("resource-a",)
        else:
            refs = resource_refs
        action = ActionContext.create(
            action_id=action_id,
            idempotency_key=idempotency_key,
            tenant_id="tenant-a",
            user_id="user-a",
            session_id="session-a",
            run_id="run-a",
            agent_id="agent-a",
            agent_version="agent-version-a",
            call_id="call-a",
            device_id="device-a",
            envelope_version=1,
            capability=capability,
            tool_name=tool_name or f"test_{capability.replace('.', '_')}",
            operation=operation or capability,
            capability_lease_id=lease_id,
            resource_refs=refs,
            normalized_arguments=arguments,
            target_snapshot_digest=target,
            policy_snapshot_digest=policy,
            nonce=f"nonce-{action_id}",
            platform_key_id=platform_signature_verifier.key_id,
            approval=approval,
        )
        return replace(
            action,
            platform_signature=platform_signature_verifier.sign(action.canonical_signed_payload()),
        )

    return create
