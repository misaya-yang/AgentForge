#!/usr/bin/env python3
"""Bounded, provider-free native Gateway baseline; no service lifecycle or credentials.

CLI prints an offline plan. An existing owned-fixture controller calls measure_case
with its live process verifier and an in-memory authenticated, fixed read profile.
The owning contract is docs/design/enterprise-performance-contract.md.
"""
from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import math
import os
import resource
import statistics
import time
import uuid
from collections import Counter
from collections.abc import Callable
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

ORIGIN = "http://127.0.0.1:18080"
HOST = "127.0.0.1"
PORT = 18080
MAX_BODY = 64 * 1024
MAX_HEADER = 8 * 1024
SCHEMA = "gateway-performance-baseline/v1"


class BaselineError(Exception):
    """A redacted, local contract failure. Never includes response or token text."""


class BaselineCancelled(asyncio.CancelledError):
    """Propagates cancellation with bounded, credential-free partial observations."""
    def __init__(self, partial_case: dict[str, Any]) -> None:
        super().__init__("baseline_cancelled")
        self.partial_case = partial_case


def digest(path: Path) -> str:
    value = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
    return value.hexdigest()


def distribution(values: list[float]) -> dict[str, Any]:
    """Nearest-rank empirical quantiles; absent observations are never zero."""
    if any(not math.isfinite(value) for value in values):
        raise BaselineError("nonfinite_measurement")
    ordered = sorted(values)
    return {
        "count": len(ordered),
        "method": "nearest_rank",
        "p50": ordered[math.ceil(len(ordered) * .50) - 1] if ordered else None,
        "p95": ordered[math.ceil(len(ordered) * .95) - 1] if ordered else None,
        "p99": ordered[math.ceil(len(ordered) * .99) - 1] if ordered else None,
        "min": ordered[0] if ordered else None,
        "max": ordered[-1] if ordered else None,
        "mean": statistics.fmean(ordered) if ordered else None,
        "tail_sample_warning": len(ordered) < 100,
    }


@dataclass(frozen=True)
class ReadProfile:
    name: str
    run_id: str | None = None
    framework: str | None = None
    token: str | None = field(default=None, repr=False, compare=False)

    def __post_init__(self) -> None:
        if self.name == "public_liveness":
            if any(value is not None for value in (self.run_id, self.framework, self.token)):
                raise BaselineError("liveness_has_no_identity")
        elif self.name == "owner_terminal_sdk_read":
            try:
                valid_id = str(uuid.UUID(self.run_id or "")) == self.run_id
            except (ValueError, TypeError, AttributeError):
                valid_id = False
            if not valid_id or self.framework not in ("openai_agents", "claude_agent_sdk"):
                raise BaselineError("invalid_owned_sdk_profile")
            if (not self.token or len(self.token) > 16 * 1024
                    or not self.token.isascii()
                    or any(ord(char) < 33 or ord(char) == 127 for char in self.token)):
                raise BaselineError("invalid_in_memory_token")
        else:
            raise BaselineError("unsupported_read_profile")

    @property
    def path(self) -> str:
        if self.name == "public_liveness":
            return "/health/live"
        return "/api/v1/assistant/runs/" + str(self.run_id)

    def valid_body(self, payload: bytes) -> bool:
        try:
            value = json.loads(payload)
        except (ValueError, UnicodeDecodeError):
            return False
        if self.name == "public_liveness":
            return value == {"status": "alive"}
        run = value.get("run") if isinstance(value, dict) else None
        return (isinstance(run, dict) and run.get("run_id") == self.run_id
                and run.get("engine") == self.framework and run.get("status") == "cancelled"
                and "harness_thread_id" in run and run["harness_thread_id"] is None
                and "harness_turn_id" in run and run["harness_turn_id"] is None
                and "request_id" not in run and "service_id" not in run)


@dataclass(frozen=True)
class Budget:
    concurrency: int = 1
    measured_requests: int = 12
    request_timeout_seconds: float = 2.0
    case_timeout_seconds: float = 15.0
    sample_interval_seconds: float = .05
    max_gateway_rss_bytes: int | None = None

    def __post_init__(self) -> None:
        numbers = (self.request_timeout_seconds, self.case_timeout_seconds,
                   self.sample_interval_seconds)
        if (type(self.concurrency) is not int or not 1 <= self.concurrency <= 8
                or type(self.measured_requests) is not int
                or not self.concurrency <= self.measured_requests <= 200
                or any(not math.isfinite(value) for value in numbers)
                or not .05 <= self.request_timeout_seconds <= 5
                or not self.request_timeout_seconds <= self.case_timeout_seconds <= 30
                or not .02 <= self.sample_interval_seconds <= 1
                or (self.max_gateway_rss_bytes is not None
                    and self.max_gateway_rss_bytes <= 0)):
            raise BaselineError("invalid_safety_budget")


@dataclass
class Observation:
    sequence: int
    status: int | None = None
    outcome: str = "not_started"
    client_queue_ms: float | None = None
    elapsed_ms: float | None = None
    first_response_byte_ms: float | None = None
    response_headers_ms: float | None = None
    first_body_byte_ms: float | None = None
    body_bytes: int = 0
    reused_connection: bool = False
    connection_ms: float | None = None


class HttpConnection:
    """One worker's exact numeric-loopback HTTP/1.1 connection, no redirect/retry.

    Only fixed GET profiles are accepted. Request-level wait_for provides an
    absolute deadline, including slow chunk/header delivery. A failed connection
    closes and is never retried for that request; subsequent requests may reconnect.
    """
    def __init__(self) -> None:
        self.reader: asyncio.StreamReader | None = None
        self.writer: asyncio.StreamWriter | None = None

    async def close(self) -> None:
        writer, self.writer = self.writer, None
        self.reader = None
        if writer is not None:
            writer.close()
            try:
                await asyncio.wait_for(writer.wait_closed(), .2)
            except (OSError, asyncio.TimeoutError):
                pass

    async def request(self, profile: ReadProfile, result: Observation,
                      timeout: float) -> Observation:
        start = time.perf_counter()
        try:
            await asyncio.wait_for(self._request(profile, result, start), timeout)
        except asyncio.TimeoutError:
            result.outcome = "timeout"
            await self.close()
        except asyncio.CancelledError:
            result.outcome = "client_cancelled_by_case_deadline"
            await self.close()
            raise
        except (OSError, EOFError, ValueError, BaselineError,
                asyncio.IncompleteReadError, asyncio.LimitOverrunError):
            result.outcome = "transport_or_protocol_error"
            await self.close()
        finally:
            result.elapsed_ms = (time.perf_counter() - start) * 1000
        return result

    async def _request(self, profile: ReadProfile, result: Observation, start: float) -> None:
        result.reused_connection = self.writer is not None
        if self.writer is None:
            connect_start = time.perf_counter()
            self.reader, self.writer = await asyncio.open_connection(HOST, PORT, limit=MAX_HEADER)
            result.connection_ms = (time.perf_counter() - connect_start) * 1000
        assert self.reader is not None and self.writer is not None
        headers = (f"GET {profile.path} HTTP/1.1\r\nHost: {HOST}:{PORT}\r\n"
                   "Accept: application/json\r\nConnection: keep-alive\r\n")
        if profile.token is not None:
            headers += "Authorization: Bearer " + profile.token + "\r\n"
        self.writer.write((headers + "\r\n").encode("ascii"))
        await self.writer.drain()
        first = await self.reader.readexactly(1)
        result.first_response_byte_ms = (time.perf_counter() - start) * 1000
        raw_headers = first + await self.reader.readuntil(b"\r\n\r\n")
        result.response_headers_ms = (time.perf_counter() - start) * 1000
        status, response_headers = parse_headers(raw_headers)
        result.status = status
        payload = await self._body(response_headers, result, start)
        result.body_bytes = len(payload)
        if status == 200:
            is_json = response_headers.get("content-type", "").split(";", 1)[0] == "application/json"
            result.outcome = "success" if is_json and profile.valid_body(payload) else "invalid_success_body"
        elif status == 429:
            result.outcome = "rate_limited"
        elif status == 503:
            result.outcome = "unavailable"
        else:
            result.outcome = "unexpected_http_status"
        if response_headers.get("connection", "").lower() == "close":
            await self.close()

    async def _body(self, headers: dict[str, str], result: Observation, start: float) -> bytes:
        assert self.reader is not None
        payload = bytearray()

        async def take(count: int) -> None:
            if count < 0 or len(payload) + count > MAX_BODY:
                raise BaselineError("response_body_limit")
            if count:
                if result.first_body_byte_ms is None:
                    payload.extend(await self.reader.readexactly(1))
                    result.first_body_byte_ms = (time.perf_counter() - start) * 1000
                    count -= 1
                payload.extend(await self.reader.readexactly(count))

        encoding = headers.get("transfer-encoding")
        length = headers.get("content-length")
        if encoding is not None:
            if encoding.lower() != "chunked" or length is not None:
                raise BaselineError("ambiguous_response_framing")
            framing_bytes = 0
            while True:
                line = await self.reader.readuntil(b"\r\n")
                framing_bytes += len(line)
                if framing_bytes > MAX_HEADER:
                    raise BaselineError("chunk_framing_limit")
                raw_size = line[:-2].split(b";", 1)[0]
                if not raw_size or any(char not in b"0123456789abcdefABCDEF" for char in raw_size):
                    raise BaselineError("invalid_chunk_size")
                size = int(raw_size, 16)
                if not size:
                    while True:
                        trailer = await self.reader.readuntil(b"\r\n")
                        framing_bytes += len(trailer)
                        if framing_bytes > MAX_HEADER:
                            raise BaselineError("trailer_limit")
                        if trailer == b"\r\n":
                            return bytes(payload)
                await take(size)
                if await self.reader.readexactly(2) != b"\r\n":
                    raise BaselineError("invalid_chunk_delimiter")
        if length is None or not length.isascii() or not length.isdecimal():
            raise BaselineError("missing_or_invalid_response_length")
        await take(int(length))
        return bytes(payload)


def parse_headers(raw: bytes) -> tuple[int, dict[str, str]]:
    if len(raw) > MAX_HEADER or not raw.endswith(b"\r\n\r\n"):
        raise BaselineError("response_header_limit")
    lines = raw[:-4].decode("iso-8859-1").split("\r\n")
    parts = lines[0].split(" ", 2)
    if (len(parts) < 2 or parts[0] != "HTTP/1.1"
            or len(parts[1]) != 3 or not parts[1].isdigit()):
        raise BaselineError("invalid_response_status")
    status = int(parts[1])
    if not 200 <= status <= 599:
        raise BaselineError("unsupported_response_status")
    headers: dict[str, str] = {}
    for line in lines[1:]:
        key, separator, value = line.partition(":")
        key = key.lower()
        if not separator or not key or key.strip() != key:
            raise BaselineError("invalid_response_header")
        if key in ("content-length", "transfer-encoding", "content-type", "connection"):
            if key in headers:
                raise BaselineError("duplicate_response_framing")
            headers[key] = value.strip()
    return status, headers


@dataclass(frozen=True)
class ProcessIdentity:
    pid: int
    start_ticks: int

    def sample(self) -> dict[str, float | int]:
        root = Path("/proc") / str(self.pid)
        fields = (root / "stat").read_text().rsplit(") ", 1)[1].split()
        if int(fields[19]) != self.start_ticks:
            raise BaselineError("gateway_process_identity_changed")
        fds = list((root / "fd").iterdir())
        sockets: set[str] = set()
        for fd in fds:
            try:
                value = os.readlink(fd)
                if value.startswith("socket:["):
                    sockets.add(value)
            except FileNotFoundError:
                pass  # An already-closed descriptor is not a live pressure sample.
        return {
            "monotonic_seconds": time.perf_counter(),
            "cpu_seconds": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
            "rss_bytes": int(fields[21]) * os.sysconf("SC_PAGE_SIZE"),
            "open_fd_count": len(fds),
            "open_socket_count": len(sockets),
        }


def summarize(observations: list[Observation], elapsed: float) -> dict[str, Any]:
    if not math.isfinite(elapsed) or elapsed <= 0:
        raise BaselineError("invalid_measurement_window")
    started = [row for row in observations if row.elapsed_ms is not None]
    success = [row for row in observations if row.outcome == "success"]
    http_responses = [row for row in observations if row.status is not None]
    latency = lambda rows, key: distribution([getattr(row, key) for row in rows
                                            if getattr(row, key) is not None])
    rejection = [row for row in observations if row.outcome in ("rate_limited", "unavailable")]
    return {
        "scheduled_requests": len(observations), "started_requests": len(started),
        "http_responses": len(http_responses), "successful_requests": len(success),
        "non_success_requests": len(observations) - len(success),
        "outcomes": dict(sorted(Counter(row.outcome for row in observations).items())),
        "http_statuses": dict(sorted(Counter(str(row.status) for row in http_responses).items())),
        "wall_seconds": elapsed,
        "scheduled_per_second": len(observations) / elapsed,
        "finished_attempts_per_second": len(started) / elapsed,
        "http_responses_per_second": len(http_responses) / elapsed,
        "success_per_second": len(success) / elapsed,
        "non_success_fraction": (len(observations) - len(success)) / len(observations) if observations else None,
        "all_attempt_elapsed_ms": latency(started, "elapsed_ms"),
        "success_elapsed_ms": latency(success, "elapsed_ms"),
        "rejection_elapsed_ms": latency(rejection, "elapsed_ms"),
        "first_response_byte_ms": latency(success, "first_response_byte_ms"),
        "response_headers_ms": latency(success, "response_headers_ms"),
        "first_body_byte_ms": latency(success, "first_body_byte_ms"),
        "client_batch_queue_ms": latency(observations, "client_queue_ms"),
        "new_connection_ms": latency(observations, "connection_ms"),
        "reused_connections": sum(row.reused_connection for row in observations),
        "response_body_bytes": sum(row.body_bytes for row in observations),
    }


def process_summary(samples: list[dict[str, float | int]]) -> dict[str, Any]:
    if len(samples) < 2:
        return {"status": "unavailable", "reason": "fewer_than_two_process_samples"}
    elapsed = float(samples[-1]["monotonic_seconds"]) - float(samples[0]["monotonic_seconds"])
    cpu = float(samples[-1]["cpu_seconds"]) - float(samples[0]["cpu_seconds"])
    if elapsed <= 0 or cpu < 0:
        raise BaselineError("invalid_process_sample_window")
    return {
        "status": "measured", "sample_count": len(samples), "window_seconds": elapsed,
        "gateway_cpu_seconds": cpu, "gateway_cpu_percent_of_one_core": 100 * cpu / elapsed,
        "cpu_tick_resolution_seconds": 1 / os.sysconf("SC_CLK_TCK"),
        "sampled_peak_rss_bytes": max(row["rss_bytes"] for row in samples),
        "rss_start_bytes": samples[0]["rss_bytes"], "rss_end_bytes": samples[-1]["rss_bytes"],
        "sampled_peak_open_fd_count": max(row["open_fd_count"] for row in samples),
        "sampled_peak_open_socket_count": max(row["open_socket_count"] for row in samples),
        "scope": "Gateway PID only, including its background work; sampled socket count is not a DB pool gauge",
    }


async def measure_case(profile: ReadProfile, budget: Budget, process: ProcessIdentity,
                       verify_owned: Callable[[], Any], *,
                       connection_factory: Callable[[], HttpConnection] = HttpConnection) -> dict[str, Any]:
    """Read one fixed profile; caller verifies binary/source and owns service lifecycle.

    Finite closed-loop batch: worker count is concurrency, pending work has bounded
    in-client queue time. No model, service discovery, mutation, retry or new identity.
    One warmup on EACH connection is recorded separately from the measured batch.
    """
    verify_owned()
    process.sample()  # Fail closed before any HTTP if PID ownership is stale.
    connections = [connection_factory() for _ in range(budget.concurrency)]
    warmups = [Observation(index) for index in range(budget.concurrency)]
    observations = [Observation(index) for index in range(budget.measured_requests)]
    samples: list[dict[str, float | int]] = []
    stop = asyncio.Event()
    sampler_error: list[str] = []
    status = "MEASURED_EXPLORATORY"
    generator_start = resource.getrusage(resource.RUSAGE_SELF)

    async def sample() -> None:
        try:
            while not stop.is_set():
                point = process.sample()
                samples.append(point)
                if (budget.max_gateway_rss_bytes is not None
                        and point["rss_bytes"] > budget.max_gateway_rss_bytes):
                    sampler_error.append("gateway_rss_safety_budget_exceeded")
                    stop.set()
                    return
                try:
                    await asyncio.wait_for(stop.wait(), budget.sample_interval_seconds)
                except asyncio.TimeoutError:
                    pass
        except (OSError, ValueError, IndexError, BaselineError):
            sampler_error.append("gateway_process_sampling_failed")
            stop.set()

    started = time.perf_counter()
    sampler: asyncio.Task | None = None
    workers: list[asyncio.Task] = []
    try:
        await asyncio.gather(*(connection.request(profile, row, budget.request_timeout_seconds)
                               for connection, row in zip(connections, warmups, strict=True)))
        if any(row.outcome != "success" for row in warmups):
            status = "WARMUP_FAILED"
        else:
            pending = iter(observations)
            generator_start = resource.getrusage(resource.RUSAGE_SELF)
            started = time.perf_counter()
            samples.append(process.sample())
            sampler = asyncio.create_task(sample())

            async def worker(connection: HttpConnection) -> None:
                for row in pending:
                    if stop.is_set():
                        return
                    row.client_queue_ms = (time.perf_counter() - started) * 1000
                    await connection.request(profile, row, budget.request_timeout_seconds)

            workers = [asyncio.create_task(worker(connection)) for connection in connections]
            try:
                await asyncio.wait_for(asyncio.gather(*workers), budget.case_timeout_seconds)
            except asyncio.TimeoutError:
                status = "CASE_DEADLINE_EXCEEDED"
            finally:
                stop.set()
                await sampler
            if sampler_error:
                status = "RESOURCE_OBSERVATION_FAILED"
            try:
                samples.append(process.sample())
            except (OSError, ValueError, IndexError, BaselineError):
                sampler_error.append("final_gateway_process_sample_failed")
                status = "RESOURCE_OBSERVATION_FAILED"
        elapsed = time.perf_counter() - started
        try:
            verify_owned()
        except (OSError, ValueError, KeyError, RuntimeError, BaselineError):
            sampler_error.append("final_gateway_ownership_verification_failed")
            status = "IDENTITY_VERIFICATION_FAILED"
        generator_end = resource.getrusage(resource.RUSAGE_SELF)
        return {
            "profile": profile.name, "budget": asdict(budget), "status": status,
            "load_model": "finite_closed_loop_batch_no_retries",
            "warmup": {"requests": len(warmups), "observations": [asdict(row) for row in warmups]},
            "summary": summarize(observations, elapsed),
            "process": process_summary(samples),
            "resource_errors": sampler_error,
            "load_generator_cpu_window": ("warmup_failed_no_measured_batch" if status == "WARMUP_FAILED"
                                          else "measured_batch_excluding_warmup"),
            "load_generator_cpu_seconds": ((generator_end.ru_utime + generator_end.ru_stime)
                                           - (generator_start.ru_utime + generator_start.ru_stime)),
            "observations": [asdict(row) for row in observations],
        }
    except asyncio.CancelledError as error:
        elapsed = max(time.perf_counter() - started, 1e-9)
        partial = {"profile": profile.name, "budget": asdict(budget),
                   "status": "CANCELLED_WITH_PARTIAL_OBSERVATIONS",
                   "warmup": {"requests": len(warmups), "observations": [asdict(row) for row in warmups]},
                   "summary": summarize(observations, elapsed),
                   "observations": [asdict(row) for row in observations]}
        raise BaselineCancelled(partial) from error
    finally:
        stop.set()
        for worker in workers:
            if not worker.done():
                worker.cancel()
        if workers:
            await asyncio.gather(*workers, return_exceptions=True)
        if sampler is not None and not sampler.done():
            await sampler
        await asyncio.gather(*(connection.close() for connection in connections))


def measurement_scope() -> dict[str, Any]:
    return {
        "measured": ["client_observed_fixed_GET_latency", "first_response_and_body_byte",
                     "empirical_p50_p95_p99", "closed_loop_throughput_and_all_errors",
                     "in_client_batch_queue_delay", "Gateway_PID_CPU_RSS_FD_socket_samples"],
        "unavailable": {
            "gateway_exclusive_overhead": "No per-request provider or internal-stage server timing on these reads; client/network costs included.",
            "provider_latency": "No model request is made; not applicable, never synthesized as zero.",
            "streaming_first_token_inter_chunk": "JSON GET profiles do not exercise SSE; no streaming acceptance.",
            "server_queue_delay": "Client batch queue is not server admission or execution queue.",
            "knowledge_retrieval": "No Knowledge query, embedding, retrieval or relevance fixture is exercised.",
            "database_pool_pressure": "FD/socket samples cannot distinguish pool occupancy, waiters or acquire duration.",
            "server_saturation_capacity": "Bounded concurrency is not a saturation search; 429 establishes rate policy rejection only.",
            "execution_cancellation": "Reading a cancelled inert SDK run does not execute or stop an SDK/provider/tool loop.",
        },
        "enterprise_slo": "UNSET_REQUIRES_OWNER_REVIEWED_WORKLOAD_AND_ENVIRONMENT",
        "acceptance": "Exploratory measurement, not release readiness, scale capacity or an enterprise SLO pass.",
    }


def plan() -> dict[str, Any]:
    return {"schema": SCHEMA, "status": "PREPARED_NOT_EXECUTED", "origin": ORIGIN,
            "profiles": ["public_liveness", "owner_terminal_sdk_read"],
            "concurrency": [1, 2, 4], "business_measured_requests_per_case": 12,
            "business_quiet_windows": "Controller preserves existing rate limits between normal cases",
            "service_lifecycle": "Existing owned-fixture controller only; this module starts/stops nothing",
            "scope": measurement_scope()}


if __name__ == "__main__":
    argparse.ArgumentParser(description=__doc__).parse_args()
    print(json.dumps(plan(), indent=2))
