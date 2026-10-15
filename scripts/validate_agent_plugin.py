#!/usr/bin/env python3
"""Validate one local Agent Plugins 1.0.0 directory without executing it."""

from __future__ import annotations

import argparse
import json

from ai_gateway_core.agent_plugins import (
    AgentPluginLoadError,
    AgentPluginMCPServer,
    LoadedAgentPlugin,
    load_agent_plugin,
)


def _server_payload(server: AgentPluginMCPServer) -> dict[str, object]:
    payload: dict[str, object] = {
        "name": server.name,
        "transport": server.transport,
    }
    if server.transport == "streamable-http":
        payload["url"] = server.url
    else:
        payload["command"] = server.command
        payload["args"] = list(server.args)
    return payload


def _validation_payload(package: LoadedAgentPlugin) -> dict[str, object]:
    return {
        "valid": True,
        "spec": "1.0.0",
        "client_mode": "skills+stdio+streamable-http-mcp",
        "plugin": package.manifest.name,
        "skills": [skill.name for skill in package.skills],
        "mcp_present": package.mcp_present,
        "mcp_supported": bool(package.mcp_servers),
        "mcp_servers": [_server_payload(server) for server in package.mcp_servers],
        "diagnostics": [item.to_dict() for item in package.diagnostics],
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Validate Agent Plugins 1.0.0 Skills and MCP compatibility."
    )
    parser.add_argument("plugin_dir")
    args = parser.parse_args()

    try:
        package = load_agent_plugin(args.plugin_dir)
    except AgentPluginLoadError as exc:
        print(json.dumps({"valid": False, "code": exc.code}, sort_keys=True))
        return 1

    print(
        json.dumps(
            _validation_payload(package),
            ensure_ascii=False,
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
