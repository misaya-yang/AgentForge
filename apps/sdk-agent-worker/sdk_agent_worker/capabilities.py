"""Narrow governed tool transport. Only Gateway/Runtime own authority or approvals."""
import asyncio
import hashlib
import json
import os
import re
import time
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import Request, build_opener, ProxyHandler
from .knowledge import NoRedirect

TERMINAL = {"succeeded", "failed", "cancelled", "timeout", "side_effect_unknown"}

def validate_bridge(bridge):
    if not isinstance(bridge, dict) or set(bridge) != {"grant", "run_id", "tools", "timeout_seconds"}:
        raise ValueError("sdk_capability_bridge_invalid")
    import uuid
    if not isinstance(bridge["run_id"], str):
        raise ValueError("sdk_capability_bridge_invalid")
    uuid.UUID(bridge["run_id"])
    if not isinstance(bridge["grant"], str) or re.fullmatch(r"[0-9a-f]{64}", bridge["grant"]) is None or type(bridge["timeout_seconds"]) is not int:
        raise ValueError("sdk_capability_bridge_invalid")
    tools = bridge["tools"]
    if not isinstance(tools, list) or not 0 <= len(tools) <= 16:
        raise ValueError("sdk_capability_bridge_invalid")
    if bridge["timeout_seconds"] != (1800 if tools else 300):
        raise ValueError("sdk_capability_bridge_invalid")
    names = set()
    identities = set()
    for tool in tools:
        if not isinstance(tool, dict) or set(tool) != {"name", "tool", "namespace", "description", "input_schema", "id", "schema_hash", "effect"}:
            raise ValueError("sdk_capability_descriptor_invalid")
        name = tool["name"]
        if not isinstance(name, str) or re.fullmatch(r"platform_capability_[0-9]+", name) is None or name in names:
            raise ValueError("sdk_capability_descriptor_invalid")
        names.add(name)
        if not isinstance(tool["tool"], str) or not 1 <= len(tool["tool"]) <= 160 or not isinstance(tool["id"], str):
            raise ValueError("sdk_capability_descriptor_invalid")
        namespace = tool["namespace"]
        if namespace is not None and (not isinstance(namespace, str) or not 1 <= len(namespace) <= 64):
            raise ValueError("sdk_capability_descriptor_invalid")
        identity = (namespace, tool["tool"])
        if identity in identities:
            raise ValueError("sdk_capability_descriptor_invalid")
        identities.add(identity)
        if tool["effect"] not in {"read", "write", "unknown"} or not isinstance(tool["description"], str) or len(tool["description"]) > 16384 or not isinstance(tool["input_schema"], dict) or len(json.dumps(tool["input_schema"])) > 65536 or re.fullmatch(r"sha256:[0-9a-f]{64}", str(tool["schema_hash"])) is None:
            raise ValueError("sdk_capability_descriptor_invalid")
    return bridge

def _request(launch, operation, body):
    bridge = launch.capability_bridge
    if bridge is None or operation not in {"admit", "execute"}:
        raise PermissionError("sdk_capability_not_enabled")
    base = os.environ.get("SDK_AGENT_GATEWAY_URL", "")
    url = urlsplit(base)
    if (url.scheme != "https" and not (url.scheme == "http" and url.hostname in ("localhost", "127.0.0.1", "::1"))) or not url.hostname or url.username or url.password or url.query or url.fragment or url.path not in ("", "/"):
        raise PermissionError("sdk_gateway_origin_invalid")
    if operation == "execute" and not bridge["tools"]:
        raise PermissionError("sdk_capability_not_enabled")
    body = dict(body, run_id=bridge["run_id"])
    request = Request(base.rstrip("/") + "/internal/v1/sdk-tools/capabilities/" + operation,
        data=json.dumps(body, separators=(",", ":"), allow_nan=False).encode(), method="POST",
        headers={"Authorization": "Bearer " + bridge["grant"], "Content-Type": "application/json"})
    try:
        response = build_opener(ProxyHandler({}), NoRedirect()).open(request, timeout=50)
    except HTTPError as error:
        # Preserve status for admission handling while releasing the error body.
        error.close()
        raise
    with response:
        raw = response.read(1048577)
        if len(raw) > 1048576:
            raise ValueError("sdk_capability_output_limit")
        return json.loads(raw)

async def ensure_access(launch):
    if launch.capability_bridge is not None:
        result = await asyncio.to_thread(_request, launch, "admit", {})
        if result.get("authorized") is not True:
            raise PermissionError("sdk_capability_not_authorized")

class GovernedTools:
    def __init__(self, launch, emit):
        self.launch, self.emit = launch, emit
        self.poisoned = False
        self._request_lock = asyncio.Lock()
        self.tools = {tool["name"]: tool for tool in launch.capability_bridge["tools"]} if launch.capability_bridge else {}

    async def invoke(self, name, arguments, call_id):
        if self.poisoned:
            raise RuntimeError("sdk_capability_reconciliation_required")
        try:
            return await self._invoke(name, arguments, call_id)
        except Exception:
            self.poisoned = True
            raise

    async def _invoke(self, name, arguments, call_id):
        if name not in self.tools or not isinstance(call_id, str) or re.fullmatch(r"[A-Za-z0-9_.:/-]{1,160}", call_id) is None or not isinstance(arguments, dict):
            raise PermissionError("sdk_tool_call_identity_required")
        # Match Gateway/Runtime compact UTF-8 argument accounting. Escaped ASCII
        # JSON rejects valid multilingual actions before approval admission.
        if len(json.dumps(arguments, ensure_ascii=False, separators=(",", ":"), allow_nan=False).encode()) > 65536:
            raise ValueError("sdk_capability_arguments_invalid")
        tool = self.tools[name]
        body = {"tool_call_id": call_id, "tool": tool["tool"], "namespace": tool["namespace"], "arguments": arguments}
        deadline = time.monotonic() + 660
        approval_id = None
        while True:
            # Transport uncertainty is fatal. No blind external retry; Runtime's
            # prepare-before-send barrier/receipts are the recovery authority.
            # Serialize boundary crossings, including approval polls. Once any
            # call becomes unknown, no other pending call may newly dispatch.
            async with self._request_lock:
                if self.poisoned:
                    raise RuntimeError("sdk_capability_reconciliation_required")
                try:
                    result = await asyncio.to_thread(_request, self.launch, "execute", body)
                    if result.get("run_id") != self.launch.capability_bridge["run_id"] or result.get("tool_call_id") != call_id or result.get("tool") != tool["tool"]:
                        raise PermissionError("sdk_capability_receipt_mismatch")
                    status = result.get("status")
                    if status not in TERMINAL | {"approval_required"}:
                        raise ValueError("sdk_capability_nonterminal")
                    if status == "side_effect_unknown":
                        self.poisoned = True
                except (Exception, asyncio.CancelledError):
                    # Cancelling to_thread does not stop an in-flight request.
                    # Fence siblings before releasing the boundary lock; its
                    # remote outcome remains unknown and must not be replayed.
                    self.poisoned = True
                    raise
            base = {"run_id": result["run_id"], "tool_call_id": call_id, "tool": tool["tool"]}
            if status == "approval_required":
                current = result.get("approval_id")
                if not isinstance(current, str) or approval_id not in (None, current):
                    raise PermissionError("sdk_approval_identity_changed")
                if approval_id is None:
                    approval_id = current
                    await self.emit(dict(base, event="approval_required", approval_id=current,
                        expires_at=result.get("expires_at"), preview=result.get("preview")))
                if time.monotonic() >= deadline:
                    raise TimeoutError("sdk_approval_timeout")
                await asyncio.sleep(1)
                continue
            if status not in TERMINAL:
                raise ValueError("sdk_capability_nonterminal")
            if approval_id:
                await self.emit(dict(base, event="approval_result", approval_id=approval_id, status=status, approval_status=result.get("approval_status")))
            await self.emit(dict(base, event="tool_result", status=status, execution_id=result.get("execution_id")))
            if status == "side_effect_unknown":
                # Do not ask the SDK to continue and potentially repeat an action.
                raise RuntimeError("sdk_capability_outcome_unknown")
            if self.poisoned:
                raise RuntimeError("sdk_capability_reconciliation_required")
            return {"status": status, "execution_id": result.get("execution_id"), "receipt": result.get("receipt"), "response": result.get("response")}

class ClaudeCallIds:
    """Bind the SDK PreToolUse ID to its custom MCP handler without inventing IDs.

    The pinned SDK handler receives only arguments. Duplicate simultaneous calls
    with identical name+arguments are ambiguous and therefore fail before dispatch.
    """
    def __init__(self, governed):
        self.names = {"mcp__platform__" + name for name in governed.tools}
        self.pending = {}
        self.seen = set()

    @staticmethod
    def key(name, arguments):
        return (name, hashlib.sha256(json.dumps(arguments, sort_keys=True, separators=(",", ":"), allow_nan=False).encode()).hexdigest())

    async def before(self, data, tool_use_id, context):
        name = data.get("tool_name")
        if name not in self.names:
            return {}
        call_id = data.get("tool_use_id")
        arguments = data.get("tool_input")
        if not isinstance(call_id, str) or re.fullmatch(r"[A-Za-z0-9_.:/-]{1,160}", call_id) is None or tool_use_id not in (None, call_id) or not isinstance(arguments, dict) or call_id in self.seen or len(self.seen) >= 128:
            return {"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "Platform tool identity could not be verified"}}
        self.seen.add(call_id)
        self.pending.setdefault(self.key(name, arguments), []).append(call_id)
        return {}

    def take(self, name, arguments):
        ids = self.pending.pop(self.key("mcp__platform__" + name, arguments), [])
        if len(ids) != 1:
            raise PermissionError("sdk_tool_call_identity_ambiguous")
        return ids[0]
