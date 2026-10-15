"""Local real-Qwen recovery acceptance with explicitly controlled crash pauses.

Read runtime-and-secrets before running. Uses the existing E2E account at execution
only. Quiz fixtures and safe evidence are retained. No credential or token output.
"""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import time
import uuid
from pathlib import Path

import asyncpg
import httpx
from dotenv import dotenv_values

from scripts.harness.runtime_recovery_observe import observe

STAGES = (
    "invocation_saved",
    "approval_saved",
    "before_dispatch",
    "worker_result_observed",
    "invocation_result_saved",
    "core_output_saved",
)
TERMINAL = ("succeeded", "failed", "cancelled")


async def docker(*args, stdin=None, allow_failure=False):
    process = await asyncio.create_subprocess_exec(
        "docker",
        *args,
        stdin=asyncio.subprocess.PIPE if stdin is not None else None,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
    )
    out, _ = await process.communicate(stdin.encode() if stdin is not None else None)
    if process.returncode and not allow_failure:
        raise RuntimeError("Docker acceptance operation failed")
    return out.decode()


class Gate:
    async def open(self):
        owners = await docker(
            "inspect",
            "ai-gateway-agent-runtime",
            "ai-gateway-agent-capability-worker-1",
            "ai-gateway-backend",
            "--format",
            '{{index .Config.Labels "com.docker.compose.project.working_dir"}}',
        )
        assert owners.splitlines() == [str(Path(__file__).resolve().parents[2])] * 3, (
            "Wrong Compose owner"
        )
        env = dotenv_values(".env")
        self.db_kwargs = {
            "host": "127.0.0.1",
            "port": int(env.get("POSTGRES_PORT") or 5432),
            "user": env["POSTGRES_USER"],
            "password": env["POSTGRES_PASSWORD"],
            "database": env.get("POSTGRES_DB") or "gateway",
        }
        self.db = await asyncpg.connect(**self.db_kwargs)
        self.client = httpx.AsyncClient(
            base_url="http://127.0.0.1:8081", timeout=30, trust_env=False
        )
        credentials = json.loads(Path("web/.playwright/e2e-user.json").read_text())
        response = await self.client.post("/api/v1/auth/login", json=credentials)
        assert response.status_code == 200, "Dedicated local E2E login failed"
        self.client.headers["Authorization"] = "Bearer " + response.json()["access_token"]
        del credentials, response
        self.run = None

    async def post(self, path, body=None):
        response = await self.client.post(path, json=body or {})
        assert response.status_code < 400, (
            f"Acceptance HTTP request rejected ({response.status_code})"
        )
        return response.json()

    async def wait(self, predicate, description, timeout=180):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            result = await predicate()
            if result:
                return result
            await asyncio.sleep(0.1)
        raise AssertionError("Acceptance timeout: " + description)

    async def create(self, title):
        thread = (await self.post("/api/v2/agent/threads", {"model_id": "qwen3.8-flash"}))["thread"]
        self.thread = thread["id"]
        parameters = {
            "title": title,
            "description": "持久恢复验收",
            "difficulty": "easy",
            "questions": [
                {
                    "question_num": 1,
                    "question_type": "mc_single",
                    "question_text": "2+3 等于多少？",
                    "options": [
                        {"label": label, "text": str(value)}
                        for label, value in zip("ABCD", (4, 5, 6, 7), strict=True)
                    ],
                    "correct_answer": ["B"],
                    "explanation": "2加3等于5。",
                }
            ],
        }
        message = (
            "持久恢复验收：只调用 generate_quiz 一次，使用下面精确JSON参数。不得添加其他字段（尤其不得添加 option）。不调用其他工具；审批拒绝、校验失败或结果未知时说明情况，不能再次申请或重试。JSON="
            + json.dumps(parameters, ensure_ascii=False)
        )
        self.run = uuid.UUID(
            (
                await self.post(
                    f"/api/v2/agent/threads/{self.thread}/turns",
                    {"message": message, "model_id": "qwen3.8-flash"},
                )
            )["turn"]["id"]
        )
        print(
            json.dumps({"phase": "original_run_admitted", "run_id": str(self.run), "title": title}),
            flush=True,
        )

    async def approval(self):
        async def pending():
            return await self.db.fetchval(
                "SELECT approval_id FROM assistant.assistant_tool_approvals WHERE run_id=$1 AND status='pending' ORDER BY created_at LIMIT 1",
                self.run,
            )

        return await self.wait(pending, "original pending approval")

    async def decide(self, approval, approved=True):
        return await self.post(
            f"/api/v2/agent/threads/{self.thread}/approvals/{approval}/decision",
            {"approved": approved, "reason": "controlled local recovery acceptance"},
        )

    async def arm(self, stage, title):
        await docker(
            "exec",
            "-i",
            "ai-gateway-agent-runtime",
            "sh",
            "-c",
            'umask 077\ncat > "$AI_PLATFORM_AGENT_HOME/recovery-acceptance-fault.json"',
            stdin=json.dumps({"stage": stage, "quiz_title": title}),
        )

    async def hit(self, stage):
        async def read():
            result = await docker(
                "exec",
                "ai-gateway-agent-runtime",
                "sh",
                "-c",
                'cat "$AI_PLATFORM_AGENT_HOME/recovery-acceptance-hit.json"',
                allow_failure=True,
            )
            try:
                value = json.loads(result)
                return (
                    value
                    if value.get("stage") == stage and value.get("run_id") == str(self.run)
                    else None
                )
            except ValueError:
                return None

        return await self.wait(read, "controlled pause " + stage)

    async def identity(self):
        row = await self.db.fetchrow(
            "SELECT o.runtime_thread_id,o.fence,l.lease_id,l.snapshot_id,l.model_id,s.snapshot::text AS snapshot,o.context::text AS context FROM assistant.assistant_runtime_execution_owners o JOIN assistant.assistant_runtime_model_leases l USING(run_id) JOIN assistant.assistant_runtime_snapshots s USING(snapshot_id) WHERE o.run_id=$1",
            self.run,
        )
        result = {
            key: str(row[key])
            for key in ("runtime_thread_id", "lease_id", "snapshot_id", "model_id")
        }
        result.update(
            snapshot_hash=hashlib.sha256(row["snapshot"].encode()).hexdigest(),
            context_hash=hashlib.sha256(row["context"].encode()).hexdigest(),
        )
        return result

    async def facts(self, output):
        await observe(self.run, output)
        return json.loads(output.read_text())

    async def restart(self, worker=False):
        # -t 0 is deliberately SIGKILL/crash evidence, not graceful shutdown.
        await docker(
            "compose",
            "restart",
            "-t",
            "0",
            "agent-runtime",
            *(["agent-capability-worker"] if worker else []),
        )

        async def ready():
            result = await docker(
                "exec",
                "ai-gateway-agent-runtime",
                "sh",
                "-c",
                "curl -fsS http://127.0.0.1:8094/health/ready",
                allow_failure=True,
            )
            return bool(result)

        await self.wait(ready, "restarted Runtime readiness", timeout=20)
        print(
            json.dumps(
                {"phase": "controlled_process_crash", "run_id": str(self.run), "worker": worker}
            ),
            flush=True,
        )

    async def recovered(self):
        async def claim():
            return await self.db.fetchval(
                "SELECT fence>=2 FROM assistant.assistant_runtime_execution_owners WHERE run_id=$1",
                self.run,
            )

        await self.wait(claim, "replacement original-run ownership")

    async def terminal(self):
        async def ended():
            status = await self.db.fetchval(
                "SELECT status FROM assistant.assistant_runs WHERE run_id=$1", self.run
            )
            return status if status in TERMINAL else None

        return await self.wait(ended, "original run terminal")

    async def assert_no_extra_run(self):
        assert (
            await self.db.fetchval(
                "SELECT COUNT(*) FROM assistant.assistant_runs WHERE harness_thread_id=$1",
                uuid.UUID(self.thread),
            )
            == 1
        )

    async def duplicate_recover(self):
        for _ in range(2):
            await self.post(f"/api/v2/agent/threads/{self.thread}/turns/{self.run}:recover")
        await self.assert_no_extra_run()

    async def cleanup(self):
        if self.run:
            status = await self.db.fetchval(
                "SELECT status FROM assistant.assistant_runs WHERE run_id=$1", self.run
            )
            if status not in TERMINAL:
                await self.post(f"/api/v2/agent/threads/{self.thread}/turns/{self.run}:interrupt")
        # Remove only this harness's one-shot dev acceptance files, never artifacts.
        await docker(
            "exec",
            "ai-gateway-agent-runtime",
            "sh",
            "-c",
            'rm -f "$AI_PLATFORM_AGENT_HOME/recovery-acceptance-fault.json" "$AI_PLATFORM_AGENT_HOME/recovery-acceptance-hit.json" "$AI_PLATFORM_AGENT_HOME/recovery-acceptance-consumed.json"',
            allow_failure=True,
        )
        await self.client.aclose()
        await self.db.close()


async def scenario(name):
    gate = Gate()
    await gate.open()
    title = "DR-CRASH-" + name + "-" + uuid.uuid4().hex[:8]
    evidence = Path("reports/runtime-recovery/cases") / title
    evidence.mkdir(parents=True, exist_ok=True)
    blocker = None
    try:
        stage = name if name in STAGES else "approval_saved"
        if name != "worker_unknown":
            await gate.arm(stage, title)
        await gate.create(title)
        if stage not in ("invocation_saved", "approval_saved") or name == "worker_unknown":
            approval = await gate.approval()
            if name == "worker_unknown":
                # DB lock deliberately stalls the side effect after dispatch;
                # a Worker crash removes the uncommitted action/receipt.
                blocker = await asyncpg.connect(**gate.db_kwargs)
                await blocker.execute("BEGIN")
                await blocker.execute("LOCK TABLE assistant.quizzes IN SHARE MODE")
            await gate.decide(approval)
        if name != "worker_unknown":
            await gate.hit(stage)
        else:

            async def dispatched_without_receipt():
                return await gate.db.fetchval(
                    "SELECT EXISTS(SELECT 1 FROM assistant.assistant_capability_executions WHERE run_id=$1 AND dispatch_fence IS NOT NULL AND status IN ('dispatched','running') AND result_summary IS NULL)",
                    gate.run,
                )

            await gate.wait(dispatched_without_receipt, "write dispatch before side effect receipt")
        original = await gate.identity()
        before = await gate.facts(evidence / "before-crash.json")
        if name in STAGES or name in ("cancel", "revoke", "reject"):
            assert before["core"]["complete"] == 0
            assert len(before["invocations"]) == 1
            if stage == "invocation_saved":
                assert before["approvals"] == [] and before["executions"] == []
            if stage == "approval_saved":
                assert before["approvals"][0]["status"] == "pending" and before["executions"] == []
            if stage == "before_dispatch":
                assert before["approvals"][0]["status"] == "approved" and before["executions"] == []
            if stage in ("worker_result_observed", "invocation_result_saved", "core_output_saved"):
                assert (
                    len(before["executions"]) == 1
                    and before["executions"][0]["status"] == "succeeded"
                )
            assert before["invocations"][0]["has_response"] == (
                stage in ("invocation_result_saved", "core_output_saved")
            )
            assert bool(before["core"]["outputs"]) == (stage == "core_output_saved")
        await gate.restart(worker=name == "worker_unknown")
        if name == "worker_unknown":
            await blocker.close()
        if name in ("cancel", "revoke"):
            if name == "cancel":
                await gate.post(f"/api/v2/agent/threads/{gate.thread}/turns/{gate.run}:interrupt")
                await gate.post(f"/api/v2/agent/threads/{gate.thread}/turns/{gate.run}:interrupt")
                expected = "cancelled"
            else:
                await gate.db.execute(
                    "INSERT INTO assistant.assistant_runtime_snapshot_revocations(snapshot_id,tenant_id,user_id,session_id,reason_code,revoked_by) SELECT snapshot_id,tenant_id,user_id,session_id,'controlled_acceptance_revocation','local-test' FROM assistant.assistant_runtime_snapshots WHERE run_id=$1",
                    gate.run,
                )
                expected = "failed"
            assert await gate.terminal() == expected
            await gate.duplicate_recover()
            after = await gate.facts(evidence / "after.json")
            assert after["executions"] == []
        else:
            await gate.recovered()
            assert await gate.identity() == original
            await gate.duplicate_recover()
            if name != "worker_unknown" and stage in ("invocation_saved", "approval_saved"):
                approval = await gate.approval()
                if before["approvals"]:
                    assert str(approval) == before["approvals"][0]["approval_id"]
                await gate.decide(approval, approved=name != "reject")
            assert await gate.terminal() == "succeeded"
            after = await gate.facts(evidence / "after.json")
            if name == "reject":
                assert after["executions"] == []
            else:
                assert len(after["executions"]) == 1
                assert after["executions"][0]["status"] == (
                    "side_effect_unknown" if name == "worker_unknown" else "succeeded"
                )
                assert (
                    after["executions"][0]["attempt_id"] == after["executions"][0]["tool_call_id"]
                )
                if before["executions"]:
                    assert (
                        after["executions"][0]["execution_id"]
                        == before["executions"][0]["execution_id"]
                    )
                assert list(after["core"]["calls"].values()) == [1]
                assert list(after["core"]["outputs"].values()) == [1]
                assert await gate.db.fetchval(
                    "SELECT COUNT(*) FROM assistant.quizzes WHERE title=$1", title
                ) == (0 if name == "worker_unknown" else 1)
            await gate.duplicate_recover()
        await gate.assert_no_extra_run()
        assert await gate.identity() == original
        assert len(after["invocations"]) == 1
        assert len(after["approvals"]) == 1
        if before["approvals"]:
            assert after["approvals"][0]["approval_id"] == before["approvals"][0]["approval_id"]
        assert after["core"]["aborted"] == 0
        if name not in ("cancel", "revoke"):
            assert after["core"]["complete"] == 1
            assert set(after["core"]["calls"]) == set(after["core"]["outputs"])
        if name in ("cancel", "revoke", "reject"):
            assert await gate.db.fetchval(
                "SELECT COUNT(*) FROM assistant.quizzes WHERE title=$1", title
            ) == 0
        (evidence / "result.json").write_text(
            json.dumps(
                {
                    "scenario": name,
                    "result": "PASS",
                    "evidence_kind": "real Qwen + controlled dev pause / SIGKILL or DB lock injection",
                    "original_identity": original,
                    "run_id": str(gate.run),
                },
                indent=2,
            )
            + "\n"
        )
        print(json.dumps({"scenario": name, "result": "PASS", "run_id": str(gate.run)}), flush=True)
    except BaseException as error:
        (evidence / "result.json").write_text(
            json.dumps(
                {
                    "scenario": name,
                    "result": "FAILED",
                    "failure_type": type(error).__name__,
                    "run_id": str(gate.run),
                    "evidence_kind": "real Qwen + controlled dev pause / SIGKILL or DB lock injection",
                },
                indent=2,
            )
            + "\n"
        )
        raise
    finally:
        if blocker is not None and not blocker.is_closed():
            await blocker.close()
        await gate.cleanup()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--scenario",
        choices=(*STAGES, "cancel", "revoke", "reject", "worker_unknown"),
        required=True,
    )
    args = parser.parse_args()
    asyncio.run(scenario(args.scenario))
