"""Phase 0 hotfix — billing shutdown draining.

Covers:
- In-flight flushes spawned via ``_spawn_tracked_flush`` are tracked in
  ``_inflight_flushes`` and awaited by ``stop()``.
- No buffer records are lost on shutdown.
"""

from __future__ import annotations

import asyncio
from unittest.mock import patch

import pytest

from src.proxy.billing_interceptor import BillingInterceptor, UsageData


def _make_usage(i: int = 0) -> UsageData:
    return UsageData(
        input_tokens=1,
        output_tokens=1,
        request_id=f"req-{i}",
        service_id="svc",
        user_id="u",
        tenant_id="t",
        model="m",
    )


class TestSpawnTrackedFlush:
    @pytest.mark.asyncio
    async def test_spawn_tracked_flush_registers_and_clears_task(self):
        """Happy path: the task ends up in _inflight_flushes, then drops via callback."""
        interceptor = BillingInterceptor()

        # Block the flush until we release it so we can observe the task live
        flush_gate = asyncio.Event()

        async def slow_flush():
            await flush_gate.wait()

        with patch.object(interceptor, "_flush_buffer", side_effect=slow_flush):
            task = interceptor._spawn_tracked_flush()
            # While the task is pending, it's tracked
            assert task in interceptor._inflight_flushes
            flush_gate.set()
            await task
            # After completion, the done-callback removes it
            assert task not in interceptor._inflight_flushes


class TestStopDrainsInflightFlushes:
    @pytest.mark.asyncio
    async def test_stop_awaits_inflight_flush_tasks(self):
        """stop() must block until inflight flush tasks complete."""
        interceptor = BillingInterceptor()

        release = asyncio.Event()
        observed = {"flush_completed_before_stop_returned": False}

        async def delayed_flush():
            await release.wait()
            observed["flush_completed_before_stop_returned"] = True

        with patch.object(interceptor, "_flush_buffer", side_effect=delayed_flush):
            # Spawn a tracked flush that is still pending
            interceptor._spawn_tracked_flush()

            # Kick stop() in background; it should be blocked on the flush
            stop_task = asyncio.create_task(interceptor.stop())
            await asyncio.sleep(0.05)
            assert not stop_task.done(), "stop() must wait for inflight flush"

            # Release the flush → stop() should complete
            release.set()
            await asyncio.wait_for(stop_task, timeout=1.0)
            assert observed["flush_completed_before_stop_returned"]

    @pytest.mark.asyncio
    async def test_stop_runs_final_flush_after_draining(self):
        """After inflight drain, a final _flush_buffer is called once more."""
        interceptor = BillingInterceptor()

        calls = []

        async def counted_flush():
            calls.append("flush")

        with patch.object(interceptor, "_flush_buffer", side_effect=counted_flush):
            # Spawn 2 tracked flushes (will run immediately since no gate)
            interceptor._spawn_tracked_flush()
            interceptor._spawn_tracked_flush()

            await interceptor.stop()

        # 2 spawned + 1 final in stop() == 3
        assert len(calls) == 3


class TestNoRecordsLostOnShutdown:
    @pytest.mark.asyncio
    async def test_buffered_records_reach_callback_on_stop(self):
        """End-to-end: records appended to buffer are flushed via stop()."""
        seen = []

        async def callback(usage):
            seen.append(usage)

        interceptor = BillingInterceptor(
            callback=callback,
            buffer_size=1000,  # large enough that nothing auto-flushes
        )

        # Short-circuit DB and Redis so we isolate callback
        with patch(
            "src.services.metrics.get_usage_recorder", return_value=None
        ):
            async with interceptor._buffer_lock:
                for i in range(5):
                    interceptor._buffer.append(_make_usage(i))

            # No inflight flushes spawned; shutdown should drain via final flush
            await interceptor.stop()

        assert len(seen) == 5
        assert [u.request_id for u in seen] == [f"req-{i}" for i in range(5)]

    @pytest.mark.asyncio
    async def test_buffer_flush_preserves_billing_metadata_on_stop(self):
        seen = []

        async def callback(usage):
            seen.append(usage)

        interceptor = BillingInterceptor(callback=callback, buffer_size=1000)
        usage = _make_usage(7)
        usage.tenant_id = "tenant-a"
        usage.user_id = "user-a"
        usage.service_id = "svc-a"
        usage.model = "gemini-3-flash-preview"
        usage.provider = "google"
        usage.raw_metadata = {
            "source": "transparent_proxy_stream",
            "token_source": "upstream",
            "pricing_status": "catalog",
        }

        with patch("src.services.metrics.get_usage_recorder", return_value=None):
            async with interceptor._buffer_lock:
                interceptor._buffer.append(usage)

            await interceptor.stop()

        assert len(seen) == 1
        flushed = seen[0]
        assert flushed.tenant_id == "tenant-a"
        assert flushed.user_id == "user-a"
        assert flushed.service_id == "svc-a"
        assert flushed.model == "gemini-3-flash-preview"
        assert flushed.provider == "google"
        assert flushed.raw_metadata["token_source"] == "upstream"

    @pytest.mark.asyncio
    async def test_inflight_flush_plus_final_flush_do_not_drop_records(self):
        """Records added after an inflight flush was spawned must still land."""
        seen = []

        async def callback(usage):
            seen.append(usage)

        interceptor = BillingInterceptor(callback=callback, buffer_size=1000)

        # Gate the first flush so we can interleave a second append
        gate = asyncio.Event()
        original_flush = interceptor._flush_buffer
        call_count = {"n": 0}

        async def gated_flush():
            call_count["n"] += 1
            if call_count["n"] == 1:
                await gate.wait()
            await original_flush()

        with patch(
            "src.services.metrics.get_usage_recorder", return_value=None
        ), patch.object(interceptor, "_flush_buffer", side_effect=gated_flush):
            # Put 2 records and spawn a tracked flush (which will block)
            async with interceptor._buffer_lock:
                interceptor._buffer.append(_make_usage(0))
                interceptor._buffer.append(_make_usage(1))
            interceptor._spawn_tracked_flush()

            # Add 3 more records while first flush is blocked
            async with interceptor._buffer_lock:
                for i in range(2, 5):
                    interceptor._buffer.append(_make_usage(i))

            # Release first flush, then call stop()
            gate.set()
            await interceptor.stop()

        assert len(seen) == 5
        assert sorted(u.request_id for u in seen) == [f"req-{i}" for i in range(5)]
