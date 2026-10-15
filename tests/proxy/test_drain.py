"""Graceful-drain unit tests for ``ai_gateway_core.proxy.drain``.

Covers:
- Inflight counter increments / decrements correctly across concurrent requests
- Begin drain → in-flight requests complete → wait_drained returns True
- Begin drain → new request → 503 with Retry-After header
- wait_drained with hung request → returns False after timeout
- ``/health``-family paths bypass drain accounting (configurable)
- Signal handler integration is a no-op on Windows
"""
from __future__ import annotations

import asyncio
import signal
import sys

import pytest
from ai_gateway_core.proxy.drain import (
    DRAIN,
    DrainMiddleware,
    DrainState,
    install_signal_handlers,
)
from fastapi import FastAPI
from httpx import ASGITransport, AsyncClient
from starlette.requests import Request
from starlette.responses import PlainTextResponse

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _make_app(drain: DrainState) -> FastAPI:
    """Build a tiny FastAPI app wired to ``drain`` for middleware tests."""
    app = FastAPI()

    @app.get("/health")
    async def health() -> dict[str, str]:
        return {"status": "ok"}

    @app.get("/health/ready")
    async def ready() -> dict[str, str]:
        return {"status": "ready"}

    @app.get("/work")
    async def work() -> dict[str, str]:
        return {"status": "done"}

    @app.get("/slow")
    async def slow(request: Request) -> PlainTextResponse:
        # Wait on an event the test sets so we can hold a request "in flight".
        gate: asyncio.Event = request.app.state.gate
        await gate.wait()
        return PlainTextResponse("slow-done")

    app.state.gate = asyncio.Event()
    app.add_middleware(DrainMiddleware, drain=drain)
    return app


# ---------------------------------------------------------------------------
# DrainState core
# ---------------------------------------------------------------------------


class TestDrainStateCounter:
    """Reference counting under ``async with``."""

    @pytest.mark.asyncio
    async def test_aenter_increments_aexit_decrements(self) -> None:
        d = DrainState()
        assert d.inflight == 0
        async with d:
            assert d.inflight == 1
            async with d:
                assert d.inflight == 2
            assert d.inflight == 1
        assert d.inflight == 0

    @pytest.mark.asyncio
    async def test_concurrent_inflight_counts(self) -> None:
        d = DrainState()
        gate = asyncio.Event()
        peak = {"value": 0}

        async def hold() -> None:
            async with d:
                peak["value"] = max(peak["value"], d.inflight)
                await gate.wait()

        tasks = [asyncio.create_task(hold()) for _ in range(8)]
        # Give all coroutines a chance to enter the context.
        for _ in range(20):
            if d.inflight == 8:
                break
            await asyncio.sleep(0.01)
        assert d.inflight == 8
        gate.set()
        await asyncio.gather(*tasks)
        assert d.inflight == 0
        assert peak["value"] == 8


class TestDrainStateBeginDrain:
    """Drain flag flip + 503 short-circuit."""

    @pytest.mark.asyncio
    async def test_begin_drain_then_aenter_raises_503(self) -> None:
        d = DrainState()
        d.begin_drain()
        from fastapi import HTTPException

        with pytest.raises(HTTPException) as excinfo:
            async with d:
                pytest.fail("should not reach body — drain has begun")
        assert excinfo.value.status_code == 503
        assert excinfo.value.headers == {"Retry-After": "5"}

    @pytest.mark.asyncio
    async def test_begin_drain_is_idempotent(self) -> None:
        d = DrainState()
        d.begin_drain()
        d.begin_drain()
        d.begin_drain()
        assert d.draining is True


class TestWaitDrained:
    """Zero-event semantics."""

    @pytest.mark.asyncio
    async def test_wait_drained_returns_true_when_inflight_zero(self) -> None:
        d = DrainState()
        # Already at zero (init state).
        assert await d.wait_drained(timeout=0.5) is True

    @pytest.mark.asyncio
    async def test_wait_drained_returns_true_after_inflight_clears(self) -> None:
        d = DrainState()
        gate = asyncio.Event()

        async def hold() -> None:
            async with d:
                await gate.wait()

        task = asyncio.create_task(hold())
        # Wait for entry.
        for _ in range(20):
            if d.inflight == 1:
                break
            await asyncio.sleep(0.01)
        assert d.inflight == 1
        d.begin_drain()
        # wait_drained should not return until the request finishes.
        wait_task = asyncio.create_task(d.wait_drained(timeout=2.0))
        await asyncio.sleep(0.05)
        assert not wait_task.done()
        gate.set()
        await task
        assert await wait_task is True

    @pytest.mark.asyncio
    async def test_wait_drained_returns_false_on_timeout(self) -> None:
        d = DrainState()
        gate = asyncio.Event()

        async def hold() -> None:
            async with d:
                await gate.wait()

        task = asyncio.create_task(hold())
        for _ in range(20):
            if d.inflight == 1:
                break
            await asyncio.sleep(0.01)
        d.begin_drain()
        # Hung request — short timeout returns False.
        assert await d.wait_drained(timeout=0.1) is False
        # Cleanup.
        gate.set()
        await task


# ---------------------------------------------------------------------------
# DrainMiddleware integration
# ---------------------------------------------------------------------------


class TestDrainMiddleware:
    """End-to-end via httpx ASGITransport."""

    @pytest.mark.asyncio
    async def test_normal_request_passes_through(self) -> None:
        d = DrainState()
        app = _make_app(d)
        transport = ASGITransport(app=app)
        async with AsyncClient(transport=transport, base_url="http://t") as client:
            r = await client.get("/work")
        assert r.status_code == 200
        assert r.json() == {"status": "done"}
        assert d.inflight == 0

    @pytest.mark.asyncio
    async def test_request_during_drain_returns_503_with_retry_after(self) -> None:
        d = DrainState()
        app = _make_app(d)
        d.begin_drain()
        transport = ASGITransport(app=app)
        async with AsyncClient(transport=transport, base_url="http://t") as client:
            r = await client.get("/work")
        assert r.status_code == 503
        assert r.headers.get("retry-after") == "5"
        body = r.json()
        assert body.get("detail") == "shutting down"

    @pytest.mark.asyncio
    async def test_health_paths_excluded_from_drain(self) -> None:
        d = DrainState()
        app = _make_app(d)
        d.begin_drain()
        transport = ASGITransport(app=app)
        async with AsyncClient(transport=transport, base_url="http://t") as client:
            # ``/health`` and ``/health/ready`` should still answer 200 even
            # though drain has begun.
            r1 = await client.get("/health")
            r2 = await client.get("/health/ready")
        assert r1.status_code == 200
        assert r2.status_code == 200
        # Probes must not be counted.
        assert d.inflight == 0

    @pytest.mark.asyncio
    async def test_custom_exclude_paths(self) -> None:
        """Constructor override changes which paths bypass DRAIN."""
        d = DrainState()
        app = FastAPI()

        @app.get("/probe")
        async def probe() -> dict[str, str]:
            return {"ok": "1"}

        @app.get("/work")
        async def work() -> dict[str, str]:
            return {"ok": "1"}

        app.add_middleware(DrainMiddleware, drain=d, exclude_paths=["/probe"])
        d.begin_drain()
        transport = ASGITransport(app=app)
        async with AsyncClient(transport=transport, base_url="http://t") as client:
            # Custom excluded path → 200 even during drain.
            r_probe = await client.get("/probe")
            # Standard ``/health`` path is no longer excluded → 503.
            r_work = await client.get("/work")
        assert r_probe.status_code == 200
        assert r_work.status_code == 503

    @pytest.mark.asyncio
    async def test_inflight_request_completes_after_begin_drain(self) -> None:
        """Begin drain mid-request → inflight finishes → wait_drained True."""
        d = DrainState()
        app = _make_app(d)
        transport = ASGITransport(app=app)
        async with AsyncClient(transport=transport, base_url="http://t") as client:
            slow_task = asyncio.create_task(client.get("/slow"))
            # Wait for the request to enter the slow handler.
            for _ in range(50):
                if d.inflight == 1:
                    break
                await asyncio.sleep(0.01)
            assert d.inflight == 1
            d.begin_drain()
            # New request → 503.
            new_resp = await client.get("/work")
            assert new_resp.status_code == 503
            # Release the slow request.
            app.state.gate.set()
            slow_resp = await slow_task
            assert slow_resp.status_code == 200
            # All in-flight cleared → wait_drained immediate.
            assert await d.wait_drained(timeout=1.0) is True


# ---------------------------------------------------------------------------
# Signal handlers
# ---------------------------------------------------------------------------


class TestSignalHandlers:
    """Signal-driven drain initiation."""

    @pytest.mark.skipif(
        sys.platform == "win32",
        reason="loop.add_signal_handler is not supported on Windows",
    )
    @pytest.mark.asyncio
    async def test_sigterm_triggers_begin_drain(self) -> None:
        d = DrainState()
        loop = asyncio.get_running_loop()
        previous = {sig: signal.getsignal(sig) for sig in (signal.SIGTERM, signal.SIGINT)}
        try:
            install_signal_handlers(loop, drain=d)
            assert d.draining is False
            # Send SIGTERM to ourselves; the installed handler runs synchronously.
            signal.raise_signal(signal.SIGTERM)
            assert d.draining is True
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)

    @pytest.mark.skipif(
        sys.platform == "win32",
        reason="POSIX signal chaining is not available on Windows",
    )
    @pytest.mark.asyncio
    async def test_sigterm_chains_existing_shutdown_handler(self) -> None:
        d = DrainState()
        loop = asyncio.get_running_loop()
        previous = {sig: signal.getsignal(sig) for sig in (signal.SIGTERM, signal.SIGINT)}
        called = {"count": 0, "signum": None}

        def existing_handler(signum, frame) -> None:
            called["count"] += 1
            called["signum"] = signum

        try:
            signal.signal(signal.SIGTERM, existing_handler)
            install_signal_handlers(loop, drain=d)

            signal.raise_signal(signal.SIGTERM)

            assert d.draining is True
            assert called == {"count": 1, "signum": signal.SIGTERM}
        finally:
            for sig, handler in previous.items():
                signal.signal(sig, handler)

    @pytest.mark.skipif(
        sys.platform != "win32", reason="Windows-only no-op check"
    )
    def test_install_signal_handlers_noop_on_windows(self) -> None:
        # Does not raise on Windows; just early-returns.
        d = DrainState()
        install_signal_handlers(asyncio.new_event_loop(), drain=d)
        assert d.draining is False


# ---------------------------------------------------------------------------
# Module-level singleton wiring
# ---------------------------------------------------------------------------


class TestModuleSingleton:
    """``DRAIN`` exists at module level and DrainMiddleware defaults to it."""

    def test_default_drain_is_module_singleton(self) -> None:
        m = DrainMiddleware(app=FastAPI())
        assert m._drain is DRAIN

    def test_constructor_override_for_tests(self) -> None:
        d = DrainState()
        m = DrainMiddleware(app=FastAPI(), drain=d)
        assert m._drain is d
        assert m._drain is not DRAIN
