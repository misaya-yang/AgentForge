"""Gateway-private Local Node broker routes.

The router is intentionally not mounted by this module.  Composition must
inject a PostgreSQL repository and an authenticated outbound device adapter
before enabling it.  No route here accepts a device credential or claims a
successful action without a device receipt.
"""

from __future__ import annotations

import contextlib
import hashlib
import hmac
import inspect
import json
import os
from dataclasses import replace
from typing import Any, Protocol

from ai_gateway_contracts.capability_proof import CapabilityProofError, verify_capability_proof
from ai_gateway_core.local_node import (
    LocalNodeAction,
    LocalNodeDeviceScope,
    LocalNodeReceipt,
    LocalNodeReceiptStatus,
)
from fastapi import APIRouter, Body, Header, HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field

from src.services.local_node.repository import (
    LocalNodeExecutionRepository,
    LocalNodeRepositoryError,
)

router = APIRouter(prefix="/internal/v2/agent-capabilities/local-node", tags=["internal-agent-capabilities"])
_ACTION_PATH = "/internal/v2/agent-capabilities/local-node/action"
_RECEIPTS_PATH = "/internal/v2/agent-capabilities/local-node/receipts"


class LocalNodeActionBody(BaseModel):
    model_config = ConfigDict(extra="forbid")

    lease_id: str = Field(min_length=1, max_length=255)
    execution_id: str = Field(min_length=1, max_length=255)
    run_id: str = Field(min_length=1, max_length=255)
    tool_call_id: str = Field(min_length=1, max_length=255)
    attempt_id: str = Field(min_length=1, max_length=255)
    device_id: str = Field(min_length=1, max_length=255)
    channel_id: str = Field(min_length=1, max_length=255)
    capability_revision: int = Field(ge=1)
    capability_id: str = Field(min_length=1, max_length=160)
    effect: str = Field(pattern=r"^(read|write|unknown)$")
    operation: str = Field(min_length=1, max_length=160)
    arguments: dict[str, Any]
    arguments_sha256: str = Field(pattern=r"^sha256:[0-9a-f]{64}$")
    idempotency_key: str = Field(min_length=1, max_length=160)
    approval_id: str | None = Field(default=None, max_length=255)
    grant_id: str | None = Field(default=None, max_length=255)
    grant_revision: int | None = Field(default=None, ge=1)


class LocalNodeReceiptBody(BaseModel):
    model_config = ConfigDict(extra="forbid")

    execution_id: str = Field(min_length=1, max_length=255)
    tenant_id: str = Field(min_length=1, max_length=255)
    user_id: str = Field(min_length=1, max_length=255)
    session_id: str = Field(min_length=1, max_length=255)
    device_id: str = Field(min_length=1, max_length=255)
    channel_id: str = Field(min_length=1, max_length=255)
    dispatch_fence: str = Field(min_length=1, max_length=255)
    sequence: int = Field(ge=1)
    status: str = Field(pattern=r"^(running|succeeded|failed|cancelled|timeout|side_effect_unknown)$")
    event: str = Field(min_length=1, max_length=64)
    payload: dict[str, Any] = Field(default_factory=dict)


class LocalNodeDeviceAdapter(Protocol):
    async def dispatch(self, action: LocalNodeAction) -> None: ...


class LocalNodeDeviceChannelVerifier(Protocol):
    async def verify(self, **kwargs: Any) -> Any: ...


def _authorized(
    *,
    body: dict[str, Any],
    path: str,
    internal_token: str | None,
    tenant_id: str | None,
    user_id: str | None,
    session_id: str | None,
    execution_id: str | None,
    run_id: str | None,
    proof: str | None,
) -> tuple[str, str, str]:
    expected = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "")
    if not expected or not internal_token or not hmac.compare_digest(internal_token, expected):
        raise HTTPException(status_code=401, detail="internal authorization failed")
    values = (tenant_id, user_id, session_id, execution_id, run_id)
    if any(not value or len(value) > 255 or any(ord(char) < 0x20 for char in value) for value in values):
        raise HTTPException(status_code=403, detail="scope is invalid")
    secret = os.getenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "")
    if not secret or not proof:
        raise HTTPException(status_code=401, detail="capability proof required")
    try:
        verify_capability_proof(
            secret,
            proof,
            method="POST",
            path=path,
            body=body,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            execution_id=execution_id,
            run_id=run_id,
        )
    except CapabilityProofError:
        raise HTTPException(status_code=401, detail="capability proof invalid") from None
    return tenant_id, user_id, session_id  # type: ignore[return-value]


def _dependencies(request: Request) -> tuple[LocalNodeExecutionRepository, LocalNodeDeviceAdapter]:
    repository = getattr(request.app.state, "local_node_execution_repository", None)
    device = getattr(request.app.state, "local_node_device_adapter", None)
    if repository is None or device is None:
        raise HTTPException(status_code=503, detail="Local Node broker unavailable")
    return repository, device


@router.post("/action")
async def dispatch_local_node_action(
    request: Request,
    payload: LocalNodeActionBody = Body(...),
    x_ai_platform_internal_token: str | None = Header(default=None),
    x_ai_tenant_id: str | None = Header(default=None),
    x_ai_user_id: str | None = Header(default=None),
    x_ai_session_id: str | None = Header(default=None),
    x_ai_capability_proof: str | None = Header(default=None),
    x_ai_execution_id: str | None = Header(default=None),
    x_ai_run_id: str | None = Header(default=None),
    x_ai_lease_id: str | None = Header(default=None),
    x_ai_tool_call_id: str | None = Header(default=None),
) -> dict[str, Any]:
    body = payload.model_dump(mode="json")
    tenant_id, user_id, session_id = _authorized(
        body=body, path=_ACTION_PATH, internal_token=x_ai_platform_internal_token,
        tenant_id=x_ai_tenant_id, user_id=x_ai_user_id, session_id=x_ai_session_id,
        execution_id=x_ai_execution_id, run_id=x_ai_run_id, proof=x_ai_capability_proof,
    )
    if (
        payload.execution_id != x_ai_execution_id
        or payload.run_id != x_ai_run_id
        or payload.lease_id != x_ai_lease_id
        or payload.tool_call_id != x_ai_tool_call_id
    ):
        raise HTTPException(status_code=403, detail="execution scope does not match headers")
    repository, device = _dependencies(request)
    action = LocalNodeAction(
        scope=LocalNodeDeviceScope(tenant_id, user_id, session_id, payload.device_id, payload.channel_id),
        lease_id=payload.lease_id, execution_id=payload.execution_id, run_id=payload.run_id,
        tool_call_id=payload.tool_call_id, attempt_id=payload.attempt_id,
        capability_revision=payload.capability_revision, capability_id=payload.capability_id,
        effect=payload.effect, operation=payload.operation,
        arguments=payload.arguments, arguments_sha256=payload.arguments_sha256,
        idempotency_key=payload.idempotency_key, approval_id=payload.approval_id,
        grant_id=payload.grant_id, grant_revision=payload.grant_revision,
    )
    try:
        claimed = await repository.claim_dispatch(action)
        if claimed:
            fence_for = getattr(repository, "dispatch_fence_for", None)
            if fence_for is not None:
                action = replace(action, dispatch_fence=await fence_for(action))
            await device.dispatch(action)
        else:
            existing = await repository.execution_result(action)
            if existing is None:
                raise HTTPException(status_code=409, detail="dispatch already claimed; receipt required")
            return {"accepted": False, "replayed": True, "execution_id": action.execution_id, "result": dict(existing)}
    except LocalNodeRepositoryError as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from None
    except Exception:
        # Dispatch may have reached the device before transport failure. The
        # Runtime must reconcile the execution as side_effect_unknown.
        if 'claimed' in locals() and claimed:
            mark_unknown = getattr(repository, "mark_side_effect_unknown", None)
            if mark_unknown is not None:
                with contextlib.suppress(Exception):
                    await mark_unknown(action)
        raise HTTPException(status_code=502, detail="Local Node dispatch outcome unknown") from None
    return {"accepted": True, "execution_id": action.execution_id, "device_id": action.scope.device_id}


@router.post("/receipts")
async def record_local_node_receipt(
    request: Request,
    payload: LocalNodeReceiptBody = Body(...),
) -> dict[str, Any]:
    verifier = getattr(request.app.state, "local_node_device_channel_verifier", None)
    repository = getattr(request.app.state, "local_node_execution_repository", None)
    if verifier is None or repository is None:
        raise HTTPException(status_code=503, detail="Local Node receipt adapter unavailable")
    body = payload.model_dump(mode="json")
    encoded = json.dumps(body, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
    body_digest = "sha256:" + hashlib.sha256(encoded).hexdigest()
    try:
        principal = verifier.verify(
            request=request,
            purpose="capability.receipt",
            body_digest=body_digest,
            expected_device_id=payload.device_id,
        )
        if inspect.isawaitable(principal):
            principal = await principal
        if any(
            (principal.get(key) if isinstance(principal, dict) else getattr(principal, key, None))
            != getattr(payload, key)
            for key in ("tenant_id", "user_id", "session_id", "device_id")
        ):
            raise HTTPException(status_code=403, detail="receipt scope mismatch")
        receipt = LocalNodeReceipt(
            execution_id=payload.execution_id,
            tenant_id=payload.tenant_id,
            user_id=payload.user_id,
            session_id=payload.session_id,
            device_id=payload.device_id,
            channel_id=payload.channel_id,
            dispatch_fence=payload.dispatch_fence,
            sequence=payload.sequence,
            status=LocalNodeReceiptStatus(payload.status),
            event=payload.event,
            payload=payload.payload,
        )
        sequence = await repository.append_receipt(receipt)
    except LocalNodeRepositoryError as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from None
    except HTTPException:
        raise
    except Exception:
        raise HTTPException(status_code=403, detail="device receipt rejected") from None
    return {"accepted": True, "accepted_through_sequence": sequence}
