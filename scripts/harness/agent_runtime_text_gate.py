#!/usr/bin/env python3
"""Real Qwen Responses gate for the isolated Agent candidate Runtime.

The gate starts disposable PostgreSQL and Runtime containers, hosts only the
Gateway's private model plane on loopback, and executes simple, long, and
restart-resumed multi-turn scenarios without printing or persisting credentials.
"""

from __future__ import annotations

import argparse
import asyncio
import contextlib
import json
import os
import secrets
import socket
import subprocess
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import asyncpg
import httpx
import uvicorn
from ai_gateway_contracts.agent_runtime_lease import RuntimeModelLeaseSigner
from ai_gateway_core.config.endpoints import (
    DASHSCOPE_DEFAULT_CHAT_BASE_URL,
    normalize_dashscope_base,
)
from ai_gateway_core.models import get_builtin_model_capabilities
from dotenv import dotenv_values
from fastapi import FastAPI

from src.api.internal.agent_model_plane import router as model_plane_router
from src.services.agent_runtime import (
    AgentModelPlane,
    AgentModelPlaneError,
    AgentRuntimeControlPlane,
)
from src.services.assistant_entry.launch_resolution import resolve_agent_launch

ROOT = Path(__file__).resolve().parents[2]
KERNEL_REVISION = "agent-runtime-text-gate"
PROVIDER_REVISION = "qwen-responses-live-v1"


class GateError(RuntimeError):
    """A stable, secret-free live-gate failure."""


def _docker(arguments: list[str], *, capture: bool = False) -> str:
    result = subprocess.run(
        ["docker", *arguments],
        check=False,
        text=True,
        stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        operation = arguments[0] if arguments else "command"
        raise GateError(f"isolated Docker {operation} failed")
    return (result.stdout or "").strip()


def _free_port() -> int:
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return int(listener.getsockname()[1])


def _runtime_image_is_valid(image: str) -> bool:
    try:
        artifact = _docker(
            [
                "image",
                "inspect",
                image,
                "--format",
                '{{index .Config.Labels "com.misaya.ai-platform.agent-runtime.artifact"}}',
            ],
            capture=True,
        )
        binary = _docker(
            [
                "image",
                "inspect",
                image,
                "--format",
                '{{index .Config.Labels "com.misaya.ai-platform.agent-runtime.binary"}}',
            ],
            capture=True,
        )
    except GateError:
        return False
    return artifact == "agent_runtime" and binary == "ai-platform-agent-runtime"


def _runtime_memory_bytes(container: str) -> int:
    value = _docker(
        ["exec", container, "sh", "-c", "cat /sys/fs/cgroup/memory.current"],
        capture=True,
    )
    try:
        return int(value)
    except ValueError as exc:
        raise GateError("Runtime returned an invalid cgroup memory value") from exc


@dataclass(frozen=True, slots=True)
class GateConfig:
    runtime_image: str
    provider_api_key: str
    provider_base_url: str
    ttft_limit_seconds: float
    long_output_min_chars: int

    @classmethod
    def load(cls, *, env_file: Path, runtime_image: str) -> GateConfig:
        file_values = dotenv_values(env_file) if env_file.is_file() else {}

        def value(name: str) -> str:
            return str(os.environ.get(name) or file_values.get(name) or "").strip()

        provider_api_key = value("DASHSCOPE_CHAT_API_KEY") or value("DASHSCOPE_API_KEY")
        if not provider_api_key:
            raise GateError("a local DashScope/Qwen credential is required")
        raw_base_url = value("DASHSCOPE_CHAT_BASE_URL") or value("DASHSCOPE_BASE_URL")
        provider_base_url = (
            normalize_dashscope_base(raw_base_url, "chat")
            if raw_base_url
            else DASHSCOPE_DEFAULT_CHAT_BASE_URL
        )
        if not _runtime_image_is_valid(runtime_image):
            raise GateError("AI_PLATFORM_AGENT_RUNTIME_IMAGE is not a locked Agent Runtime artifact")
        try:
            ttft_limit = float(value("AI_PLATFORM_AGENT_RUNTIME_TEXT_TTFT_LIMIT_SECONDS") or "8")
            long_min_chars = int(value("AI_PLATFORM_AGENT_RUNTIME_LONG_OUTPUT_MIN_CHARS") or "700")
        except ValueError as exc:
            raise GateError("live-gate numeric configuration is invalid") from exc
        if ttft_limit <= 0 or not 200 <= long_min_chars <= 5000:
            raise GateError("live-gate limits are outside their safe range")
        return cls(
            runtime_image=runtime_image,
            provider_api_key=provider_api_key,
            provider_base_url=provider_base_url,
            ttft_limit_seconds=ttft_limit,
            long_output_min_chars=long_min_chars,
        )


class _Database:
    def __init__(self, pool: asyncpg.Pool) -> None:
        self.pool = pool

    async def fetchrow(self, query: str, *args: Any):
        row = await self.pool.fetchrow(query, *args)
        if row is not None and "snapshot" in row and isinstance(row["snapshot"], str):
            value = dict(row)
            value["snapshot"] = json.loads(value["snapshot"])
            return value
        return row

    async def execute(self, query: str, *args: Any):
        return await self.pool.execute(query, *args)


class _ModelService:
    async def get_model(self, tenant_id: str, model_id: str) -> dict[str, Any]:
        if not tenant_id or model_id != "qwen3.7-plus":
            raise GateError("unexpected model lookup scope")
        profile = get_builtin_model_capabilities("dashscope", model_id)
        if profile is None or profile.get("wire_protocols", {}).get("preferred") != "responses_v1":
            raise GateError("Qwen capability profile is not Responses-first")
        return {
            "model_id": model_id,
            "provider_id": "dashscope",
            "is_enabled": True,
            "effective_capabilities": profile,
            "capability_revision": 1,
            "context_window": 1_000_000,
            "max_output_tokens": 65_536,
            "input_price_per_1k": 0.001,
            "output_price_per_1k": 0.002,
        }


class _ProviderService:
    def __init__(self, config: GateConfig) -> None:
        self.config = config

    async def get_provider(self, tenant_id: str, provider_id: str) -> dict[str, Any]:
        if not tenant_id or provider_id != "dashscope":
            raise GateError("unexpected provider lookup scope")
        return {
            "provider_id": provider_id,
            "api_type": "openai",
            "is_enabled": True,
            "updated_at": PROVIDER_REVISION,
        }

    async def get_runtime_provider_config(
        self,
        tenant_id: str,
        provider_id: str,
    ) -> dict[str, Any]:
        if not tenant_id or provider_id != "dashscope":
            raise GateError("unexpected runtime provider lookup scope")
        return {
            "updated_at": PROVIDER_REVISION,
            "api_key": self.config.provider_api_key,
            "runtime_base_url": self.config.provider_base_url,
        }


class _AssignmentStore:
    async def resolve(self, **scope: str) -> SimpleNamespace:
        if not all(scope.get(key) for key in ("tenant_id", "user_id", "session_id")):
            raise GateError("candidate assignment scope is incomplete")
        return SimpleNamespace(
            runtime_owner="agent_runtime",
            kernel_revision=KERNEL_REVISION,
        )


class _ObservedModelPlane(AgentModelPlane):
    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.authorization_calls = 0
        self.last_authorization_error: str | None = None
        self.response_event_types: list[str] = []
        self.first_provider_visible_seconds: float | None = None

    async def authorize_and_reserve(
        self,
        *,
        body: dict[str, Any],
        turn_metadata: dict[str, Any],
    ):
        self.authorization_calls += 1
        try:
            return await super().authorize_and_reserve(
                body=body,
                turn_metadata=turn_metadata,
            )
        except AgentModelPlaneError as exc:
            self.last_authorization_error = exc.code
            raise

    async def stream(self, **kwargs: Any):
        started_at = time.perf_counter()
        async for chunk in super().stream(**kwargs):
            for line in chunk.decode("utf-8", errors="replace").splitlines():
                if not line.startswith("event:"):
                    continue
                event_type = line.removeprefix("event:").strip()
                self.response_event_types.append(event_type)
                if (
                    self.first_provider_visible_seconds is None
                    and event_type
                    in {
                        "response.reasoning_summary_text.delta",
                        "response.reasoning_text.delta",
                        "response.output_text.delta",
                    }
                ):
                    self.first_provider_visible_seconds = time.perf_counter() - started_at
            yield chunk


@dataclass(frozen=True, slots=True)
class TurnReceipt:
    label: str
    run_id: str
    text: str
    first_visible_seconds: float
    event_types: tuple[str, ...]
    model_calls: int
    input_tokens: int
    output_tokens: int


class LiveTextGate:
    def __init__(self, config: GateConfig) -> None:
        suffix = uuid.uuid4().hex[:12]
        self.config = config
        self.network = f"agent-runtime-text-gate-{suffix}"
        self.postgres = f"agent-runtime-text-gate-pg-{suffix}"
        self.runtime = f"agent-runtime-text-gate-runtime-{suffix}"
        self.postgres_password = secrets.token_hex(24)
        self.internal_token = secrets.token_hex(32)
        self.model_plane_token = secrets.token_hex(32)
        self.lease_secret = secrets.token_hex(32)
        self.tenant_id = f"tenant-text-gate-{suffix}"
        self.user_id = f"user-text-gate-{suffix}"
        self.session_id = f"session-text-gate-{suffix}"
        self.postgres_port = _free_port()
        self.runtime_port = _free_port()
        self.model_plane_port = _free_port()
        self.pool: asyncpg.Pool | None = None
        self.database: _Database | None = None
        self.model_plane: _ObservedModelPlane | None = None
        self.control: AgentRuntimeControlPlane | None = None
        self.server: uvicorn.Server | None = None
        self.server_task: asyncio.Task[None] | None = None
        self.thread_id: str | None = None

    async def __aenter__(self) -> LiveTextGate:
        try:
            await self._start()
        except BaseException:
            await self.close()
            raise
        return self

    async def __aexit__(self, *_exc: object) -> None:
        await self.close()

    async def _start(self) -> None:
        _docker(["network", "create", self.network])
        _docker(
            [
                "run",
                "--detach",
                "--rm",
                "--name",
                self.postgres,
                "--network",
                self.network,
                "--publish",
                f"127.0.0.1:{self.postgres_port}:5432",
                "--env",
                "POSTGRES_USER=runtime_text_gate",
                "--env",
                f"POSTGRES_PASSWORD={self.postgres_password}",
                "--env",
                "POSTGRES_DB=runtime_text_gate",
                "postgres:16-alpine",
            ]
        )
        await self._wait_for_postgres()
        self.pool = await asyncpg.create_pool(
            host="127.0.0.1",
            port=self.postgres_port,
            user="runtime_text_gate",
            password=self.postgres_password,
            database="runtime_text_gate",
            min_size=1,
            max_size=8,
        )
        self.database = _Database(self.pool)
        await self._initialize_database()
        await self._start_model_plane()
        self._start_runtime_container()
        await self._wait_for_runtime()
        await self._verify_runtime_can_reach_model_plane()
        self.control = AgentRuntimeControlPlane(
            database=self.database,
            model_service=_ModelService(),
            provider_service=_ProviderService(self.config),
            assignment_store=_AssignmentStore(),
            lease_signer=RuntimeModelLeaseSigner(self.lease_secret),
            runtime_url=f"http://127.0.0.1:{self.runtime_port}",
            runtime_internal_token=self.internal_token,
            model_plane_base_url=(
                f"http://host.docker.internal:{self.model_plane_port}/internal/v1/agent-model-plane"
            ),
            kernel_revision=KERNEL_REVISION,
        )

    async def _wait_for_postgres(self) -> None:
        for _ in range(80):
            result = subprocess.run(
                [
                    "docker",
                    "exec",
                    self.postgres,
                    "pg_isready",
                    "-U",
                    "runtime_text_gate",
                    "-d",
                    "runtime_text_gate",
                ],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
            if result.returncode == 0:
                try:
                    connection = await asyncpg.connect(
                        host="127.0.0.1",
                        port=self.postgres_port,
                        user="runtime_text_gate",
                        password=self.postgres_password,
                        database="runtime_text_gate",
                        timeout=1,
                    )
                except (OSError, ConnectionError, asyncpg.PostgresError):
                    pass
                else:
                    await connection.close()
                    return
            await asyncio.sleep(0.25)
        raise GateError("isolated PostgreSQL readiness timed out")

    async def _initialize_database(self) -> None:
        assert self.pool is not None
        async with self.pool.acquire() as connection:
            for path in (
                ROOT / "deploy/agent-runtime-source/runtime-smoke-base.sql",
                ROOT / "database/migrations/089_agent_runtime_thread_store.sql",
                ROOT / "database/migrations/090_agent_runtime_model_leases.sql",
                ROOT / "database/migrations/092_agent_runtime_legacy_import.sql",
            ):
                await connection.execute(path.read_text(encoding="utf-8"))
            await connection.execute(
                """
                INSERT INTO sessions (session_id, service_id, user_id, tenant_id)
                VALUES ($1, '__builtin_assistant__', $2, $3)
                """,
                self.session_id,
                self.user_id,
                self.tenant_id,
            )
            await connection.execute(
                """
                INSERT INTO assistant_session_runtime_assignments (
                    tenant_id, user_id, session_id, runtime_owner,
                    kernel_revision, assignment_reason
                ) VALUES ($3, $2, $1, 'agent_runtime', $4, 'live_gate')
                """,
                self.session_id,
                self.user_id,
                self.tenant_id,
                KERNEL_REVISION,
            )

    async def _start_model_plane(self) -> None:
        assert self.database is not None
        self.model_plane = _ObservedModelPlane(
            database=self.database,
            provider_service=_ProviderService(self.config),
            lease_signer=RuntimeModelLeaseSigner(self.lease_secret),
        )
        app = FastAPI()
        app.include_router(model_plane_router)
        app.add_api_route("/healthz", lambda: {"status": "ok"}, include_in_schema=False)
        app.state.agent_model_plane_internal_token = self.model_plane_token
        app.state.agent_model_plane = self.model_plane
        self.server = uvicorn.Server(
            uvicorn.Config(
                app,
                host="0.0.0.0",
                port=self.model_plane_port,
                log_level="error",
                access_log=False,
            )
        )
        self.server_task = asyncio.create_task(self.server.serve())
        for _ in range(80):
            if self.server.started:
                return
            if self.server_task.done():
                raise GateError("private model plane exited before readiness")
            await asyncio.sleep(0.05)
        raise GateError("private model plane readiness timed out")

    def _start_runtime_container(self) -> None:
        _docker(
            [
                "run",
                "--detach",
                "--name",
                self.runtime,
                "--network",
                self.network,
                "--publish",
                f"127.0.0.1:{self.runtime_port}:8094",
                "--memory",
                "256m",
                "--pids-limit",
                "256",
                "--tmpfs",
                "/var/lib/ai-platform-agent-runtime/runtime-home:rw,uid=10001,gid=10001,mode=0700",
                "--env",
                f"AI_PLATFORM_INTERNAL_TOKEN={self.internal_token}",
                "--env",
                "AI_PLATFORM_RUNTIME_BIND=0.0.0.0:8094",
                "--env",
                "AI_PLATFORM_RUNTIME_WORKDIR=/workspace",
                "--env",
                "AI_PLATFORM_AGENT_HOME=/var/lib/ai-platform-agent-runtime/runtime-home",
                "--env",
                f"AI_PLATFORM_AGENT_RUNTIME_MODEL_PLANE_INTERNAL_TOKEN={self.model_plane_token}",
                "--env",
                f"POSTGRES_HOST={self.postgres}",
                "--env",
                "POSTGRES_PORT=5432",
                "--env",
                "POSTGRES_USER=runtime_text_gate",
                "--env",
                f"POSTGRES_PASSWORD={self.postgres_password}",
                "--env",
                "POSTGRES_DB=runtime_text_gate",
                self.config.runtime_image,
            ]
        )

    async def _wait_for_runtime(self) -> None:
        url = f"http://127.0.0.1:{self.runtime_port}/health/ready"
        async with httpx.AsyncClient(timeout=1.0, trust_env=False) as client:
            for _ in range(120):
                with contextlib.suppress(httpx.HTTPError):
                    response = await client.get(url)
                    if response.status_code == 200:
                        return
                running = _docker(
                    ["inspect", self.runtime, "--format", "{{.State.Running}}"],
                    capture=True,
                )
                if running != "true":
                    raise GateError("Runtime container exited before readiness")
                await asyncio.sleep(0.25)
        raise GateError("Runtime readiness timed out")

    async def _verify_runtime_can_reach_model_plane(self) -> None:
        health_url = f"http://host.docker.internal:{self.model_plane_port}/healthz"
        last_error = ""
        for _ in range(20):
            result = await asyncio.to_thread(
                subprocess.run,
                [
                    "docker",
                    "exec",
                    self.runtime,
                    "curl",
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "2",
                    health_url,
                ],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.PIPE,
                text=True,
            )
            if result.returncode == 0:
                return
            last_error = (result.stderr or "").strip().replace(health_url, "[model-plane]")
            await asyncio.sleep(0.25)
        raise GateError(
            "Runtime cannot reach the private model plane"
            + (f" ({last_error[-160:]})" if last_error else "")
        )

    async def restart_and_resume(self) -> None:
        if not self.thread_id:
            raise GateError("cannot resume before a Thread exists")
        _docker(["rm", "--force", self.runtime])
        self._start_runtime_container()
        await self._wait_for_runtime()
        await self._verify_runtime_can_reach_model_plane()
        headers = {
            "x-ai-platform-internal-token": self.internal_token,
            "x-ai-tenant-id": self.tenant_id,
            "x-ai-user-id": self.user_id,
            "x-ai-session-id": self.session_id,
        }
        async with httpx.AsyncClient(timeout=30.0, trust_env=False) as client:
            response = await client.post(
                f"http://127.0.0.1:{self.runtime_port}/internal/v1/threads/{self.thread_id}/resume",
                headers=headers,
                json={
                    "model": "qwen3.7-plus",
                    "modelPlaneBaseUrl": (
                        f"http://host.docker.internal:{self.model_plane_port}"
                "/internal/v1/agent-model-plane"
                    ),
                },
            )
        if response.status_code != 200:
            raise GateError("Runtime failed to resume the persisted Thread")
        payload = response.json()
        if str((payload.get("thread") or {}).get("id") or "") != self.thread_id:
            raise GateError("Runtime resumed a different Thread")

    async def run_turn(
        self,
        *,
        label: str,
        message: str,
        max_tokens: int,
    ) -> TurnReceipt:
        assert self.control is not None
        assert self.pool is not None
        started_at = time.perf_counter()
        launch = await resolve_agent_launch(
            entrypoint="assistant",
            tenant_id=self.tenant_id,
            user_id=self.user_id,
            session_id=self.session_id,
            model_id="qwen3.7-plus",
            model_service=self.control.model_service,
            reasoning_option="auto",
            max_tokens=max_tokens,
            enable_dynamic_tools=False,
        )
        turn = await self.control.start_turn(
            tenant_id=self.tenant_id,
            user_id=self.user_id,
            session_id=self.session_id,
            message=message,
            model_id="qwen3.7-plus",
            reasoning_option="auto",
            legacy_thinking_level=None,
            max_tokens=max_tokens,
            resolved_agent_launch=launch,
        )
        self.thread_id = turn.runtime_thread_id
        first_visible: float | None = None
        text_parts: list[str] = []
        event_types: list[str] = []
        terminal_count = 0
        terminal_status: str | None = None
        terminal_code = ""
        sequences: list[int] = []
        async for frame in self.control.stream_events(
            turn=turn,
            tenant_id=self.tenant_id,
            user_id=self.user_id,
            session_id=self.session_id,
        ):
            for line in frame.decode("utf-8", errors="strict").splitlines():
                if not line.startswith("data:"):
                    continue
                envelope = json.loads(line[5:].strip())
                if str((envelope.get("data") or {}).get("run_id") or "") != turn.run_id:
                    continue
                event_type = str(envelope.get("event_type") or "")
                event_types.append(event_type)
                sequence = envelope.get("sequence")
                if isinstance(sequence, int) and not isinstance(sequence, bool):
                    sequences.append(sequence)
                data = envelope.get("data") or {}
                if event_type in {"thinking_delta", "text_delta"} and first_visible is None:
                    first_visible = time.perf_counter() - started_at
                if event_type == "text_delta" and isinstance(data.get("content"), str):
                    text_parts.append(data["content"])
                if event_type in {"run_finished", "run_error"}:
                    terminal_count += 1
                    terminal_status = str(data.get("status") or "failed")
                    terminal_code = str(data.get("code") or data.get("error_code") or "")
        if terminal_count != 1 or "run_started" not in event_types:
            raise GateError(f"{label} violated the one-start/one-terminal contract")
        if sequences != sorted(set(sequences)):
            raise GateError(f"{label} produced duplicate or out-of-order sequences")
        text = "".join(text_parts).strip()
        async with self.pool.acquire() as connection:
            accounting = await connection.fetchrow(
                """
                SELECT COUNT(*)::int AS model_calls,
                       COUNT(*) FILTER (WHERE status = 'completed')::int AS completed_calls,
                       COALESCE(SUM(input_tokens), 0)::bigint AS input_tokens,
                       COALESCE(SUM(output_tokens), 0)::bigint AS output_tokens,
                       COUNT(*) FILTER (WHERE status IN ('failed', 'unknown'))::int AS bad_calls
                  FROM assistant_runtime_model_calls
                 WHERE run_id = $1
                """,
                uuid.UUID(turn.run_id),
            )
            run = await connection.fetchrow(
                "SELECT status, engine FROM assistant_runs WHERE run_id = $1",
                uuid.UUID(turn.run_id),
            )
            call_errors = await connection.fetch(
                """
                SELECT status, COALESCE(error_code, '') AS error_code
                  FROM assistant_runtime_model_calls
                 WHERE run_id = $1
                 ORDER BY reserved_at
                """,
                uuid.UUID(turn.run_id),
            )
        if terminal_status != "succeeded":
            safe_errors = (
                ",".join(f"{row['status']}:{row['error_code']}" for row in call_errors)
                or "no_model_call"
            )
            if self.model_plane is not None:
                plane_status = (
                    self.model_plane.last_authorization_error
                    or f"authorize_calls={self.model_plane.authorization_calls}"
                )
                safe_errors = f"{safe_errors};model_plane={plane_status}"
            safe_errors += (
                f";events={','.join(dict.fromkeys(event_types)) or 'none'}"
                f";terminal_code={terminal_code or 'none'}"
            )
            raise GateError(f"{label} failed before closure ({safe_errors})")
        if first_visible is None:
            raise GateError(
                f"{label} emitted no visible reasoning or text "
                f"(events={','.join(event_types) or 'none'})"
            )
        if first_visible > self.config.ttft_limit_seconds:
            provider_events = (
                ",".join(self.model_plane.response_event_types[-20:])
                if self.model_plane is not None
                else "unavailable"
            )
            raise GateError(
                f"{label} exceeded the configured first-visible-token limit "
                f"({first_visible:.3f}s > {self.config.ttft_limit_seconds:.3f}s; "
                f"events={','.join(event_types)}; provider_events={provider_events})"
            )
        if not text:
            raise GateError(f"{label} completed without text")
        if (
            accounting is None
            or accounting["model_calls"] != 1
            or accounting["completed_calls"] != 1
            or accounting["bad_calls"] != 0
            or run is None
            or run["status"] != "succeeded"
            or run["engine"] != "agent_runtime"
        ):
            raise GateError(f"{label} did not close one Agent-only billing path")
        return TurnReceipt(
            label=label,
            run_id=turn.run_id,
            text=text,
            first_visible_seconds=first_visible,
            event_types=tuple(dict.fromkeys(event_types)),
            model_calls=int(accounting["model_calls"]),
            input_tokens=int(accounting["input_tokens"]),
            output_tokens=int(accounting["output_tokens"]),
        )

    async def close(self) -> None:
        if self.control is not None:
            with contextlib.suppress(Exception):
                await self.control.close()
            self.control = None
        if self.model_plane is not None:
            with contextlib.suppress(Exception):
                await self.model_plane.close()
            self.model_plane = None
        if self.server is not None:
            self.server.should_exit = True
        if self.server_task is not None:
            with contextlib.suppress(Exception):
                await asyncio.wait_for(self.server_task, timeout=10.0)
            self.server_task = None
        if self.pool is not None:
            with contextlib.suppress(Exception):
                await self.pool.close()
            self.pool = None
        for container in (self.runtime, self.postgres):
            subprocess.run(
                ["docker", "rm", "--force", container],
                check=False,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
            )
        subprocess.run(
            ["docker", "network", "rm", self.network],
            check=False,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )


async def _run(config: GateConfig) -> None:
    codeword = f"AZURE-{secrets.token_hex(4).upper()}"
    async with LiveTextGate(config) as gate:
        simple = await gate.run_turn(
            label="simple",
            message="只回复 EXACT_200，不要调用工具，也不要添加其他内容。",
            max_tokens=256,
        )
        if simple.text != "EXACT_200":
            raise GateError("simple turn did not preserve exact-output semantics")

        long_turn = await gate.run_turn(
            label="long",
            message=(
                "请直接用中文写一篇约800字的说明，向有编程基础但不了解深度学习的人解释"
                "Transformer。必须包含注意力、位置编码、训练和推理四部分；不要调用任何工具。"
            ),
            max_tokens=2048,
        )
        visible_chars = len("".join(long_turn.text.split()))
        if visible_chars < config.long_output_min_chars:
            raise GateError("long turn output was shorter than the configured product scenario")

        remembered = await gate.run_turn(
            label="remember",
            message=f"请记住口令 {codeword}，只回复“已记住”，不要调用工具。",
            max_tokens=256,
        )
        if "已记住" not in remembered.text:
            raise GateError("multi-turn setup was not acknowledged")

        await gate.restart_and_resume()
        recalled = await gate.run_turn(
            label="resume",
            message="刚才让我记住的口令是什么？只回复口令本身，不要调用工具。",
            max_tokens=256,
        )
        if codeword not in recalled.text:
            raise GateError("resumed Thread did not preserve multi-turn context")

        receipts = (simple, long_turn, remembered, recalled)
        memory_bytes = _runtime_memory_bytes(gate.runtime)
        print("AI_PLATFORM_AGENT_RUNTIME_TEXT_GATE_OK")
        print("scenarios=" + ",".join(receipt.label for receipt in receipts))
        print(
            "first_visible_seconds="
            + ",".join(
                f"{receipt.label}:{receipt.first_visible_seconds:.3f}" for receipt in receipts
            )
        )
        print("long_output_chars=" + str(visible_chars))
        print("model_calls=" + str(sum(receipt.model_calls for receipt in receipts)))
        print(
            "model_tokens="
            + str(sum(receipt.input_tokens for receipt in receipts))
            + "/"
            + str(sum(receipt.output_tokens for receipt in receipts))
        )
        print("runtime_memory_bytes=" + str(memory_bytes))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--env-file", type=Path, default=ROOT / ".env")
    parser.add_argument("--runtime-image", default=os.environ.get("AI_PLATFORM_AGENT_RUNTIME_IMAGE", ""))
    arguments = parser.parse_args()
    env_file = arguments.env_file.expanduser().resolve()
    runtime_image = str(
        arguments.runtime_image
        or (dotenv_values(env_file).get("AI_PLATFORM_AGENT_RUNTIME_IMAGE") if env_file.is_file() else "")
        or ""
    ).strip()
    if not runtime_image:
        print("ERROR: AI_PLATFORM_AGENT_RUNTIME_IMAGE is required")
        return 2
    try:
        config = GateConfig.load(
            env_file=env_file,
            runtime_image=runtime_image,
        )
        asyncio.run(_run(config))
    except (GateError, OSError, asyncpg.PostgresError, httpx.HTTPError) as exc:
        message = str(exc) if isinstance(exc, GateError) else "live-gate dependency failed"
        print(f"ERROR: {message}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
