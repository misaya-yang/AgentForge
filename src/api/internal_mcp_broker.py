"""Scope-bound Runtime -> Gateway broker for read-only MCP calls."""

from __future__ import annotations

import hmac
import os
import re
import uuid
from typing import Any, Literal

from ai_gateway_contracts.capability_proof import (
    CapabilityProofError,
    canonical_body_hash,
    verify_capability_proof,
)
from fastapi import APIRouter, Body, Header, HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field, model_validator

from ..services.agent_runtime.mcp_gateway_broker import MCPGatewayBrokerError

router = APIRouter(prefix="/internal/v2/agent-capabilities", tags=["internal-agent-capabilities"])

_PATH = "/internal/v2/agent-capabilities/mcp/read"
_HASH = re.compile(r"^sha256:[0-9a-f]{64}$")


class MCPReadRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    connection_id: str = Field(min_length=1, max_length=255)
    principal_type: Literal["service_account", "user_delegated"]
    channel: str = Field(default="assistant", min_length=1, max_length=64)
    runtime_name: str = Field(min_length=1, max_length=255)
    schema_hash: str = Field(pattern=r"^sha256:[0-9a-f]{64}$")
    risk_level: Literal["low", "medium", "high", "critical"]
    arguments: dict[str, Any] = Field(default_factory=dict)
    arguments_hash: str = Field(pattern=r"^sha256:[0-9a-f]{64}$")

    @model_validator(mode="after")
    def validate_arguments_hash(self) -> MCPReadRequest:
        if self.arguments_hash != f"sha256:{canonical_body_hash(self.arguments)}":
            raise ValueError("arguments hash mismatch")
        return self


def _scope(value: str | None) -> str:
    if not value or len(value) > 255 or any(ord(character) < 32 for character in value):
        raise HTTPException(status_code=403, detail="capability scope invalid")
    return value


async def _authorize_execution(
    request: Request,
    *,
    body: dict[str, Any],
    payload: MCPReadRequest,
    internal_token: str | None,
    tenant_id: str | None,
    user_id: str | None,
    session_id: str | None,
    execution_id: str | None,
    run_id: str | None,
    tool_call_id: str | None,
    proof: str | None,
) -> tuple[str, str, str]:
    expected_token = os.getenv("AI_PLATFORM_INTERNAL_TOKEN", "")
    if (
        not expected_token
        or not internal_token
        or not hmac.compare_digest(internal_token, expected_token)
    ):
        raise HTTPException(status_code=401, detail="internal authorization failed")
    tenant, user, session = _scope(tenant_id), _scope(user_id), _scope(session_id)
    execution, run, tool_call = _scope(execution_id), _scope(run_id), _scope(tool_call_id)
    proof_secret = os.getenv("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "")
    if not proof_secret or not proof:
        raise HTTPException(status_code=401, detail="capability proof required")
    try:
        verify_capability_proof(
            proof_secret,
            proof,
            method="POST",
            path=_PATH,
            body=body,
            tenant_id=tenant,
            user_id=user,
            session_id=session,
            execution_id=execution,
            run_id=run,
        )
        execution_uuid, run_uuid = uuid.UUID(execution), uuid.UUID(run)
    except (CapabilityProofError, ValueError):
        raise HTTPException(status_code=401, detail="capability proof invalid") from None
    pool = getattr(getattr(request.app.state, "database", None), "_pool", None)
    if pool is None:
        raise HTTPException(status_code=424, detail="capability execution store unavailable")
    row = await pool.fetchrow(
        """SELECT capability_id, effect, approval_status, status,
                  tool_call_id, arguments_sha256
             FROM assistant_capability_executions
            WHERE execution_id=$1 AND tenant_id=$2 AND user_id=$3
              AND session_id=$4 AND run_id=$5""",
        execution_uuid,
        tenant,
        user,
        session,
        run_uuid,
    )
    if (
        not row
        or row["capability_id"] != payload.runtime_name
        or row["effect"] != "read"
        or row["approval_status"] != "not_required"
        or row["status"] not in {"dispatched", "running"}
        or row["tool_call_id"] != tool_call
        or not _HASH.fullmatch(payload.arguments_hash)
        or str(row["arguments_sha256"]).strip() != payload.arguments_hash[7:]
    ):
        raise HTTPException(status_code=403, detail="capability execution not active")
    return tenant, user, session


@router.post("/mcp/read")
async def broker_mcp_read(
    request: Request,
    body: dict[str, Any] = Body(...),
    x_ai_platform_internal_token: str | None = Header(default=None),
    x_ai_tenant_id: str | None = Header(default=None),
    x_ai_user_id: str | None = Header(default=None),
    x_ai_session_id: str | None = Header(default=None),
    x_ai_execution_id: str | None = Header(default=None),
    x_ai_run_id: str | None = Header(default=None),
    x_ai_tool_call_id: str | None = Header(default=None),
    x_ai_capability_proof: str | None = Header(default=None),
) -> dict[str, Any]:
    try:
        payload = MCPReadRequest.model_validate(body)
    except ValueError:
        raise HTTPException(status_code=422, detail="invalid MCP read request") from None
    tenant_id, user_id, _ = await _authorize_execution(
        request,
        body=body,
        payload=payload,
        internal_token=x_ai_platform_internal_token,
        tenant_id=x_ai_tenant_id,
        user_id=x_ai_user_id,
        session_id=x_ai_session_id,
        execution_id=x_ai_execution_id,
        run_id=x_ai_run_id,
        tool_call_id=x_ai_tool_call_id,
        proof=x_ai_capability_proof,
    )
    broker = getattr(request.app.state, "mcp_gateway_broker", None)
    if broker is None:
        raise HTTPException(status_code=424, detail="MCP broker unavailable")
    try:
        return await broker.invoke_read_only(
            tenant_id=tenant_id,
            user_id=user_id,
            authenticated=True,
            channel=payload.channel,
            runtime_name=payload.runtime_name,
            schema_hash=payload.schema_hash,
            risk_level=payload.risk_level,
            connection_id=payload.connection_id,
            principal_type=payload.principal_type,
            arguments=payload.arguments,
        )
    except MCPGatewayBrokerError as exc:
        raise HTTPException(status_code=502, detail=exc.code) from None
    except Exception as exc:
        code = str(getattr(exc, "code", ""))
        if code.startswith("MCP_"):
            raise HTTPException(status_code=403, detail=code) from None
        raise HTTPException(status_code=502, detail="MCP broker request failed") from exc


__all__ = ["MCPReadRequest", "router"]
