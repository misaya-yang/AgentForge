"""Read-only, credential-redacted observations for original-run crash acceptance."""

from __future__ import annotations

import argparse
import asyncio
import json
from pathlib import Path
from uuid import UUID

import asyncpg
from dotenv import dotenv_values


async def observe(run_id: UUID, output: Path) -> None:
    env = dotenv_values(".env")
    conn = await asyncpg.connect(
        host="127.0.0.1",
        port=int(env.get("POSTGRES_PORT") or 5432),
        user=env["POSTGRES_USER"],
        password=env["POSTGRES_PASSWORD"],
        database=env.get("POSTGRES_DB") or "gateway",
    )
    try:
        run = await conn.fetchrow(
            "SELECT r.run_id,r.status,r.session_id,r.harness_thread_id,o.owner_id,o.fence,l.lease_id,l.snapshot_id,l.model_id,l.status AS lease_status FROM assistant.assistant_runs r JOIN assistant.assistant_runtime_execution_owners o ON o.run_id=r.run_id JOIN assistant.assistant_runtime_model_leases l ON l.run_id=r.run_id WHERE r.run_id=$1",
            run_id,
        )
        if run is None:
            raise RuntimeError("Original durable run not found")
        approvals = await conn.fetch(
            "SELECT approval_id,status,tool_call_id,expires_at FROM assistant.assistant_tool_approvals WHERE run_id=$1 ORDER BY created_at",
            run_id,
        )
        invocations = await conn.fetch(
            "SELECT call_id,params->>'tool' AS tool,response IS NOT NULL AS has_response FROM assistant.assistant_runtime_invocations WHERE run_id=$1 ORDER BY created_at",
            run_id,
        )
        executions = await conn.fetch(
            "SELECT execution_id,tool_call_id,attempt_id,capability_id,status,dispatch_fence,result_summary IS NOT NULL AS has_receipt FROM assistant.assistant_capability_executions WHERE run_id=$1 ORDER BY created_at",
            run_id,
        )
        items = await conn.fetch(
            "SELECT payload FROM assistant.assistant_runtime_items WHERE kernel_thread_id=$1 AND event_type='rollout/item' ORDER BY sequence",
            run["harness_thread_id"],
        )
        active = False
        started = complete = aborted = 0
        calls, outputs = {}, {}
        for row in items:
            item = json.loads(row["payload"]) if isinstance(row["payload"], str) else row["payload"]
            payload = item.get("payload", {})
            if item.get("type") == "event_msg":
                kind = payload.get("type")
                if kind == "task_started":
                    active = payload.get("turn_id") == str(run_id)
                    started += int(active)
                if payload.get("turn_id") == str(run_id):
                    complete += int(kind == "task_complete")
                    aborted += int(kind == "turn_aborted")
            elif active and item.get("type") == "response_item":
                kind, call = payload.get("type"), payload.get("call_id")
                if kind == "function_call":
                    calls[call] = calls.get(call, 0) + 1
                if kind == "function_call_output":
                    outputs[call] = outputs.get(call, 0) + 1
        facts = {
            "run": dict(run),
            "approvals": [dict(row) for row in approvals],
            "invocations": [dict(row) for row in invocations],
            "executions": [dict(row) for row in executions],
            "core": {
                "started": started,
                "complete": complete,
                "aborted": aborted,
                "calls": calls,
                "outputs": outputs,
            },
        }
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(facts, default=str, indent=2) + "\n")
        print(json.dumps(facts, default=str))
    finally:
        await conn.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run-id", type=UUID, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    asyncio.run(observe(args.run_id, args.output))
