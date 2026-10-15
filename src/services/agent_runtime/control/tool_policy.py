"""Authenticated startup ceiling and per-turn tool selection for the Codex host.

The generic Assistant already permits authenticated users to opt into hosted
search (V2 create_turn); this product ceiling is independent of model support.
A turn still needs explicit web-search consent and a supported model profile.
Signed and function-only launches do not inherit that generic product grant.
"""

from __future__ import annotations

from typing import Any

from .snapshot_builder import dynamic_tools

_KERNEL_TOOLS = {
    None: ("update_plan", "spawn_agent", "spawn_subagent", "steer", "wait", "interrupt",
           "send_input", "close_agent", "resume_agent", "send_message", "followup_task",
           "wait_agent", "list_agents", "interrupt_agent"),
    "collaboration": ("spawn_agent", "send_message", "followup_task", "wait_agent", "list_agents", "interrupt_agent"),
    "multi_agent_v1": ("spawn_agent", "send_input", "wait", "wait_agent", "close_agent", "resume_agent"),
}


def runtime_tool_policy(
    readonly: dict[str, Any], *, native_search_allowed: bool = False, startup: bool = False,
    generic_product: bool = False,
) -> dict[str, Any]:
    """Project only pinned descriptors and the existing kernel control tools.

    Startup ignores a temporary ``none`` choice; each turn reapplies it as an
    empty selection. Native shell and Code Mode wrappers gain no implicit grant.
    """
    if not startup and readonly.get("responses_tool_choice") == "none":
        return {"allowedTools": []}
    identities = {(None, tool["name"]) for tool in dynamic_tools(readonly)}
    if readonly.get("responses_tool_names") is None:
        identities.update((namespace, name) for namespace, names in _KERNEL_TOOLS.items() for name in names)
        if native_search_allowed:
            identities.add((None, "web_search"))
        if startup and generic_product:
            # Actual attachment references and ACL stay in each turn snapshot.
            identities.add((None, "read_attachment"))
    return {"allowedTools": [
        {"namespace": namespace, "name": name}
        for namespace, name in sorted(identities, key=lambda value: (value[0] or "", value[1]))
    ]}
