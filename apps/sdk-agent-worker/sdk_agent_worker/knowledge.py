"""Narrow Gateway-owned KB reads. Never forwards provider API keys or user JWTs."""
import asyncio
import json
import os
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import Request, build_opener, HTTPRedirectHandler, ProxyHandler
from .tools import TOOLS

class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise PermissionError("sdk_gateway_redirect_forbidden")

    def http_error_302(self, request, response, code, message, headers):
        try:
            return super().http_error_302(request, response, code, message, headers)
        except HTTPError:
            # The bridge's open() error boundary owns this response wrapper.
            raise
        except Exception:
            # Location parsing can fail before redirect_request is called.
            # Never drain an untrusted response, including on fixed denial.
            response.close()
            raise

    http_error_301 = http_error_303 = http_error_307 = http_error_308 = http_error_302


def _request(launch, operation, body=None):
    bridge=launch.read_bridge
    if bridge is None:
        raise PermissionError("sdk_knowledge_not_enabled")
    base=os.environ.get("SDK_AGENT_GATEWAY_URL", "")
    url=urlsplit(base)
    if (url.scheme != "https" and not (url.scheme == "http" and url.hostname in ("localhost","127.0.0.1","::1"))) or not url.hostname or url.username or url.password or url.query or url.fragment or url.path not in ("", "/"):
        raise PermissionError("sdk_gateway_origin_invalid")
    if operation not in ("admit","retrieve"):
        raise PermissionError("sdk_knowledge_operation_invalid")
    request=Request(base.rstrip("/")+"/internal/v1/sdk-tools/knowledge/"+operation,
        data=json.dumps(body or {},separators=(",",":")).encode(),method="POST",
        headers={"Authorization":"Bearer "+bridge["grant"],"Content-Type":"application/json"})
    # Operator origin only, no ambient proxy, redirect or caller-chosen path.
    try:
        response = build_opener(ProxyHandler({}),NoRedirect()).open(request,timeout=35)
    except HTTPError as error:
        # open() raises before a response context can acquire this body.
        error.close()
        raise
    with response:
        raw=response.read(1048577)
        if len(raw)>1048576:
            raise ValueError("sdk_knowledge_output_limit")
        return json.loads(raw)

async def ensure_access(launch):
    from .capabilities import ensure_access as capability_access
    await capability_access(launch)
    if launch.read_bridge is not None:
        value=await asyncio.to_thread(_request,launch,"admit")
        if value.get("authorized") is not True:
            raise PermissionError("sdk_knowledge_not_authorized")

def atomic_json(target, value):
    temporary=target.with_suffix(".tmp")
    with temporary.open("w") as stream:
        json.dump(value,stream,separators=(",",":"))
        stream.flush()
        os.fsync(stream.fileno())
    temporary.replace(target)
    directory=os.open(target.parent,os.O_RDONLY | os.O_DIRECTORY)
    try: os.fsync(directory)
    finally: os.close(directory)

def ensure_profile(state, launch):
    """Dataset selection is immutable for an existing session to fence old context."""
    profile={"schema":"sdk-session-tools/v1","dataset_ids":launch.read_bridge["dataset_ids"] if launch.read_bridge else []}
    if launch.capability_bridge is not None and launch.capability_bridge["tools"]:
        profile["capability_tools"]=launch.capability_bridge["tools"]
    target=state/"sdk-profile.json"
    if target.exists():
        if json.loads(target.read_text())!=profile:
            raise PermissionError("sdk_session_profile_changed")
        return
    if (launch.read_bridge is not None or (launch.capability_bridge is not None and launch.capability_bridge["tools"])) and ((state/"openai.sqlite").exists() or (state/"claude-session.json").exists() or any(state.glob("run-*.json"))):
        raise PermissionError("sdk_session_profile_migration_required")
    atomic_json(target,profile)

def tools_for(launch):
    tools=dict(TOOLS)
    if launch.read_bridge is None:
        return tools
    datasets=launch.read_bridge["dataset_ids"]
    async def retrieve(args):
        if not isinstance(args,dict) or set(args)!={"dataset_id","query","top_k"} or args["dataset_id"] not in datasets or not isinstance(args["query"],str) or not 1<=len(args["query"].encode())<=4096 or type(args["top_k"]) is not int or not 1<=args["top_k"]<=5:
            raise ValueError("sdk_knowledge_arguments_invalid")
        return await asyncio.to_thread(_request,launch,"retrieve",args)
    tools["knowledge_retrieve"]=("Retrieve relevant passages from explicitly selected platform knowledge datasets; cite returned sources.",
        {"type":"object","properties":{"dataset_id":{"type":"string","enum":datasets},"query":{"type":"string","maxLength":4096},"top_k":{"type":"integer","minimum":1,"maximum":5}},"required":["dataset_id","query","top_k"],"additionalProperties":False},retrieve)
    return tools
