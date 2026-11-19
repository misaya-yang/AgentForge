"""LangChain create_agent through the local gateway and its read-only MCP tools."""
import json
import os
from urllib.parse import urlsplit

import httpx
from langchain.agents import create_agent
from langchain_core.tools import StructuredTool
from langchain_openai import ChatOpenAI

EVIDENCE = {"calls": [], "item_ids": set()}


def config():
    base = (os.environ.get("OPENAI_BASE_URL") or os.environ.get("BASE_URL") or "http://127.0.0.1:8080/v1").rstrip("/")
    key = os.environ.get("OPENAI_API_KEY") or os.environ.get("API_KEY")
    if not key:
        raise RuntimeError("Set OPENAI_API_KEY to a platform gateway key")
    origin = urlsplit(base)
    if os.environ.get("LIVE") == "1" and origin.hostname not in ("127.0.0.1", "localhost", "::1"):
        raise RuntimeError("LIVE examples must use the local platform gateway")
    mcp = os.environ.get("MCP_URL") or f"{origin.scheme}://{origin.netloc}/mcp"
    return base, key, mcp


def _record(value):
    if isinstance(value, dict):
        for key, child in value.items():
            if key in ("item_id", "itemId") and isinstance(child, str):
                EVIDENCE["item_ids"].add(child)
            _record(child)
    elif isinstance(value, list):
        for child in value:
            _record(child)
    elif isinstance(value, str):
        try:
            child = json.loads(value)
        except (ValueError, TypeError):
            return
        if isinstance(child, (dict, list)):
            _record(child)


def platform_tools():
    _, key, mcp = config()
    headers = {"Authorization": f"Bearer {key}", "Content-Type": "application/json", "Accept": "application/json, text/event-stream", "MCP-Protocol-Version": "2025-11-25"}

    def rpc(method, params, ident=None):
        body = {"jsonrpc": "2.0", "method": method, "params": params}
        if ident is not None:
            body["id"] = ident
        response = httpx.post(mcp, headers=headers, json=body, timeout=30, trust_env=False)
        response.raise_for_status()
        if response.headers.get("mcp-session-id"):
            headers["MCP-Session-Id"] = response.headers["mcp-session-id"]
        if ident is None:
            return {}
        if "text/event-stream" in response.headers.get("content-type", ""):
            frames = [json.loads(line[5:].strip()) for line in response.text.splitlines() if line.startswith("data:") and line[5:].strip() != "[DONE]"]
            value = next(frame for frame in frames if frame.get("id") == ident)
        else:
            value = response.json()
        if value.get("error"):
            raise RuntimeError(f"Platform MCP {method} failed")
        return value.get("result") or {}

    rpc("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "langgraph-py", "version": "1.0.0"}}, 1)
    rpc("notifications/initialized", {})

    def build(spec):
        def call(**arguments):
            result = rpc("tools/call", {"name": spec["name"], "arguments": arguments}, 3)
            if result.get("isError"):
                raise RuntimeError("Platform tool failed")
            EVIDENCE["calls"].append(spec["name"])
            _record(result)
            return json.dumps(result)
        return StructuredTool.from_function(func=call, name=spec["name"], description=spec.get("description") or spec["name"], args_schema=spec["inputSchema"])

    return [build(spec) for spec in rpc("tools/list", {}, 2).get("tools", []) if spec["name"] in ("space_list", "space_search", "space_read_page")]


def smoke_prompt():
    if os.environ.get("PROMPT"):
        return os.environ["PROMPT"]
    if os.environ.get("ITEM_ID"):
        return f"Use space_read_page to read item {os.environ['ITEM_ID']}. Summarize it and cite its exact item_id in brackets."
    return f"Use space_search for {os.environ.get('QUERY', 'platform')!r}. Summarize one page and cite its exact item_id in brackets. Do not invent sources."


def assert_live_answer(text):
    assert EVIDENCE["calls"], "LIVE agent did not execute a platform tool"
    assert any(item in text for item in EVIDENCE["item_ids"]), "LIVE answer must cite an actual returned item_id; seed an accessible page and set QUERY or ITEM_ID"


def answer(prompt):
    EVIDENCE["calls"].clear()
    EVIDENCE["item_ids"].clear()
    base, key, _ = config()
    model = ChatOpenAI(model=os.environ.get("MODEL", "qwen3.8-flash"), base_url=base, api_key=key, http_client=httpx.Client(trust_env=False))
    agent = create_agent(model, platform_tools(), system_prompt="Answer from platform tools. Cite only exact item_id values returned by tools in [item_id] brackets.")
    result = agent.invoke({"messages": [{"role": "user", "content": prompt}]})
    content = result["messages"][-1].content
    if isinstance(content, list):
        return "".join(part.get("text", "") for part in content if isinstance(part, dict))
    return str(content)
