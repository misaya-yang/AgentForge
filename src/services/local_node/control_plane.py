"""Gateway-owned Local Node control plane.

The service is intentionally constructed with a PostgreSQL pool.  It refuses
to start without one, so a missing database cannot silently become an
in-memory pairing, grant, or receipt authority.
"""

from __future__ import annotations

import hashlib
import json
import secrets
import uuid
from collections.abc import Mapping
from datetime import datetime, timedelta, timezone
from typing import Any

from .repository import LocalNodeRepositoryError


def _now() -> datetime:
    return datetime.now(timezone.utc)


def _owner(tenant_id: str, user_id: str) -> tuple[str, str]:
    if not tenant_id or not user_id:
        raise LocalNodeRepositoryError("tenant and user ownership are required")
    return tenant_id, user_id


def _channel_value(channel: Any, name: str, default: str | None = None) -> str | None:
    value = channel.get(name) if isinstance(channel, Mapping) else getattr(channel, name, default)
    return value if isinstance(value, str) and value else default


class PostgresLocalNodeControlPlane:
    """Public Gateway control-plane implementation backed only by Postgres."""

    def __init__(self, pool: Any, *, channel_verifier: Any | None = None) -> None:
        if pool is None:
            raise ValueError("Local Node control plane requires a PostgreSQL pool")
        self._pool = pool
        self._channel_verifier = channel_verifier

    async def call(self, operation: str, *, tenant_id: str, user_id: str, **kwargs: Any) -> Any:
        handlers = {
            "pairing.create": self.create_pairing_challenge,
            "pairing.complete": self.complete_pairing,
            "devices.list": self.list_devices,
            "device.status": self.device_status,
            "device.capabilities": self.device_capabilities,
            "device.doctor": self.device_doctor,
            "grants.list": self.list_grants,
            "grants.create": self.create_grant,
            "grants.revoke": self.revoke_grant,
            "events.list": self.list_events,
        }
        handler = handlers.get(operation)
        if handler is None:
            raise LocalNodeRepositoryError("unsupported Local Node operation")
        return await handler(tenant_id=tenant_id, user_id=user_id, **kwargs)

    async def create_pairing_challenge(self, *, tenant_id: str, user_id: str, expires_in_seconds: int = 600, **_: Any) -> dict[str, Any]:
        _owner(tenant_id, user_id)
        challenge_id = uuid.uuid4()
        user_code = secrets.token_urlsafe(9).replace("_", "-")[:12].upper()
        expires_at = _now() + timedelta(seconds=expires_in_seconds)
        code_hash = hashlib.sha256(user_code.encode("utf-8")).hexdigest()
        async with self._pool.acquire() as connection:
            await connection.execute(
                """
                INSERT INTO local_node_pairing_challenges(
                    challenge_id,tenant_id,user_id,user_code_sha256,expires_at
                ) VALUES($1,$2,$3,$4,$5)
                """, challenge_id, tenant_id, user_id, code_hash, expires_at,
            )
        return {"challenge_id": str(challenge_id), "user_code": user_code, "expires_at": expires_at}

    async def complete_pairing(self, *, tenant_id: str, user_id: str, challenge_id: str,
                               channel: Any | None = None, display_name: str = "Local Node",
                               platform: str = "unknown", node_version: str = "unknown",
                               protocol_version: str = "ai-platform.local-node.v1",
                               capability_claims: list[str] | None = None,
                               permission_snapshot_digest: str = "", **_: Any) -> dict[str, Any]:
        _owner(tenant_id, user_id)
        challenge_uuid = _parse_uuid(challenge_id, "pairing challenge")
        device_id = _channel_value(channel, "device_id") or str(uuid.uuid4())
        channel_id = _channel_value(channel, "channel_id") or str(uuid.uuid4())
        channel_fingerprint = _channel_value(channel, "fingerprint") or "unavailable"
        async with self._pool.acquire() as connection, connection.transaction():
            challenge = await connection.fetchrow(
                """
                UPDATE local_node_pairing_challenges
                   SET consumed_at=now()
                 WHERE challenge_id=$1 AND tenant_id=$2 AND user_id=$3
                   AND consumed_at IS NULL AND expires_at>now()
                RETURNING challenge_id
                """, challenge_uuid, tenant_id, user_id,
            )
            if challenge is None:
                raise LocalNodeRepositoryError("pairing challenge is expired, consumed, or not owned")
            await connection.execute(
                """
                INSERT INTO local_node_devices(
                    device_id,tenant_id,user_id,display_name,platform,node_version,
                    protocol_version,status,capability_revision,capabilities,
                    permission_snapshot_digest,last_seen_at
                ) VALUES($1,$2,$3,$4,$5,$6,$7,'online',1,$8::jsonb,$9,now())
                ON CONFLICT(device_id) DO UPDATE SET display_name=EXCLUDED.display_name,
                    platform=EXCLUDED.platform,node_version=EXCLUDED.node_version,
                    protocol_version=EXCLUDED.protocol_version,status='online',
                    capability_revision=local_node_devices.capability_revision+1,
                    capabilities=EXCLUDED.capabilities,permission_snapshot_digest=EXCLUDED.permission_snapshot_digest,
                    last_seen_at=now(),revoked_at=NULL
                 WHERE local_node_devices.tenant_id=$2 AND local_node_devices.user_id=$3
                """, device_id, tenant_id, user_id, display_name, platform, node_version,
                protocol_version, json.dumps(capability_claims or []), permission_snapshot_digest,
            )
            await connection.execute(
                """
                INSERT INTO local_node_channels(channel_id,device_id,tenant_id,user_id,fingerprint,status,last_seen_at)
                VALUES($1,$2,$3,$4,$5,'online',now())
                ON CONFLICT(channel_id) DO UPDATE SET status='online',last_seen_at=now()
                 WHERE local_node_channels.device_id=$2 AND local_node_channels.tenant_id=$3
                   AND local_node_channels.user_id=$4
                """, channel_id, device_id, tenant_id, user_id, channel_fingerprint,
            )
        return {"device": {"device_id": device_id, "display_name": display_name, "platform": platform,
                            "node_version": node_version, "status": "online", "channel_id": channel_id}}

    async def list_devices(self, *, tenant_id: str, user_id: str, **_: Any) -> dict[str, Any]:
        _owner(tenant_id, user_id)
        async with self._pool.acquire() as connection:
            rows = await connection.fetch(
                """SELECT device_id,display_name,platform,node_version,status,last_seen_at,capability_revision
                   FROM local_node_devices WHERE tenant_id=$1 AND user_id=$2 AND revoked_at IS NULL
                   ORDER BY created_at DESC""", tenant_id, user_id,
            )
        return {"devices": [dict(row) for row in rows]}

    async def device_status(self, *, tenant_id: str, user_id: str, device_id: str, **_: Any) -> dict[str, Any]:
        return {"device": await self._device(tenant_id, user_id, device_id)}

    async def device_capabilities(self, *, tenant_id: str, user_id: str, device_id: str, **_: Any) -> dict[str, Any]:
        device = await self._device(tenant_id, user_id, device_id)
        return {"device_id": device_id, "revision": device["capability_revision"], "capabilities": device.get("capabilities", [])}

    async def device_doctor(self, *, tenant_id: str, user_id: str, device_id: str, **_: Any) -> dict[str, Any]:
        await self._device(tenant_id, user_id, device_id)
        return {"device_id": device_id, "status": "ready", "permissions": []}

    async def list_grants(self, *, tenant_id: str, user_id: str, device_id: str, **_: Any) -> dict[str, Any]:
        await self._device(tenant_id, user_id, device_id)
        async with self._pool.acquire() as connection:
            rows = await connection.fetch(
                """SELECT grant_id,device_id,kind,display_name,capabilities,resource_ref,session_id,
                          revision,status,created_at,expires_at
                   FROM local_node_grants WHERE tenant_id=$1 AND user_id=$2 AND device_id=$3
                   ORDER BY created_at DESC""", tenant_id, user_id, device_id,
            )
        return {"grants": [dict(row) for row in rows]}

    async def create_grant(self, *, tenant_id: str, user_id: str, device_id: str, kind: str,
                           display_name: str, capabilities: list[str], resource_ref: str | None = None,
                           session_id: str | None = None, **_: Any) -> dict[str, Any]:
        await self._device(tenant_id, user_id, device_id)
        grant_id = uuid.uuid4()
        async with self._pool.acquire() as connection:
            row = await connection.fetchrow(
                """INSERT INTO local_node_grants(grant_id,tenant_id,user_id,device_id,kind,display_name,
                          capabilities,resource_ref,session_id,revision,status)
                   VALUES($1,$2,$3,$4,$5,$6,$7::jsonb,$8,$9,1,'active') RETURNING *""",
                grant_id, tenant_id, user_id, device_id, kind, display_name, json.dumps(capabilities), resource_ref, session_id,
            )
        return {"grant": dict(row)}

    async def revoke_grant(self, *, tenant_id: str, user_id: str, device_id: str, grant_id: str, **_: Any) -> dict[str, Any]:
        grant_uuid = _parse_uuid(grant_id, "grant")
        async with self._pool.acquire() as connection:
            row = await connection.fetchrow(
                """UPDATE local_node_grants SET status='revoked',revision=revision+1,revoked_at=now()
                   WHERE grant_id=$1 AND tenant_id=$2 AND user_id=$3 AND device_id=$4
                   RETURNING grant_id,device_id,revision,revoked_at""",
                grant_uuid, tenant_id, user_id, device_id,
            )
        if row is None:
            raise LocalNodeRepositoryError("grant is not owned or does not exist")
        return {"revoked": True, **dict(row)}

    async def list_events(self, *, tenant_id: str, user_id: str, device_id: str, after_sequence: int = 0, **_: Any) -> dict[str, Any]:
        await self._device(tenant_id, user_id, device_id)
        async with self._pool.acquire() as connection:
            rows = await connection.fetch(
                """SELECT event_id,sequence,channel_id,execution_id,event,status,payload,created_at
                   FROM local_node_events WHERE tenant_id=$1 AND user_id=$2 AND device_id=$3 AND sequence>$4
                   ORDER BY sequence LIMIT 500""", tenant_id, user_id, device_id, after_sequence,
            )
        return {"device_id": device_id, "events": [dict(row) for row in rows], "after_sequence": after_sequence}

    async def _device(self, tenant_id: str, user_id: str, device_id: str) -> dict[str, Any]:
        async with self._pool.acquire() as connection:
            row = await connection.fetchrow(
                """SELECT * FROM local_node_devices WHERE device_id=$1 AND tenant_id=$2 AND user_id=$3""",
                device_id, tenant_id, user_id,
            )
        if row is None or row["revoked_at"] is not None:
            raise LocalNodeRepositoryError("device is not owned or does not exist")
        return dict(row)


def _parse_uuid(value: str, label: str) -> uuid.UUID:
    try:
        return uuid.UUID(value)
    except (ValueError, AttributeError, TypeError) as exc:
        raise LocalNodeRepositoryError(f"{label} identity is invalid") from exc


def build_local_node_control_plane(pool: Any, *, channel_verifier: Any | None = None) -> PostgresLocalNodeControlPlane:
    """Public composition hook for Gateway startup."""
    return PostgresLocalNodeControlPlane(pool, channel_verifier=channel_verifier)
