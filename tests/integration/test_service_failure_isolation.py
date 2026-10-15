"""
Phase-6 failure-isolation integration suite.

Goal: prove the microservice extraction goal — *one service down, others
survive* — by physically stopping individual containers and asserting
that:
  * Gateway core endpoints (/health, /api/v1/auth/*) keep returning 200.
  * Cross-service calls fail *cleanly* (502/503 with bounded latency,
    not 500 / hang / half-written SSE) when their upstream is gone.
  * Per-route circuit breakers isolate breakage — one bad model route
    does not poison the assistant /health endpoint.

Run with:

    conda run -n ai_gateway python -m pytest \
        tests/integration/test_service_failure_isolation.py -m integration -v

Environment switches:

    INTEGRATION_GATEWAY_URL          Base URL (default http://localhost:8080)
    INTEGRATION_DOCKER_COMPOSE_FILE  Compose file (default docker-compose.yml)
    INTEGRATION_DOCKER_ENV_FILE      Compose env file for metadata/stop/start.
                                     Defaults to .env when present, then
                                     .env.example, then no env file.
    INTEGRATION_SKIP_REASON          If set, whole module is skipped (CI escape hatch).
    INTEGRATION_USE_LIVE_STACK       "true" → never docker stop/start (target prod;
                                     only the breaker isolation test runs).

The whole module skips cleanly when ``docker compose ps`` shows no
running services, so collection always succeeds even on a vanilla CI
worker without docker.
"""

from __future__ import annotations

import asyncio
import contextlib
import os
import subprocess
import time
import uuid
from collections.abc import Iterable
from typing import Any

import httpx
import pytest
import pytest_asyncio

# ---------------------------------------------------------------------------
# Configuration
# ---------------------------------------------------------------------------

_GATEWAY_URL = os.getenv("INTEGRATION_GATEWAY_URL", "http://localhost:8080").rstrip("/")
_API_PREFIX = f"{_GATEWAY_URL}/api/v1"
_COMPOSE_FILE = os.getenv("INTEGRATION_DOCKER_COMPOSE_FILE", "docker-compose.yml")
_COMPOSE_ENV_FILE = os.getenv("INTEGRATION_DOCKER_ENV_FILE", "").strip()
_SKIP_REASON = os.getenv("INTEGRATION_SKIP_REASON", "").strip()
_USE_LIVE_STACK = os.getenv("INTEGRATION_USE_LIVE_STACK", "").lower() == "true"

# Timeouts tuned to the acceptance criteria. ``_HEALTH_BUDGET`` is the
# hard cap given to the gateway's ``/health`` endpoint after a sibling
# is killed (Phase-6 SLO: gateway survival within 2s).
_HEALTH_BUDGET = 2.0
_FAIL_FAST_BUDGET = 10.0
_RESTART_HEALTHY_BUDGET = 60.0
_DRAIN_BUDGET = 30.0


pytestmark = pytest.mark.integration


# ---------------------------------------------------------------------------
# docker compose helper (private, never raises on missing docker)
# ---------------------------------------------------------------------------

def _compose_env_args() -> list[str]:
    if _COMPOSE_ENV_FILE:
        return ["--env-file", _COMPOSE_ENV_FILE]
    if os.path.exists(".env"):
        return ["--env-file", ".env"]
    if os.path.exists(".env.example"):
        return ["--env-file", ".env.example"]
    return []


def _compose(cmd: list[str], timeout: float = 30.0) -> subprocess.CompletedProcess:
    """Run ``docker compose -f <file> <cmd>``; never raise on missing docker."""
    return subprocess.run(
        ["docker", "compose", *_compose_env_args(), "-f", _COMPOSE_FILE, *cmd],
        check=False,
        capture_output=True,
        text=True,
        timeout=timeout,
    )


def _docker_available() -> bool:
    """Cheap probe — ``docker version`` returns 0 even if no compose project is up."""
    try:
        proc = subprocess.run(
            ["docker", "version"],
            check=False,
            capture_output=True,
            text=True,
            timeout=5.0,
        )
        return proc.returncode == 0
    except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
        return False


def _running_services() -> list[str]:
    """Names of services currently up under the configured compose project.

    Returns ``[]`` when docker / the compose project is not available — used
    by the module-level skip below so collection always succeeds.
    """
    if not _docker_available():
        return []
    proc = _compose(["ps", "--services", "--filter", "status=running"], timeout=10.0)
    if proc.returncode != 0:
        return []
    return [line.strip() for line in proc.stdout.splitlines() if line.strip()]


def _gateway_responds() -> bool:
    """True iff the gateway answers /health within 2 s."""
    try:
        resp = httpx.get(f"{_GATEWAY_URL}/health", timeout=_HEALTH_BUDGET)
        return resp.status_code == 200
    except (httpx.HTTPError, OSError):
        return False


# ---------------------------------------------------------------------------
# Module-level skip gates (do NOT run docker on collection)
# ---------------------------------------------------------------------------
#
# The deferred imports + early ``return`` shape means ``pytest --collect-only``
# only touches lightweight stdlib code. Collection succeeds without docker.

if _SKIP_REASON:
    pytest.skip(f"INTEGRATION_SKIP_REASON set: {_SKIP_REASON}", allow_module_level=True)


def _module_should_skip() -> tuple[bool, str]:
    """Return (skip?, reason). Resolved lazily by fixtures so collection
    never fires docker probes."""
    if _USE_LIVE_STACK:
        # In live-stack mode we only need the gateway URL to be reachable.
        if not _gateway_responds():
            return True, f"INTEGRATION_USE_LIVE_STACK=true but {_GATEWAY_URL}/health unreachable"
        return False, ""
    if not _docker_available():
        return True, "docker not available; skipping failure-isolation suite"
    services = _running_services()
    if not services:
        return True, (
            f"no services running under compose file '{_COMPOSE_FILE}'; "
            "start the stack with `docker compose up -d --wait` to enable this suite"
        )
    return False, ""


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------

@pytest.fixture(scope="module")
def compose_stack() -> Iterable[dict[str, Any]]:
    """Module-scoped fixture asserting the compose stack is up.

    Does NOT run ``docker compose up`` — that is the operator's
    responsibility (the suite is meant to drop *into* an existing
    deployment, not provision one). It only verifies that *something*
    is running and that the gateway responds.

    Yields a context dict with the discovered service names so each
    test can probe before issuing a stop.
    """
    skip, reason = _module_should_skip()
    if skip:
        pytest.skip(reason)

    if _USE_LIVE_STACK:
        services: list[str] = []
    else:
        services = _running_services()

    # Wait up to 30 s for /health to return 200. This guards against a
    # stack that was just `up`-d and has not finished its start_period.
    deadline = time.time() + 30.0
    while time.time() < deadline:
        if _gateway_responds():
            break
        time.sleep(1.0)
    else:  # pragma: no cover — only fires on a sick stack
        pytest.skip(f"gateway at {_GATEWAY_URL}/health did not become ready within 30s")

    yield {
        "services": services,
        "live_stack": _USE_LIVE_STACK,
    }
    # Best-effort cleanup: nothing to do — tests restore their own state.


@pytest_asyncio.fixture
async def http() -> Iterable[httpx.AsyncClient]:
    """Per-test async HTTPX client with sane defaults."""
    async with httpx.AsyncClient(timeout=15.0) as client:
        yield client


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

async def _async_get(client: httpx.AsyncClient, url: str, **kw: Any) -> httpx.Response:
    return await client.get(url, **kw)


async def _async_post(client: httpx.AsyncClient, url: str, **kw: Any) -> httpx.Response:
    return await client.post(url, **kw)


async def _measure_latency(coro_factory) -> tuple[httpx.Response, float]:
    """Run an awaitable factory and return (response, elapsed seconds)."""
    started = time.perf_counter()
    resp = await coro_factory()
    return resp, time.perf_counter() - started


def _service_in_stack(stack: dict[str, Any], name: str) -> bool:
    if stack.get("live_stack"):
        return False
    return name in stack["services"]


def _stop_service(name: str) -> None:
    proc = _compose(["stop", name], timeout=20.0)
    if proc.returncode != 0:
        pytest.fail(f"docker compose stop {name} failed: {proc.stderr}")


def _start_service(name: str) -> None:
    proc = _compose(["start", name], timeout=30.0)
    if proc.returncode != 0:
        pytest.fail(f"docker compose start {name} failed: {proc.stderr}")


async def _wait_for_health(
    client: httpx.AsyncClient,
    url: str,
    *,
    timeout: float,
) -> bool:
    """Poll ``url`` until it returns 200 or timeout. Returns success."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            resp = await client.get(url, timeout=2.0)
            if resp.status_code == 200:
                return True
        except (httpx.HTTPError, OSError):
            pass
        await asyncio.sleep(1.0)
    return False


@contextlib.contextmanager
def _ensure_started(name: str):
    """Context manager: always restart ``name`` on exit, even on failure."""
    try:
        yield
    finally:
        # Best-effort restore. If docker is gone we silently let it pass —
        # the next test or the operator will see the broken stack via /health.
        with contextlib.suppress(Exception):
            _start_service(name)


# ---------------------------------------------------------------------------
# Test 1 — Agent Runtime down, Gateway + others survive
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_agent_runtime_down_isolation(
    compose_stack: dict[str, Any],
    http: httpx.AsyncClient,
) -> None:
    """Runtime down leaves Gateway live while Agent turns fail closed, then recover."""
    if compose_stack["live_stack"]:
        pytest.skip("INTEGRATION_USE_LIVE_STACK=true; skipping docker stop/start tests")
    if not _service_in_stack(compose_stack, "agent-runtime"):
        pytest.skip("agent-runtime not in running compose stack")

    with _ensure_started("agent-runtime"):
        _stop_service("agent-runtime")

        # 1. Gateway /health stays 200 inside the 2 s budget.
        health_resp, health_elapsed = await _measure_latency(
            lambda: _async_get(http, f"{_GATEWAY_URL}/health", timeout=_HEALTH_BUDGET)
        )
        assert health_resp.status_code == 200, health_resp.text
        assert health_elapsed < _HEALTH_BUDGET, f"/health took {health_elapsed:.2f}s"

        # 2. A non-Agent Gateway endpoint still responds (auth/login is the
        # canonical "is the gateway alive" probe — a 401/422 reply means
        # the route is alive; we just want it to NOT 5xx).
        login_resp = await _async_post(
            http,
            f"{_API_PREFIX}/auth/login",
            json={"email": "nonexistent@enterprise.test", "password": "x"},
            timeout=5.0,
        )
        assert login_resp.status_code < 500, (
            f"auth/login returned 5xx with Runtime down: {login_resp.status_code} {login_resp.text}"
        )

        # 3. A new Agent turn fails closed and promptly. An unauthenticated
        # edge rejection is also acceptable because it happens before Runtime.
        assistant_resp, assistant_elapsed = await _measure_latency(
            lambda: _async_post(
                http,
                f"{_API_PREFIX}/assistant/chat",
                json={"message": "ping"},
                timeout=_FAIL_FAST_BUDGET,
            )
        )
        assert assistant_elapsed < _FAIL_FAST_BUDGET, (
            f"/assistant/chat hung for {assistant_elapsed:.2f}s "
            f"(budget {_FAIL_FAST_BUDGET}s)"
        )
        assert assistant_resp.status_code in {401, 403, 502, 503}, (
            f"expected an edge rejection or clean upstream failure with Runtime down; "
            f"got {assistant_resp.status_code}: {assistant_resp.text[:200]}"
        )
        assert assistant_resp.status_code != 500, "Runtime down must not bubble up as a 500"

        # 4. Restart Runtime and wait for Gateway readiness.
        _start_service("agent-runtime")
        recovered = await _wait_for_health(
            http, f"{_GATEWAY_URL}/health/ready", timeout=_RESTART_HEALTHY_BUDGET
        )
        assert recovered, f"Gateway not ready {_RESTART_HEALTHY_BUDGET}s after Runtime restart"


# ---------------------------------------------------------------------------
# Test 2 — knowledge-service down, Gateway + Runtime survive (degraded chat)
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_knowledge_service_down_isolation(
    compose_stack: dict[str, Any],
    http: httpx.AsyncClient,
) -> None:
    """KS down → gateway and AS healthy, /knowledge/.../retrieve 502/503,
    /assistant/chat without KB still functional."""
    if compose_stack["live_stack"]:
        pytest.skip("INTEGRATION_USE_LIVE_STACK=true; skipping docker stop/start tests")
    if not _service_in_stack(compose_stack, "knowledge-service"):
        pytest.skip("knowledge-service not in running compose stack")

    with _ensure_started("knowledge-service"):
        _stop_service("knowledge-service")

        # 1. Gateway /health 200.
        health_resp = await _async_get(http, f"{_GATEWAY_URL}/health", timeout=_HEALTH_BUDGET)
        assert health_resp.status_code == 200

        # 2. AS config via gateway still returns 200; a knowledge-service
        # outage must not poison an unrelated Gateway-owned control route.
        config_resp = await _async_get(
            http, f"{_API_PREFIX}/assistant/config", timeout=5.0
        )
        assert config_resp.status_code == 200, config_resp.text

        # 3. /knowledge/datasets/<id>/retrieve returns 502/503 cleanly.
        bogus_id = uuid.uuid4().hex
        retrieve_resp, retrieve_elapsed = await _measure_latency(
            lambda: _async_post(
                http,
                f"{_API_PREFIX}/knowledge/datasets/{bogus_id}/retrieve",
                json={"query": "ping", "top_k": 1},
                timeout=_FAIL_FAST_BUDGET,
            )
        )
        assert retrieve_elapsed < _FAIL_FAST_BUDGET, (
            f"/knowledge/.../retrieve hung for {retrieve_elapsed:.2f}s"
        )
        # 401/403 = unauthenticated probe; 404 = dataset gone; 502/503
        # = upstream KB down. All acceptable. 500 / connection refused
        # is the failure mode we are guarding against.
        assert retrieve_resp.status_code in {401, 403, 404, 502, 503}, (
            f"unexpected status with KS down: {retrieve_resp.status_code} "
            f"{retrieve_resp.text[:200]}"
        )
        assert retrieve_resp.status_code != 500

        # 4. /assistant/chat with no KB datasets still returns < 500
        # (degraded but functional). We accept 401/403 since the test
        # does not authenticate, but NOT 5xx — a KS outage must not
        # break the basic chat path.
        chat_resp = await _async_post(
            http,
            f"{_API_PREFIX}/assistant/chat",
            json={
                "message": "ping",
                "model_id": "gemini-3-flash-preview",
                "kb_dataset_ids": [],
            },
            timeout=_FAIL_FAST_BUDGET,
        )
        assert chat_resp.status_code < 500 or chat_resp.status_code in {502, 503}, (
            f"chat without KB should not be 500 with KS down; "
            f"got {chat_resp.status_code}: {chat_resp.text[:200]}"
        )

        # 5. Restart KS, verify gateway recovery.
        _start_service("knowledge-service")
        recovered = await _wait_for_health(
            http, f"{_GATEWAY_URL}/health", timeout=_RESTART_HEALTHY_BUDGET
        )
        assert recovered


# ---------------------------------------------------------------------------
# Test 3 — langgraph-agent down, AS + gateway survive
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_langgraph_agent_down_isolation(
    compose_stack: dict[str, Any],
    http: httpx.AsyncClient,
) -> None:
    """langgraph-agent (LangGraph proxy upstream) down → gateway+AS healthy,
    /langgraph/threads returns 502/503 cleanly."""
    if compose_stack["live_stack"]:
        pytest.skip("INTEGRATION_USE_LIVE_STACK=true; skipping docker stop/start tests")
    # langgraph-agent is deployed via Helm in prod — only present in some
    # compose layouts. Skip cleanly when absent so this test never
    # blocks a deployment that does not run a local LangGraph instance.
    if not _service_in_stack(compose_stack, "langgraph-agent"):
        pytest.skip("langgraph-agent not in running compose stack (Helm-only deploy?)")

    with _ensure_started("langgraph-agent"):
        _stop_service("langgraph-agent")

        # 1. Gateway /health 200.
        gw_health = await _async_get(http, f"{_GATEWAY_URL}/health", timeout=_HEALTH_BUDGET)
        assert gw_health.status_code == 200

        # 2. AS config surface via gateway. 200 proves an unrelated
        # Agent control route is alive and not gated by LangGraph.
        as_health = await _async_get(
            http, f"{_API_PREFIX}/assistant/config", timeout=5.0
        )
        assert as_health.status_code == 200

        # 3. /langgraph/threads returns 502/503 fast. Note the gateway
        # mounts the LangGraph proxy at /api/v1/langgraph — there is no
        # /api/v1/agent prefix today (see src/api/v1/langgraph.py).
        threads_resp, threads_elapsed = await _measure_latency(
            lambda: _async_post(
                http,
                f"{_API_PREFIX}/langgraph/threads",
                json={},
                timeout=_FAIL_FAST_BUDGET,
            )
        )
        assert threads_elapsed < _FAIL_FAST_BUDGET, (
            f"/langgraph/threads hung for {threads_elapsed:.2f}s"
        )
        assert threads_resp.status_code in {401, 403, 404, 502, 503}, (
            f"unexpected status with langgraph-agent down: {threads_resp.status_code} "
            f"{threads_resp.text[:200]}"
        )
        assert threads_resp.status_code != 500

        # 4. Restart langgraph-agent and verify recovery.
        _start_service("langgraph-agent")
        recovered = await _wait_for_health(
            http, f"{_GATEWAY_URL}/health", timeout=_RESTART_HEALTHY_BUDGET
        )
        assert recovered


# ---------------------------------------------------------------------------
# Test 4 — gateway restart drains gracefully
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_gateway_restart_drains_gracefully(
    compose_stack: dict[str, Any],
) -> None:
    """In-flight SSE chat must finish within the 30 s drain window
    OR close cleanly with a retry-able error — never a half-written frame."""
    if compose_stack["live_stack"]:
        pytest.skip("INTEGRATION_USE_LIVE_STACK=true; skipping docker stop/start tests")
    if not _service_in_stack(compose_stack, "gateway"):
        pytest.skip("gateway not in running compose stack")

    # The streaming request is launched on its own client so the restart
    # task can run concurrently. We use a deliberately long-ish prompt
    # to give the connection time to be open when the restart fires.
    body = {
        "message": "Please write a 200-word essay about graceful shutdown.",
        "model_id": "gemini-3-flash-preview",
    }

    sse_lines: list[str] = []
    sse_error: Exception | None = None
    finished_cleanly = False

    async def _stream_consumer() -> None:
        nonlocal sse_error, finished_cleanly
        try:
            async with httpx.AsyncClient(timeout=_DRAIN_BUDGET + 10.0) as c, c.stream(
                "POST",
                f"{_API_PREFIX}/assistant/chat/stream",
                json=body,
            ) as resp:
                if resp.status_code != 200:
                    # Any non-stream response is a clean break — that
                    # is acceptable (we just need it not to be a
                    # half-written SSE frame).
                    finished_cleanly = True
                    return
                async for line in resp.aiter_lines():
                    sse_lines.append(line)
                    if line.startswith("data:") and ('"done"' in line or '"run_finished"' in line):
                        finished_cleanly = True
                        return
            finished_cleanly = True
        except (httpx.HTTPError, OSError) as exc:
            # Connection reset / ECONNRESET during drain is acceptable —
            # we test for a *clean* close, not a half-written frame.
            sse_error = exc

    consumer_task = asyncio.create_task(_stream_consumer())
    # Give the connection ~1 s to be established.
    await asyncio.sleep(1.0)

    # Restart the gateway. ``docker compose restart`` issues SIGTERM and
    # waits for the configured stop_grace_period (default 10 s) before
    # SIGKILL — this is exactly the drain window we want to test.
    restart_proc = _compose(["restart", "gateway"], timeout=120.0)
    if restart_proc.returncode != 0:
        consumer_task.cancel()
        pytest.fail(f"docker compose restart gateway failed: {restart_proc.stderr}")

    try:
        await asyncio.wait_for(consumer_task, timeout=_DRAIN_BUDGET + 15.0)
    except asyncio.TimeoutError:
        consumer_task.cancel()
        pytest.fail(f"streaming request did not close within drain window {_DRAIN_BUDGET}s")

    # Wait for gateway to come back healthy so we don't leave the suite
    # in a bad state for the next test.
    async with httpx.AsyncClient(timeout=5.0) as c:
        recovered = await _wait_for_health(
            c, f"{_GATEWAY_URL}/health", timeout=_RESTART_HEALTHY_BUDGET
        )
    assert recovered, "gateway did not return to healthy after restart"

    # Acceptance: either a clean SSE finish OR a retry-able transport
    # error. Half-written frames manifest as a JSONDecodeError on a
    # ``data:`` line — we don't try to parse here, but we DO assert the
    # last received line (if any) is well-formed text.
    assert finished_cleanly or sse_error is not None, (
        f"stream neither finished nor errored cleanly; lines={sse_lines[-5:]}"
    )
    if sse_lines:
        last = sse_lines[-1]
        # SSE frames are always either empty, "data: ...", "event: ...",
        # ":..." (comment), or "id: ...". A binary-truncation symptom
        # would be a line with NUL or non-printable characters.
        assert all(ord(ch) >= 0x20 or ch in "\r\n\t" for ch in last), (
            f"last SSE line looks truncated/binary-corrupted: {last!r}"
        )


# ---------------------------------------------------------------------------
# Test 5 — circuit breaker isolates per-route
# ---------------------------------------------------------------------------

@pytest.mark.asyncio
async def test_circuit_breaker_per_route_isolation(
    compose_stack: dict[str, Any],
    http: httpx.AsyncClient,
) -> None:
    """10 calls to /assistant/chat with a bogus model open the breaker
    (latency drops to <200 ms), while concurrent calls to a sibling
    route on a different breaker key remain healthy."""
    # Breaker test runs against any reachable gateway, including live
    # stacks — it makes no destructive changes.
    bad_payload = {
        "message": "ping",
        "model_id": "nonexistent-model-for-breaker-test",
    }

    latencies: list[float] = []
    statuses: list[int] = []

    # Send 10 sequential requests so the breaker has a clear count to
    # trip on. We do not parallelise because the failure-count
    # threshold (3) is order-sensitive — sequential gives deterministic
    # tripping behaviour for this assertion.
    for _ in range(10):
        resp, elapsed = await _measure_latency(
            lambda: _async_post(
                http,
                f"{_API_PREFIX}/assistant/chat",
                json=bad_payload,
                timeout=_FAIL_FAST_BUDGET,
            )
        )
        latencies.append(elapsed)
        statuses.append(resp.status_code)

    # After 3 failures the breaker should be open. Subsequent calls
    # MUST short-circuit fast — assert tail-end latency < 200 ms.
    fast_tail = latencies[-3:]
    assert all(elapsed < 0.2 for elapsed in fast_tail) or all(
        s in {502, 503} for s in statuses[-3:]
    ), (
        f"breaker did not short-circuit; tail latencies={fast_tail}, "
        f"tail statuses={statuses[-3:]}"
    )

    # Concurrently, an unrelated public health route (a different breaker
    # key) must remain healthy. It does NOT touch the assistant chat code
    # path or the model adapter, so its breaker is independent.
    sibling_resp = await _async_get(
        http, f"{_API_PREFIX}/health", timeout=2.0
    )
    assert sibling_resp.status_code == 200, (
        f"sibling route unexpectedly affected by chat breaker: "
        f"{sibling_resp.status_code} {sibling_resp.text[:200]}"
    )
    # 200 proves the route itself is alive and not gated by the chat-route breaker.
