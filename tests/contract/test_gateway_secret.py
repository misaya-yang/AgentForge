"""Phase-5a contract tests for ``X-Gateway-Secret`` HMAC.

Locks the sign/verify contract between Gateway and internal Python services:
- round-trip succeeds with matching secrets
- mismatched secret rejected
- timestamp skew rejected
- tampered signature rejected
- replay of the same request_id rejected
- missing / malformed header rejected

Reference: GATE G5a-4 in
``plans/SystemDesign-Assistant-Service-True-Extraction-Phase5-2026-04-23.md``.
"""
from __future__ import annotations

import time
from unittest.mock import patch

import pytest
from ai_gateway_contracts.replay import InMemoryReplayStore
from ai_gateway_core.auth.gateway_secret import (
    GatewaySecret,
    InvalidGatewaySecret,
    RedisReplayStore,
)

SECRET = "0" * 32  # 32 hex-chars, passes the min-length guard
OTHER = "1" * 32


@pytest.fixture
def signer() -> GatewaySecret:
    return GatewaySecret(secret=SECRET, replay_store=InMemoryReplayStore())


@pytest.fixture
def verifier() -> GatewaySecret:
    return GatewaySecret(secret=SECRET, replay_store=InMemoryReplayStore())


def test_roundtrip(signer: GatewaySecret, verifier: GatewaySecret) -> None:
    header = signer.sign()
    assert verifier.verify(header) is not None


def test_request_id_preserved(signer: GatewaySecret, verifier: GatewaySecret) -> None:
    header = signer.sign(request_id="abc-123")
    assert verifier.verify(header) == "abc-123"


def test_mismatched_secret_rejected() -> None:
    signer = GatewaySecret(secret=SECRET, replay_store=InMemoryReplayStore())
    verifier = GatewaySecret(secret=OTHER, replay_store=InMemoryReplayStore())
    header = signer.sign()
    with pytest.raises(InvalidGatewaySecret, match="signature"):
        verifier.verify(header)


def test_tampered_signature_rejected(signer: GatewaySecret, verifier: GatewaySecret) -> None:
    header = signer.sign()
    parts = header.split(":")
    # Flip one hex nibble in the signature
    bad_sig = parts[5][:-1] + ("0" if parts[5][-1] != "0" else "1")
    tampered = ":".join(parts[:5] + [bad_sig])
    with pytest.raises(InvalidGatewaySecret, match="signature"):
        verifier.verify(tampered)


def test_stale_timestamp_rejected(verifier: GatewaySecret) -> None:
    # Craft a header with ts = now - 120s, well outside the ±60s window.
    old_signer = GatewaySecret(
        secret=SECRET, replay_store=InMemoryReplayStore(), max_skew_ms=60_000
    )
    now_ms = int(time.time() * 1000)
    with patch(
        "ai_gateway_core.auth.gateway_secret._epoch_ms",
        return_value=now_ms - 120_000,
    ):
        header = old_signer.sign()
    with pytest.raises(InvalidGatewaySecret, match="skew"):
        verifier.verify(header)


def test_future_timestamp_rejected(verifier: GatewaySecret) -> None:
    future_signer = GatewaySecret(
        secret=SECRET, replay_store=InMemoryReplayStore(), max_skew_ms=60_000
    )
    now_ms = int(time.time() * 1000)
    with patch(
        "ai_gateway_core.auth.gateway_secret._epoch_ms",
        return_value=now_ms + 120_000,
    ):
        header = future_signer.sign()
    with pytest.raises(InvalidGatewaySecret, match="skew"):
        verifier.verify(header)


def test_missing_header_rejected(verifier: GatewaySecret) -> None:
    with pytest.raises(InvalidGatewaySecret, match="missing"):
        verifier.verify(None)
    with pytest.raises(InvalidGatewaySecret, match="missing"):
        verifier.verify("")


def test_malformed_header_rejected(verifier: GatewaySecret) -> None:
    with pytest.raises(InvalidGatewaySecret, match="malformed"):
        verifier.verify("not-a-valid-header")
    with pytest.raises(InvalidGatewaySecret, match="malformed"):
        verifier.verify("v2:rid:1234:abc")  # malformed v2
    with pytest.raises(InvalidGatewaySecret, match="version"):
        verifier.verify("v1:only:three")  # rejected downgrade


def test_replay_rejected(signer: GatewaySecret) -> None:
    shared_store = InMemoryReplayStore()
    verifier = GatewaySecret(secret=SECRET, replay_store=shared_store)
    header = signer.sign(request_id="replay-once")
    assert verifier.verify(header) == "replay-once"
    with pytest.raises(InvalidGatewaySecret, match="replay"):
        verifier.verify(header)


def test_replay_store_isolation_across_ids(signer: GatewaySecret) -> None:
    shared_store = InMemoryReplayStore()
    verifier = GatewaySecret(secret=SECRET, replay_store=shared_store)
    assert verifier.verify(signer.sign(request_id="one")) == "one"
    assert verifier.verify(signer.sign(request_id="two")) == "two"


def test_secret_length_guard() -> None:
    with pytest.raises(ValueError, match="16 chars"):
        GatewaySecret(secret="short")
    with pytest.raises(ValueError, match="16 chars"):
        GatewaySecret(secret="")


def test_forged_header_does_not_poison_replay_store() -> None:
    """A signature-invalid header must not record its request_id.

    Otherwise an attacker could pre-burn legitimate request_ids by
    submitting forged headers with the same rid — causing legit
    requests from the real gateway to be rejected as replays.
    """
    shared_store = InMemoryReplayStore()
    verifier = GatewaySecret(secret=SECRET, replay_store=shared_store)

    # Forge a header with a valid ts but a garbage signature
    ts = str(int(time.time() * 1000))
    forged = f"v1:hot-rid:{ts}:{'f' * 64}"
    with pytest.raises(InvalidGatewaySecret):
        verifier.verify(forged)

    # Legit request with the same rid must still succeed
    legit_signer = GatewaySecret(secret=SECRET, replay_store=InMemoryReplayStore())
    legit_header = legit_signer.sign(request_id="hot-rid")
    assert verifier.verify(legit_header) == "hot-rid"


def test_v2_signature_binds_method_path_query_and_body() -> None:
    signer = GatewaySecret(
        secret=SECRET,
        version="v2",
        key_id="local",
        keys={"local": SECRET},
        replay_store=InMemoryReplayStore(),
    )
    verifier = GatewaySecret(
        secret=SECRET,
        version="v2",
        key_id="local",
        keys={"local": SECRET},
        replay_store=InMemoryReplayStore(),
    )

    body = b'{"query":"hello"}'
    header = signer.sign(
        request_id="bound-request",
        method="POST",
        path="/api/v1/knowledge/datasets/demo/retrieve",
        query="top_k=5",
        body=body,
    )

    assert (
        verifier.verify(
            header,
            method="POST",
            path="/api/v1/knowledge/datasets/demo/retrieve",
            query="top_k=5",
            body=body,
        )
        == "bound-request"
    )

    with pytest.raises(InvalidGatewaySecret, match="signature"):
        verifier.verify(
            header,
            method="POST",
            path="/api/v1/assistant/chat/stream",
            query="top_k=5",
            body=body,
        )


def test_v2_signature_binds_forwarded_identity_headers() -> None:
    signer = GatewaySecret(
        secret=SECRET,
        version="v2",
        replay_store=InMemoryReplayStore(),
    )
    verifier = GatewaySecret(
        secret=SECRET,
        version="v2",
        replay_store=InMemoryReplayStore(),
    )
    identity = {"X-User-Id": "user-a", "X-Tenant-Id": "tenant-a", "X-User-Roles": "user"}
    header = signer.sign(
        request_id="identity-bound",
        method="POST",
        path="/api/v1/assistant/chat",
        body=b"{}",
        identity_headers=identity,
    )

    assert verifier.verify(
        header,
        method="POST",
        path="/api/v1/assistant/chat",
        body=b"{}",
        identity_headers=identity,
    ) == "identity-bound"

    with pytest.raises(InvalidGatewaySecret, match="signature"):
        verifier.verify(
            header,
            method="POST",
            path="/api/v1/assistant/chat",
            body=b"{}",
            identity_headers={**identity, "X-Tenant-Id": "tenant-b"},
        )


def test_v1_header_is_rejected_by_v2_verifier() -> None:
    legacy = GatewaySecret(
        secret=SECRET,
        version="v1",
        replay_store=InMemoryReplayStore(),
    )
    verifier = GatewaySecret(
        secret=SECRET,
        version="v2",
        replay_store=InMemoryReplayStore(),
    )

    with pytest.raises(InvalidGatewaySecret, match="version"):
        verifier.verify(legacy.sign(request_id="legacy"))


def test_v2_signature_uses_key_id_for_rotation() -> None:
    keys = {"previous": OTHER, "active": SECRET}
    signer = GatewaySecret(
        secret=SECRET,
        version="v2",
        key_id="active",
        keys=keys,
        replay_store=InMemoryReplayStore(),
    )
    verifier = GatewaySecret(
        secret=OTHER,
        version="v2",
        key_id="previous",
        keys=keys,
        replay_store=InMemoryReplayStore(),
    )

    header = signer.sign(
        request_id="rotated",
        method="GET",
        path="/health/ready",
        query="",
        body=b"",
    )

    assert (
        verifier.verify(
            header,
            method="GET",
            path="/health/ready",
            query="",
            body=b"",
        )
        == "rotated"
    )


def test_v2_key_scope_binds_caller_audience_and_path() -> None:
    kwargs = {
        "secret": SECRET,
        "version": "v2",
        "key_id": "active",
        "keys": {"active": SECRET, "previous": OTHER},
        "allowed_path_prefixes": ("/api/v1/knowledge",),
    }
    signer = GatewaySecret(
        **kwargs,
        caller_service="gateway",
        audience="knowledge-service",
        replay_store=InMemoryReplayStore(),
    )
    header = signer.sign(
        request_id="service-bound",
        method="POST",
        path="/api/v1/knowledge/dataset-a/retrieve",
        body=b"{}",
    )
    verifier = GatewaySecret(
        **kwargs,
        caller_service="gateway",
        audience="knowledge-service",
        replay_store=InMemoryReplayStore(),
    )
    assert verifier.verify(
        header,
        method="POST",
        path="/api/v1/knowledge/dataset-a/retrieve",
        body=b"{}",
    ) == "service-bound"

    wrong_audience = GatewaySecret(
        **kwargs,
        caller_service="gateway",
        audience="agent-runtime",
        replay_store=InMemoryReplayStore(),
    )
    with pytest.raises(InvalidGatewaySecret, match="signature"):
        wrong_audience.verify(
            header,
            method="POST",
            path="/api/v1/knowledge/dataset-a/retrieve",
            body=b"{}",
        )
    with pytest.raises(InvalidGatewaySecret, match="path outside key scope"):
        signer.sign(method="POST", path="/api/v1/assistant/chat", body=b"{}")


class _FakeRedis:
    def __init__(self) -> None:
        self.values: dict[str, str] = {}
        self.calls: list[tuple[str, int | None]] = []

    def set(self, key: str, value: str, *, nx: bool = False, px: int | None = None):
        self.calls.append((key, px))
        if nx and key in self.values:
            return False
        self.values[key] = value
        return True


def test_redis_replay_store_uses_atomic_set_nx_with_ttl() -> None:
    redis = _FakeRedis()
    store = RedisReplayStore(redis, prefix="test:replay")

    assert store.seen_or_record("rid-1", ttl_ms=12_000) is False
    assert store.seen_or_record("rid-1", ttl_ms=12_000) is True
    assert redis.calls == [
        ("test:replay:rid-1", 12_000),
        ("test:replay:rid-1", 12_000),
    ]
