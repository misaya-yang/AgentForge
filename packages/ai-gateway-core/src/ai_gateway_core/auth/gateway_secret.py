"""Gateway → microservice HMAC signing (``X-Gateway-Secret``).

Design reference: ``plans/SystemDesign-Assistant-Service-True-Extraction-Phase5-2026-04-23.md`` §4.5.

Trust model — only the **middle hop** is secured by this module; the outer
(browser→gateway) is JWT/API-key, the inner (as→DB) is DSN-level.

Header format
-------------
One header, one value::

    X-Gateway-Secret: v2:<key_id>:<request_id>:<epoch_ms>:<body_hash>:<hmac_hex>

Verification rules
------------------
1. Shape: 6 colon-separated segments with ``v2`` prefix.
2. Signature binds method, path, query, body hash, trusted identity headers,
   request ID, timestamp, and key ID.
3. Timestamp must be within ``max_skew_ms`` of the verifier's clock
   (default ±60s).
4. Replay protection: ``request_id`` is remembered in a seen-ids store
   with a TTL slightly longer than ``max_skew_ms`` so a freshly signed
   request can't be replayed within its validity window.

Replay store
------------
``InMemoryReplayStore`` is process-local. For multi-worker / multi-instance
deployments, swap in a Redis-backed impl (same ``ReplayStore`` protocol).
"""
from __future__ import annotations

import hmac
import logging
import os
import secrets
import time
from collections.abc import Mapping
from dataclasses import dataclass
from hashlib import sha256

from ai_gateway_contracts.replay import InMemoryReplayStore, ReplayStore

logger = logging.getLogger(__name__)

_SIG_PREFIX = "v1"
_DEFAULT_MAX_SKEW_MS = 60_000
_DEFAULT_REPLAY_TTL_MS = 120_000  # 2× max skew — covers worst-case window
_SIGNED_IDENTITY_HEADERS = frozenset(
    {
        "x-user-id",
        "x-tenant-id",
        "x-user-tier",
        "x-user-type",
        "x-user-roles",
        "x-user-email",
        "x-user-name",
        "x-app-user-id",
        "x-app-tenant-id",
    }
)


class InvalidGatewaySecret(Exception):
    """Raised when ``X-Gateway-Secret`` fails verification.

    Middleware translates this to ``HTTP 401`` so clients never see the
    underlying reason (prevents oracle-style probing of the secret).
    """


# ``ReplayStore`` and ``InMemoryReplayStore`` moved to
# ``ai_gateway_contracts.replay`` (ARC-04 first batch, 2026-08-29) and are
# re-imported above; the public names stay available from this module.
# Removal conditions match the other ARC-04 shims: delete the re-export once
# the ARC-08 external compatibility window closes and the generated inventory
# shows no repository consumers other than ``ai_gateway_core.auth`` and the
# shim identity contract. Remove both facade exports and the identity-test row
# in the same change; ``GatewaySecret`` and ``RedisReplayStore`` remain here.


class RedisReplayStore:
    """Redis-backed replay store for multi-worker / multi-replica services.

    The operation is a single ``SET key value NX PX ttl`` call, so two workers
    racing on the same request id share one replay decision.
    """

    def __init__(
        self,
        redis_client,
        *,
        prefix: str = "ai-gateway:internal:replay",
    ) -> None:
        self._redis = redis_client
        self._prefix = prefix.rstrip(":")

    @classmethod
    def from_url(
        cls,
        redis_url: str,
        *,
        prefix: str = "ai-gateway:internal:replay",
    ) -> RedisReplayStore:
        try:
            import redis
        except ImportError as exc:  # pragma: no cover - dependency declared by package
            raise RuntimeError("redis package is required for RedisReplayStore") from exc
        return cls(
            redis.Redis.from_url(redis_url, decode_responses=True),
            prefix=prefix,
        )

    def seen_or_record(self, request_id: str, ttl_ms: int) -> bool:
        key = f"{self._prefix}:{request_id}"
        try:
            recorded = self._redis.set(key, "1", nx=True, px=ttl_ms)
        except Exception as exc:  # noqa: BLE001
            raise RuntimeError("redis replay store unavailable") from exc
        return not bool(recorded)


@dataclass
class GatewaySecret:
    """Bi-directional HMAC ``X-Gateway-Secret`` handler.

    Gateway and private services construct this with the same platform
    internal token.
    """

    secret: str
    max_skew_ms: int = _DEFAULT_MAX_SKEW_MS
    replay_ttl_ms: int = _DEFAULT_REPLAY_TTL_MS
    replay_store: ReplayStore | None = None
    header_name: str = "X-Gateway-Secret"
    version: str | None = None
    key_id: str | None = None
    keys: dict[str, str] | None = None
    caller_service: str = "gateway"
    audience: str = "internal"
    allowed_path_prefixes: tuple[str, ...] = ()

    def __post_init__(self) -> None:
        if not self.secret or len(self.secret) < 16:
            raise ValueError("internal service secret must be at least 16 chars")
        self.version = (
            self.version or "v2"
        ).strip().lower()
        if self.version not in {"v1", "v2"}:
            raise ValueError("INTERNAL_AUTH_VERSION must be 'v1' or 'v2'")
        self.caller_service = _validated_binding("caller_service", self.caller_service)
        self.audience = _validated_binding("audience", self.audience)
        self.allowed_path_prefixes = tuple(
            _validated_path_prefix(value) for value in self.allowed_path_prefixes
        )

        env_keys = _parse_internal_auth_keys(os.getenv("INTERNAL_AUTH_KEYS", ""))
        if self.keys is None:
            self.keys = env_keys or {}
        self.key_id = (
            self.key_id
            or os.getenv("INTERNAL_AUTH_ACTIVE_KEY_ID", "").strip()
            or "local"
        )
        if not self.keys:
            self.keys = {self.key_id: self.secret}
        elif self.key_id not in self.keys:
            # Keep startup deterministic: if the active id is wrong, fall back
            # to the first configured key rather than signing unverifiable
            # headers with the legacy single-secret value.
            self.key_id = next(iter(self.keys))

        for kid, value in self.keys.items():
            if not kid or ":" in kid:
                raise ValueError(
                    "internal auth key ids must be non-empty and colon-free"
                )
            if not value or len(value) < 16:
                raise ValueError("internal auth secrets must be at least 16 chars")

        if self.replay_store is None:
            self.replay_store = _default_replay_store()

    # ----- sign -----

    def sign(
        self,
        request_id: str | None = None,
        *,
        method: str | None = None,
        path: str | None = None,
        query: str | None = "",
        body: bytes | str | None = None,
        identity_headers: Mapping[str, str] | None = None,
    ) -> str:
        """Produce a fresh header value. ``request_id`` auto-generated if omitted."""
        rid = request_id or secrets.token_hex(16)
        ts = str(_epoch_ms())
        if self.version == "v2":
            self._require_allowed_path(path or "")
            kid = self.key_id or "local"
            body_hash = _body_sha256(body)
            sig = self._hmac_v2(
                key_id=kid,
                request_id=rid,
                ts=ts,
                method=method or "",
                path=path or "",
                query=query or "",
                body_hash=body_hash,
                identity_headers=identity_headers,
            )
            return f"v2:{kid}:{rid}:{ts}:{body_hash}:{sig}"

        sig = self._hmac(rid, ts)
        return f"{_SIG_PREFIX}:{rid}:{ts}:{sig}"

    # ----- verify -----

    def verify(
        self,
        header_value: str | None,
        *,
        method: str | None = None,
        path: str | None = None,
        query: str | None = "",
        body: bytes | str | None = None,
        identity_headers: Mapping[str, str] | None = None,
    ) -> str:
        """Verify ``header_value``. Returns the ``request_id`` on success.

        Raises ``InvalidGatewaySecret`` for any failure mode. The middleware
        translates that to ``401 Unauthorized``.
        """
        if not header_value:
            raise InvalidGatewaySecret("missing header")

        parts = header_value.split(":")
        if not parts:
            raise InvalidGatewaySecret("malformed header")
        if parts[0] not in {"v1", "v2"}:
            raise InvalidGatewaySecret("malformed header")
        if parts[0] != self.version:
            raise InvalidGatewaySecret("version mismatch")
        if parts[0] == "v2":
            return self._verify_v2(
                parts,
                method=method or "",
                path=path or "",
                query=query or "",
                body=body,
                identity_headers=identity_headers,
            )
        if len(parts) != 4 or parts[0] != _SIG_PREFIX:
            raise InvalidGatewaySecret("malformed header")

        _, rid, ts_str, sig = parts
        try:
            ts = int(ts_str)
        except ValueError as e:
            raise InvalidGatewaySecret("bad timestamp") from e

        now = _epoch_ms()
        if abs(now - ts) > self.max_skew_ms:
            raise InvalidGatewaySecret("timestamp skew")

        expected = self._hmac(rid, ts_str)
        if not hmac.compare_digest(expected, sig):
            raise InvalidGatewaySecret("signature mismatch")

        # Replay protection — must happen AFTER signature validation so
        # a forged header can't populate the seen set and DoS legit
        # request_ids.
        assert self.replay_store is not None  # post_init ensures this
        try:
            replayed = self.replay_store.seen_or_record(rid, self.replay_ttl_ms)
        except Exception as exc:  # noqa: BLE001
            raise InvalidGatewaySecret("replay store unavailable") from exc
        if replayed:
            raise InvalidGatewaySecret("replay detected")

        return rid

    # ----- internals -----

    def _hmac(self, request_id: str, ts: str) -> str:
        mac = hmac.new(
            self.secret.encode("utf-8"),
            f"{request_id}:{ts}".encode(),
            sha256,
        )
        return mac.hexdigest()

    def _verify_v2(
        self,
        parts: list[str],
        *,
        method: str,
        path: str,
        query: str,
        body: bytes | str | None,
        identity_headers: Mapping[str, str] | None,
    ) -> str:
        self._require_allowed_path(path)
        if len(parts) != 6:
            raise InvalidGatewaySecret("malformed header")
        _, key_id, rid, ts_str, body_hash, sig = parts
        try:
            ts = int(ts_str)
        except ValueError as e:
            raise InvalidGatewaySecret("bad timestamp") from e

        now = _epoch_ms()
        if abs(now - ts) > self.max_skew_ms:
            raise InvalidGatewaySecret("timestamp skew")

        expected_body_hash = _body_sha256(body)
        if not hmac.compare_digest(expected_body_hash, body_hash):
            raise InvalidGatewaySecret("body hash mismatch")

        expected = self._hmac_v2(
            key_id=key_id,
            request_id=rid,
            ts=ts_str,
            method=method,
            path=path,
            query=query,
            body_hash=body_hash,
            identity_headers=identity_headers,
        )
        if not hmac.compare_digest(expected, sig):
            raise InvalidGatewaySecret("signature mismatch")

        assert self.replay_store is not None
        try:
            replayed = self.replay_store.seen_or_record(rid, self.replay_ttl_ms)
        except Exception as exc:  # noqa: BLE001
            raise InvalidGatewaySecret("replay store unavailable") from exc
        if replayed:
            raise InvalidGatewaySecret("replay detected")
        return rid

    def _hmac_v2(
        self,
        *,
        key_id: str,
        request_id: str,
        ts: str,
        method: str,
        path: str,
        query: str,
        body_hash: str,
        identity_headers: Mapping[str, str] | None,
    ) -> str:
        keys = self.keys or {}
        secret = keys.get(key_id)
        if not secret:
            raise InvalidGatewaySecret("unknown key id")
        canonical = "\n".join(
            [
                self.caller_service,
                self.audience,
                method.upper(),
                path,
                query,
                body_hash,
                _canonical_identity_headers(identity_headers),
                request_id,
                ts,
                key_id,
            ]
        )
        mac = hmac.new(secret.encode("utf-8"), canonical.encode("utf-8"), sha256)
        return mac.hexdigest()

    def _require_allowed_path(self, path: str) -> None:
        if self.allowed_path_prefixes and not any(
            _path_matches_prefix(path, prefix) for prefix in self.allowed_path_prefixes
        ):
            raise InvalidGatewaySecret("path outside key scope")


def _validated_binding(name: str, value: str) -> str:
    normalized = str(value or "").strip()
    if (
        not normalized
        or len(normalized) > 64
        or any(character not in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_." for character in normalized)
    ):
        raise ValueError(f"{name} must be a non-empty service identifier")
    return normalized


def _validated_path_prefix(value: str) -> str:
    normalized = str(value or "").strip().rstrip("/") or "/"
    if not normalized.startswith("/") or any(character in normalized for character in "\r\n?#"):
        raise ValueError("internal auth path prefixes must be absolute paths")
    return normalized


def _path_matches_prefix(path: str, prefix: str) -> bool:
    if prefix == "/":
        return path.startswith("/")
    return path == prefix or path.startswith(f"{prefix}/")


def _canonical_identity_headers(headers: Mapping[str, str] | None) -> str:
    """Canonicalize only gateway-owned identity headers for HMAC v2."""

    if not headers:
        return ""
    normalized = {
        str(key).lower(): str(value)
        for key, value in headers.items()
        if str(key).lower() in _SIGNED_IDENTITY_HEADERS
    }
    return "\n".join(f"{key}:{normalized[key]}" for key in sorted(normalized))


def _epoch_ms() -> int:
    return int(time.time() * 1000)


def _body_sha256(body: bytes | str | None) -> str:
    if body is None:
        data = b""
    elif isinstance(body, str):
        data = body.encode("utf-8")
    else:
        data = body
    return sha256(data).hexdigest()


def _parse_internal_auth_keys(raw: str) -> dict[str, str]:
    keys: dict[str, str] = {}
    for item in (raw or "").split(","):
        item = item.strip()
        if not item or ":" not in item:
            continue
        key_id, secret = item.split(":", 1)
        key_id = key_id.strip()
        secret = secret.strip()
        if key_id and secret:
            keys[key_id] = secret
    return keys


def _default_replay_store() -> ReplayStore:
    environment = os.getenv("ENVIRONMENT", "").strip().lower()
    local_test = environment in {"local", "dev", "development", "test", "testing"}
    if os.getenv("PYTEST_CURRENT_TEST"):
        local_test = True
    backend = os.getenv(
        "INTERNAL_COMM_STATE_BACKEND",
        "memory" if local_test else "redis",
    ).strip().lower()
    if backend == "redis":
        redis_url = os.getenv("INTERNAL_COMM_REDIS_URL", "").strip()
        if not redis_url:
            raise RuntimeError(
                "INTERNAL_COMM_STATE_BACKEND=redis requires INTERNAL_COMM_REDIS_URL"
            )
        return RedisReplayStore.from_url(redis_url)
    return InMemoryReplayStore()


__all__ = [
    "GatewaySecret",
    "InvalidGatewaySecret",
    "InMemoryReplayStore",
    "RedisReplayStore",
    "ReplayStore",
]
