"""Actual additive epoch and service-role recovery invariants on synthetic DBs.

RUNTIME_RECOVERY_DB_LIVE=1 is required. Dedicated databases are retained for
inspection; credentials are read only while executing and never reported.
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import os
import uuid
from pathlib import Path
from urllib.parse import quote

import asyncpg
import pytest
from dotenv import dotenv_values

from database.authority.bootstrap import (
    fresh_install,
    provision_extensions_admin,
    provision_roles_admin,
)
from database.authority.commands import (
    command_migrate,
    command_verify,
    default_paths,
    load_baseline,
)
from database.authority.runner import MigrationAuthority

pytestmark = pytest.mark.skipif(os.getenv("RUNTIME_RECOVERY_DB_LIVE") != "1", reason="isolated PostgreSQL recovery matrix opt-in")


async def _seed(conn, previous=None):
    thread, run, snapshot, lease = [uuid.uuid4() for _ in range(4)]
    tenant, user, session = "recovery-fixture-tenant", "recovery-fixture-user", f"recovery-{uuid.uuid4()}"
    if previous:
        thread, tenant, user, session = (previous[key] for key in ("thread", "tenant", "user", "session"))
    payload = json.dumps({"schema_version": "agent-runtime-snapshot/v1", "model": {"id": "fixture-model"}, "capability_revision": 1}, sort_keys=True, separators=(",", ":"))
    if not previous:
        await conn.execute("INSERT INTO assistant.sessions(session_id,service_id,user_id,tenant_id) VALUES($1,'__builtin_assistant__',$2,$3)", session, user, tenant)
        await conn.execute("INSERT INTO assistant.assistant_runtime_threads(runtime_thread_id,tenant_id,user_id,session_id) VALUES($1,$2,$3,$4)", thread, tenant, user, session)
        await conn.execute("INSERT INTO assistant.assistant_session_runtime_assignments(tenant_id,user_id,session_id,runtime_owner,kernel_revision) VALUES($1,$2,$3,'agent_runtime','fixture-kernel')", tenant, user, session)
    await conn.execute("SELECT assistant.issue_assistant_runtime_turn($1,$2,$3,$4,$5,$6,$7,'fixture-kernel','agent-runtime-snapshot/v1',$8::jsonb,$9,1,'minimal','agent-runtime-model-lease/v1','fixture-provider','fixture-model','fixture-revision',$10,8,10000,1000,1000000,NOW()+INTERVAL '10 minutes','fixture')", snapshot, lease, run, thread, tenant, user, session, payload, hashlib.sha256(payload.encode()).hexdigest(), "a" * 64)
    return {"thread": thread, "run": run, "snapshot": snapshot, "lease": lease, "tenant": tenant, "user": user, "session": session}


async def _claim(conn, prefix, run, owner):
    async with conn.transaction():
        await conn.execute(f'SET LOCAL ROLE "{prefix}runtime"')
        return await conn.fetchrow("SELECT * FROM assistant.claim_runtime_execution($1,$2,$3)", run, owner, '{"schema":"fixture"}')


@pytest.mark.asyncio
@pytest.mark.parametrize("prefix", ["ai_gateway_", "p1ref_"])
async def test_recovery_claim_journal_revocation_and_dispatch_contract(prefix):
    env = dotenv_values(".env")
    kwargs = {"host": "127.0.0.1", "port": int(env.get("POSTGRES_PORT") or 5432), "user": env["POSTGRES_USER"], "password": env["POSTGRES_PASSWORD"]}
    name = "runtime_recovery_" + uuid.uuid4().hex[:10]
    admin = await asyncpg.connect(**kwargs, database=env.get("POSTGRES_DB") or "gateway")
    await admin.execute(f'CREATE DATABASE "{name}"')
    await admin.close()
    conn = await asyncpg.connect(**kwargs, database=name)
    paths = default_paths()
    await provision_roles_admin(conn, paths, prefix)
    await provision_extensions_admin(conn, paths)
    baseline, sha = load_baseline(paths, "2026_08_post_kb_v1")
    await fresh_install(conn, paths, baseline, sha, role_prefix=prefix)
    dsn = f"postgresql://{quote(kwargs['user'], safe='')}:{quote(kwargs['password'], safe='')}@127.0.0.1:{kwargs['port']}/{name}"
    authority = MigrationAuthority(dsn, paths, role_prefix=prefix)
    await command_migrate(authority, log=lambda _: None)
    await conn.execute("SET search_path=pg_catalog,assistant,gateway,knowledge,public")

    facts = await _seed(conn)
    owner1, owner2 = uuid.uuid4(), uuid.uuid4()
    peer = await asyncpg.connect(**kwargs, database=name)
    outcomes = await asyncio.gather(_claim(conn, prefix, facts["run"], owner1), _claim(peer, prefix, facts["run"], owner2), return_exceptions=True)
    assert sum(not isinstance(result, Exception) for result in outcomes) == 1
    winner = next(result for result in outcomes if not isinstance(result, Exception))
    assert winner["fence"] == 1
    loser = owner2 if winner["owner_id"] == owner1 else owner1
    await peer.close()
    with pytest.raises(asyncpg.PostgresError, match="ALREADY_OWNED"):
        await _claim(conn, prefix, facts["run"], loser)
    # Synthetic lease expiry exercises takeover atomically; this is an isolated
    # DB test, not claimed as a process crash or real provider observation.
    await conn.execute("UPDATE assistant_runtime_execution_owners SET lease_until=NOW()-INTERVAL '1 second' WHERE run_id=$1", facts["run"])
    replacement = await _claim(conn, prefix, facts["run"], loser)
    assert replacement["fence"] == 2
    with pytest.raises(asyncpg.PostgresError, match="FENCE_LOST"):
        await conn.fetchval("SELECT assert_runtime_execution($1,$2,1)", facts["run"], winner["owner_id"])
    # Worker mutation and takeover share locks, even when a request waited long
    # enough for the original lease to expire. No check/mutation gap exists.
    peer = await asyncpg.connect(**kwargs, database=name)
    async with conn.transaction():
        await conn.execute(f'SET LOCAL ROLE "{prefix}capability_worker"')
        assert await conn.fetchval("SELECT assert_worker_execution_owner($1,$2,$3,$4,$5,2,TRUE)", facts["run"], facts["tenant"], facts["user"], facts["session"], loser)
        await conn.execute("RESET ROLE")
        await conn.execute("UPDATE assistant_runtime_execution_owners SET lease_until=clock_timestamp()-INTERVAL '1 second' WHERE run_id=$1", facts["run"])
        takeover = asyncio.create_task(_claim(peer, prefix, facts["run"], winner["owner_id"]))
        await asyncio.sleep(0.1)
        assert not takeover.done()
    assert (await takeover)["fence"] == 3
    with pytest.raises(asyncpg.PostgresError, match="FENCE_LOST"):
        async with conn.transaction():
            await conn.execute(f'SET LOCAL ROLE "{prefix}capability_worker"')
            await conn.fetchval("SELECT assert_worker_execution_owner($1,$2,$3,$4,$5,2,TRUE)", facts["run"], facts["tenant"], facts["user"], facts["session"], loser)
    await peer.close()

    params = '{"tool":"fixture","arguments":{"value":1}}'
    response = '{"contentItems":[{"type":"inputText","text":"original result"}],"success":true}'
    async with conn.transaction():
        await conn.execute(f'SET LOCAL ROLE "{prefix}runtime"')
        await conn.execute("INSERT INTO assistant_runtime_invocations(run_id,kernel_thread_id,call_id,params) VALUES($1,$2,'original-call',$3)", facts["run"], facts["thread"], params)
        await conn.execute("UPDATE assistant_runtime_invocations SET response=$3 WHERE run_id=$1 AND kernel_thread_id=$2", facts["run"], facts["thread"], response)
        await conn.execute("UPDATE assistant_runtime_invocations SET response=$3 WHERE run_id=$1 AND kernel_thread_id=$2", facts["run"], facts["thread"], response)
    for column, value in [("params", '{}'), ("response", '{"success":false}')]:
        with pytest.raises(asyncpg.PostgresError, match="INVOCATION_IMMUTABLE"):
            await conn.execute(f"UPDATE assistant_runtime_invocations SET {column}=$2 WHERE run_id=$1", facts["run"], value)
    for role in ("gateway", "capability_worker"):
        assert not await conn.fetchval("SELECT has_table_privilege($1,'assistant.assistant_runtime_invocations','INSERT,UPDATE,DELETE')", prefix + role)
    assert not await conn.fetchval("SELECT has_function_privilege($1,'assistant.claim_runtime_execution(uuid,uuid,jsonb)','EXECUTE')", prefix + "knowledge_api")

    for revoked in (False, True):
        test = await _seed(conn)
        owner = uuid.uuid4()
        await _claim(conn, prefix, test["run"], owner)
        execution, cap_lease = uuid.uuid4(), uuid.uuid4()
        arguments = '{"fixture":1}'
        args_hash = hashlib.sha256(arguments.encode()).hexdigest()
        arguments_list = [execution, cap_lease, test["tenant"], test["user"], test["session"], test["run"], "call-a", "call-a", "fixture-read", 1, arguments, args_hash, "fixture-idempotency", "read", "never", None, "not_required", f"/internal/v2/capabilities/executions/{execution}/events", '{}']
        reserve_sql = "SELECT * FROM assistant.reserve_assistant_capability_execution(" + ",".join(f"${index}" for index in range(1, 20)) + ")"
        async with conn.transaction():
            await conn.execute(f'SET LOCAL ROLE "{prefix}capability_worker"')
            original = await conn.fetchrow(reserve_sql, *arguments_list)
            arguments_list[0], arguments_list[1] = uuid.uuid4(), uuid.uuid4()
            replay = await conn.fetchrow(reserve_sql, *arguments_list)
            assert original["execution_id"] == replay["execution_id"] == execution
            assert replay["lease_id"] == cap_lease
        if revoked:
            await conn.execute("INSERT INTO assistant_runtime_snapshot_revocations(snapshot_id,tenant_id,user_id,session_id,reason_code,revoked_by) VALUES($1,$2,$3,$4,'fixture_revoked','fixture-admin')", test["snapshot"], test["tenant"], test["user"], test["session"])
        else:
            async with conn.transaction():
                await conn.execute(f'SET LOCAL ROLE "{prefix}runtime"')
                cancel_args = [test[key] for key in ("run", "thread", "tenant", "user", "session")]
                assert await conn.fetchval("SELECT assistant.cancel_runtime_execution($1,$2,$3,$4,$5)", *cancel_args)
                assert not await conn.fetchval("SELECT assistant.cancel_runtime_execution($1,$2,$3,$4,$5)", *cancel_args)
            assert await conn.fetchval("SELECT status FROM assistant_runtime_model_leases WHERE run_id=$1", test["run"]) == "revoked"
        with pytest.raises(asyncpg.PostgresError, match="AUTHORITY_REVOKED"):
            await conn.fetchval("SELECT assert_runtime_execution($1,$2,1)", test["run"], owner)
        # Receipt persistence has a narrower fence contract after cancellation.
        assert await conn.fetchval("SELECT assert_runtime_execution_fence($1,$2,1)", test["run"], owner)
        with pytest.raises(asyncpg.PostgresError, match="AUTHORITY_REVOKED"):
            async with conn.transaction():
                await conn.execute(f'SET LOCAL ROLE "{prefix}capability_worker"')
                await conn.fetchrow("SELECT * FROM assistant.dispatch_assistant_capability_execution($1,$2,$3,$4,$5,30000)", execution, test["tenant"], test["user"], test["session"], uuid.uuid4())
        assert await conn.fetchval("SELECT dispatch_fence IS NULL FROM assistant_capability_executions WHERE execution_id=$1", execution)

    # The real Worker role, not the admin connection, can atomically persist a
    # Quiz and its question. This is service-role DB evidence, not provider QA.
    quiz = uuid.uuid4()
    async with conn.transaction():
        await conn.execute(f'SET LOCAL ROLE "{prefix}capability_worker"')
        await conn.execute("INSERT INTO assistant.quizzes(id,tenant_id,created_by,title,question_count,status) VALUES($1,'fixture-tenant','fixture-user','durable fixture',1,'ready')", quiz)
        await conn.execute("INSERT INTO assistant.quiz_questions(id,quiz_id,question_num,question_type,question_text,options,correct_answer) VALUES($1,$2,1,'single_choice','fixture question','[]','[]')", uuid.uuid4(), quiz)
        assert await conn.fetchval("SELECT COUNT(*) FROM assistant.quizzes WHERE id=$1", quiz) == 1
    assert await conn.fetchval("SELECT COUNT(*) FROM assistant.quiz_questions WHERE quiz_id=$1", quiz) == 1
    # Execute the actual history query: cold cancellation before run_started,
    # a later successful turn, duplicate cancel repair, and late failure.
    from src.services.agent_runtime.thread_store import AgentThreadStore

    stopped = await _seed(conn)
    await conn.execute("INSERT INTO assistant_runtime_thread_members(kernel_thread_id,runtime_thread_id,kernel_session_id,relation_kind,tenant_id,user_id,session_id) VALUES($1,$1,$1,'root',$2,$3,$4)", stopped["thread"], stopped["tenant"], stopped["user"], stopped["session"])

    async def append_event(facts, kind, data, key, *, bad_hash=False):
        payload = json.dumps({"schema_version": "assistant-turn-contract/v1", "event_type": kind, "data": {"run_id": str(facts["run"]), "session_id": facts["session"], "thread_id": str(facts["thread"]), **data}, "timestamp": 1.0}, sort_keys=True, separators=(",", ":"))
        return await conn.fetchval("SELECT append_assistant_runtime_item($1,$1,$2,$3,$4,$5,$6,$7,NULL,$8,'assistant_turn_event_v1',$9,$10,$11)", facts["thread"], facts["tenant"], facts["user"], facts["session"], uuid.uuid5(facts["run"], key), key, str(facts["run"]), "compat/v1/" + kind, data.get("status"), payload, "z" * 64 if bad_hash else hashlib.sha256(payload.encode()).hexdigest())

    with pytest.raises(asyncpg.PostgresError):
        async with conn.transaction():
            await conn.execute(f'SET LOCAL ROLE "{prefix}runtime"')
            await conn.fetchval("SELECT cancel_runtime_execution($1,$2,$3,$4,$5)", *[stopped[key] for key in ("run", "thread", "tenant", "user", "session")])
            await append_event(stopped, "cancelled", {"status": "cancelled"}, "cancel-receipt", bad_hash=True)
    assert await conn.fetchval("SELECT status FROM assistant_runs WHERE run_id=$1", stopped["run"]) == "running"
    async with conn.transaction():
        await conn.execute(f'SET LOCAL ROLE "{prefix}runtime"')
        await conn.fetchval("SELECT cancel_runtime_execution($1,$2,$3,$4,$5)", *[stopped[key] for key in ("run", "thread", "tenant", "user", "session")])
        first_receipt = await append_event(stopped, "cancelled", {"status": "cancelled"}, "cancel-receipt")
    later = await _seed(conn, stopped)
    await append_event(later, "run_started", {"status": "running"}, "later-started")
    await append_event(later, "text_delta", {"content": "later success"}, "later-text")
    await append_event(later, "run_finished", {"status": "succeeded"}, "later-complete")
    await conn.execute("UPDATE assistant_runs SET status='succeeded' WHERE run_id=$1", later["run"])
    assert await append_event(stopped, "cancelled", {"status": "cancelled"}, "cancel-receipt") == first_receipt
    await append_event(stopped, "run_error", {"status": "failed"}, "late-failed-not-authority")
    messages, total = await AgentThreadStore(conn).history_messages(tenant_id=stopped["tenant"], user_id=stopped["user"], runtime_thread_id=str(stopped["thread"]), limit=20)
    assert total == 4
    assert [message["role"] for message in messages] == ["user", "assistant", "user", "assistant"]
    assert messages[1]["content"] == ""
    assert messages[1]["metadata"]["runtime_run_id"] == str(stopped["run"])
    assert messages[1]["metadata"]["process_summary"]["status"] == "cancelled"
    assert messages[3]["content"] == "later success"

    # Recovery emits another started for the SAME run, not another answer.
    # Exercise the real SQL with old lifecycle-only unknown receipts as well.
    recovered = await _seed(conn)
    await conn.execute("INSERT INTO assistant_runtime_thread_members(kernel_thread_id,runtime_thread_id,kernel_session_id,relation_kind,tenant_id,user_id,session_id) VALUES($1,$1,$1,'root',$2,$3,$4)", recovered["thread"], recovered["tenant"], recovered["user"], recovered["session"])
    await append_event(recovered, "run_started", {"status": "running"}, "original-start")
    await append_event(recovered, "run_started", {"status": "running"}, "recovered-start")
    await append_event(recovered, "text_delta", {"content": "same answer"}, "recovered-text")
    await append_event(recovered, "run_finished", {"status": "succeeded"}, "recovered-complete")

    async def append_item(facts, event_type, item_type, status, payload, key):
        encoded = json.dumps(payload, sort_keys=True, separators=(",", ":"))
        await conn.fetchval("SELECT append_assistant_runtime_item($1,$1,$2,$3,$4,$5,$6,$7,NULL,$8,$9,$10,$11,$12)", facts["thread"], facts["tenant"], facts["user"], facts["session"], uuid.uuid5(facts["run"], key), key, str(facts["run"]) if event_type != "rollout/item" else None, event_type, item_type, status, encoded, hashlib.sha256(encoded.encode()).hexdigest())

    await append_item(recovered, "agent-runtime/tool-lifecycle", "tool_result", "side_effect_unknown", {"turn_id": str(recovered["run"]), "result_status": "side_effect_unknown", "detail": "private detail"}, "unknown-receipt")
    messages, total = await AgentThreadStore(conn).history_messages(tenant_id=recovered["tenant"], user_id=recovered["user"], runtime_thread_id=str(recovered["thread"]), limit=20)
    assert total == 2 and messages[1]["content"] == "same answer"
    assert messages[1]["metadata"]["process_summary"]["outcome_uncertain"] is True
    assert "private detail" not in json.dumps(messages)
    core_message = {"type": "event_msg", "payload": {"type": "agent_message", "message": "same answer"}}
    await append_item(recovered, "rollout/item", "event_msg", None, core_message, "original-core-answer")
    messages, total = await AgentThreadStore(conn).history_messages(tenant_id=recovered["tenant"], user_id=recovered["user"], runtime_thread_id=str(recovered["thread"]), limit=20)
    assert total == 2 and messages[1]["content"] == "same answer"
    assert messages[1]["metadata"]["process_summary"]["outcome_uncertain"] is True
    another = await _seed(conn, recovered)
    await append_event(another, "run_started", {"status": "running"}, "another-start")
    await append_event(another, "text_delta", {"content": "same answer"}, "another-text")
    await append_item(another, "rollout/item", "event_msg", None, core_message, "another-core-answer")
    await append_event(another, "run_finished", {"status": "succeeded"}, "another-complete")
    messages, total = await AgentThreadStore(conn).history_messages(tenant_id=recovered["tenant"], user_id=recovered["user"], runtime_thread_id=str(recovered["thread"]), limit=20)
    assert total == 4
    answers = [message for message in messages if message["role"] == "assistant"]
    assert [message["content"] for message in answers] == ["same answer", "same answer"]
    assert {message["metadata"]["runtime_run_id"] for message in answers} == {str(recovered["run"]), str(another["run"])}

    await command_verify(authority, log=lambda _: None)
    await conn.close()
    evidence = Path("tmp/runtime-durable-recovery")
    evidence.mkdir(parents=True, exist_ok=True)
    (evidence / f"database-{prefix}.json").write_text(json.dumps({"database": name, "role_prefix": prefix, "matrix": "PASS", "source": "isolated synthetic database"}) + "\n")
