"""Stable facade for the Gateway-owned Agent Runtime control plane."""

from __future__ import annotations

import asyncio
import contextlib
import logging
import os
import uuid
from collections.abc import AsyncIterator, Mapping
from typing import Any

import httpx
from ai_gateway_contracts.agent_launch import ResolvedAgentLaunchV1
from ai_gateway_contracts.agent_runtime_lease import RuntimeModelLeaseSigner

from ...core.gateway.admission import _finish_cleanup
from .capability_catalog import (
    CapabilityCatalogClient,
    HttpCapabilityCatalogClient,
    UnavailableCapabilityCatalogClient,
)
from .control import (
    approvals,
    capability_catalog,
    capacity,
    event_stream,
    memory_context,
    run_ledger,
    snapshot_builder,
    thread_lifecycle,
    turn_start,
)
from .control.types import (
    BASE_AGENT_INSTRUCTIONS_V1,
    AgentRuntimeControlError,
    AgentTurn,
    _Database,
)
from .control.types import DISCOVERY_BRIDGE_NAMES as DISCOVERY_BRIDGE_NAMES
from .control.types import (
    GENERIC_AGENT_INSTRUCTIONS_V1 as GENERIC_AGENT_INSTRUCTIONS_V1,
)
from .control.types import (
    KERNEL_OWNED_AGENT_TOOL_ALIASES as KERNEL_OWNED_AGENT_TOOL_ALIASES,
)
from .control.types import (
    _provider_revision as _control_provider_revision,
)

logger = logging.getLogger(__name__)


def _project_child_runtime_event(
    envelope: dict[str, Any], parent_turn_id: str
) -> dict[str, Any] | None:
    """Preserve the historical module-level projection seam."""
    return event_stream.project_child_runtime_event(envelope, parent_turn_id)


def _provider_revision(value: Any) -> str:
    """Preserve the historical module-level provider-revision seam."""
    return _control_provider_revision(value)


class AgentRuntimeControlPlane:
    """Compatibility facade delegating each responsibility to ``control``."""

    def __init__(
        self,
        *,
        database: _Database,
        model_service: Any,
        provider_service: Any,
        assignment_store: Any,
        lease_signer: RuntimeModelLeaseSigner,
        runtime_url: str,
        runtime_internal_token: str,
        model_plane_base_url: str,
        kernel_revision: str,
        memory_service: Any | None = None,
        http_client: httpx.AsyncClient | None = None,
        capability_catalog_client: CapabilityCatalogClient | None = None,
        admission_controller: Any | None = None,
        capacity_resolver: Any | None = None,
    ) -> None:
        if not runtime_url or not runtime_internal_token or not model_plane_base_url:
            raise ValueError("Agent Runtime control-plane endpoints and token are required")
        if not kernel_revision:
            raise ValueError("Agent Runtime kernel revision is required")
        self.admission_controller = admission_controller
        self.capacity_resolver = capacity_resolver
        self._run_capacity_leases: dict[str, Any] = {}
        self._run_capacity_tasks: dict[str, Any] = {}
        self._run_capacity_contexts: dict[str, Any] = {}
        self.database = database
        self.model_service = model_service
        self.provider_service = provider_service
        self.assignment_store = assignment_store
        self.lease_signer = lease_signer
        self.runtime_url = runtime_url.rstrip("/")
        self.runtime_internal_token = runtime_internal_token
        self.model_plane_base_url = model_plane_base_url.rstrip("/")
        self.kernel_revision = kernel_revision
        self.memory_service = memory_service
        if self.memory_service is None:
            with contextlib.suppress(Exception):
                from ai_gateway_core.memory import MemoryService

                self.memory_service = MemoryService(database)
        self.http_client = http_client or httpx.AsyncClient(
            timeout=httpx.Timeout(connect=10.0, read=300.0, write=30.0, pool=10.0),
            limits=httpx.Limits(max_connections=100, max_keepalive_connections=20),
            trust_env=False,
        )
        self._owns_http_client = http_client is None
        self.capability_catalog_client = (
            capability_catalog_client or UnavailableCapabilityCatalogClient()
        )
        self._capability_plane_url = ""
        self.lease_ttl_seconds = max(
            30,
            min(
                int(os.getenv("AI_PLATFORM_AGENT_RUNTIME_LEASE_TTL_SECONDS", "900")),
                3600,
            ),
        )
        self.max_model_calls = max(
            1,
            min(
                int(
                    # Preserve a hard cap while leaving enough room for a
                    # bounded multi-document Web research turn, approvals,
                    # one schema repair, and a final response.
                    os.getenv("AI_PLATFORM_AGENT_RUNTIME_MAX_MODEL_CALLS_PER_TURN", "24")
                ),
                128,
            ),
        )
        self.max_cost_microusd = max(
            1,
            int(os.getenv("AI_PLATFORM_AGENT_RUNTIME_MAX_COST_MICROUSD_PER_TURN", "5000000")),
        )
        self._thread_locks: dict[tuple[str, str, str], asyncio.Lock] = {}
        self._thread_lock_users: dict[asyncio.Lock, int] = {}

    @property
    def capability_plane_url(self) -> str:
        """Dated compatibility seam for an explicitly external catalog service."""

        return self._capability_plane_url

    @capability_plane_url.setter
    def capability_plane_url(self, value: str) -> None:
        normalized = str(value or "").strip().rstrip("/")
        if not normalized:
            self._capability_plane_url = ""
            self.capability_catalog_client = UnavailableCapabilityCatalogClient()
            return
        self.capability_catalog_client = HttpCapabilityCatalogClient(
            base_url=normalized,
            internal_token=self.runtime_internal_token,
            http_client=self.http_client,
        )
        self._capability_plane_url = normalized

    async def close(self) -> None:
        await capacity.close(self)
        if self._owns_http_client:
            await self.http_client.aclose()

    async def cleanup_session(
        self,
        *,
        tenant_id: str,
        user_id: str,
        session_id: str,
    ) -> bool:
        return await thread_lifecycle.cleanup_session(
            self, tenant_id=tenant_id, user_id=user_id, session_id=session_id
        )

    async def _assignment(self, tenant_id: str, user_id: str, session_id: str):
        return await thread_lifecycle.assignment(self, tenant_id, user_id, session_id)

    async def _existing_thread(
        self, tenant_id: str, user_id: str, session_id: str
    ) -> dict[str, Any] | None:
        return await thread_lifecycle.existing_thread(self, tenant_id, user_id, session_id)

    @staticmethod
    def _assert_dynamic_tool_fingerprint(
        existing: dict[str, Any], requested_fingerprint: str
    ) -> None:
        thread_lifecycle.assert_dynamic_tool_fingerprint(existing, requested_fingerprint)

    async def _bind_dynamic_tool_fingerprint(
        self,
        *,
        existing: dict[str, Any],
        fingerprint: str,
        tenant_id: str,
        user_id: str,
        session_id: str,
        created_by_this_request: bool = False,
    ) -> dict[str, Any]:
        return await thread_lifecycle.bind_dynamic_tool_fingerprint(
            self,
            existing=existing,
            fingerprint=fingerprint,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            created_by_this_request=created_by_this_request,
        )

    def _runtime_model_config(
        self, model_id: str, *, native_web_search_enabled: bool = False
    ) -> dict[str, Any]:
        return snapshot_builder.runtime_model_config(
            self.model_plane_base_url,
            model_id,
            native_web_search_enabled=native_web_search_enabled,
        )

    @staticmethod
    def _dynamic_tools(readonly: dict[str, Any]) -> list[dict[str, Any]]:
        return snapshot_builder.dynamic_tools(readonly)

    @staticmethod
    def _validate_catalog_descriptor(
        descriptor: Any,
        *,
        tenant_id: str,
        capability_revision: int,
        allow_deferred: bool = False,
    ) -> dict[str, Any]:
        return snapshot_builder.validate_catalog_descriptor(
            descriptor,
            tenant_id=tenant_id,
            capability_revision=capability_revision,
            allow_deferred=allow_deferred,
        )

    @staticmethod
    def _allowlisted_catalog_descriptors(
        descriptors: list[dict[str, Any]],
        allowlist: list[dict[str, Any]] | None,
    ) -> list[dict[str, Any]]:
        return snapshot_builder.allowlisted_catalog_descriptors(
            descriptors, allowlist, _logger=logger
        )

    @staticmethod
    def _snapshot_capability_allowlist(
        snapshot: dict[str, Any] | None,
    ) -> list[dict[str, Any]] | None:
        return snapshot_builder.snapshot_capability_allowlist(snapshot)

    async def ensure_thread(
        self,
        *,
        tenant_id: str,
        user_id: str,
        session_id: str,
        model_id: str,
        provider_id: str | None = None,
        readonly_capabilities: dict[str, Any] | None = None,
        capability_allowlist: list[dict[str, Any]] | None = None,
        native_web_search_enabled: bool = False,
        developer_instructions: str | None = None,
    ) -> dict[str, Any]:
        return await thread_lifecycle.ensure_thread(
            self,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            model_id=model_id,
            provider_id=provider_id,
            readonly_capabilities=readonly_capabilities,
            capability_allowlist=capability_allowlist,
            native_web_search_enabled=native_web_search_enabled,
            developer_instructions=developer_instructions,
        )

    @staticmethod
    def _dynamic_tool_fingerprint(readonly: dict[str, Any]) -> str:
        return snapshot_builder.dynamic_tool_fingerprint(readonly)

    @staticmethod
    def _attachment_tool_descriptor(
        *, tenant_id: str, capability_revision: int, references: list[str]
    ) -> dict[str, Any]:
        return snapshot_builder.attachment_tool_descriptor(
            tenant_id=tenant_id,
            capability_revision=capability_revision,
            references=references,
        )

    @classmethod
    def _attach_read_attachment_descriptors(
        cls,
        readonly: dict[str, Any],
        *,
        tenant_id: str,
        capability_revision: int,
    ) -> None:
        snapshot_builder.attach_read_attachment_descriptors(
            readonly,
            tenant_id=tenant_id,
            capability_revision=capability_revision,
            descriptor_factory=cls._attachment_tool_descriptor,
        )

    @staticmethod
    def _worker_ready_for_writes() -> bool:
        return capability_catalog.worker_ready_for_writes()

    async def _resume_thread(
        self,
        *,
        runtime_thread_id: uuid.UUID,
        tenant_id: str,
        user_id: str,
        session_id: str,
        model_id: str | None,
        base_instructions: str | None = BASE_AGENT_INSTRUCTIONS_V1,
        developer_instructions: str | None = None,
        model_context_window: int | None = None,
        auto_compact_token_limit: int | None = None,
        native_web_search_enabled: bool | None = False,
        tool_policy: dict[str, Any] | None = None,
    ) -> None:
        await thread_lifecycle.resume_thread(
            self,
            runtime_thread_id=runtime_thread_id,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            model_id=model_id,
            base_instructions=base_instructions,
            developer_instructions=developer_instructions,
            model_context_window=model_context_window,
            auto_compact_token_limit=auto_compact_token_limit,
            native_web_search_enabled=native_web_search_enabled,
            tool_policy=tool_policy,
        )

    async def verify_thread(
        self,
        *,
        runtime_thread_id: str,
        tenant_id: str,
        user_id: str,
        session_id: str,
        model_id: str,
    ) -> None:
        await thread_lifecycle.verify_thread(
            self,
            runtime_thread_id=runtime_thread_id,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            model_id=model_id,
        )

    @staticmethod
    def _readonly_capability_payload(
        value: dict[str, Any] | None,
        *,
        tenant_id: str,
        capability_revision: int,
    ) -> dict[str, Any]:
        return snapshot_builder.readonly_capability_payload(
            value,
            tenant_id=tenant_id,
            capability_revision=capability_revision,
        )

    @staticmethod
    def _turn_prompt_readonly(readonly: Mapping[str, Any]) -> dict[str, Any]:
        return snapshot_builder.turn_prompt_readonly(readonly)

    async def _fetch_capability_catalog(
        self,
        readonly: dict[str, Any],
        *,
        tenant_id: str,
        user_id: str,
        session_id: str,
        model_id: str,
        capability_revision: int,
        capability_allowlist: list[dict[str, Any]] | None = None,
    ) -> None:
        await capability_catalog.fetch_capability_catalog(
            self,
            readonly,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            model_id=model_id,
            capability_revision=capability_revision,
            capability_allowlist=capability_allowlist,
        )

    async def _load_memory_context(
        self,
        *,
        tenant_id: str,
        user_id: str,
        mode: str,
    ) -> dict[str, Any] | None:
        return await memory_context.load_memory_context(
            self.memory_service, tenant_id=tenant_id, user_id=user_id, mode=mode
        )

    async def start_turn(
        self,
        *,
        tenant_id: str,
        user_id: str,
        session_id: str,
        message: str,
        image_inputs: list[str] | None = None,
        model_id: str,
        reasoning_option: str | None,
        legacy_thinking_level: str | None,
        max_tokens: int | None,
        temperature: float | None = None,
        readonly_capabilities: dict[str, Any] | None = None,
        resolved_agent_snapshot: dict[str, Any] | None = None,
        resolved_agent_launch: ResolvedAgentLaunchV1 | None = None,
        developer_instructions: str | None = None,
        style_guidance: str | None = None,
        memory_mode: str = "auto",
        memory_profile: str | None = None,
        enable_dynamic_tools: bool = True,
    ) -> AgentTurn:
        lease = await capacity.acquire(self, tenant_id=tenant_id, user_id=user_id,
                                       kind="run", request_id=str(uuid.uuid4()))
        pending: dict[str, Any] = {}
        capture = capacity.PENDING_START.set(pending)
        try:
            turn = await turn_start.start_turn(
                self,
                tenant_id=tenant_id,
                user_id=user_id,
                session_id=session_id,
                message=message,
                image_inputs=image_inputs,
                model_id=model_id,
                reasoning_option=reasoning_option,
                legacy_thinking_level=legacy_thinking_level,
                max_tokens=max_tokens,
                temperature=temperature,
                readonly_capabilities=readonly_capabilities,
                resolved_agent_snapshot=resolved_agent_snapshot,
                resolved_agent_launch=resolved_agent_launch,
                developer_instructions=developer_instructions,
                style_guidance=style_guidance,
                memory_mode=memory_mode,
                memory_profile=memory_profile,
                enable_dynamic_tools=enable_dynamic_tools,
                _logger=logger,
                _provider_revision_func=_provider_revision,
        )
        except BaseException as exc:
            if lease is not None:
                pending_turn = pending.get("turn")
                if pending_turn is None or isinstance(exc, (httpx.ConnectError, httpx.ConnectTimeout, httpx.PoolTimeout)):
                    await lease.release()
                else:
                    scope = {"tenant_id": tenant_id, "user_id": user_id, "session_id": session_id}
                    self._run_capacity_leases[pending_turn.run_id] = lease
                    self._run_capacity_contexts[pending_turn.run_id] = (pending_turn, scope)
                    if await _finish_cleanup(asyncio.create_task(capacity.stop_run(self, pending_turn, scope))):
                        await capacity.release_run(self, pending_turn.run_id)
            raise
        finally:
            capacity.PENDING_START.reset(capture)
        if lease is not None:
            self._run_capacity_leases[turn.run_id] = lease
            self._run_capacity_contexts[turn.run_id] = (turn, {
                "tenant_id": tenant_id, "user_id": user_id, "session_id": session_id,
            })
            self._run_capacity_tasks[turn.run_id] = asyncio.create_task(capacity.watch_run(
                self, turn, lease, tenant_id=tenant_id, user_id=user_id, session_id=session_id,
            ))
        return turn


    async def stream_events(
        self,
        *,
        turn: AgentTurn,
        tenant_id: str,
        user_id: str,
        session_id: str,
    ) -> AsyncIterator[bytes]:
        lease = await capacity.acquire(self, tenant_id=tenant_id, user_id=user_id,
                                       kind="sse", request_id=str(uuid.uuid4()))
        chunks = event_stream.stream_events(
            self, turn=turn, tenant_id=tenant_id, user_id=user_id, session_id=session_id, _logger=logger,
        )
        try:
            async with contextlib.aclosing(chunks):
                async for frame in chunks:
                    yield frame
        finally:
            if lease is not None:
                await lease.release()

    async def stream_thread_events(
        self,
        *,
        runtime_thread_id: str,
        tenant_id: str,
        user_id: str,
        session_id: str,
        after_sequence: int = 0,
        limit: int = 1000,
        turn_id: str | None = None,
    ) -> AsyncIterator[dict[str, Any]]:
        lease = await capacity.acquire(self, tenant_id=tenant_id, user_id=user_id,
                                       kind="sse", request_id=str(uuid.uuid4()))
        chunks = event_stream.stream_thread_events(
                self,
                runtime_thread_id=runtime_thread_id,
                tenant_id=tenant_id,
                user_id=user_id,
                session_id=session_id,
                after_sequence=after_sequence,
                limit=limit,
                turn_id=turn_id,
                _projector=_project_child_runtime_event,
        )
        try:
            async with contextlib.aclosing(chunks):
                async for envelope in chunks:
                    yield envelope
        finally:
            if lease is not None:
                await lease.release()

    async def recover_turn(self, *, runtime_thread_id: str, turn_id: str, tenant_id: str, user_id: str, session_id: str) -> dict[str, Any]:
        return await thread_lifecycle.recover_turn(self, runtime_thread_id=runtime_thread_id, turn_id=turn_id, tenant_id=tenant_id, user_id=user_id, session_id=session_id)

    async def interrupt_turn(
        self,
        *,
        runtime_thread_id: str,
        turn_id: str,
        tenant_id: str,
        user_id: str,
        session_id: str,
        reason: str = "client_interrupt",
    ) -> None:
        await thread_lifecycle.interrupt_turn(
            self,
            runtime_thread_id=runtime_thread_id,
            turn_id=turn_id,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
            reason=reason,
        )

    async def get_approval(
        self,
        *,
        approval_id: str,
        tenant_id: str,
        user_id: str,
        session_id: str,
    ) -> dict[str, Any] | None:
        return await approvals.get_approval(
            self,
            approval_id=approval_id,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
        )

    async def decide_approval(
        self,
        *,
        approval_id: str,
        approved: bool,
        reason: str | None = None,
        tenant_id: str,
        user_id: str,
        session_id: str,
    ) -> dict[str, Any]:
        return await approvals.decide_approval(
            self,
            approval_id=approval_id,
            approved=approved,
            reason=reason,
            tenant_id=tenant_id,
            user_id=user_id,
            session_id=session_id,
        )

    async def _complete_run(self, run_id: uuid.UUID, terminal_status: str) -> None:
        await run_ledger.complete_run(self, run_id, terminal_status)
        await capacity.release_run(self, str(run_id))

    async def _fail_run(self, run_id: uuid.UUID, snapshot_id: uuid.UUID, reason: str) -> None:
        await run_ledger.fail_run(self, run_id, snapshot_id, reason)


__all__ = [
    "AgentTurn",
    "AgentRuntimeControlError",
    "AgentRuntimeControlPlane",
]
