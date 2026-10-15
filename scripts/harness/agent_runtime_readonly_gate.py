"""Deterministic CHR-03 read-only contributor contract gate.

The gate is intentionally in-process: it proves the binding and projection
contract without starting a second Agent loop or making provider requests.
Docker/provider acceptance remains a parent-runtime gate.
"""

from __future__ import annotations

import asyncio

from src.services.agent_runtime.readonly_capabilities import (
    CapabilityDescriptor,
    CapabilityItem,
    ReadonlyCapabilityBridge,
    RuntimeCapabilityScope,
    TurnInput,
)


class _ContextContributor:
    def contribute(self, scope: RuntimeCapabilityScope) -> list[CapabilityItem]:
        return [
            CapabilityItem(
                item_id="context:agent-spec",
                kind="context",
                tenant_id=scope.tenant_id,
                capability_revision=scope.capability_revision,
                source="agent-spec",
                payload={"schema_version": "agent-spec/v1"},
            )
        ]


class _TurnInputContributor:
    async def contribute(
        self, scope: RuntimeCapabilityScope, turn: TurnInput
    ) -> list[CapabilityItem]:
        return [
            CapabilityItem(
                item_id=f"turn-input:{turn.turn_id}",
                kind="turn_input",
                tenant_id=scope.tenant_id,
                capability_revision=scope.capability_revision,
                source="turn-input",
                payload={
                    "attachment_ids": list(turn.attachment_ids),
                    "knowledge_refs": list(turn.knowledge_refs),
                },
            )
        ]


class _ToolContributor:
    def describe(self, scope: RuntimeCapabilityScope) -> list[CapabilityDescriptor]:
        return [
            CapabilityDescriptor(
                name="knowledge.read",
                description="Read authorized Knowledge content.",
                schema={"type": "object", "properties": {}},
                tenant_id=scope.tenant_id,
                capability_revision=scope.capability_revision,
                source="knowledge",
                kind="knowledge",
                tags=("read", "retrieval"),
                category="retrieval",
            ),
            CapabilityDescriptor(
                name="office.read",
                description="Read authorized office artifacts.",
                schema={"type": "object", "properties": {}},
                tenant_id=scope.tenant_id,
                capability_revision=scope.capability_revision,
                source="office",
                kind="office_read",
                tags=("read", "artifact"),
                category="retrieval",
            ),
        ]


class _McpContributor:
    async def discover(self, scope: RuntimeCapabilityScope) -> list[CapabilityDescriptor]:
        return [
            CapabilityDescriptor(
                name="docs.read",
                description="Read an authorized MCP documentation resource.",
                schema={"type": "object", "properties": {}},
                tenant_id=scope.tenant_id,
                capability_revision=scope.capability_revision,
                source="mcp:docs",
                kind="mcp",
                protocol="mcp",
                tags=("read", "documentation"),
            )
        ]


async def _run() -> None:
    scope = RuntimeCapabilityScope("tenant-gate", "user-gate", "session-gate", 11, "snapshot-gate")
    bridge = ReadonlyCapabilityBridge(
        scope,
        context_contributors=[_ContextContributor()],
        turn_input_contributors=[_TurnInputContributor()],
        tool_contributors=[_ToolContributor()],
        mcp_contributors=[_McpContributor()],
    )
    context = await bridge.collect_context()
    inputs = await bridge.collect_turn_input(
        TurnInput("turn-gate", attachment_ids=("attachment-1",), knowledge_refs=("source-1",))
    )
    tools = await bridge.discover_tools(metadata={"tags": ["read"]})
    items = await bridge.project_items(
        [
            bridge.knowledge(source_id="source-1", content="untrusted retrieval"),
            bridge.attachment(attachment_id="attachment-1", content_ref="blob-1"),
            bridge.citation(citation_id="citation-1", source_id="source-1", locator="p.1"),
            bridge.artifact(artifact_id="artifact-1", content_ref="blob-2", media_type="text/plain"),
            bridge.office_read(artifact_id="artifact-1", format="docx", extracted={"paragraphs": 1}),
        ]
    )
    assert len(context) == 1
    assert inputs[0].payload["knowledge_refs"] == ["source-1"]
    assert [item.name for item in tools] == ["knowledge.read", "docs.read", "office.read"]
    assert {item.kind for item in items} == {
        "knowledge",
        "attachment",
        "citation",
        "artifact",
        "office_read",
    }
    assert all(item.tenant_id == scope.tenant_id for item in items)
    assert all(item.capability_revision == scope.capability_revision for item in items)


def run_gate() -> None:
    asyncio.run(_run())


if __name__ == "__main__":
    run_gate()
    print("AI_PLATFORM_AGENT_RUNTIME_READONLY_GATE_OK")
