"""Owner-only, bounded projection of the exact action awaiting approval."""

from __future__ import annotations

import hashlib
import json
from typing import Any


def _record(value: Any) -> dict[str, Any] | None:
    if isinstance(value, str):
        try:
            value = json.loads(value)
        except (TypeError, ValueError):
            return None
    return value if isinstance(value, dict) else None


def _contains_secret_key(value: Any) -> bool:
    if isinstance(value, dict):
        return any(
            any(word in str(key).lower() for word in ("token", "secret", "password", "credential", "api_key"))
            or _contains_secret_key(item)
            for key, item in value.items()
        )
    if isinstance(value, list):
        return any(_contains_secret_key(item) for item in value)
    return False


def _action_details(tool_name: str, arguments: dict[str, Any]) -> tuple[str, list[dict[str, str]]] | None:
    if tool_name == "execute_python_code":
        code = arguments.get("code")
        inputs = arguments.get("inputs", [])
        if not isinstance(code, str) or not code or len(code) > 12_000 or not isinstance(inputs, list) or len(inputs) > 20:
            return None
        files: list[str] = []
        for item in inputs:
            if not isinstance(item, dict) or not isinstance(item.get("filename"), str) or not isinstance(item.get("size_bytes"), int):
                return None
            files.append(f"{item['filename']} ({item['size_bytes']} bytes)")
        return "Isolated Python execution", [
            {"name": "code", "value": code},
            {"name": "input files", "value": ", ".join(files) or "none"},
        ]
    if tool_name == "confluence_write":
        action = arguments.get("action")
        if action not in {"create_page", "update_page", "find_replace", "move_page", "comment", "delete_page"}:
            return None
        target = arguments.get("page_id") or arguments.get("space_key")
        if not isinstance(target, str) or not target.strip():
            return None
        allowed = {"action", "page_id", "space_key", "title", "content", "parent_id", "target_parent_id", "find", "replace", "raw_html", "body"}
        if set(arguments) - allowed:
            return None
        fields = []
        for key in ("action", "page_id", "space_key", "title", "parent_id", "target_parent_id", "find", "replace", "raw_html", "content", "body"):
            if key in arguments:
                value = arguments[key]
                if not isinstance(value, (str, bool)) or len(str(value)) > 12_000:
                    return None
                fields.append({"name": key, "value": str(value)})
        return target, fields
    if tool_name == "local_node_action":
        device = arguments.get("device_id")
        operation = arguments.get("operation")
        params = arguments.get("arguments")
        if not isinstance(device, str) or not device or not isinstance(operation, str) or not operation or not isinstance(params, dict):
            return None
        if _contains_secret_key(params):
            return None
        serialized = json.dumps(params, ensure_ascii=False, sort_keys=True)
        if len(serialized) > 12_000:
            return None
        return device, [
            {"name": "operation", "value": operation},
            {"name": "device", "value": device},
            {"name": "arguments", "value": serialized},
        ]
    if tool_name in {
        "context_compact", "generate_image", "generate_quiz",
        "mcp_docgen__generate_document", "todo_write", "update_user_memory",
    }:
        if _contains_secret_key(arguments):
            return None
        serialized = json.dumps(arguments, ensure_ascii=False, sort_keys=True)
        if len(serialized) > 50_000:
            return None
        target = next(
            (str(arguments[key]) for key in ("title", "topic", "key", "goal", "prompt")
             if isinstance(arguments.get(key), str) and arguments[key].strip()),
            "This assistant conversation",
        )
        parameters = [
            {"name": key, "value": value if isinstance(value, str) else json.dumps(value, ensure_ascii=False)}
            for key, value in arguments.items()
        ]
        return target, parameters
    return None


async def owner_approval_preview(
    database: Any,
    *,
    approval_id: str,
    runtime_thread_id: str,
    tenant_id: str,
    user_id: str,
    session_id: str,
    runtime_summary: dict[str, Any],
) -> dict[str, Any]:
    unavailable = {
        "can_approve": False,
        "reason": "Action details could not be verified. Reject this request or check the affected system.",
        "authorization_scope": "this action only",
    }
    try:
        row = await database.fetchrow(
            "SELECT a.run_id, a.tool_name, a.arguments, a.status "
            "FROM assistant_tool_approvals AS a "
            "JOIN assistant_runs AS r ON r.run_id = a.run_id "
            "WHERE a.approval_id = $1::uuid AND a.tenant_id = $2 AND a.user_id = $3 "
            "AND a.session_id = $4 AND r.tenant_id = $2 AND r.user_id = $3 "
            "AND r.session_id = $4 AND r.harness_thread_id = $5::uuid",
            approval_id, tenant_id, user_id, session_id, runtime_thread_id,
        )
    except Exception:
        return unavailable
    if not row:
        return unavailable
    arguments = _record(row["arguments"])
    if arguments is None:
        return unavailable
    digest = hashlib.sha256(
        json.dumps(arguments, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()
    tool_name = str(row["tool_name"])
    remaining = runtime_summary.get("expires_in_seconds")
    if (
        str(row["run_id"]) != str(runtime_summary.get("run_id"))
        or tool_name != runtime_summary.get("tool_name")
        or row["status"] != "pending"
        or runtime_summary.get("status") != "pending"
        or digest != runtime_summary.get("arguments_hash")
        or not isinstance(remaining, (int, float))
        or remaining <= 0
    ):
        return unavailable
    details = _action_details(tool_name, arguments)
    if details is None:
        return unavailable
    target, parameters = details
    return {
        "can_approve": True,
        "tool_name": tool_name,
        "effect": "write",
        "target": target,
        "parameters": parameters,
        "authorization_scope": "this action only",
        "expires_in_seconds": int(remaining),
        "arguments_hash": digest,
    }
