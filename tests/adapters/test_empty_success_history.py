"""Empty local success uses the real adapter, dispatcher and history projection."""
from __future__ import annotations

import asyncio
from types import SimpleNamespace
from unittest.mock import AsyncMock, MagicMock, patch

import httpx
import pytest
from ai_gateway_core.comm.retry import UpstreamOutcomeUnknown
from ai_gateway_core.enums import ContentType, InvocationMode, ServiceType, TransportType
from langchain_core.messages import AIMessage, AIMessageChunk

from src.adapters.langgraph import LangGraphAdapter
from src.api.v1.sessions import get_history
from src.core.auth.user_resolver import UserContext
from src.core.gateway.dispatcher import GatewayDispatcher
from src.models.request import ContentItem, UnifiedRequest
from src.models.response import UnifiedResponse
from src.models.service import ServiceDefinition
from src.services.session.session_manager import SessionManager


def setup_case(text, ending, enabled):
    service = ServiceDefinition(
        service_id="empty-history", name="Empty history", service_type=ServiceType.CUSTOM,
        connector_type=TransportType.IN_PROCESS,
        supported_modes=[InvocationMode.SYNC, InvocationMode.STREAM],
        session_enabled=enabled, max_retries=1, circuit_breaker_enabled=False,
    )
    calls = []

    def terminate():
        if ending == "failure":
            raise RuntimeError("controlled-secret-sentinel")
        if ending == "cancel":
            raise asyncio.CancelledError()
        if ending == "transport_unknown":
            raise httpx.ReadTimeout("controlled-secret-sentinel")
        if ending == "outcome_unknown":
            raise UpstreamOutcomeUnknown("controlled-secret-sentinel")

    async def invoke(state, config):
        calls.append("invoke")
        terminate()
        return AIMessage(content=text)

    async def events(state, config, *, version):
        calls.append("stream")
        terminate()
        yield {"event": "on_chat_model_stream", "run_id": "model", "data": {
            "chunk": AIMessageChunk(content=text),
        }}

    adapter = object.__new__(LangGraphAdapter)
    adapter.remote = False
    adapter.graph = SimpleNamespace(ainvoke=invoke, astream_events=events)
    adapter.service = service
    adapter.config = {}
    adapter.assistant_id = "fixture-agent"
    registry = MagicMock()
    registry.get = AsyncMock(return_value=service)
    registry.get_adapter.return_value = adapter
    validator, limiter = MagicMock(), MagicMock()
    validator.validate, limiter.enforce = AsyncMock(), AsyncMock()
    sessions = SessionManager()
    gateway = GatewayDispatcher(registry, validator, limiter, MagicMock(), MagicMock(), sessions)
    request = UnifiedRequest(
        request_id="empty-request", service_id=service.service_id,
        inputs=[ContentItem(ContentType.TEXT, "Question")], user_id="user", tenant_id="tenant",
    )
    return gateway, request, sessions, calls, adapter


async def roundtrip(mode, text, *, ending="success", enabled=True):
    gateway, request, sessions, calls, adapter = setup_case(text, ending, enabled)
    chunks, failure, response = [], None, None
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        try:
            if mode == "invoke":
                response = await gateway.invoke(request, ["user"])
            else:
                stream = gateway.stream(request, ["user"])
                try:
                    async for chunk in stream:
                        chunks.append(chunk)
                        if ending == "close_after_terminal" and chunk.is_final:
                            break
                finally:
                    await stream.aclose()
                    await stream.aclose()
        except (RuntimeError, asyncio.CancelledError, httpx.ReadTimeout, UpstreamOutcomeUnknown) as exc:
            failure = exc
    if enabled:
        history = await get_history(
            request.session_id, 50, sessions,
            UserContext(user_id="user", tenant_id="tenant"),
        )
        assert [m["content"] for m in history if m["role"] == "user"] == ["Question"]
    else:
        assert request.session_id is None
        assert not sessions._sessions
        history = []
    assert calls == [mode if mode == "invoke" else "stream"]
    assert all(row["inflight"] == 0 for row in gateway.admission_controller.snapshot().values())
    return SimpleNamespace(
        assistants=[m for m in history if m["role"] == "assistant"],
        chunks=chunks, failure=failure, response=response,
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
@pytest.mark.parametrize("text", ["", " \t\n", "answer"], ids=["empty", "whitespace", "text"])
async def test_success_retains_exact_content_once(mode, text):
    result = await roundtrip(mode, text)
    assert result.failure is None
    assert len(result.assistants) == 1
    row = result.assistants[0]
    assert row["content"] == text
    assert row["metadata"]["gateway_stream_status"] == "completed"
    assert not row["metadata"].get("tool_calls")
    if mode == "stream":
        assert sum(chunk.is_final for chunk in result.chunks) == 1
        assert result.chunks[-1].event_type.value == "stream_end"
    else:
        assert result.response.status == "success"
        assert result.response.outputs[0].data == text


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
@pytest.mark.parametrize("ending", ["failure", "cancel", "transport_unknown", "outcome_unknown"])
async def test_first_frame_failure_and_cancel_do_not_invent_completion(mode, ending):
    result = await roundtrip(mode, "", ending=ending)
    expected = {
        "failure": RuntimeError, "cancel": asyncio.CancelledError,
        "transport_unknown": UpstreamOutcomeUnknown if mode == "invoke" else httpx.ReadTimeout,
        "outcome_unknown": UpstreamOutcomeUnknown,
    }[ending]
    assert isinstance(result.failure, expected)
    assert len(result.assistants) == 1
    row = result.assistants[0]
    assert row["content"] == ""
    assert row["metadata"]["gateway_stream_status"] == (
        "failed" if ending == "failure" else "interrupted"
    )
    assert not row["metadata"].get("tool_calls")
    assert "controlled-secret-sentinel" not in str(row)
    assert not any(chunk.is_final for chunk in result.chunks)


@pytest.mark.asyncio
@pytest.mark.parametrize("text", ["", " \t\n"], ids=["empty", "whitespace"])
async def test_close_while_suspended_at_terminal_preserves_prior_interruption_policy(text):
    result = await roundtrip("stream", text, ending="close_after_terminal")
    assert result.failure is None
    assert sum(chunk.is_final for chunk in result.chunks) == 1
    assert len(result.assistants) == 1
    assert result.assistants[0]["content"] == text
    assert result.assistants[0]["metadata"]["gateway_stream_status"] == "interrupted"


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
@pytest.mark.parametrize("text", ["", " \t\n"], ids=["empty", "whitespace"])
async def test_session_disabled_does_not_create_history(mode, text):
    result = await roundtrip(mode, text, enabled=False)
    assert result.failure is None and result.assistants == []


@pytest.mark.asyncio
@pytest.mark.parametrize("status", ["error", "failed", "unknown", "cancelled", "pending"])
async def test_non_success_blocking_response_does_not_gain_empty_completed_row(status):
    gateway, request, sessions, _, adapter = setup_case("", "success", True)
    adapter.invoke = AsyncMock(return_value=UnifiedResponse(request.request_id, status, []))
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        response = await gateway.invoke(request, ["user"])
    assert response.status == status
    history = await get_history(
        request.session_id, 50, sessions, UserContext(user_id="user", tenant_id="tenant"),
    )
    assert [row["role"] for row in history] == ["user"]


@pytest.mark.asyncio
async def test_conflicting_success_and_error_does_not_create_empty_completed_row():
    gateway, request, sessions, _, adapter = setup_case("", "success", True)
    adapter.invoke = AsyncMock(return_value=UnifiedResponse(
        request.request_id, "success", [], error={"code": "controlled_error"},
    ))
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        response = await gateway.invoke(request, ["user"])
    assert response.error == {"code": "controlled_error"}
    history = await get_history(
        request.session_id, 50, sessions, UserContext(user_id="user", tenant_id="tenant"),
    )
    assert [row["role"] for row in history] == ["user"]


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
@pytest.mark.parametrize("ending", ["failure", "cancel", "transport_unknown", "outcome_unknown"])
async def test_failed_session_disabled_does_not_create_history(mode, ending):
    result = await roundtrip(mode, "", ending=ending, enabled=False)
    assert result.failure is not None and result.assistants == []


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
async def test_actual_task_cancel_before_first_frame_retains_local_interruption(mode):
    gateway, request, sessions, calls, adapter = setup_case("", "success", True)
    entered, finished = asyncio.Event(), asyncio.Event()

    async def wait_for_cancel(state, config):
        entered.set()
        try:
            await asyncio.Event().wait()
        finally:
            finished.set()

    async def events(state, config, *, version):
        await wait_for_cancel(state, config)
        yield  # pragma: no cover

    adapter.graph = SimpleNamespace(ainvoke=wait_for_cancel, astream_events=events)

    async def consume():
        if mode == "invoke":
            await gateway.invoke(request, ["user"])
        else:
            async for _ in gateway.stream(request, ["user"]):
                pytest.fail("No first frame expected")

    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        task = asyncio.create_task(consume())
        await asyncio.wait_for(entered.wait(), timeout=2)
        task.cancel()
        with pytest.raises(asyncio.CancelledError):
            await asyncio.wait_for(task, timeout=2)
    assert finished.is_set()
    history = await get_history(
        request.session_id, 50, sessions, UserContext(user_id="user", tenant_id="tenant"),
    )
    assistants = [row for row in history if row["role"] == "assistant"]
    assert len(assistants) == 1
    assert assistants[0]["content"] == ""
    assert assistants[0]["metadata"]["gateway_stream_status"] == "interrupted"
    assert all(row["inflight"] == 0 for row in gateway.admission_controller.snapshot().values())


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
async def test_admission_failure_does_not_invent_adapter_outcome(mode):
    gateway, request, sessions, calls, _ = setup_case("", "success", True)
    failure = RuntimeError("admission rejected")
    gateway._admit = AsyncMock(side_effect=failure)
    with (
        patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()),
        pytest.raises(RuntimeError) as raised,
    ):
        if mode == "invoke":
            await gateway.invoke(request, ["user"])
        else:
            _ = [chunk async for chunk in gateway.stream(request, ["user"])]
    assert raised.value is failure and calls == []
    history = await get_history(
        request.session_id, 50, sessions, UserContext(user_id="user", tenant_id="tenant"),
    )
    assert [row["role"] for row in history] == ["user"]


@pytest.mark.asyncio
@pytest.mark.parametrize("mode", ["invoke", "stream"])
async def test_history_storage_failure_does_not_mask_original_adapter_exception(mode):
    gateway, request, sessions, _, adapter = setup_case("", "success", True)
    failure = RuntimeError("controlled-original-failure")

    async def fail(state, config):
        raise failure

    async def events(state, config, *, version):
        await fail(state, config)
        yield  # pragma: no cover

    adapter.graph = SimpleNamespace(ainvoke=fail, astream_events=events)
    add_message = sessions.add_message
    writes = []

    async def storage(session_id, role, content, metadata=None):
        if role == "assistant":
            writes.append((content, metadata))
            raise RuntimeError("controlled storage failure")
        return await add_message(session_id, role, content, metadata=metadata)

    sessions.add_message = storage
    with (
        patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()),
        pytest.raises(RuntimeError) as raised,
    ):
        if mode == "invoke":
            await gateway.invoke(request, ["user"])
        else:
            _ = [chunk async for chunk in gateway.stream(request, ["user"])]
    assert raised.value is failure
    assert len(writes) == 1 and writes[0][0] == ""
    assert writes[0][1]["gateway_stream_status"] == "failed"
    assert "controlled-original-failure" not in str(writes)


@pytest.mark.asyncio
@pytest.mark.parametrize("ending", ["failure", "cancel"])
async def test_invoke_outcome_write_is_joined_under_repeated_cancellation(ending):
    gateway, request, sessions, _, _ = setup_case("", ending, True)
    entered, release, finished = asyncio.Event(), asyncio.Event(), asyncio.Event()
    original_add = sessions.add_message

    async def storage(session_id, role, content, metadata=None):
        if role == "assistant":
            entered.set()
            await release.wait()
        result = await original_add(session_id, role, content, metadata=metadata)
        if role == "assistant":
            finished.set()
        return result

    sessions.add_message = storage
    with patch("src.services.metrics.get_usage_recorder", return_value=AsyncMock()):
        task = asyncio.create_task(gateway.invoke(request, ["user"]))
        await asyncio.wait_for(entered.wait(), timeout=2)
        task.cancel()
        await asyncio.sleep(0)
        task.cancel()
        await asyncio.sleep(0)
        assert not task.done() and not finished.is_set()
        release.set()
        expected = RuntimeError if ending == "failure" else asyncio.CancelledError
        with pytest.raises(expected):
            await asyncio.wait_for(task, timeout=2)
    assert finished.is_set()
    history = await get_history(
        request.session_id, 50, sessions, UserContext(user_id="user", tenant_id="tenant"),
    )
    assistants = [row for row in history if row["role"] == "assistant"]
    assert len(assistants) == 1 and assistants[0]["content"] == ""
    assert assistants[0]["metadata"]["gateway_stream_status"] == (
        "failed" if ending == "failure" else "interrupted"
    )


@pytest.mark.asyncio
@pytest.mark.parametrize("ending", ["failure", "cancel", "success"])
@pytest.mark.parametrize("gate", ["close", "release", "history", "usage", "none"])
@pytest.mark.parametrize("failure", ["none", "close", "storage", "release", "close_storage", "storage_cancel"])
async def test_stream_cleanup_stays_owned_and_preserves_primary(ending, gate, failure):
    gateway, request, sessions, _, adapter = setup_case("", "success", True)
    body_entered, blocked, resume, stored = (asyncio.Event() for _ in range(4))
    original = RuntimeError("original-private-sentinel")
    close_error = RuntimeError("close-private-sentinel")
    release_error = RuntimeError("release-private-sentinel")
    storage_cancel = asyncio.CancelledError("storage-cancel")
    observed = []
    order = []

    class Producer:
        def __aiter__(self):
            return self

        async def __anext__(self):
            body_entered.set()
            if ending == "cancel":
                try:
                    await asyncio.Event().wait()
                except asyncio.CancelledError as exc:
                    observed.append(exc)
                    raise
            if ending == "failure":
                observed.append(original)
                raise original
            raise StopAsyncIteration

        async def aclose(self):
            order.append("close")
            if gate == "close":
                blocked.set()
                await resume.wait()
            if failure in {"close", "close_storage"}:
                raise close_error

    adapter.stream = lambda _request: Producer()
    original_admit = gateway._admit

    async def admit(*args):
        lease = await original_admit(*args)
        original_release = lease.release

        async def release():
            order.append("release")
            if gate == "release":
                blocked.set()
                await resume.wait()
            await original_release()
            if failure == "release":
                raise release_error

        lease.release = release
        return lease

    gateway._admit = admit
    original_add = sessions.add_message

    async def storage(session_id, role, content, metadata=None):
        if role != "assistant":
            return await original_add(session_id, role, content, metadata=metadata)
        order.append("history")
        if gate == "history":
            blocked.set()
            await resume.wait()
        try:
            if failure == "storage_cancel":
                raise storage_cancel
            if failure in {"storage", "close_storage"}:
                raise RuntimeError("storage-private-sentinel")
            return await original_add(session_id, role, content, metadata=metadata)
        finally:
            stored.set()

    sessions.add_message = storage

    async def usage(**kwargs):
        order.append("usage")
        if gate == "usage":
            blocked.set()
            await resume.wait()

    gateway._record_usage_event = usage

    async def consume():
        try:
            return [chunk async for chunk in gateway.stream(request, ["user"])]
        except BaseException as exc:
            return exc

    existing_tasks = set(asyncio.all_tasks())
    task = asyncio.create_task(consume())
    try:
        await asyncio.wait_for(body_entered.wait(), timeout=2)
        if ending == "cancel":
            task.cancel("original-cancel")
        # Baseline close failure can skip history entirely; do not hang the harness.
        for _ in range(50):
            if blocked.is_set() or task.done():
                break
            await asyncio.sleep(0)
        reached_gate = blocked.is_set()
        if gate != "none":
            task.cancel("repeat-one")
            await asyncio.sleep(0)
            task.cancel("repeat-two")
            await asyncio.sleep(0)
        owned_while_blocked = not task.done()
    finally:
        resume.set()
        result = (await asyncio.wait_for(asyncio.gather(task, return_exceptions=True), 2))[0]
        # Drain a baseline shield-created storage task before making assertions.
        for _ in range(50):
            if stored.is_set() or "history" not in order:
                break
            await asyncio.sleep(0)

    assert not (set(asyncio.all_tasks()) - existing_tasks)
    if gate != "none":
        assert reached_gate and owned_while_blocked
    assert order == ["close", "release", "history", "usage"]
    assert stored.is_set()
    if observed:
        assert result is observed[0]
    elif gate != "none":
        assert isinstance(result, asyncio.CancelledError)
        assert result.args == ("repeat-one",)
    elif failure in {"close", "close_storage"}:
        assert result is close_error
    elif failure == "release":
        assert result is release_error
    elif failure == "storage_cancel":
        assert result is storage_cancel
    else:
        assert isinstance(result, list) and result[-1].is_final
    assert all(row["inflight"] == 0 for row in gateway.admission_controller.snapshot().values())
    history = await get_history(
        request.session_id, 50, sessions, UserContext(user_id="user", tenant_id="tenant"),
    )
    assistants = [row for row in history if row["role"] == "assistant"]
    if failure in {"storage", "close_storage", "storage_cancel"}:
        assert assistants == []
    else:
        assert len(assistants) == 1 and assistants[0]["content"] == ""
        assert assistants[0]["metadata"]["gateway_stream_status"] == {
            "failure": "failed", "cancel": "interrupted", "success": "completed",
        }[ending]
        assert "private-sentinel" not in str(assistants)
