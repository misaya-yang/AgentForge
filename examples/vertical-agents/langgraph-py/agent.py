"""LangChain 1.x create_agent against a platform base URL and MCP endpoint."""
import json
import os

import httpx
from langchain.agents import create_agent
from langchain_core.tools import StructuredTool
from langchain_openai import ChatOpenAI


def _headers() -> dict[str, str]:
    return {
        "Authorization": f"Bearer {os.environ['API_KEY']}",
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
    }


def _rpc(method: str, params: dict, ident: int):
    response = httpx.post(
        os.environ["MCP_URL"],
        headers=_headers(),
        json={"jsonrpc": "2.0", "id": ident, "method": method, "params": params},
        timeout=30,
    )
    response.raise_for_status()
    body = response.json()
    if "error" in body:
        raise RuntimeError(str(body["error"]))
    return body.get("result") or {}


def platform_tools() -> list[StructuredTool]:
    _rpc(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "langgraph-py", "version": "0.1.0"},
        },
        1,
    )
    httpx.post(
        os.environ["MCP_URL"],
        headers=_headers(),
        json={"jsonrpc": "2.0", "method": "notifications/initialized"},
        timeout=30,
    )
    listed = _rpc("tools/list", {}, 2)
    tools = []
    for spec in listed.get("tools", []):
        name = spec["name"]

        def call(item_id: str, tool_name: str = name) -> str:
            return json.dumps(
                _rpc("tools/call", {"name": tool_name, "arguments": {"item_id": item_id}}, 3)
            )

        tools.append(
            StructuredTool.from_function(
                func=call,
                name=name,
                description=spec.get("description") or name,
            )
        )
    return tools


def answer(prompt: str) -> str:
    model = ChatOpenAI(
        model="gpt-4.1-mini",
        base_url=os.environ["BASE_URL"],
        api_key=os.environ["API_KEY"],
    )
    agent = create_agent(
        model,
        platform_tools(),
        system_prompt="Answer from the platform space. Cite the item id you read.",
    )
    result = agent.invoke({"messages": [{"role": "user", "content": prompt}]})
    content = result["messages"][-1].content
    if isinstance(content, list):
        return "".join(part.get("text", "") for part in content if isinstance(part, dict))
    return str(content)
