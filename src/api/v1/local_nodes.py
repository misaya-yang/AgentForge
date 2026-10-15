"""Owner-safe Gateway Local Node control-plane API.

This router contains only the public contract.  The injected control plane is
Gateway-owned and must use PostgreSQL/Redis in production; no Assistant
Service proxy or in-memory fallback is permitted here.  Composition mounts it
only after providing ``app.state.local_node_control_plane``.
"""

from __future__ import annotations

import inspect
from typing import Any, Protocol

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field

from ...core.auth.user_resolver import UserContext
from ...services.local_node.repository import LocalNodeRepositoryError
from ..deps import get_user_context

router = APIRouter(prefix="/local-nodes", tags=["Local Nodes"])


class LocalNodeControlPlane(Protocol):
    async def call(self, operation: str, *, tenant_id: str, user_id: str, **kwargs: Any) -> Any: ...


class LocalNodeChannelVerifier(Protocol):
    async def verify(self, *, request: Request, purpose: str, challenge_id: str | None = None,
                     expected_device_id: str | None = None, **kwargs: Any) -> Any: ...


class StrictBody(BaseModel):
    model_config = ConfigDict(extra="forbid")


class PairingChallengeBody(StrictBody):
    expires_in_seconds: int = Field(default=600, ge=60, le=900)


class PairingCompleteBody(StrictBody):
    display_name: str = Field(default="Local Node", min_length=1, max_length=160)
    platform: str = Field(default="unknown", min_length=1, max_length=32)
    node_version: str = Field(default="unknown", min_length=1, max_length=64)
    protocol_version: str = Field(default="ai-platform.local-node.v1", min_length=1, max_length=64)
    capability_claims: list[str] = Field(default_factory=list, max_length=64)
    permission_snapshot_digest: str = Field(default="", max_length=80)


class GrantBody(StrictBody):
    kind: str = Field(pattern=r"^(workspace|app|domain)$")
    display_name: str = Field(min_length=1, max_length=160)
    capabilities: list[str] = Field(min_length=1, max_length=16)
    resource_ref: str | None = Field(default=None, max_length=255)
    session_id: str | None = Field(default=None, max_length=255)


def _service(request: Request) -> LocalNodeControlPlane:
    service = getattr(request.app.state, "local_node_control_plane", None)
    if service is None:
        raise HTTPException(status_code=503, detail="Local Node control plane unavailable")
    return service


def _channel_verifier(request: Request) -> LocalNodeChannelVerifier:
    verifier = getattr(request.app.state, "local_node_channel_verifier", None)
    if verifier is None:
        raise HTTPException(status_code=503, detail="Local Node channel verifier unavailable")
    return verifier


async def _call(request: Request, user: UserContext, operation: str, **kwargs: Any) -> Any:
    try:
        return await _service(request).call(
            operation,
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            **kwargs,
        )
    except LocalNodeRepositoryError as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from None


@router.post("/pairing/challenges", status_code=201)
async def create_pairing_challenge(
    body: PairingChallengeBody,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> Any:
    return await _call(request, user, "pairing.create", **body.model_dump())


@router.post("/pairing/challenges/{challenge_id}/complete", status_code=201)
async def complete_pairing(
    challenge_id: str,
    body: PairingCompleteBody,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> Any:
    verifier = _channel_verifier(request)
    channel = verifier.verify(
        request=request, purpose="pairing.complete", challenge_id=challenge_id,
        expected_device_id=None, body=body.model_dump(mode="json"),
    )
    if inspect.isawaitable(channel):
        channel = await channel
    return await _call(request, user, "pairing.complete", challenge_id=challenge_id,
                       channel=channel, **body.model_dump())


@router.get("")
async def list_devices(request: Request, user: UserContext = Depends(get_user_context)) -> Any:
    return await _call(request, user, "devices.list")


@router.get("/{device_id}/status")
async def device_status(device_id: str, request: Request, user: UserContext = Depends(get_user_context)) -> Any:
    return await _call(request, user, "device.status", device_id=device_id)


@router.get("/{device_id}/capabilities")
async def device_capabilities(device_id: str, request: Request, user: UserContext = Depends(get_user_context)) -> Any:
    return await _call(request, user, "device.capabilities", device_id=device_id)


@router.get("/{device_id}/doctor")
async def device_doctor(device_id: str, request: Request, user: UserContext = Depends(get_user_context)) -> Any:
    return await _call(request, user, "device.doctor", device_id=device_id)


@router.get("/{device_id}/grants")
async def list_grants(device_id: str, request: Request, user: UserContext = Depends(get_user_context)) -> Any:
    return await _call(request, user, "grants.list", device_id=device_id)


@router.post("/{device_id}/grants", status_code=201)
async def create_grant(
    device_id: str,
    body: GrantBody,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> Any:
    return await _call(request, user, "grants.create", device_id=device_id, **body.model_dump())


@router.delete("/{device_id}/grants/{grant_id}")
async def revoke_grant(
    device_id: str,
    grant_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
) -> Any:
    return await _call(request, user, "grants.revoke", device_id=device_id, grant_id=grant_id)


@router.get("/{device_id}/events")
async def list_events(
    device_id: str,
    request: Request,
    after_sequence: int = 0,
    user: UserContext = Depends(get_user_context),
) -> Any:
    if after_sequence < 0:
        raise HTTPException(status_code=422, detail="after_sequence must be non-negative")
    return await _call(request, user, "events.list", device_id=device_id, after_sequence=after_sequence)
