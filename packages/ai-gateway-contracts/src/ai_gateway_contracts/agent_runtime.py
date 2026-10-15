"""Canonical Agent Runtime Snapshot and signed Envelope.

The browser-facing request never carries trusted model, prompt, capability, or
Snapshot fields. The Gateway resolves those fields, signs this closed Envelope,
and the Assistant independently recalculates hashes before atomically consuming
the nonce. Ordinary service-to-service authentication remains a separate layer.
"""

from __future__ import annotations

import copy
import hmac
import json
import re
import secrets
import time
from dataclasses import dataclass
from hashlib import sha256
from typing import Any, Final

from .replay import InMemoryReplayStore, ReplayStore

AGENT_RUNTIME_SCHEMA_VERSION: Final = "agent-runtime/v1"
AGENT_RUNTIME_ENVELOPE_SCHEMA_VERSION: Final = "agent-runtime-envelope/v1"

_SNAPSHOT_KEYS: Final = frozenset(
    {
        "schema_version",
        "tenant_id",
        "agent_id",
        "agent_version_id",
        "publication",
        "model",
        "instructions",
        "capabilities",
        "knowledge",
        "memory",
        "channel_policy",
        "fingerprints",
    }
)
_PUBLICATION_KEYS: Final = frozenset({"id", "channel", "auth_mode"})
_MODEL_KEYS: Final = frozenset({"id", "provider", "parameters"})
_INSTRUCTION_KEYS: Final = frozenset({"agent", "prompt_hash"})
_CAPABILITY_KEYS: Final = frozenset({"type", "id", "version", "schema_hash", "risk", "config"})
_KNOWLEDGE_KEYS: Final = frozenset({"datasets", "retrieval"})
_MEMORY_KEYS: Final = frozenset({"mode"})
_CHANNEL_POLICY_KEYS: Final = frozenset({"attachments", "high_risk_tools", "allowed_origins"})
_FINGERPRINT_KEYS: Final = frozenset({"spec", "tool_schema", "skills", "knowledge_revision"})
_ENVELOPE_KEYS: Final = frozenset(
    {
        "schema_version",
        "issuer",
        "tenant_id",
        "caller_principal",
        "agent_id",
        "agent_version_id",
        "draft_revision",
        "publication_id",
        "channel",
        "session_id",
        "resolved_snapshot",
        "request_body_hash",
        "snapshot_hash",
        "spec_hash",
        "issued_at_ms",
        "expires_at_ms",
        "nonce",
        "signature",
    }
)
_CHANNELS: Final = frozenset({"preview", "hosted", "embed", "api", "builtin"})
_AUTH_MODES: Final = frozenset({"private", "tenant", "public", "token"})
_CAPABILITY_TYPES: Final = frozenset({"platform", "mcp", "skill", "connector"})
_RISKS: Final = frozenset({"low", "medium", "high", "critical"})
_MEMORY_MODES: Final = frozenset({"off", "session", "user"})
_SENSITIVE_KEYS: Final = frozenset(
    {
        "apikey",
        "apitoken",
        "authorization",
        "bearertoken",
        "clientsecret",
        "credential",
        "credentialref",
        "password",
        "privatekey",
        "refreshtoken",
        "secret",
        "secretref",
        "tokenhash",
        "tokenref",
    }
)


def agent_memory_principal(
    caller_principal: str,
    agent_id: str,
    version_scope: str,
) -> str:
    """Build an opaque Agent memory principal that fits VARCHAR(64) stores."""

    digest = sha256(f"{caller_principal}:{agent_id}:{version_scope}".encode()).hexdigest()
    return f"am_{digest[:61]}"


class AgentRuntimeEnvelopeError(ValueError):
    """Stable fail-closed error raised for every Envelope verification failure."""

    def __init__(self, code: str):
        self.code = code
        super().__init__(code)


@dataclass(frozen=True)
class VerifiedAgentRuntime:
    """Immutable, verified execution context returned to Assistant code."""

    tenant_id: str
    caller_principal: str
    agent_id: str
    agent_version_id: str | None
    draft_revision: int | None
    publication_id: str | None
    channel: str
    session_id: str
    runtime_fingerprint: str
    spec_hash: str
    capability_ids: frozenset[str]
    resolved_snapshot: dict[str, Any]

    def dimensions(self) -> dict[str, Any]:
        return {
            "agent_id": self.agent_id,
            "agent_version_id": self.agent_version_id,
            "agent_draft_revision": self.draft_revision,
            "publication_id": self.publication_id,
            "channel": self.channel,
            "runtime_fingerprint": self.runtime_fingerprint,
            "agent_spec_hash": self.spec_hash,
        }


def canonical_runtime_json(value: Any) -> str:
    """Return the deterministic UTF-8 JSON representation used for HMACs."""

    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        )
    except (TypeError, ValueError) as exc:
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_CANONICALIZATION_FAILED") from exc


def runtime_sha256(value: Any) -> str:
    digest = sha256(canonical_runtime_json(value).encode("utf-8")).hexdigest()
    return f"sha256:{digest}"


def _canonical_key(value: Any) -> str:
    return re.sub(r"[^a-z0-9]", "", str(value).lower())


def _reject_sensitive_keys(value: Any) -> None:
    if isinstance(value, dict):
        for key, child in value.items():
            if _canonical_key(key) in _SENSITIVE_KEYS:
                raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SECRET_FORBIDDEN")
            _reject_sensitive_keys(child)
    elif isinstance(value, list):
        for child in value:
            _reject_sensitive_keys(child)


def _closed_mapping(value: Any, keys: frozenset[str], code: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != set(keys):
        raise AgentRuntimeEnvelopeError(code)
    return value


def _non_empty_string(value: Any, code: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise AgentRuntimeEnvelopeError(code)
    return value


def _optional_string(value: Any, code: str) -> str | None:
    if value is None:
        return None
    return _non_empty_string(value, code)


def _validate_snapshot(snapshot: Any) -> dict[str, Any]:
    snapshot = _closed_mapping(snapshot, _SNAPSHOT_KEYS, "AGENT_RUNTIME_SNAPSHOT_INVALID")
    if snapshot["schema_version"] != AGENT_RUNTIME_SCHEMA_VERSION:
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_VERSION_UNSUPPORTED")
    _non_empty_string(snapshot["tenant_id"], "AGENT_RUNTIME_SNAPSHOT_INVALID")
    _non_empty_string(snapshot["agent_id"], "AGENT_RUNTIME_SNAPSHOT_INVALID")
    _optional_string(snapshot["agent_version_id"], "AGENT_RUNTIME_SNAPSHOT_INVALID")

    publication = _closed_mapping(
        snapshot["publication"],
        _PUBLICATION_KEYS,
        "AGENT_RUNTIME_SNAPSHOT_INVALID",
    )
    _optional_string(publication["id"], "AGENT_RUNTIME_SNAPSHOT_INVALID")
    if publication["channel"] not in _CHANNELS:
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")
    if publication["auth_mode"] not in _AUTH_MODES:
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")

    model = _closed_mapping(snapshot["model"], _MODEL_KEYS, "AGENT_RUNTIME_SNAPSHOT_INVALID")
    _non_empty_string(model["id"], "AGENT_RUNTIME_SNAPSHOT_INVALID")
    _non_empty_string(model["provider"], "AGENT_RUNTIME_SNAPSHOT_INVALID")
    if not isinstance(model["parameters"], dict):
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")

    instructions = _closed_mapping(
        snapshot["instructions"],
        _INSTRUCTION_KEYS,
        "AGENT_RUNTIME_SNAPSHOT_INVALID",
    )
    if not isinstance(instructions["agent"], str):
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")
    _non_empty_string(instructions["prompt_hash"], "AGENT_RUNTIME_SNAPSHOT_INVALID")

    capabilities = snapshot["capabilities"]
    if not isinstance(capabilities, list):
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")
    capability_ids: set[str] = set()
    for raw in capabilities:
        item = _closed_mapping(raw, _CAPABILITY_KEYS, "AGENT_RUNTIME_SNAPSHOT_INVALID")
        if item["type"] not in _CAPABILITY_TYPES or item["risk"] not in _RISKS:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")
        capability_id = _non_empty_string(item["id"], "AGENT_RUNTIME_SNAPSHOT_INVALID")
        if capability_id in capability_ids:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")
        capability_ids.add(capability_id)
        _optional_string(item["version"], "AGENT_RUNTIME_SNAPSHOT_INVALID")
        _optional_string(item["schema_hash"], "AGENT_RUNTIME_SNAPSHOT_INVALID")
        if not isinstance(item["config"], dict):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")

    knowledge = _closed_mapping(
        snapshot["knowledge"],
        _KNOWLEDGE_KEYS,
        "AGENT_RUNTIME_SNAPSHOT_INVALID",
    )
    if not isinstance(knowledge["datasets"], list) or not all(
        isinstance(dataset_id, str) and dataset_id for dataset_id in knowledge["datasets"]
    ):
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")
    if not isinstance(knowledge["retrieval"], dict):
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")

    memory = _closed_mapping(snapshot["memory"], _MEMORY_KEYS, "AGENT_RUNTIME_SNAPSHOT_INVALID")
    if memory["mode"] not in _MEMORY_MODES:
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")

    channel_policy = _closed_mapping(
        snapshot["channel_policy"],
        _CHANNEL_POLICY_KEYS,
        "AGENT_RUNTIME_SNAPSHOT_INVALID",
    )
    if not isinstance(channel_policy["attachments"], bool) or not isinstance(
        channel_policy["high_risk_tools"], bool
    ):
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")
    if not isinstance(channel_policy["allowed_origins"], list) or not all(
        isinstance(origin, str) for origin in channel_policy["allowed_origins"]
    ):
        raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_INVALID")

    fingerprints = _closed_mapping(
        snapshot["fingerprints"],
        _FINGERPRINT_KEYS,
        "AGENT_RUNTIME_SNAPSHOT_INVALID",
    )
    for value in fingerprints.values():
        _non_empty_string(value, "AGENT_RUNTIME_SNAPSHOT_INVALID")

    _reject_sensitive_keys(snapshot)
    return snapshot


class AgentRuntimeSigner:
    """Create and verify request-bound Agent Runtime Envelopes."""

    def __init__(
        self,
        *,
        secret: str,
        issuer: str = "ai-gateway",
        replay_store: ReplayStore | None = None,
        max_ttl_ms: int = 60_000,
        max_clock_skew_ms: int = 5_000,
    ) -> None:
        if not secret or len(secret) < 16:
            raise ValueError("Agent Runtime signing secret must be at least 16 chars")
        if not issuer or ":" in issuer:
            raise ValueError("Agent Runtime issuer must be non-empty and colon-free")
        if max_ttl_ms <= 0 or max_clock_skew_ms < 0:
            raise ValueError("Agent Runtime time bounds must be positive")
        self._secret = secret
        self.issuer = issuer
        self.replay_store = replay_store or InMemoryReplayStore()
        self.max_ttl_ms = int(max_ttl_ms)
        self.max_clock_skew_ms = int(max_clock_skew_ms)

    def sign(
        self,
        *,
        tenant_id: str,
        caller_principal: str,
        agent_id: str,
        agent_version_id: str | None,
        draft_revision: int | None,
        publication_id: str | None,
        channel: str,
        session_id: str,
        resolved_snapshot: dict[str, Any],
        request_body: dict[str, Any],
        spec_hash: str,
        issued_at_ms: int | None = None,
        expires_at_ms: int | None = None,
        nonce: str | None = None,
    ) -> dict[str, Any]:
        snapshot = copy.deepcopy(_validate_snapshot(copy.deepcopy(resolved_snapshot)))
        issued = int(issued_at_ms if issued_at_ms is not None else time.time() * 1000)
        expires = int(expires_at_ms if expires_at_ms is not None else issued + 30_000)
        envelope: dict[str, Any] = {
            "schema_version": AGENT_RUNTIME_ENVELOPE_SCHEMA_VERSION,
            "issuer": self.issuer,
            "tenant_id": _non_empty_string(tenant_id, "AGENT_RUNTIME_IDENTITY_INVALID"),
            "caller_principal": _non_empty_string(
                caller_principal, "AGENT_RUNTIME_IDENTITY_INVALID"
            ),
            "agent_id": _non_empty_string(agent_id, "AGENT_RUNTIME_IDENTITY_INVALID"),
            "agent_version_id": _optional_string(
                agent_version_id, "AGENT_RUNTIME_IDENTITY_INVALID"
            ),
            "draft_revision": draft_revision,
            "publication_id": _optional_string(publication_id, "AGENT_RUNTIME_IDENTITY_INVALID"),
            "channel": channel,
            "session_id": _non_empty_string(session_id, "AGENT_RUNTIME_IDENTITY_INVALID"),
            "resolved_snapshot": snapshot,
            "request_body_hash": runtime_sha256(request_body),
            "snapshot_hash": runtime_sha256(snapshot),
            "spec_hash": _non_empty_string(spec_hash, "AGENT_RUNTIME_IDENTITY_INVALID"),
            "issued_at_ms": issued,
            "expires_at_ms": expires,
            "nonce": nonce or secrets.token_urlsafe(24),
        }
        # Signing validates structure/TTL against the chosen issue instant.
        # Freshness against the receiver's wall clock is enforced in verify().
        self._validate_unsigned(envelope, now_ms=issued)
        envelope["signature"] = self._signature(envelope)
        return envelope

    def verify(
        self,
        envelope: dict[str, Any],
        *,
        request_body: dict[str, Any],
        expected_tenant_id: str,
        expected_caller_principal: str,
        expected_session_id: str,
        now_ms: int | None = None,
    ) -> VerifiedAgentRuntime:
        if not isinstance(envelope, dict) or set(envelope) != set(_ENVELOPE_KEYS):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ENVELOPE_INVALID")
        unsigned = {
            key: copy.deepcopy(value) for key, value in envelope.items() if key != "signature"
        }
        self._validate_unsigned(unsigned, now_ms=now_ms)

        snapshot_hash = runtime_sha256(unsigned["resolved_snapshot"])
        if not hmac.compare_digest(snapshot_hash, str(unsigned["snapshot_hash"])):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SNAPSHOT_HASH_MISMATCH")
        body_hash = runtime_sha256(request_body)
        if not hmac.compare_digest(body_hash, str(unsigned["request_body_hash"])):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_BODY_HASH_MISMATCH")

        signature = str(envelope.get("signature") or "")
        expected_signature = self._signature(unsigned)
        if not hmac.compare_digest(signature, expected_signature):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SIGNATURE_INVALID")

        if unsigned["issuer"] != self.issuer:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ISSUER_INVALID")
        if unsigned["tenant_id"] != expected_tenant_id:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_TENANT_MISMATCH")
        if unsigned["caller_principal"] != expected_caller_principal:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_CALLER_MISMATCH")
        if unsigned["session_id"] != expected_session_id:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_SESSION_MISMATCH")

        snapshot = unsigned["resolved_snapshot"]
        publication = snapshot["publication"]
        if (
            snapshot["tenant_id"] != unsigned["tenant_id"]
            or snapshot["agent_id"] != unsigned["agent_id"]
            or snapshot["agent_version_id"] != unsigned["agent_version_id"]
            or publication["id"] != unsigned["publication_id"]
            or publication["channel"] != unsigned["channel"]
            or snapshot["fingerprints"]["spec"] != unsigned["spec_hash"]
        ):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_IDENTITY_MISMATCH")

        now = int(now_ms if now_ms is not None else time.time() * 1000)
        ttl_ms = max(1, int(unsigned["expires_at_ms"]) - now + self.max_clock_skew_ms)
        replay_key = ":".join(
            [
                str(unsigned["issuer"]),
                str(unsigned["tenant_id"]),
                str(unsigned["nonce"]),
            ]
        )
        try:
            replayed = self.replay_store.seen_or_record(replay_key, ttl_ms)
        except Exception as exc:  # noqa: BLE001
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_REPLAY_STORE_UNAVAILABLE") from exc
        if replayed:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_REPLAYED")

        capability_ids = frozenset(str(item["id"]) for item in snapshot["capabilities"])
        return VerifiedAgentRuntime(
            tenant_id=str(unsigned["tenant_id"]),
            caller_principal=str(unsigned["caller_principal"]),
            agent_id=str(unsigned["agent_id"]),
            agent_version_id=unsigned["agent_version_id"],
            draft_revision=unsigned["draft_revision"],
            publication_id=unsigned["publication_id"],
            channel=str(unsigned["channel"]),
            session_id=str(unsigned["session_id"]),
            runtime_fingerprint=str(unsigned["snapshot_hash"]),
            spec_hash=str(unsigned["spec_hash"]),
            capability_ids=capability_ids,
            resolved_snapshot=copy.deepcopy(snapshot),
        )

    def _validate_unsigned(
        self,
        envelope: dict[str, Any],
        *,
        now_ms: int | None = None,
    ) -> None:
        if set(envelope) != set(_ENVELOPE_KEYS - {"signature"}):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ENVELOPE_INVALID")
        if envelope["schema_version"] != AGENT_RUNTIME_ENVELOPE_SCHEMA_VERSION:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ENVELOPE_VERSION_UNSUPPORTED")
        for key in (
            "issuer",
            "tenant_id",
            "caller_principal",
            "agent_id",
            "session_id",
            "spec_hash",
            "nonce",
        ):
            _non_empty_string(envelope[key], "AGENT_RUNTIME_ENVELOPE_INVALID")
        _optional_string(envelope["agent_version_id"], "AGENT_RUNTIME_ENVELOPE_INVALID")
        _optional_string(envelope["publication_id"], "AGENT_RUNTIME_ENVELOPE_INVALID")
        if envelope["channel"] not in _CHANNELS:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ENVELOPE_INVALID")
        draft_revision = envelope["draft_revision"]
        if draft_revision is not None and (
            not isinstance(draft_revision, int) or draft_revision < 1
        ):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ENVELOPE_INVALID")
        if envelope["channel"] == "preview":
            has_draft_pin = draft_revision is not None
            has_version_pin = envelope["agent_version_id"] is not None
            if (
                has_draft_pin == has_version_pin
                or envelope["publication_id"] is not None
            ):
                raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ENVELOPE_INVALID")
        elif envelope["channel"] == "builtin":
            if (
                draft_revision is not None
                or envelope["agent_version_id"] is not None
                or envelope["publication_id"] is not None
            ):
                raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ENVELOPE_INVALID")
        elif (
            draft_revision is not None
            or envelope["agent_version_id"] is None
            or envelope["publication_id"] is None
        ):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ENVELOPE_INVALID")
        if not isinstance(envelope["issued_at_ms"], int) or not isinstance(
            envelope["expires_at_ms"], int
        ):
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_ENVELOPE_INVALID")
        issued = envelope["issued_at_ms"]
        expires = envelope["expires_at_ms"]
        if expires <= issued or expires - issued > self.max_ttl_ms:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_TTL_INVALID")
        now = int(now_ms if now_ms is not None else time.time() * 1000)
        if now < issued - self.max_clock_skew_ms:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_NOT_YET_VALID")
        if now > expires:
            raise AgentRuntimeEnvelopeError("AGENT_RUNTIME_EXPIRED")
        _non_empty_string(envelope["request_body_hash"], "AGENT_RUNTIME_ENVELOPE_INVALID")
        _non_empty_string(envelope["snapshot_hash"], "AGENT_RUNTIME_ENVELOPE_INVALID")
        _validate_snapshot(envelope["resolved_snapshot"])

    def _signature(self, envelope: dict[str, Any]) -> str:
        unsigned = {key: value for key, value in envelope.items() if key != "signature"}
        digest = hmac.new(
            self._secret.encode("utf-8"),
            canonical_runtime_json(unsigned).encode("utf-8"),
            sha256,
        ).hexdigest()
        return f"sha256:{digest}"
