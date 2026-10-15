"""Private worker contract. Public credentials/options cannot override execution policy."""
from dataclasses import dataclass, field
import hashlib
import json
import uuid

FRAMEWORKS = {"openai_agents", "claude_agent_sdk"}
PROTOCOL = "ai-platform/sdk-agent/v1"

@dataclass(frozen=True)
class Launch:
    framework: str
    service_id: str
    tenant_id: str
    user_id: str
    request_id: str
    session_id: str
    model: str
    instructions: str
    max_turns: int
    input: str
    stream: bool
    api_key: str = field(repr=False)
    read_bridge: dict | None = field(default=None, repr=False)
    capability_bridge: dict | None = field(default=None, repr=False)

    @property
    def scope(self):
        # No path fragments, upstream session IDs or user-supplied filesystem paths.
        value = [self.tenant_id, self.user_id, self.service_id, self.framework, self.session_id]
        return hashlib.sha256(json.dumps(value, separators=(",", ":")).encode()).hexdigest()

    @classmethod
    def parse(cls, body, key):
        fields = {"protocol", "framework", "service_id", "tenant_id", "user_id", "request_id", "session_id", "agent", "input", "stream"}
        if not isinstance(body, dict) or set(body) - {"read_bridge", "capability_bridge"} != fields or body.get("protocol") != PROTOCOL:
            raise ValueError("sdk_request_invalid")
        agent = body["agent"]
        if not isinstance(agent, dict) or set(agent) != {"model", "instructions", "max_turns"}:
            raise ValueError("sdk_agent_invalid")
        for name in ("service_id", "tenant_id", "user_id", "request_id", "session_id"):
            value = body[name]
            if not isinstance(value, str) or not value.strip() or len(value) > 256 or any(ord(c) < 32 for c in value):
                raise ValueError("sdk_identity_invalid")
        if body["framework"] not in FRAMEWORKS or type(body["stream"]) is not bool:
            raise ValueError("sdk_framework_invalid")
        if type(agent["max_turns"]) is not int or not 1 <= agent["max_turns"] <= 32:
            raise ValueError("sdk_max_turns_invalid")
        if isinstance(agent["instructions"], str) and not agent["instructions"].strip():
            agent = dict(agent, instructions="You are a helpful assistant.")
        for name, value, maximum in (("model", agent["model"], 160), ("instructions", agent["instructions"], 32768), ("input", body["input"], 262144)):
            if not isinstance(value, str) or not value.strip() or len(value.encode()) > maximum:
                raise ValueError("sdk_" + name + "_invalid")
        if not isinstance(key, str) or not 16 <= len(key) <= 4096 or any(ord(c) < 33 for c in key):
            raise ValueError("sdk_provider_key_required")
        bridge=body.get("read_bridge")
        if bridge is not None:
            if not isinstance(bridge,dict) or set(bridge)!={"grant","dataset_ids"} or not isinstance(bridge["grant"],str) or len(bridge["grant"])!=64 or any(c not in "0123456789abcdef" for c in bridge["grant"]):
                raise ValueError("sdk_read_bridge_invalid")
            ids=bridge["dataset_ids"]
            if not isinstance(ids,list) or not 1<=len(ids)<=16 or any(not isinstance(v,str) for v in ids) or len(set(ids))!=len(ids):
                raise ValueError("sdk_read_bridge_invalid")
            for value in ids: uuid.UUID(value)
        capability=body.get("capability_bridge")
        if capability is not None:
            from .capabilities import validate_bridge
            validate_bridge(capability)
            if capability["tools"] and body["stream"] is not True:
                raise ValueError("sdk_capabilities_require_stream")
        return cls(read_bridge=bridge, capability_bridge=capability, **{k: body[k] for k in ("framework", "service_id", "tenant_id", "user_id", "request_id", "session_id", "input", "stream")}, **agent, api_key=key)
