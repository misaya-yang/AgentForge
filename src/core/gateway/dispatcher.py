from __future__ import annotations

import asyncio
import logging
from collections.abc import AsyncIterator
from contextlib import suppress
from typing import Any

from ai_gateway_core.comm.retry import RetryPolicy, UpstreamOutcomeUnknown, outcome_is_unknown
from ai_gateway_core.enums import ContentType, StreamEventType
from ai_gateway_core.exceptions import RateLimitExceededError, ServiceNotFoundError

from ...models.request import UnifiedRequest
from ...models.response import StreamChunk, UnifiedResponse
from ...models.service import ServiceDefinition
from ...services.metrics.usage_parser import (
    extract_assistant_id,
    extract_model,
    extract_provider,
    extract_token_usage,
)
from ...services.registry.service_registry import ServiceRegistry
from ...services.session.session_manager import SessionManager
from ...services.task.task_manager import TaskManager
from ..auth.rbac import RBAC
from ..utils import estimate_tokens
from .admission import CapacityAdmissionController, _finish_cleanup
from .capacity import CapacityResolver
from .circuit_breaker import CircuitBreaker
from .rate_limiter import RateLimiter
from .validator import RequestValidator

logger = logging.getLogger(__name__)


_TOKEN_FIELDS = ("input_tokens", "output_tokens", "total_tokens")
_TOKEN_SOURCES = {"upstream", "estimated", "derived", "unknown"}


def _observe_token_provenance(usage: dict[str, Any]) -> dict[str, str]:
    """Record the adapter observation boundary, not provider measurement/billing."""
    labels = usage.get("token_provenance", {})
    observed = {}
    for key in _TOKEN_FIELDS:
        if key not in usage:
            continue
        label = labels.get(key, "upstream") if isinstance(labels, dict) else "unknown"
        observed[key] = (
            label if isinstance(label, str) and label in _TOKEN_SOURCES
            and type(usage[key]) is int and usage[key] >= 0 else "unknown"
        )
    return observed


def _annotate_token_provenance(
    stats: dict[str, Any], observed: dict[str, str], *,
    estimated: tuple[str, ...] = (), derived_total: bool = False,
) -> None:
    """Add labels without normalizing, filling or charging any token quantity."""
    if "sdk_usage" in stats:
        return
    derived_total = derived_total and all(
        type(stats.get(key)) is int and stats[key] >= 0 for key in _TOKEN_FIELDS
    ) and stats["total_tokens"] == stats["input_tokens"] + stats["output_tokens"]
    stats["token_provenance"] = {
        key: (
            "unknown" if type(stats[key]) is not int or stats[key] < 0
            else "estimated" if key in estimated
            else "derived" if key == "total_tokens" and derived_total
            else observed.get(key, "unknown")
        )
        for key in _TOKEN_FIELDS if key in stats
    }


class GatewayDispatcher:
    def __init__(
        self,
        registry: ServiceRegistry,
        validator: RequestValidator,
        rate_limiter: RateLimiter,
        rbac: RBAC,
        task_manager: TaskManager,
        session_manager: SessionManager,
        admission_controller: CapacityAdmissionController | None = None,
        capacity_resolver: CapacityResolver | None = None,
    ):
        self.registry = registry
        self.validator = validator
        self.rate_limiter = rate_limiter
        self.rbac = rbac
        self.task_manager = task_manager
        self.session_manager = session_manager
        self._circuit_breakers: dict[str, CircuitBreaker] = {}
        self.admission_controller = admission_controller or CapacityAdmissionController()
        self.capacity_resolver = capacity_resolver or CapacityResolver()

    def _inputs_to_text(self, request: UnifiedRequest) -> str:
        parts: list[str] = []
        for item in request.inputs or []:
            try:
                if getattr(item, "type", None) == ContentType.TEXT:
                    if item.data is not None:
                        parts.append(str(item.data))
                else:
                    if item.data is not None:
                        parts.append(str(item.data))
            except Exception:
                continue
        return "\n".join(p for p in parts if p is not None).strip()

    def _response_to_text(self, resp: UnifiedResponse) -> str:
        for out in resp.outputs or []:
            try:
                if out.data is None:
                    continue
                return str(out.data)
            except Exception:
                continue
        return ""

    def _resolve_model_name(
        self,
        request: UnifiedRequest,
        service: ServiceDefinition,
        payload: Any,
    ) -> str:
        params = request.parameters or {}
        service_meta = service.metadata or {}
        connector_cfg = service.connector_config or {}
        return (
            extract_model(payload)
            or str(params.get("model") or "")
            or str(service_meta.get("model") or "")
            or str(connector_cfg.get("model") or "")
            or "unknown"
        )

    def _resolve_provider_name(
        self,
        service: ServiceDefinition,
        payload: Any,
    ) -> str:
        service_meta = service.metadata or {}
        connector_cfg = service.connector_config or {}
        return (
            extract_provider(payload)
            or str(service_meta.get("provider") or "")
            or str(connector_cfg.get("provider") or "")
            or ""
        )

    def _resolve_assistant_id(
        self,
        request: UnifiedRequest,
        service: ServiceDefinition,
        payload: Any,
    ) -> str:
        params = request.parameters or {}
        service_meta = service.metadata or {}
        connector_cfg = service.connector_config or {}
        return (
            extract_assistant_id(payload)
            or str(params.get("assistant_id") or "")
            or str(connector_cfg.get("assistant_id") or "")
            or str(service_meta.get("assistant_id") or "")
            or ""
        )

    def _resolve_usage_tokens(
        self,
        payload: Any,
        input_text: str,
        output_text: str,
    ) -> dict[str, int | str]:
        extracted = extract_token_usage(payload) or {}

        input_raw = extracted.get("input_tokens") if "input_tokens" in extracted else None
        output_raw = extracted.get("output_tokens") if "output_tokens" in extracted else None
        total_raw = extracted.get("total_tokens") if "total_tokens" in extracted else None
        token_source = "upstream" if extracted else "estimated"

        input_tokens = int(input_raw) if input_raw is not None else estimate_tokens(input_text)
        output_tokens = int(output_raw) if output_raw is not None else estimate_tokens(output_text)
        total_tokens = int(total_raw) if total_raw is not None else (input_tokens + output_tokens)
        if total_tokens < input_tokens + output_tokens:
            total_tokens = input_tokens + output_tokens
        return {
            "input_tokens": max(input_tokens, 0),
            "output_tokens": max(output_tokens, 0),
            "total_tokens": max(total_tokens, 0),
            "token_source": token_source,
        }

    async def _record_usage_event(
        self,
        request: UnifiedRequest,
        service: ServiceDefinition,
        payload: Any,
        duration_ms: int,
        request_type: str,
        status: str,
        output_text: str,
    ) -> None:
        try:
            from ...services.metrics import get_usage_recorder

            tokens = self._resolve_usage_tokens(
                payload=payload,
                input_text=self._inputs_to_text(request),
                output_text=output_text,
            )
            observation = (payload.get("metadata") or {}).get("local_model_usage") if isinstance(
                payload, dict
            ) and isinstance(payload.get("metadata"), dict) else None
            recorder = get_usage_recorder()
            await recorder.record_usage(
                tenant_id=request.tenant_id or "default",
                user_id=request.user_id or "anonymous",
                model=self._resolve_model_name(request, service, payload),
                input_tokens=tokens["input_tokens"],
                output_tokens=tokens["output_tokens"],
                request_id=request.request_id or "",
                service_id=service.service_id,
                assistant_id=self._resolve_assistant_id(request, service, payload),
                provider=self._resolve_provider_name(service, payload),
                latency_ms=max(duration_ms, 0),
                status=status,
                request_type=request_type,
                metadata={
                    "source": "dispatcher",
                    "token_source": tokens["token_source"],
                    **({"local_model_usage": observation} if isinstance(observation, dict) else {}),
                },
            )
        except Exception as e:
            logger.debug(f"Failed to record dispatcher usage: {e}")

    async def _get_service(self, service_id: str) -> ServiceDefinition:
        service = await self.registry.get(service_id)
        if not service:
            raise ServiceNotFoundError(service_id)
        return service

    def _get_circuit_breaker(self, service: ServiceDefinition) -> CircuitBreaker:
        if service.service_id not in self._circuit_breakers:
            self._circuit_breakers[service.service_id] = CircuitBreaker(
                service_id=service.service_id,
                failure_threshold=service.failure_threshold,
                timeout=service.recovery_timeout,
            )
        return self._circuit_breakers[service.service_id]

    async def _admit(self, request: UnifiedRequest, service: ServiceDefinition, request_class: str):
        budgets = await self.capacity_resolver.resolve(
            tenant_id=request.tenant_id or "default", service_id=service.service_id,
            request_class=request_class, upstream_group=None, provider_id=None,
            service_config=service,
        )
        return await self.admission_controller.acquire(
            budgets=budgets, tenant_id=request.tenant_id or "default",
            user_id=request.user_id or "anonymous", service_id=service.service_id,
            request_class=request_class, request_id=request.request_id or "",
        )

    async def _record_rate_limit_event(self, request: UnifiedRequest, service_id: str) -> None:
        try:
            from ...services.metrics import get_security_event_recorder

            recorder = get_security_event_recorder()
            await recorder.record_event(
                tenant_id=request.tenant_id or "public",
                user_id=request.user_id or None,
                service_id=service_id,
                event_type="rate_limited",
            )
        except Exception:
            pass

    async def invoke(
        self,
        request: UnifiedRequest,
        roles: list[str],
        client_ip: str | None = None,
    ) -> UnifiedResponse:
        import time

        t0 = time.perf_counter()

        service = await self._get_service(request.service_id)
        await self.validator.validate(request, service, roles)
        try:
            await self.rate_limiter.enforce(request, service, client_ip)
        except RateLimitExceededError:
            await self._record_rate_limit_event(request, service.service_id)
            raise

        # Session persistence (for chat history / multi-turn context)
        session_id: str | None = None
        if service.session_enabled:
            session = await self.session_manager.get_or_create(
                request.session_id,
                user_id=request.user_id or "local",
                tenant_id=request.tenant_id or "local",
                service_id=service.service_id,
            )
            session_id = session.session_id
            request.session_id = session_id
            user_text = self._inputs_to_text(request)
            if user_text:
                await self.session_manager.add_message(session_id, "user", user_text)

        adapter = self.registry.get_adapter(service)
        circuit = self._get_circuit_breaker(service)
        connector = service.connector_config or {}
        mapping = connector.get("request_mapping") or {}
        method = str(mapping.get("method") or connector.get("method") or "POST").upper()
        policy = RetryPolicy(max_attempts=max(1, min(int(service.max_retries), 3)))

        adapter_started = False

        async def with_retries() -> UnifiedResponse:
            nonlocal adapter_started
            for attempt in range(policy.max_attempts):
                try:
                    lease = await self._admit(request, service, "sync")
                    async with lease:
                        adapter_started = True
                        return await asyncio.wait_for(adapter.invoke(request), timeout=service.timeout)
                except Exception as exc:
                    if outcome_is_unknown(exc, method=method) or (
                        isinstance(exc, TimeoutError) and method not in {"GET", "HEAD", "OPTIONS"}
                    ):
                        raise UpstreamOutcomeUnknown("UPSTREAM_OUTCOME_UNKNOWN") from exc
                    if attempt + 1 >= policy.max_attempts or not policy.can_retry_exception(
                        exc, method=method, body_replayable=True,
                        # No adapter currently declares a verified upstream idempotency contract.
                        idempotency_key=False,
                    ):
                        raise
                    await asyncio.sleep(policy.delay_seconds(attempt + 1))
            raise RuntimeError("invoke_failed")

        try:
            if service.circuit_breaker_enabled:
                resp = await circuit.call(with_retries)
            else:
                resp = await with_retries()
        except BaseException as exc:
            status = "unknown" if isinstance(exc, UpstreamOutcomeUnknown) else "error"
            if adapter_started and session_id and service.session_enabled:
                # Persist only the local observation, never exception/provider text.
                interrupted = isinstance(
                    exc, (UpstreamOutcomeUnknown, asyncio.CancelledError, GeneratorExit)
                )
                async def persist_outcome() -> None:
                    try:
                        await self.session_manager.add_message(
                            session_id, "assistant", "", metadata={
                                "gateway_stream_status": "interrupted" if interrupted else "failed",
                            },
                        )
                    except Exception:
                        logger.error("Failed to persist invoke outcome history")

                # Owned persistence finishes before the original outcome is re-raised below.
                with suppress(asyncio.CancelledError):
                    await _finish_cleanup(asyncio.create_task(persist_outcome()))
            await self._record_usage_event(
                request=request, service=service, payload={},
                duration_ms=int((time.perf_counter() - t0) * 1000),
                request_type="invoke", status=status, output_text="",
            )
            raise
        t_end = time.perf_counter()

        # Ensure callers always receive the effective session_id if sessioning is enabled.
        if session_id and service.session_enabled and not resp.session_id:
            resp.session_id = session_id

        if session_id and service.session_enabled:
            assistant_text = self._response_to_text(resp)
            if assistant_text or (resp.status == "success" and not resp.error):
                # Build stats metadata for sync responses to persist in history.
                stats: dict[str, Any] = {}
                usage = resp.usage if isinstance(resp.usage, dict) else None
                if usage:
                    stats.update(usage)
                    if "input_tokens" not in stats and "prompt_tokens" in stats:
                        stats["input_tokens"] = stats.get("prompt_tokens")
                    if "output_tokens" not in stats and "completion_tokens" in stats:
                        stats["output_tokens"] = stats.get("completion_tokens")
                    if "total_tokens" not in stats:
                        in_toks = stats.get("input_tokens") or 0
                        out_toks = stats.get("output_tokens") or 0
                        stats["total_tokens"] = in_toks + out_toks

                observed_usage = dict(usage or {})
                for target, alias in (("input_tokens", "prompt_tokens"), ("output_tokens", "completion_tokens")):
                    if target not in observed_usage and alias in observed_usage:
                        observed_usage[target] = observed_usage[alias]
                observed_provenance = _observe_token_provenance(observed_usage)
                estimated_fields = tuple(key for key in _TOKEN_FIELDS[:2] if key not in stats)

                # Estimate tokens if missing
                if "input_tokens" not in stats or "output_tokens" not in stats:
                    user_text = self._inputs_to_text(request)
                    stats.setdefault("input_tokens", estimate_tokens(user_text))
                    stats.setdefault("output_tokens", estimate_tokens(assistant_text))
                    stats.setdefault(
                        "total_tokens", stats.get("input_tokens", 0) + stats.get("output_tokens", 0)
                    )

                _annotate_token_provenance(
                    stats, observed_provenance, estimated=estimated_fields,
                    derived_total="total_tokens" not in (usage or {}),
                )
                duration_ms = int((t_end - t0) * 1000)
                stats.setdefault("duration_ms", duration_ms)
                stats.setdefault("first_token_ms", duration_ms)

                metadata = {"stats": stats} if stats else None
                if resp.status == "success" and not resp.error:
                    metadata = {**(metadata or {}), "gateway_stream_status": "completed"}
                observation = resp.metadata.get("local_model_usage") if isinstance(resp.metadata, dict) else None
                if isinstance(observation, dict):
                    metadata = {**(metadata or {}), "local_model_usage": observation}
                await self.session_manager.add_message(
                    session_id, "assistant", assistant_text, metadata=metadata
                )

        await self._record_usage_event(
            request=request,
            service=service,
            payload={
                "usage": resp.usage if isinstance(resp.usage, dict) else {},
                "metadata": resp.metadata if isinstance(resp.metadata, dict) else {},
            },
            duration_ms=int((t_end - t0) * 1000),
            request_type="invoke",
            status=str(resp.status or "success"),
            output_text=self._response_to_text(resp),
        )
        return resp

    async def stream(
        self,
        request: UnifiedRequest,
        roles: list[str],
        client_ip: str | None = None,
    ) -> AsyncIterator[StreamChunk]:
        import time

        t0 = time.perf_counter()

        # Step 1: Get service (required for subsequent steps)
        service = await self._get_service(request.service_id)
        t_service = time.perf_counter()

        # Step 2a: Run validate and rate_limit in parallel
        # 先验证再创建 session，避免验证失败时产生孤儿会话
        async def do_validate():
            await self.validator.validate(request, service, roles)

        async def do_rate_limit():
            try:
                await self.rate_limiter.enforce(request, service, client_ip)
            except RateLimitExceededError:
                await self._record_rate_limit_event(request, service.service_id)
                raise

        # Execute validation and rate limiting in parallel
        await asyncio.gather(
            do_validate(),
            do_rate_limit(),
            return_exceptions=False,  # Raise first exception
        )

        # Step 2b: Create session only after validation passes
        session = None
        if service.session_enabled:
            session = await self.session_manager.get_or_create(
                request.session_id,
                user_id=request.user_id or "local",
                tenant_id=request.tenant_id or "local",
                service_id=service.service_id,
            )
        t_parallel = time.perf_counter()
        session_id: str | None = None
        if session:
            session_id = session.session_id
            request.session_id = session_id

        # Circuit breaker check (fast, no need to parallelize)
        circuit = self._get_circuit_breaker(service)
        if service.circuit_breaker_enabled:
            await circuit.allow()

        # User message persistence - wait for completion to ensure message is saved
        acc_text: str = ""
        if session_id and service.session_enabled:
            user_text = self._inputs_to_text(request)
            if user_text:
                try:
                    await self.session_manager.add_message(session_id, "user", user_text)
                    logger.debug(f"User message saved to session {session_id}")
                except Exception as e:
                    logger.error(f"Failed to persist user message to session {session_id}: {e}")

        t_ready = time.perf_counter()
        logger.debug(
            f"[TIMING] dispatcher.stream preflight: "
            f"service={((t_service - t0) * 1000):.1f}ms, "
            f"parallel(validate+rate_limit+session)={((t_parallel - t_service) * 1000):.1f}ms, "
            f"total={((t_ready - t0) * 1000):.1f}ms"
        )

        adapter = self.registry.get_adapter(service)
        first_chunk = True
        t_first_chunk: float | None = None

        # Collect tool calls and stats for session metadata
        tool_calls_map: dict[str, dict] = {}  # tool_call_id -> {name, arguments, result}
        usage_stats: dict | None = None
        usage_provenance: dict[str, str] = {}
        stream_status = "success"
        stream_error: BaseException | None = None
        record_stats: dict[str, Any] = {}

        lease = await self._admit(request, service, "stream")
        lease.bind_owner()
        producer = adapter.stream(request)
        try:
            async for chunk in producer:
                event_type = None
                if first_chunk:
                    t_first_chunk = time.perf_counter()
                    logger.info(
                        f"[TIMING] dispatcher first chunk: {((t_first_chunk - t0) * 1000):.1f}ms from stream start"
                    )
                    first_chunk = False
                try:
                    event_type = getattr(chunk, "event_type", None)

                    if event_type == StreamEventType.TEXT_DELTA:
                        delta = getattr(getattr(chunk, "content", None), "data", None)
                        if isinstance(delta, str) and delta:
                            acc_text += delta

                    # Collect tool call information
                    elif event_type in (
                        StreamEventType.TOOL_CALL_START,
                        StreamEventType.TOOL_CALL_DELTA,
                        StreamEventType.TOOL_CALL_END,
                    ):
                        tc = getattr(chunk, "tool_call", None)
                        if tc:
                            tc_id = getattr(tc, "tool_call_id", None)
                            if tc_id:
                                if tc_id not in tool_calls_map:
                                    tool_calls_map[tc_id] = {
                                        "tool_call_id": tc_id,
                                        "name": getattr(tc, "name", ""),
                                        "arguments": "",
                                        "result": None,
                                        "status": tc.status,
                                    }
                                tool_calls_map[tc_id]["status"] = tc.status
                                # Update name if provided
                                if getattr(tc, "name", None):
                                    tool_calls_map[tc_id]["name"] = tc.name
                                # Update arguments - keep the longest/most complete version
                                tc_args = getattr(tc, "arguments", "")
                                if tc_args and len(tc_args) > len(
                                    tool_calls_map[tc_id].get("arguments", "")
                                ):
                                    tool_calls_map[tc_id]["arguments"] = tc_args

                    # Collect tool result
                    elif event_type == StreamEventType.TOOL_RESULT:
                        tc = getattr(chunk, "tool_call", None)
                        if tc:
                            tc_id = getattr(tc, "tool_call_id", None)
                            if tc_id:
                                collected = tool_calls_map.setdefault(tc_id, {
                                    "tool_call_id": tc_id, "name": tc.name,
                                    "arguments": tc.arguments, "result": None,
                                })
                                collected["status"] = tc.status
                                # Empty text is a valid tool result, distinct from no result.
                                result_data = getattr(getattr(chunk, "content", None), "data", None)
                                if result_data is not None:
                                    collected["result"] = str(result_data)

                    # Collect usage stats from any chunk that has it
                    chunk_meta = getattr(chunk, "metadata", None)
                    if chunk_meta and isinstance(chunk_meta, dict):
                        chunk_usage = chunk_meta.get("usage")
                        if chunk_usage and isinstance(chunk_usage, dict):
                            # Merge usage stats (later values override earlier ones)
                            if usage_stats is None:
                                usage_stats = {}
                            usage_stats.update(chunk_usage)
                            # Only fields in this update replace earlier provenance.
                            usage_provenance.update(_observe_token_provenance(chunk_usage))

                except (AttributeError, TypeError, KeyError) as e:
                    # 只捕获已知的元数据处理错误，避免隐藏严重问题
                    logger.debug(f"Error processing chunk metadata in stream: {e}", exc_info=True)
                # Ensure the gateway "stream_end" event is the only final marker
                # so clients don't stop before stats/tool_calls are emitted.
                if event_type == StreamEventType.FINAL and getattr(chunk, "is_final", False):
                    chunk = StreamChunk(
                        request_id=chunk.request_id,
                        chunk_index=chunk.chunk_index,
                        content=chunk.content,
                        is_final=False,
                        event_type=StreamEventType.TEXT_DELTA,  # 修改为 TEXT_DELTA，避免客户端误认为流结束
                        tool_call=chunk.tool_call,
                        metadata=chunk.metadata,
                    )
                yield chunk

            # After stream completes, send final stats chunk to frontend
            from ...models.request import ContentItem
            from ...models.response import StreamChunk as DomainStreamChunk

            t_end = time.perf_counter()

            # Build stats with usage and timing
            stats: dict = {}
            if usage_stats:
                stats.update(usage_stats)

            estimated_fields = tuple(key for key in _TOKEN_FIELDS[:2] if key not in stats)
            original_total = "total_tokens" in stats
            # Estimate tokens if not provided by upstream
            if "input_tokens" not in stats or "output_tokens" not in stats:
                user_text = self._inputs_to_text(request)

                if "input_tokens" not in stats:
                    stats["input_tokens"] = estimate_tokens(user_text)
                if "output_tokens" not in stats:
                    stats["output_tokens"] = estimate_tokens(acc_text)
                if "total_tokens" not in stats:
                    stats["total_tokens"] = stats.get("input_tokens", 0) + stats.get(
                        "output_tokens", 0
                    )

            # Add timing stats
            stats["duration_ms"] = int((t_end - t0) * 1000)
            if t_first_chunk is not None:
                stats["first_token_ms"] = int((t_first_chunk - t0) * 1000)

            # Send stream_end event with stats
            logger.info(f"[STREAM END] Sending stats to frontend: {stats}")
            # Keep the legacy extractor payload separate from presentation labels.
            # Nested incoming metadata can affect accounting/identity extraction.
            record_stats = dict(stats)
            _annotate_token_provenance(
                stats, usage_provenance, estimated=estimated_fields,
                derived_total=not original_total and "total_tokens" in stats,
            )
            yield DomainStreamChunk(
                request_id=request.request_id,
                chunk_index=9999,
                content=ContentItem(type="text", data=""),
                is_final=True,
                event_type=StreamEventType.STREAM_END,
                metadata={"usage": stats} if stats else None,
            )
        except BaseException as exc:
            stream_error = exc
            stream_status = (
                "unknown" if outcome_is_unknown(exc, method="POST")
                or isinstance(exc, (UpstreamOutcomeUnknown, asyncio.CancelledError, GeneratorExit)) else "error"
            )
            raise
        finally:
            async def finalize_stream() -> BaseException | None:
                cleanup_error: BaseException | None = None
                try:
                    close = getattr(producer, "aclose", None)
                    if close is not None:
                        await close()
                except BaseException as exc:
                    cleanup_error = exc
                    logger.error("Stream producer close failed")
                try:
                    await lease.release()
                except BaseException as exc:
                    if cleanup_error is None:
                        cleanup_error = exc
                    logger.error("Stream capacity release failed")
                logger.info(
                    f"[STREAM FINALLY] session_id={session_id}, session_enabled={service.session_enabled if service else 'N/A'}, acc_text_len={len(acc_text)}"
                )
                if not record_stats:
                    # Error/cancel path may skip stream_end，补齐最终统计。
                    if usage_stats:
                        record_stats.update(usage_stats)
                    if stream_status == "error" and not usage_stats and not acc_text.strip():
                        # Avoid over-counting failed requests when upstream never returned usage.
                        record_stats.setdefault("input_tokens", 0)
                        record_stats.setdefault("output_tokens", 0)
                        record_stats.setdefault("total_tokens", 0)
                    else:
                        if "input_tokens" not in record_stats:
                            record_stats["input_tokens"] = estimate_tokens(
                                self._inputs_to_text(request)
                            )
                        if "output_tokens" not in record_stats:
                            record_stats["output_tokens"] = estimate_tokens(acc_text)
                        if "total_tokens" not in record_stats:
                            record_stats["total_tokens"] = record_stats.get(
                                "input_tokens", 0
                            ) + record_stats.get("output_tokens", 0)
                    t_final = time.perf_counter()
                    record_stats["duration_ms"] = int((t_final - t0) * 1000)
                    if t_first_chunk is not None:
                        record_stats["first_token_ms"] = int((t_first_chunk - t0) * 1000)

                if session_id and service.session_enabled:
                    # Build metadata with tool calls and stats for session storage
                    # Local Gateway observation, not proof the upstream stopped.
                    metadata: dict = {"gateway_stream_status": {
                        "success": "completed", "error": "failed",
                    }.get(stream_status, "interrupted")}

                    if tool_calls_map:
                        metadata["tool_calls"] = list(tool_calls_map.values())
                        logger.debug(
                            f"Collected {len(tool_calls_map)} tool calls for session {session_id}"
                        )

                    # Reuse stats from stream_end event if available
                    # Otherwise recalculate (fallback for error cases)
                    final_stats: dict = {}
                    if usage_stats:
                        final_stats.update(usage_stats)
                        logger.debug(f"Usage stats: {usage_stats}")

                    estimated_fields = tuple(key for key in _TOKEN_FIELDS[:2] if key not in final_stats)
                    original_total = "total_tokens" in final_stats
                    # Estimate tokens if not provided by upstream
                    if "input_tokens" not in final_stats or "output_tokens" not in final_stats:
                        user_text = self._inputs_to_text(request)

                        if "input_tokens" not in final_stats:
                            final_stats["input_tokens"] = estimate_tokens(user_text)
                        if "output_tokens" not in final_stats:
                            final_stats["output_tokens"] = estimate_tokens(acc_text)
                        if "total_tokens" not in final_stats:
                            final_stats["total_tokens"] = final_stats.get(
                                "input_tokens", 0
                            ) + final_stats.get("output_tokens", 0)
                        logger.debug(
                            f"Estimated tokens: input={final_stats.get('input_tokens')}, output={final_stats.get('output_tokens')}"
                        )

                    _annotate_token_provenance(
                        final_stats, usage_provenance, estimated=estimated_fields,
                        derived_total=not original_total and "total_tokens" in final_stats,
                    )
                    # Add timing stats (recalculate for session storage)
                    t_final = time.perf_counter()
                    final_stats["duration_ms"] = int((t_final - t0) * 1000)
                    if t_first_chunk is not None:
                        final_stats["first_token_ms"] = int((t_first_chunk - t0) * 1000)

                    metadata["stats"] = final_stats
                    logger.info(
                        f"Saving assistant message to session {session_id} with metadata: tool_calls={len(tool_calls_map)}, stats={final_stats}"
                    )

                    try:
                        await self.session_manager.add_message(
                            session_id, "assistant", acc_text, metadata=metadata
                        )
                    except asyncio.CancelledError as exc:
                        if cleanup_error is None:
                            cleanup_error = exc
                        logger.error("Stream history storage cancelled")
                    except Exception:
                        logger.error("Failed to persist stream history")

                await self._record_usage_event(
                    request=request,
                    service=service,
                    payload={"usage": record_stats},
                    duration_ms=int(record_stats.get("duration_ms") or 0),
                    request_type="stream",
                    status=stream_status,
                    output_text=acc_text,
                )
                return cleanup_error

            # Keep ownership through close, lease release, history and usage. Repeated
            # caller cancellation must not detach writes or replace the primary outcome.
            cleanup_task = asyncio.create_task(finalize_stream())
            cleanup_cancel: asyncio.CancelledError | None = None
            while not cleanup_task.done():
                try:
                    await asyncio.shield(cleanup_task)
                except asyncio.CancelledError as exc:
                    if cleanup_cancel is None:
                        cleanup_cancel = exc
                except BaseException:
                    break  # Retrieve the owned task's exception below.
            try:
                cleanup_error = cleanup_task.result()
            except BaseException as exc:
                cleanup_error = exc
                logger.error("Stream finalization failed")
            if stream_error is None:
                if cleanup_cancel is not None:
                    raise cleanup_cancel
                if cleanup_error is not None:
                    raise cleanup_error

    async def submit(
        self,
        request: UnifiedRequest,
        roles: list[str],
        client_ip: str | None = None,
    ) -> str:
        service = await self._get_service(request.service_id)
        await self.validator.validate(request, service, roles)
        await self.rate_limiter.enforce(request, service, client_ip)

        if service.session_enabled:
            session = await self.session_manager.get_or_create(
                request.session_id,
                user_id=request.user_id or "local",
                tenant_id=request.tenant_id or "local",
                service_id=service.service_id,
            )
            request.session_id = session.session_id
            user_text = self._inputs_to_text(request)
            if user_text:
                await self.session_manager.add_message(session.session_id, "user", user_text)

        task = await self.task_manager.create_task(request, roles, client_ip)
        return task.task_id

    async def get_task_result(self, task_id: str) -> UnifiedResponse:
        return await self.task_manager.get_result(task_id)
