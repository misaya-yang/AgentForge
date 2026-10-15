"""Offline regressions only: no socket bind/connect, process start or credential read."""
from __future__ import annotations

import asyncio
import json
import time
import unittest
from unittest.mock import AsyncMock, patch

from scripts.harness import gateway_performance_baseline as baseline

RUN_ID = "3e00e9b4-3e0b-49d1-a9a4-4b512ca48072"
FAKE_TOKEN = "synthetic.unit.test"


class FakeWriter:
    def __init__(self):
        self.closed = False
        self.output = bytearray()

    def write(self, value):
        self.output.extend(value)

    async def drain(self):
        pass

    def close(self):
        self.closed = True

    async def wait_closed(self):
        pass


def stream(value: bytes, eof=True):
    reader = asyncio.StreamReader(limit=baseline.MAX_HEADER)
    reader.feed_data(value)
    if eof:
        reader.feed_eof()
    return reader


def response(payload=b'{"status":"alive"}', status=200, extra=b""):
    return (f"HTTP/1.1 {status} Status\r\nContent-Type: application/json\r\n"
            f"Content-Length: {len(payload)}\r\n").encode() + extra + b"\r\n" + payload


class StatisticsTests(unittest.TestCase):
    def test_empirical_quantiles_and_empty_are_honest(self):
        values = baseline.distribution(list(range(1, 101)))
        self.assertEqual((values["p50"], values["p95"], values["p99"]), (50, 95, 99))
        self.assertFalse(values["tail_sample_warning"])
        self.assertIsNone(baseline.distribution([])["p99"])
        self.assertTrue(baseline.distribution([1, 2])["tail_sample_warning"])
        with self.assertRaises(baseline.BaselineError):
            baseline.distribution([float("nan")])

    def test_denominators_include_errors_cancelled_and_not_started(self):
        rows = [baseline.Observation(0, status=200, outcome="success", elapsed_ms=2),
                baseline.Observation(1, status=429, outcome="rate_limited", elapsed_ms=1),
                baseline.Observation(2, outcome="timeout", elapsed_ms=100),
                baseline.Observation(3)]
        result = baseline.summarize(rows, 2)
        self.assertEqual(result["scheduled_requests"], 4)
        self.assertEqual(result["successful_requests"], 1)
        self.assertEqual(result["non_success_fraction"], .75)
        self.assertEqual(result["http_responses_per_second"], 1)
        self.assertEqual(result["success_per_second"], .5)
        self.assertEqual(result["success_elapsed_ms"]["p99"], 2)
        self.assertEqual(result["all_attempt_elapsed_ms"]["p99"], 100)
        self.assertEqual(result["rejection_elapsed_ms"]["p99"], 1)
        self.assertEqual(result["outcomes"]["not_started"], 1)

    def test_no_invalid_wall_window(self):
        for elapsed in (0, -1, float("nan")):
            with self.assertRaises(baseline.BaselineError):
                baseline.summarize([], elapsed)

    def test_process_cpu_is_one_gateway_pid_not_machine_percent(self):
        result = baseline.process_summary([
            {"monotonic_seconds": 1, "cpu_seconds": 3, "rss_bytes": 100,
             "open_fd_count": 10, "open_socket_count": 3},
            {"monotonic_seconds": 3, "cpu_seconds": 4, "rss_bytes": 200,
             "open_fd_count": 12, "open_socket_count": 4}])
        self.assertEqual(result["gateway_cpu_percent_of_one_core"], 50)
        self.assertEqual(result["sampled_peak_rss_bytes"], 200)
        self.assertIn("not a DB pool", result["scope"])

    def test_live_self_proc_parser_and_stale_start_rejection_are_read_only(self):
        import os
        from pathlib import Path
        fields = (Path("/proc") / str(os.getpid()) / "stat").read_text().rsplit(") ", 1)[1].split()
        identity = baseline.ProcessIdentity(os.getpid(), int(fields[19]))
        measured = identity.sample()
        self.assertGreater(measured["rss_bytes"], 0)
        self.assertGreater(measured["open_fd_count"], 0)
        with self.assertRaises(baseline.BaselineError):
            baseline.ProcessIdentity(os.getpid(), int(fields[19]) + 1).sample()

    def test_scope_cannot_claim_unmeasured_enterprise_acceptance(self):
        plan = baseline.plan()
        self.assertEqual(plan["status"], "PREPARED_NOT_EXECUTED")
        unavailable = plan["scope"]["unavailable"]
        for key in ("provider_latency", "gateway_exclusive_overhead", "server_queue_delay",
                    "streaming_first_token_inter_chunk", "knowledge_retrieval",
                    "database_pool_pressure", "execution_cancellation"):
            self.assertIn(key, unavailable)


class ProfileTests(unittest.TestCase):
    def test_only_exact_fixed_get_profiles(self):
        with self.assertRaises(baseline.BaselineError):
            baseline.ReadProfile("http://external.example/")
        with self.assertRaises(baseline.BaselineError):
            baseline.ReadProfile("public_liveness", token=FAKE_TOKEN)
        for run_id in ("../../health", "?redirect=external", "not-a-uuid", RUN_ID.upper()):
            with self.assertRaises(baseline.BaselineError):
                baseline.ReadProfile("owner_terminal_sdk_read", run_id, "openai_agents", FAKE_TOKEN)

    def test_no_header_injection_and_no_token_repr(self):
        for token in ("x\r\nHost: external", "x\nx", "x x", "é", "", "x" * 16385):
            with self.assertRaises(baseline.BaselineError):
                baseline.ReadProfile("owner_terminal_sdk_read", RUN_ID, "openai_agents", token)
        profile = baseline.ReadProfile("owner_terminal_sdk_read", RUN_ID, "openai_agents", FAKE_TOKEN)
        self.assertNotIn(FAKE_TOKEN, repr(profile))

    def test_owner_identity_engine_and_terminal_state_are_checked(self):
        profile = baseline.ReadProfile("owner_terminal_sdk_read", RUN_ID, "openai_agents", FAKE_TOKEN)
        body = {"run": {"run_id": RUN_ID, "engine": "openai_agents", "status": "cancelled",
                        "harness_thread_id": None, "harness_turn_id": None}}
        self.assertTrue(profile.valid_body(json.dumps(body).encode()))
        for field, value in (("run_id", str(__import__("uuid").uuid4())), ("status", "running"),
                             ("engine", "claude_agent_sdk"), ("harness_thread_id", "fabricated"),
                             ("request_id", "private")):
            changed = {"run": {**body["run"], field: value}}
            self.assertFalse(profile.valid_body(json.dumps(changed).encode()))
        self.assertFalse(profile.valid_body(b"not-json"))

    def test_safety_budgets_reject_unbounded_work(self):
        for values in ({"concurrency": 9}, {"measured_requests": 201}, {"concurrency": True},
                       {"request_timeout_seconds": float("nan")},
                       {"case_timeout_seconds": 31}, {"sample_interval_seconds": .001}):
            with self.assertRaises(baseline.BaselineError):
                baseline.Budget(**values)


class TransportTests(unittest.IsolatedAsyncioTestCase):
    async def request(self, wire, *, timeout=.1, eof=True):
        writer = FakeWriter()
        connection = baseline.HttpConnection()
        with patch.object(baseline.asyncio, "open_connection", AsyncMock(return_value=(stream(wire, eof), writer))) as opener:
            result = await connection.request(baseline.ReadProfile("public_liveness"), baseline.Observation(0), timeout)
            opener.assert_awaited_once_with("127.0.0.1", 18080, limit=baseline.MAX_HEADER)
        return result, writer, connection

    async def test_content_length_timing_and_no_proxy_redirect_retry(self):
        result, writer, connection = await self.request(response())
        self.assertEqual(result.outcome, "success")
        self.assertEqual(result.body_bytes, 18)
        self.assertGreaterEqual(result.first_body_byte_ms, result.first_response_byte_ms)
        self.assertGreaterEqual(result.elapsed_ms, result.response_headers_ms)
        self.assertIn(b"GET /health/live HTTP/1.1", writer.output)
        self.assertNotIn(b"Authorization", writer.output)
        await connection.close()
        self.assertTrue(writer.closed)

    async def test_chunked_body_is_bounded_decoded_json_not_sse(self):
        wire = (b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n"
                b'9\r\n{"status"\r\n9\r\n:"alive"}\r\n0\r\n\r\n')
        result, _, connection = await self.request(wire)
        self.assertEqual(result.outcome, "success")
        await connection.close()

    async def test_keepalive_connection_reused_without_reconnect(self):
        writer = FakeWriter()
        connection = baseline.HttpConnection()
        with patch.object(baseline.asyncio, "open_connection", AsyncMock(return_value=(stream(response() * 2), writer))) as opener:
            first = await connection.request(baseline.ReadProfile("public_liveness"), baseline.Observation(0), .1)
            second = await connection.request(baseline.ReadProfile("public_liveness"), baseline.Observation(1), .1)
            self.assertEqual(opener.await_count, 1)
        self.assertFalse(first.reused_connection)
        self.assertTrue(second.reused_connection)
        self.assertIsNone(second.connection_ms)
        self.assertEqual(second.outcome, "success")
        await connection.close()

    async def test_redirects_are_recorded_without_following(self):
        result, _, connection = await self.request(response(b"", 302, b"Location: https://external.example/\r\n"))
        self.assertEqual(result.status, 302)
        self.assertEqual(result.outcome, "unexpected_http_status")
        await connection.close()

    async def test_429_and_503_are_distinct_from_success(self):
        for status, outcome in ((429, "rate_limited"), (503, "unavailable")):
            result, _, connection = await self.request(response(b"{}", status))
            self.assertEqual(result.outcome, outcome)
            await connection.close()

    async def test_truncated_or_oversized_or_ambiguous_body_closes_without_retry(self):
        examples = [response()[:-2], response(extra=b"Transfer-Encoding: chunked\r\n"),
                    b"HTTP/1.1 200 OK\r\nContent-Length: 65537\r\n\r\n",
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nContent-Length: 2\r\n\r\nx",
                    b"HTTP/1.1 200 OK\r\nContent-Length: -1\r\n\r\n",
                    b"HTTP/1.1 200 OK\r\nX-Large: " + b"x" * 9000 + b"\r\n\r\n"]
        for wire in examples:
            result, writer, _ = await self.request(wire)
            self.assertEqual(result.outcome, "transport_or_protocol_error")
            self.assertTrue(writer.closed)

    async def test_absolute_timeout_closes_local_transport(self):
        result, writer, _ = await self.request(b"HTTP/1.1 200", timeout=.01, eof=False)
        self.assertEqual(result.outcome, "timeout")
        self.assertTrue(writer.closed)

    async def test_client_cancellation_closes_transport_without_remote_claim(self):
        writer = FakeWriter()
        connection = baseline.HttpConnection()
        result = baseline.Observation(0)
        with patch.object(baseline.asyncio, "open_connection", AsyncMock(return_value=(stream(b"", False), writer))):
            task = asyncio.create_task(connection.request(baseline.ReadProfile("public_liveness"), result, 2))
            await asyncio.sleep(.001)
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await task
        self.assertEqual(result.outcome, "client_cancelled_by_case_deadline")
        self.assertTrue(writer.closed)

    async def test_false_200_is_not_a_success(self):
        result, _, connection = await self.request(response(b'{"status":"unready"}'))
        self.assertEqual(result.outcome, "invalid_success_body")
        await connection.close()


class FakeProcess:
    def sample(self):
        return {"monotonic_seconds": time.perf_counter(), "cpu_seconds": 0,
                "rss_bytes": 100, "open_fd_count": 4, "open_socket_count": 2}


class FakeConnection:
    active = 0
    peak = 0
    calls = 0
    closed = 0

    async def request(self, profile, row, timeout):
        type(self).active += 1
        type(self).peak = max(type(self).peak, type(self).active)
        type(self).calls += 1
        try:
            await asyncio.sleep(.001)
            row.outcome = "success"
            row.status = 200
            row.elapsed_ms = 1
            return row
        finally:
            type(self).active -= 1

    async def close(self):
        type(self).closed += 1


class CaseTests(unittest.IsolatedAsyncioTestCase):
    async def test_bounded_workers_warmup_exclusion_and_secret_free_report(self):
        FakeConnection.active = FakeConnection.peak = FakeConnection.calls = FakeConnection.closed = 0
        verified = []
        profile = baseline.ReadProfile("owner_terminal_sdk_read", RUN_ID, "openai_agents", FAKE_TOKEN)
        result = await baseline.measure_case(profile, baseline.Budget(concurrency=2, measured_requests=6),
                                             FakeProcess(), lambda: verified.append(True),
                                             connection_factory=FakeConnection)
        self.assertEqual(len(verified), 2)
        self.assertEqual(FakeConnection.peak, 2)
        self.assertEqual(FakeConnection.calls, 8)
        self.assertEqual(FakeConnection.closed, 2)
        self.assertEqual(result["warmup"]["requests"], 2)
        self.assertEqual(result["summary"]["successful_requests"], 6)
        self.assertEqual(result["summary"]["client_batch_queue_ms"]["count"], 6)
        self.assertNotIn(FAKE_TOKEN, json.dumps(result))
        self.assertNotIn(RUN_ID, json.dumps(result))

    async def test_ownership_failure_precedes_any_connection(self):
        def reject():
            raise baseline.BaselineError("stale_owner")
        with self.assertRaises(baseline.BaselineError):
            await baseline.measure_case(baseline.ReadProfile("public_liveness"), baseline.Budget(),
                                        FakeProcess(), reject,
                                        connection_factory=lambda: self.fail("must not construct transport"))

    async def test_load_generator_cpu_excludes_warmup_like_wall_window(self):
        from types import SimpleNamespace
        usage = [SimpleNamespace(ru_utime=value, ru_stime=0) for value in (0, 10, 11)]
        with patch.object(baseline.resource, "getrusage", side_effect=usage):
            result = await baseline.measure_case(
                baseline.ReadProfile("public_liveness"), baseline.Budget(),
                FakeProcess(), lambda: None, connection_factory=FakeConnection)
        self.assertEqual(result["load_generator_cpu_seconds"], 1)
        self.assertEqual(result["load_generator_cpu_window"], "measured_batch_excluding_warmup")

    async def test_final_identity_failure_preserves_measured_observations(self):
        checks = 0
        def verify():
            nonlocal checks
            checks += 1
            if checks == 2:
                raise baseline.BaselineError("identity_changed")
        result = await baseline.measure_case(baseline.ReadProfile("public_liveness"), baseline.Budget(),
                                             FakeProcess(), verify, connection_factory=FakeConnection)
        self.assertEqual(result["status"], "IDENTITY_VERIFICATION_FAILED")
        self.assertEqual(len(result["observations"]), 12)
        self.assertEqual(result["summary"]["successful_requests"], 12)

    async def test_cancelled_case_propagates_with_partial_observations(self):
        class Slow(FakeConnection):
            async def request(self, profile, row, timeout):
                await asyncio.sleep(1)
        task = asyncio.create_task(baseline.measure_case(
            baseline.ReadProfile("public_liveness"), baseline.Budget(), FakeProcess(),
            lambda: None, connection_factory=Slow))
        await asyncio.sleep(.001)
        task.cancel()
        with self.assertRaises(baseline.BaselineCancelled) as raised:
            await task
        partial = raised.exception.partial_case
        self.assertEqual(partial["status"], "CANCELLED_WITH_PARTIAL_OBSERVATIONS")
        self.assertEqual(len(partial["observations"]), 12)
        self.assertNotIn(FAKE_TOKEN, json.dumps(partial))

    async def test_failed_warmups_are_reported_never_excluded_into_pass(self):
        class Rejected(FakeConnection):
            async def request(self, profile, row, timeout):
                row.outcome = "rate_limited"
                row.status = 429
                row.elapsed_ms = 1
                return row
        result = await baseline.measure_case(baseline.ReadProfile("public_liveness"), baseline.Budget(),
                                             FakeProcess(), lambda: None, connection_factory=Rejected)
        self.assertEqual(result["status"], "WARMUP_FAILED")
        self.assertEqual(result["summary"]["outcomes"], {"not_started": 12})

    async def test_rss_guard_aborts_without_claiming_capacity_or_slo(self):
        result = await baseline.measure_case(baseline.ReadProfile("public_liveness"),
                                             baseline.Budget(max_gateway_rss_bytes=50),
                                             FakeProcess(), lambda: None, connection_factory=FakeConnection)
        self.assertEqual(result["status"], "RESOURCE_OBSERVATION_FAILED")
        self.assertIn("gateway_rss_safety_budget_exceeded", result["resource_errors"])
        self.assertGreater(result["summary"]["outcomes"].get("not_started", 0), 0)


if __name__ == "__main__":
    unittest.main()
