from __future__ import annotations

import asyncio

import pytest

from src.adapters.langgraph_proxy import LangGraphProxy
from src.core.middleware._streaming import logging as streaming_logging_module
from src.core.middleware.streaming import StreamingLogConfig, StreamingLoggingMiddleware
from src.core.observability.metrics import get_metrics


@pytest.mark.asyncio
async def test_streaming_metrics_task_is_retained_until_recording_finishes(monkeypatch):
    started = asyncio.Event()
    release = asyncio.Event()

    class Recorder:
        async def record_request(self, **_kwargs):
            started.set()
            await release.wait()

    monkeypatch.setattr(streaming_logging_module, "get_metrics_recorder", Recorder)
    middleware = StreamingLoggingMiddleware(
        app=lambda *_args, **_kwargs: None,
        config=StreamingLogConfig(),
    )

    middleware._schedule_metrics_record(
        method="GET",
        path="/v1/models",
        status_code=200,
        duration_ms=1.0,
        user_id="user-a",
        service_id="gateway",
        error_label="metrics failed",
    )
    await asyncio.wait_for(started.wait(), timeout=1)
    assert len(middleware._background_tasks) == 1

    pending = tuple(middleware._background_tasks)
    release.set()
    await asyncio.gather(*pending)
    await asyncio.sleep(0)

    assert not middleware._background_tasks


@pytest.mark.asyncio
async def test_streaming_metrics_backlog_is_hard_bounded_and_cleans_up(monkeypatch):
    started = 0
    release = asyncio.Event()

    class Recorder:
        async def record_request(self, **_kwargs):
            nonlocal started
            started += 1
            await release.wait()

    monkeypatch.setattr(streaming_logging_module, "get_metrics_recorder", Recorder)
    middleware = StreamingLoggingMiddleware(
        app=lambda *_args, **_kwargs: None,
        config=StreamingLogConfig(),
    )
    middleware._background_task_limit = 3
    request_metrics = get_metrics().request_metrics
    dropped_before = request_metrics.telemetry_records_dropped_total.get(reason="capacity")

    for index in range(100):
        middleware._schedule_metrics_record(
            method="GET",
            path=f"/ordinary/{index}",
            status_code=200,
            duration_ms=1.0,
            user_id="user-a",
            service_id="gateway",
            error_label="metrics failed",
        )

    await asyncio.sleep(0)
    assert started == 3
    assert middleware.metrics_backlog == 3
    assert request_metrics.telemetry_background_backlog.get() == 3
    assert (
        request_metrics.telemetry_records_dropped_total.get(reason="capacity")
        - dropped_before
        == 97
    )

    pending = tuple(middleware._background_tasks)
    release.set()
    await asyncio.gather(*pending)
    await asyncio.sleep(0)

    assert middleware.metrics_backlog == 0
    assert request_metrics.telemetry_background_backlog.get() == 0


@pytest.mark.asyncio
async def test_streaming_terminal_telemetry_bypasses_saturated_normal_backlog(monkeypatch):
    release = asyncio.Event()
    recorded_statuses: list[int] = []

    class Recorder:
        async def record_request(self, **kwargs):
            recorded_statuses.append(kwargs["status_code"])
            if kwargs["path"] == "/ordinary":
                await release.wait()

    monkeypatch.setattr(streaming_logging_module, "get_metrics_recorder", Recorder)

    async def app(_scope, _receive, send):
        await send({"type": "http.response.start", "status": 200, "headers": []})
        await send(
            {
                "type": "http.response.body",
                "body": b'data: {"exit_reason":"side_effect_unknown"}\n\n',
                "more_body": False,
            }
        )

    middleware = StreamingLoggingMiddleware(app=app, config=StreamingLogConfig())
    middleware._background_task_limit = 1
    assert middleware._schedule_metrics_record(
        method="GET",
        path="/ordinary",
        status_code=200,
        duration_ms=1.0,
        user_id="user-a",
        service_id="gateway",
        error_label="metrics failed",
    )
    await asyncio.sleep(0)

    sent: list[dict] = []

    async def receive():
        return {"type": "http.request", "body": b"", "more_body": False}

    async def send(message):
        sent.append(message)

    await middleware(
        {
            "type": "http",
            "method": "POST",
            "path": "/v1/responses",
            "headers": [],
            "state": {},
        },
        receive,
        send,
    )

    assert recorded_statuses == [200, 200]
    assert middleware.metrics_backlog == 1
    assert sent[-1]["type"] == "http.response.body"

    pending = tuple(middleware._background_tasks)
    release.set()
    await asyncio.gather(*pending)


@pytest.mark.asyncio
async def test_cancelled_stream_records_priority_telemetry_before_propagating(monkeypatch):
    recorded: list[dict] = []

    class Recorder:
        async def record_request(self, **kwargs):
            recorded.append(kwargs)

    monkeypatch.setattr(streaming_logging_module, "get_metrics_recorder", Recorder)

    async def cancelled_app(_scope, _receive, _send):
        raise asyncio.CancelledError

    middleware = StreamingLoggingMiddleware(
        app=cancelled_app,
        config=StreamingLogConfig(),
    )

    async def receive():
        return {"type": "http.request", "body": b"", "more_body": False}

    async def send(_message):
        return None

    with pytest.raises(asyncio.CancelledError):
        await middleware(
            {
                "type": "http",
                "method": "POST",
                "path": "/v1/responses",
                "headers": [],
                "state": {},
            },
            receive,
            send,
        )

    assert len(recorded) == 1
    assert recorded[0]["status_code"] == 499
    assert middleware.metrics_backlog == 0


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("status_code", "slow_threshold_ms"),
    [(503, 1000.0), (200, -1.0)],
)
async def test_error_and_slow_request_telemetry_bypass_normal_capacity(
    monkeypatch,
    status_code,
    slow_threshold_ms,
):
    recorded: list[dict] = []

    class Recorder:
        async def record_request(self, **kwargs):
            recorded.append(kwargs)

    monkeypatch.setattr(streaming_logging_module, "get_metrics_recorder", Recorder)

    async def app(_scope, _receive, send):
        await send({"type": "http.response.start", "status": status_code, "headers": []})
        await send({"type": "http.response.body", "body": b"", "more_body": False})

    middleware = StreamingLoggingMiddleware(app=app, config=StreamingLogConfig())
    middleware._background_task_limit = 0
    middleware._slow_request_threshold_ms = slow_threshold_ms

    async def receive():
        return {"type": "http.request", "body": b"", "more_body": False}

    async def send(_message):
        return None

    await middleware(
        {
            "type": "http",
            "method": "GET",
            "path": "/api/v1/models",
            "headers": [],
            "state": {},
        },
        receive,
        send,
    )

    assert len(recorded) == 1
    assert recorded[0]["status_code"] == status_code
    assert middleware.metrics_backlog == 0


@pytest.mark.asyncio
async def test_critical_telemetry_has_bounded_recorder_concurrency(monkeypatch):
    release = asyncio.Event()
    active = 0
    maximum_active = 0

    class Recorder:
        async def record_request(self, **_kwargs):
            nonlocal active, maximum_active
            active += 1
            maximum_active = max(maximum_active, active)
            try:
                await release.wait()
            finally:
                active -= 1

    monkeypatch.setattr(streaming_logging_module, "get_metrics_recorder", Recorder)
    middleware = StreamingLoggingMiddleware(
        app=lambda *_args, **_kwargs: None,
        config=StreamingLogConfig(),
    )
    middleware._critical_metrics_semaphore = asyncio.Semaphore(1)

    tasks = [
        asyncio.create_task(
            middleware._record_metrics(
                method="POST",
                path="/v1/responses",
                status_code=500,
                duration_ms=2_000,
                user_id="user-a",
                service_id="gateway",
                error_label="critical metrics failed",
                critical=True,
            )
        )
        for _ in range(3)
    ]
    await asyncio.sleep(0)
    assert maximum_active == 1
    release.set()
    await asyncio.gather(*tasks)
    assert maximum_active == 1


@pytest.mark.asyncio
async def test_recorder_cancellation_does_not_reclassify_success_as_499(monkeypatch):
    recorded_statuses: list[int] = []

    class Recorder:
        async def record_request(self, **kwargs):
            recorded_statuses.append(kwargs["status_code"])
            raise asyncio.CancelledError

    monkeypatch.setattr(streaming_logging_module, "get_metrics_recorder", Recorder)

    async def app(_scope, _receive, send):
        await send({"type": "http.response.start", "status": 200, "headers": []})
        await send({"type": "http.response.body", "body": b"ok", "more_body": False})

    middleware = StreamingLoggingMiddleware(app=app, config=StreamingLogConfig())
    middleware._slow_request_threshold_ms = 0

    async def receive():
        return {"type": "http.request", "body": b"", "more_body": False}

    async def send(_message):
        return None

    await middleware(
        {"type": "http", "method": "GET", "path": "/slow", "headers": [], "state": {}},
        receive,
        send,
    )
    assert recorded_statuses == [200]


@pytest.mark.asyncio
async def test_langgraph_cache_invalidation_task_is_retained_until_delete_finishes():
    started = asyncio.Event()
    release = asyncio.Event()

    class FakeRedis:
        enabled = True

        async def delete(self, _key):
            started.set()
            await release.wait()

    proxy = LangGraphProxy(load_balancer=object(), redis_client=FakeRedis())

    proxy._invalidate_assistant_cache("assistant-1")
    await asyncio.wait_for(started.wait(), timeout=1)
    assert len(proxy._background_tasks) == 1

    pending = tuple(proxy._background_tasks)
    release.set()
    await asyncio.gather(*pending)
    await asyncio.sleep(0)

    assert not proxy._background_tasks
