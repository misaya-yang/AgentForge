"""Tenant-bound, read-only capability projection for the Agent Runtime.

This module is deliberately a bridge, not an Agent loop.  Contributors provide
already-authorized context, input items, and schemas; the bridge only validates
the immutable runtime scope, filters read-only metadata, and projects items to
the Agent turn boundary.  In particular, discovery has no prompt or keyword
argument.  A caller can select capabilities only through explicit metadata
(``kind``, ``source``, ``tags``, and ``protocol``).
"""

from __future__ import annotations

import copy
import inspect
import re
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any, Protocol, TypeVar


class ReadonlyCapabilityError(ValueError):
    """Raised when a capability crosses a tenant/revision or safety boundary."""


_NAME_RE = re.compile(r"^[A-Za-z0-9_.:/-]{1,160}$")
_ALLOWED_ITEM_KINDS = frozenset(
    {
        "context",
        "turn_input",
        "knowledge",
        "attachment",
        "citation",
        "artifact",
        "office_read",
    }
)
_ALLOWED_FILTERS = frozenset({"kind", "source", "tags", "protocol", "category"})
_FORBIDDEN_DISCOVERY_KEYS = frozenset({"query", "prompt", "keywords", "text"})


@dataclass(frozen=True, slots=True)
class RuntimeCapabilityScope:
    """Immutable identity of the runtime snapshot that owns a read."""

    tenant_id: str
    user_id: str
    session_id: str
    capability_revision: int
    snapshot_id: str = ""

    def __post_init__(self) -> None:
        if not all(isinstance(value, str) and value for value in (self.tenant_id, self.user_id, self.session_id)):
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_SCOPE_INVALID")
        if not isinstance(self.capability_revision, int) or self.capability_revision < 1:
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_REVISION_INVALID")


@dataclass(frozen=True, slots=True)
class TurnInput:
    """Request-scoped inputs; no free-form prompt is used for discovery."""

    turn_id: str
    attachment_ids: tuple[str, ...] = ()
    knowledge_refs: tuple[str, ...] = ()
    memory_refs: tuple[str, ...] = ()
    page_context: Mapping[str, Any] = field(default_factory=dict)


@dataclass(frozen=True, slots=True)
class CapabilityItem:
    """One visible, structured contribution to a Agent turn."""

    item_id: str
    kind: str
    tenant_id: str
    capability_revision: int
    source: str
    payload: Mapping[str, Any]
    untrusted: bool = True
    authority: str = "non_authoritative"

    def __post_init__(self) -> None:
        if not _NAME_RE.fullmatch(self.item_id) or not _NAME_RE.fullmatch(self.source):
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_ITEM_ID_INVALID")
        if self.kind not in _ALLOWED_ITEM_KINDS:
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_ITEM_KIND_INVALID")
        if not isinstance(self.payload, Mapping):
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_ITEM_PAYLOAD_INVALID")
        if self.authority != "non_authoritative":
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_AUTHORITY_INVALID")


@dataclass(frozen=True, slots=True)
class CapabilityDescriptor:
    """A tenant-authorized tool schema suitable for metadata discovery."""

    name: str
    description: str
    schema: Mapping[str, Any]
    tenant_id: str
    capability_revision: int
    source: str
    kind: str = "tool"
    read_only: bool = True
    tags: tuple[str, ...] = ()
    protocol: str = "internal"
    category: str = "general"
    metadata: Mapping[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not _NAME_RE.fullmatch(self.name) or not _NAME_RE.fullmatch(self.source):
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_DESCRIPTOR_ID_INVALID")
        if not isinstance(self.description, str) or not self.description:
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_DESCRIPTOR_DESCRIPTION_INVALID")
        if not isinstance(self.schema, Mapping):
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_DESCRIPTOR_SCHEMA_INVALID")
        if not self.read_only:
            raise ReadonlyCapabilityError("RUNTIME_READONLY_CAPABILITY_REQUIRED")
        if any(not isinstance(tag, str) or not tag for tag in self.tags):
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_TAG_INVALID")


class ContextContributor(Protocol):
    def contribute(self, scope: RuntimeCapabilityScope) -> Iterable[CapabilityItem]: ...


class TurnInputContributor(Protocol):
    def contribute(
        self, scope: RuntimeCapabilityScope, turn: TurnInput
    ) -> Iterable[CapabilityItem] | Any: ...


class ToolContributor(Protocol):
    def describe(self, scope: RuntimeCapabilityScope) -> Iterable[CapabilityDescriptor] | Any: ...


class McpServerContributor(Protocol):
    def discover(self, scope: RuntimeCapabilityScope) -> Iterable[CapabilityDescriptor] | Any: ...


class TurnItemContributor(Protocol):
    def project(
        self, scope: RuntimeCapabilityScope, item: CapabilityItem
    ) -> CapabilityItem | None | Any: ...


T = TypeVar("T")


async def _collect(value: Any) -> list[Any]:
    if inspect.isawaitable(value):
        value = await value
    if value is None:
        return []
    if isinstance(value, (str, bytes, Mapping)) or not isinstance(value, Iterable):
        raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_CONTRIBUTION_INVALID")
    return list(value)


def _copy_payload(value: Mapping[str, Any]) -> dict[str, Any]:
    try:
        return copy.deepcopy(dict(value))
    except (TypeError, ValueError) as exc:
        raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_PAYLOAD_INVALID") from exc


class ReadonlyCapabilityBridge:
    """Collect and project read-only capabilities for one runtime snapshot."""

    def __init__(
        self,
        scope: RuntimeCapabilityScope,
        *,
        context_contributors: Sequence[ContextContributor] = (),
        turn_input_contributors: Sequence[TurnInputContributor] = (),
        tool_contributors: Sequence[ToolContributor] = (),
        mcp_contributors: Sequence[McpServerContributor] = (),
        item_contributors: Sequence[TurnItemContributor] = (),
    ) -> None:
        self.scope = scope
        self.context_contributors = tuple(context_contributors)
        self.turn_input_contributors = tuple(turn_input_contributors)
        self.tool_contributors = tuple(tool_contributors)
        self.mcp_contributors = tuple(mcp_contributors)
        self.item_contributors = tuple(item_contributors)

    def _check_scope(self, scope: RuntimeCapabilityScope | None) -> None:
        if scope is not None and scope != self.scope:
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_SCOPE_MISMATCH")

    def _check_item(self, item: CapabilityItem) -> CapabilityItem:
        if (
            item.tenant_id != self.scope.tenant_id
            or item.capability_revision != self.scope.capability_revision
        ):
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_REVISION_MISMATCH")
        return CapabilityItem(
            item_id=item.item_id,
            kind=item.kind,
            tenant_id=item.tenant_id,
            capability_revision=item.capability_revision,
            source=item.source,
            payload=_copy_payload(item.payload),
            untrusted=item.untrusted,
            authority=item.authority,
        )

    def _check_descriptor(self, descriptor: CapabilityDescriptor) -> CapabilityDescriptor:
        if (
            descriptor.tenant_id != self.scope.tenant_id
            or descriptor.capability_revision != self.scope.capability_revision
        ):
            raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_REVISION_MISMATCH")
        return CapabilityDescriptor(
            name=descriptor.name,
            description=descriptor.description,
            schema=_copy_payload(descriptor.schema),
            tenant_id=descriptor.tenant_id,
            capability_revision=descriptor.capability_revision,
            source=descriptor.source,
            kind=descriptor.kind,
            read_only=descriptor.read_only,
            tags=tuple(sorted(set(descriptor.tags))),
            protocol=descriptor.protocol,
            category=descriptor.category,
            metadata=_copy_payload(descriptor.metadata),
        )

    async def collect_context(self, *, scope: RuntimeCapabilityScope | None = None) -> list[CapabilityItem]:
        self._check_scope(scope)
        items: list[CapabilityItem] = []
        for contributor in self.context_contributors:
            items.extend(self._check_item(item) for item in await _collect(contributor.contribute(self.scope)))
        return self._stable_items(items)

    async def collect_turn_input(
        self,
        turn: TurnInput,
        *,
        scope: RuntimeCapabilityScope | None = None,
    ) -> list[CapabilityItem]:
        self._check_scope(scope)
        if not isinstance(turn, TurnInput) or not turn.turn_id:
            raise ReadonlyCapabilityError("RUNTIME_TURN_INPUT_INVALID")
        items: list[CapabilityItem] = []
        for contributor in self.turn_input_contributors:
            items.extend(
                self._check_item(item)
                for item in await _collect(contributor.contribute(self.scope, turn))
            )
        return self._stable_items(items)

    async def discover_tools(
        self,
        *,
        metadata: Mapping[str, Any] | None = None,
        scope: RuntimeCapabilityScope | None = None,
    ) -> list[CapabilityDescriptor]:
        """Return schemas selected only by explicit metadata, never prompt text."""

        self._check_scope(scope)
        filters = dict(metadata or {})
        if _FORBIDDEN_DISCOVERY_KEYS.intersection(filters):
            raise ReadonlyCapabilityError("RUNTIME_METADATA_ONLY_DISCOVERY")
        if set(filters) - _ALLOWED_FILTERS:
            raise ReadonlyCapabilityError("RUNTIME_DISCOVERY_FILTER_INVALID")
        descriptors: list[CapabilityDescriptor] = []
        for contributor in self.tool_contributors:
            descriptors.extend(
                self._check_descriptor(item)
                for item in await _collect(contributor.describe(self.scope))
            )
        for contributor in self.mcp_contributors:
            descriptors.extend(
                self._check_descriptor(item)
                for item in await _collect(contributor.discover(self.scope))
            )
        unique: dict[tuple[str, str], CapabilityDescriptor] = {}
        for descriptor in descriptors:
            key = (descriptor.source, descriptor.name)
            if key in unique:
                raise ReadonlyCapabilityError("RUNTIME_CAPABILITY_NAME_COLLISION")
            if _matches_metadata(descriptor, filters):
                unique[key] = descriptor
        return [unique[key] for key in sorted(unique, key=lambda value: (value[0].casefold(), value[1].casefold()))]

    async def project_item(
        self,
        item: CapabilityItem,
        *,
        scope: RuntimeCapabilityScope | None = None,
    ) -> CapabilityItem | None:
        self._check_scope(scope)
        current = self._check_item(item)
        for contributor in self.item_contributors:
            projected = contributor.project(self.scope, current)
            if inspect.isawaitable(projected):
                projected = await projected
            if projected is None:
                continue
            current = self._check_item(projected)
        return current

    async def project_items(
        self,
        items: Iterable[CapabilityItem],
        *,
        scope: RuntimeCapabilityScope | None = None,
    ) -> list[CapabilityItem]:
        self._check_scope(scope)
        projected: list[CapabilityItem] = []
        for item in items:
            value = await self.project_item(item, scope=scope)
            if value is not None:
                projected.append(value)
        return self._stable_items(projected)

    def project(
        self,
        *,
        item_id: str,
        kind: str,
        source: str,
        payload: Mapping[str, Any],
        untrusted: bool = True,
    ) -> CapabilityItem:
        return self._check_item(
            CapabilityItem(
                item_id=item_id,
                kind=kind,
                tenant_id=self.scope.tenant_id,
                capability_revision=self.scope.capability_revision,
                source=source,
                payload=_copy_payload(payload),
                untrusted=untrusted,
            )
        )

    def knowledge(self, *, source_id: str, content: str, **metadata: Any) -> CapabilityItem:
        return self.project(
            item_id=f"knowledge:{source_id}",
            kind="knowledge",
            source=source_id,
            payload={"source_id": source_id, "content": content, **metadata},
        )

    def attachment(self, *, attachment_id: str, content_ref: str, **metadata: Any) -> CapabilityItem:
        return self.project(
            item_id=f"attachment:{attachment_id}",
            kind="attachment",
            source=attachment_id,
            payload={"attachment_id": attachment_id, "content_ref": content_ref, **metadata},
        )

    def citation(self, *, citation_id: str, source_id: str, locator: str, **metadata: Any) -> CapabilityItem:
        return self.project(
            item_id=f"citation:{citation_id}",
            kind="citation",
            source=source_id,
            payload={"citation_id": citation_id, "source_id": source_id, "locator": locator, **metadata},
        )

    def artifact(self, *, artifact_id: str, content_ref: str, media_type: str, **metadata: Any) -> CapabilityItem:
        return self.project(
            item_id=f"artifact:{artifact_id}",
            kind="artifact",
            source=artifact_id,
            payload={"artifact_id": artifact_id, "content_ref": content_ref, "media_type": media_type, **metadata},
        )

    def office_read(self, *, artifact_id: str, format: str, extracted: Any, **metadata: Any) -> CapabilityItem:
        return self.project(
            item_id=f"office-read:{artifact_id}",
            kind="office_read",
            source=artifact_id,
            payload={"artifact_id": artifact_id, "format": format, "extracted": extracted, **metadata},
        )

    @staticmethod
    def _stable_items(items: Iterable[CapabilityItem]) -> list[CapabilityItem]:
        unique: dict[tuple[str, str], CapabilityItem] = {}
        for item in items:
            key = (item.kind, item.item_id)
            if key not in unique:
                unique[key] = item
        return [unique[key] for key in sorted(unique, key=lambda value: (value[0], value[1]))]


def _matches_metadata(descriptor: CapabilityDescriptor, filters: Mapping[str, Any]) -> bool:
    values: dict[str, Any] = {
        "kind": descriptor.kind,
        "source": descriptor.source,
        "protocol": descriptor.protocol,
        "category": descriptor.category,
        "tags": set(descriptor.tags),
    }
    for key, expected in filters.items():
        if key == "tags":
            wanted = {str(value) for value in expected} if isinstance(expected, (list, tuple, set)) else {str(expected)}
            if not wanted.issubset(values["tags"]):
                return False
        elif values.get(key) != expected:
            return False
    return True


__all__ = [
    "CapabilityDescriptor",
    "CapabilityItem",
    "ContextContributor",
    "McpServerContributor",
    "ReadonlyCapabilityBridge",
    "ReadonlyCapabilityError",
    "RuntimeCapabilityScope",
    "ToolContributor",
    "TurnInput",
    "TurnInputContributor",
    "TurnItemContributor",
]
