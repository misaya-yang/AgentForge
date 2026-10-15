from __future__ import annotations

import pytest

from src.services.agent_runtime.readonly_capabilities import (
    CapabilityDescriptor,
    CapabilityItem,
    ReadonlyCapabilityBridge,
    ReadonlyCapabilityError,
    RuntimeCapabilityScope,
    TurnInput,
)

SCOPE = RuntimeCapabilityScope(
    tenant_id="tenant-a",
    user_id="user-a",
    session_id="session-a",
    capability_revision=7,
    snapshot_id="snapshot-a",
)


class Contexts:
    def contribute(self, scope: RuntimeCapabilityScope):
        return [
            CapabilityItem(
                item_id="context:policy",
                kind="context",
                tenant_id=scope.tenant_id,
                capability_revision=scope.capability_revision,
                source="platform-policy",
                payload={"text": "read-only policy"},
            )
        ]


class Inputs:
    async def contribute(self, scope: RuntimeCapabilityScope, turn: TurnInput):
        return [
            CapabilityItem(
                item_id=f"turn:{turn.turn_id}:knowledge",
                kind="turn_input",
                tenant_id=scope.tenant_id,
                capability_revision=scope.capability_revision,
                source="turn-input",
                payload={"knowledge_refs": list(turn.knowledge_refs)},
            )
        ]


class Tools:
    def describe(self, scope: RuntimeCapabilityScope):
        return [
            CapabilityDescriptor(
                name="knowledge.read",
                description="Read an authorized knowledge source.",
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
                description="Read an authorized office artifact.",
                schema={"type": "object", "properties": {}},
                tenant_id=scope.tenant_id,
                capability_revision=scope.capability_revision,
                source="office",
                kind="office_read",
                tags=("read", "artifact"),
                category="retrieval",
            ),
        ]


class McpTools:
    async def discover(self, scope: RuntimeCapabilityScope):
        return [
            CapabilityDescriptor(
                name="docs.search",
                description="Search an explicitly authorized documentation server.",
                schema={"type": "object", "properties": {}},
                tenant_id=scope.tenant_id,
                capability_revision=scope.capability_revision,
                source="mcp:docs",
                kind="mcp",
                protocol="mcp",
                tags=("read", "documentation"),
            )
        ]


@pytest.mark.asyncio
async def test_collectors_bind_scope_and_project_all_readonly_item_kinds() -> None:
    bridge = ReadonlyCapabilityBridge(
        SCOPE,
        context_contributors=[Contexts()],
        turn_input_contributors=[Inputs()],
        tool_contributors=[Tools()],
        mcp_contributors=[McpTools()],
    )

    context = await bridge.collect_context()
    turn_input = await bridge.collect_turn_input(
        TurnInput(turn_id="turn-a", knowledge_refs=("source-a",))
    )
    tools = await bridge.discover_tools(metadata={"tags": ["read"]})
    projected = await bridge.project_items(
        [
            bridge.knowledge(source_id="source-a", content="retrieved"),
            bridge.attachment(attachment_id="attachment-a", content_ref="blob:a"),
            bridge.citation(citation_id="citation-a", source_id="source-a", locator="p.1"),
            bridge.artifact(artifact_id="artifact-a", content_ref="blob:b", media_type="text/plain"),
            bridge.office_read(artifact_id="artifact-a", format="docx", extracted={"paragraphs": 1}),
        ]
    )

    assert [item.item_id for item in context] == ["context:policy"]
    assert turn_input[0].payload["knowledge_refs"] == ["source-a"]
    assert [tool.name for tool in tools] == ["knowledge.read", "docs.search", "office.read"]
    assert {item.kind for item in projected} == {
        "knowledge",
        "attachment",
        "citation",
        "artifact",
        "office_read",
    }


@pytest.mark.asyncio
async def test_discovery_is_metadata_only_and_never_crosses_tenant_or_revision() -> None:
    bridge = ReadonlyCapabilityBridge(SCOPE, tool_contributors=[Tools()])

    with pytest.raises(ReadonlyCapabilityError, match="METADATA_ONLY"):
        await bridge.discover_tools(metadata={"query": "knowledge"})
    with pytest.raises(ReadonlyCapabilityError, match="DISCOVERY_FILTER"):
        await bridge.discover_tools(metadata={"description": "knowledge"})
    with pytest.raises(ReadonlyCapabilityError, match="SCOPE_MISMATCH"):
        await bridge.collect_context(scope=RuntimeCapabilityScope("tenant-b", "user-a", "session-a", 7))

    class ForeignTools:
        def describe(self, scope: RuntimeCapabilityScope):
            del scope
            return [
                CapabilityDescriptor(
                    name="foreign.read",
                    description="Foreign tenant read.",
                    schema={"type": "object"},
                    tenant_id="tenant-b",
                    capability_revision=7,
                    source="foreign",
                )
            ]

    with pytest.raises(ReadonlyCapabilityError, match="REVISION_MISMATCH"):
        await ReadonlyCapabilityBridge(SCOPE, tool_contributors=[ForeignTools()]).discover_tools()


def test_projection_payload_is_copied_and_authority_is_non_authoritative() -> None:
    bridge = ReadonlyCapabilityBridge(SCOPE)
    raw = {"content": {"value": 1}}
    item = bridge.project(item_id="source:a", kind="knowledge", source="source", payload=raw)
    raw["content"]["value"] = 9
    assert item.payload["content"]["value"] == 1
    assert item.authority == "non_authoritative"
    with pytest.raises(ReadonlyCapabilityError, match="READONLY"):
        CapabilityDescriptor(
            name="write.danger",
            description="Not allowed.",
            schema={"type": "object"},
            tenant_id=SCOPE.tenant_id,
            capability_revision=SCOPE.capability_revision,
            source="danger",
            read_only=False,
        )
