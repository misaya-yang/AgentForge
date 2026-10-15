"""Explain platform, tenant-visible, and current assistant tool sets separately."""

from __future__ import annotations

import json

from fastapi import APIRouter, Depends, HTTPException, Request

from ....core.assistant_capability_catalog import (
    load_assistant_capability_catalog,
    load_gateway_assistant_policies,
    project_assistant_tools,
)
from ....core.auth.user_resolver import UserContext
from ....services.agent_runtime.control.capability_catalog import AgentRuntimeControlError
from ....services.assistant_entry.session_binding import get_session_manager
from ...deps import get_user_context

router = APIRouter()


def _projection(readonly: dict, records: dict[str, dict]) -> list[dict]:
    result: list[dict] = []
    for key in ("tools", "mcp", "deferred"):
        for descriptor in readonly.get(key) or []:
            if not isinstance(descriptor, dict):
                continue
            name = descriptor.get("name")
            if not isinstance(name, str):
                continue
            record = records.get(name, {})
            schema = record.get("input_schema") or {}
            result.append({
                "name": name,
                "effect": descriptor.get("effect") or record.get("effect") or "unknown",
                "approval": descriptor.get("approval") or record.get("approval") or "unknown",
                "required_inputs": schema.get("required") if isinstance(schema.get("required"), list) else [],
                "device_required": name in {"local_node_catalog", "local_node_action"},
            })
    return result


@router.get("/sessions/{session_id}/tools")
async def get_session_tools(
    session_id: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
):
    if not user.is_authenticated:
        raise HTTPException(401, "Authentication required")
    session = await get_session_manager(request).get(session_id)
    if not session or session.user_id != user.user_id or session.tenant_id != user.tenant_id:
        raise HTTPException(404, "Session not found")
    database = getattr(request.app.state, "database", None)
    if database is None:
        raise HTTPException(503, "Tool catalog is unavailable")
    try:
        _, catalog = load_assistant_capability_catalog()
        records = {str(item["name"]): item for item in catalog}
        policy = await load_gateway_assistant_policies(request, user, catalog)
        visible = {item["name"] for item in project_assistant_tools(user, tenant_policy=policy)}
    except Exception:
        raise HTTPException(503, "Tool catalog is unavailable") from None

    latest_run = None
    row = await database.fetchrow(
        "SELECT s.snapshot, r.run_id, r.status FROM assistant_runtime_snapshots AS s "
        "JOIN assistant_runs AS r ON r.run_id = s.run_id "
        "WHERE s.session_id = $1 AND s.tenant_id = $2 AND s.user_id = $3 "
        "ORDER BY s.created_at DESC LIMIT 1",
        session_id, user.tenant_id, user.user_id,
    )
    if row:
        payload = json.loads(row["snapshot"]) if isinstance(row["snapshot"], str) else row["snapshot"]
        readonly = payload.get("readonly_capabilities") if isinstance(payload, dict) else None
        latest_run = {
            "run_id": str(row["run_id"]),
            "status": row["status"],
            "tools": _projection(readonly, records) if isinstance(readonly, dict) else [],
        }

    # This is a read-only estimate using the same Worker/tenant resolver as
    # turn start. The actual run pins a fresh snapshot at admission.
    current: dict = {"status": "unavailable", "tools": []}
    control = getattr(request.app.state, "agent_runtime_control", None)
    session_config = getattr(session, "config", None) or {}
    if not isinstance(session_config, dict):
        session_config = vars(session_config)
    settings = getattr(request.app.state, "settings", None)
    model_id = session_config.get("selected_model") or str(getattr(settings, "default_model", "") or "")
    if control is not None and model_id:
        try:
            model = await control.model_service.get_model(user.tenant_id, model_id)
            revision = int(model.get("capability_revision") or 1) if model else 1
            readonly: dict = {}
            await control._fetch_capability_catalog(
                readonly,
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                session_id=session_id,
                model_id=model_id,
                capability_revision=revision,
            )
            current = {"status": "available_now", "tools": _projection(readonly, records)}
        except (AgentRuntimeControlError, AttributeError, ValueError, TypeError):
            current = {"status": "unavailable", "tools": []}

    return {
        "platform_catalog": [
            {
                "name": name,
                "description": record["description"],
                "tenant_visible": name in visible,
                "required_inputs": (record.get("input_schema") or {}).get("required") or [],
                "device_required": name in {"local_node_catalog", "local_node_action"},
            }
            for name, record in records.items()
        ],
        "next_turn_estimate": current,
        "last_run_pinned": latest_run,
        "note": "The next run pins its own tool set; Worker, permission and device state may change before it starts.",
    }
