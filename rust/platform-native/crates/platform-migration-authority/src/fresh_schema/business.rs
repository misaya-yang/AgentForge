//! Real Gateway-role legacy import, hashes, idempotency and tenant negatives.
//! All fixture rows and writes are rolled back on the private runtime connection.
use super::*;
use sqlx::{Connection, PgConnection, Row};
use uuid::Uuid;
pub(super) async fn actual() -> Result<Value> {
    let prefix = crate::credentials::role_prefix_from_env()?;
    let c = crate::credentials::connection_for_principal("gateway")?;
    if c.tls_key_identity.is_none() {
        return Err(refused(
            "business acceptance requires actual Gateway file/mTLS",
        ));
    }
    let mut conn = PgConnection::connect_with(&c.options)
        .await
        .map_err(|_| refused("Gateway business connection unavailable"))?;
    install::login(&mut conn, &prefix, "gateway", true).await?;
    let mut outcome = exercise(&mut conn, &prefix, false).await;
    if let Ok(value) = &mut outcome {
        value["actual_gateway_file_mtls"] = json!(true);
    }
    execution::discard(conn).await;
    let value = outcome?;
    let op = operator::LocalOperator::from_env()?;
    let options = sqlx::postgres::PgConnectOptions::new()
        .host(
            op.socket_directory
                .to_str()
                .ok_or_else(|| refused("operator path encoding"))?,
        )
        .username(&op.username)
        .password("")
        .database(DATABASE)
        .ssl_mode(sqlx::postgres::PgSslMode::Disable)
        .application_name("fresh_business_rollback");
    let mut admin = PgConnection::connect_with(&options).await?;
    let safe:bool=sqlx::query_scalar("SELECT current_database()='gateway_dev_fresh' AND inet_client_addr() IS NULL AND current_user=session_user AND (SELECT rolsuper FROM pg_catalog.pg_roles WHERE rolname=current_user)").fetch_one(&mut admin).await?;
    if !safe {
        return Err(refused(
            "business operator is not independent local authority",
        ));
    }
    let mut tx = admin.begin().await?;
    role(&mut tx, &prefix, "gateway").await?;
    let seeded = exercise(&mut tx, &prefix, true).await?;
    tx.rollback().await?;
    execution::discard(admin).await;
    let mut denied_roles = Vec::new();
    for principal in [
        "migrator",
        "runtime",
        "capability_worker",
        "knowledge_api",
        "knowledge_worker",
    ] {
        let c = crate::credentials::connection_for_principal(principal)?;
        let mut conn = PgConnection::connect_with(&c.options).await?;
        install::login(&mut conn, &prefix, principal, true).await?;
        let allowed:bool=sqlx::query_scalar("SELECT NOT(count(*)=2 AND bool_and(NOT pg_catalog.has_function_privilege(current_user,p.oid,'EXECUTE'))) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='assistant' AND p.proname IN ('ensure_assistant_runtime_thread','import_assistant_legacy_session')").fetch_one(&mut conn).await?;
        if allowed {
            return Err(refused("unrelated principal has business entry EXECUTE"));
        }
        deny(
            &mut conn,
            false,
            Uuid::new_v4(),
            "x",
            "x",
            "x",
            "native",
            "42501",
        )
        .await?;
        deny(
            &mut conn,
            true,
            Uuid::new_v4(),
            "x",
            "x",
            "x",
            "native",
            "42501",
        )
        .await?;
        execution::discard(conn).await;
        denied_roles.push(principal);
    }
    Ok(
        json!({"gateway_file_mtls":value,"local_authority_seeded_gateway_role":seeded,"other_principals_actual_execute_denials":denied_roles}),
    )
}
pub(super) async fn exercise(
    conn: &mut PgConnection,
    prefix: &str,
    role_fixture: bool,
) -> Result<Value> {
    let sid = format!("fresh-closure-{}", Uuid::new_v4());
    let thread = Uuid::new_v4();
    let approval = Uuid::new_v4();
    let history = json!([{"role":"user","content":"look up 工具 🔑"},{"role":"assistant","content":"account is active","metadata":{"tool_calls":[{"id":"call-1","name":"account_lookup","arguments":{"account_id":"acct-1"}}],"tool_results":[{"tool_call_id":"call-1","result":{"active":true}}]}}]);
    let mut tx = conn.begin().await?;
    let identity: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&mut *tx)
        .await?;
    if identity != format!("{prefix}gateway") {
        return Err(refused("business fixture must execute as Gateway"));
    }
    // Caller-owned shadows must never influence the three owner definers.
    for table in [
        "sessions",
        "assistant_runtime_threads",
        "assistant_runtime_thread_members",
        "assistant_runtime_thread_projections",
        "assistant_runtime_items",
        "assistant_runs",
        "assistant_command_queue",
        "assistant_tool_approvals",
    ] {
        execution::execute(
            &mut tx,
            &format!("CREATE TEMP TABLE {table}(shadow_marker bool) ON COMMIT DROP"),
        )
        .await?;
    }
    let native_sid = format!("fresh-native-{}", Uuid::new_v4());
    let native_thread = Uuid::new_v4();
    sqlx::query("INSERT INTO assistant.sessions(session_id,tenant_id,user_id) VALUES($1,'fresh-closure-tenant','fresh-closure-user')").bind(&native_sid).execute(&mut *tx).await?;
    for _ in 0..2 {
        ensure(
            &mut tx,
            native_thread,
            "fresh-closure-tenant",
            "fresh-closure-user",
            &native_sid,
            "native",
        )
        .await?;
    }
    let native:bool=sqlx::query_scalar("SELECT t.import_status='not_required' AND t.source_kind='native' AND t.kernel_owner='agent_runtime' AND (SELECT count(*)=1 FROM assistant.assistant_runtime_thread_members m WHERE m.runtime_thread_id=t.runtime_thread_id) FROM assistant.assistant_runtime_threads t WHERE t.runtime_thread_id=$1").bind(native_thread).fetch_one(&mut *tx).await?;
    if !native {
        return Err(refused(
            "actual new native session/root member idempotency failed",
        ));
    }
    for (tenant, user, session) in [
        ("other", "fresh-closure-user", native_sid.as_str()),
        ("fresh-closure-tenant", "other", native_sid.as_str()),
        ("fresh-closure-tenant", "fresh-closure-user", "absent"),
    ] {
        deny(
            &mut tx,
            false,
            Uuid::new_v4(),
            tenant,
            user,
            session,
            "native",
            "42501",
        )
        .await?;
    }
    deny(
        &mut tx,
        false,
        Uuid::new_v4(),
        "fresh-closure-tenant",
        "fresh-closure-user",
        &native_sid,
        "invalid",
        "22023",
    )
    .await?;
    sqlx::query("INSERT INTO assistant.sessions(session_id,tenant_id,user_id,history) VALUES($1,'fresh-closure-tenant','fresh-closure-user',$2)").bind(&sid).bind(&history).execute(&mut *tx).await?;
    // UUID collision must not rebind an existing native thread to this second session.
    deny(
        &mut tx,
        false,
        native_thread,
        "fresh-closure-tenant",
        "fresh-closure-user",
        &sid,
        "native",
        "42501",
    )
    .await?;
    if role_fixture {
        role(&mut tx, prefix, "owner").await?;
        let run = Uuid::new_v4();
        sqlx::query("INSERT INTO assistant.assistant_runs(run_id,tenant_id,user_id,session_id) VALUES($1,'fresh-closure-tenant','fresh-closure-user',$2)").bind(run).bind(&sid).execute(&mut *tx).await?;
        role(&mut tx, prefix, "runtime").await?;
        for status in [
            "running",
            "blocked",
            "awaiting_approval",
            "approval_claimed",
            "side_effect_unknown",
            "terminal_persistence_unknown",
        ] {
            sqlx::query("UPDATE assistant.assistant_runs SET status=$2 WHERE run_id=$1")
                .bind(run)
                .bind(status)
                .execute(&mut *tx)
                .await?;
            role(&mut tx, prefix, "gateway").await?;
            deny(
                &mut tx,
                true,
                thread,
                "fresh-closure-tenant",
                "fresh-closure-user",
                &sid,
                "native",
                "55000",
            )
            .await?;
            role(&mut tx, prefix, "runtime").await?;
        }
        sqlx::query("UPDATE assistant.assistant_runs SET status='completed' WHERE run_id=$1")
            .bind(run)
            .execute(&mut *tx)
            .await?;
        sqlx::query("INSERT INTO assistant.assistant_tool_approvals(approval_id,tenant_id,user_id,session_id,run_id,tool_name,arguments,status,approved_by,approved_at) VALUES($1,'fresh-closure-tenant','fresh-closure-user',$2,$3,'account_lookup','{\"account_id\":\"acct-1\"}'::jsonb,'pending','fresh-closure-user',NOW())").bind(approval).bind(&sid).bind(run).execute(&mut *tx).await?;
        for status in ["pending", "claimed", "approved"] {
            sqlx::query(
                "UPDATE assistant.assistant_tool_approvals SET status=$2 WHERE approval_id=$1",
            )
            .bind(approval)
            .bind(status)
            .execute(&mut *tx)
            .await?;
            role(&mut tx, prefix, "gateway").await?;
            deny(
                &mut tx,
                true,
                thread,
                "fresh-closure-tenant",
                "fresh-closure-user",
                &sid,
                "native",
                "55000",
            )
            .await?;
            role(&mut tx, prefix, "runtime").await?;
        }
        sqlx::query(
            "UPDATE assistant.assistant_tool_approvals SET status='consumed' WHERE approval_id=$1",
        )
        .bind(approval)
        .execute(&mut *tx)
        .await?;
        role(&mut tx, prefix, "owner").await?;
        let command = Uuid::new_v4();
        sqlx::query("INSERT INTO assistant.assistant_command_queue(command_id,tenant_id,user_id,session_id,command_key,tool_name) VALUES($1,'fresh-closure-tenant','fresh-closure-user',$2,$3,'account_lookup')").bind(command).bind(&sid).bind(Uuid::new_v4()).execute(&mut *tx).await?;
        for status in [
            "queued",
            "running",
            "awaiting_approval",
            "approval_claimed",
            "side_effect_unknown",
        ] {
            sqlx::query(
                "UPDATE assistant.assistant_command_queue SET status=$2 WHERE command_id=$1",
            )
            .bind(command)
            .bind(status)
            .execute(&mut *tx)
            .await?;
            role(&mut tx, prefix, "gateway").await?;
            deny(
                &mut tx,
                true,
                thread,
                "fresh-closure-tenant",
                "fresh-closure-user",
                &sid,
                "native",
                "55000",
            )
            .await?;
            role(&mut tx, prefix, "owner").await?;
        }
        sqlx::query(
            "UPDATE assistant.assistant_command_queue SET status='completed' WHERE command_id=$1",
        )
        .bind(command)
        .execute(&mut *tx)
        .await?;
        role(&mut tx, prefix, "gateway").await?;
    }
    deny(
        &mut tx,
        true,
        native_thread,
        "fresh-closure-tenant",
        "fresh-closure-user",
        &sid,
        "native",
        "42501",
    )
    .await?;
    eprintln!("fresh-schema phase=business.inflight state=complete");
    let canonical: String =
        sqlx::query_scalar("SELECT history::text FROM assistant.sessions WHERE session_id=$1")
            .bind(&sid)
            .fetch_one(&mut *tx)
            .await?;
    for _ in 0..2 {
        let row=sqlx::query("SELECT * FROM assistant.import_assistant_legacy_session($1,'fresh-closure-tenant','fresh-closure-user',$2)").bind(thread).bind(&sid).fetch_one(&mut *tx).await?;
        if row.try_get::<String, _>("import_status")? != "ready"
            || row.try_get::<i32, _>("source_history_count")? != 2
            || row.try_get::<String, _>("source_history_sha256")? != sha(canonical.as_bytes())
        {
            return Err(refused(
                "actual Gateway legacy import/history hash or replay differs",
            ));
        }
    }
    let rows=sqlx::query("SELECT event_type,payload,payload::text AS canonical,payload_sha256::text FROM assistant.assistant_runtime_items WHERE runtime_thread_id=$1 ORDER BY sequence").bind(thread).fetch_all(&mut *tx).await?;
    if rows.len() != if role_fixture { 5 } else { 4 } {
        return Err(refused(
            "actual legacy tool/message/approval output incomplete",
        ));
    }
    let mut types = Vec::new();
    let mut receipts = 0;
    for row in &rows {
        if row.try_get::<String, _>("payload_sha256")?
            != sha(row.try_get::<String, _>("canonical")?.as_bytes())
        {
            return Err(refused("actual legacy event byte hash differs"));
        }
        let payload: Value = row.try_get("payload")?;
        if row.try_get::<String, _>("event_type")? == "rollout/item" {
            types.push(payload["payload"]["type"].as_str().unwrap_or("").to_owned());
        } else {
            let args:String=sqlx::query_scalar("SELECT arguments::text FROM assistant.assistant_tool_approvals WHERE approval_id=$1").bind(approval).fetch_one(&mut *tx).await?;
            if payload["approval_id"] != approval.to_string()
                || payload.get("arguments").is_some()
                || payload["arguments_sha256"] != sha(args.as_bytes())
            {
                return Err(refused("actual approval receipt redaction or hash differs"));
            }
            receipts += 1;
        }
    }
    if types
        != [
            "message",
            "function_call",
            "function_call_output",
            "message",
        ]
        || receipts != usize::from(role_fixture)
    {
        return Err(refused(
            "actual legacy tool and approval normalization differs",
        ));
    }
    let rollout = rows
        .iter()
        .filter_map(|row| {
            (row.try_get::<String, _>("event_type").ok().as_deref() == Some("rollout/item"))
                .then(|| row.try_get::<Value, _>("payload").ok())
                .flatten()
        })
        .collect::<Vec<_>>();
    if rollout[1]["payload"]
        != json!({"type":"function_call","name":"account_lookup","arguments":"{\"account_id\": \"acct-1\"}","call_id":"call-1"})
        || rollout[2]["payload"]["call_id"] != "call-1"
        || serde_json::from_str::<Value>(rollout[2]["payload"]["output"].as_str().unwrap_or("null"))
            .ok()
            != Some(json!({"active":true}))
        || rollout[0]["payload"]["content"][0]["text"] != "look up 工具 🔑"
        || rollout[3]["payload"]["content"][0]["text"] != "account is active"
    {
        return Err(refused(
            "actual legacy tool/message output is not equivalent",
        ));
    }
    if role_fixture {
        role(&mut tx, prefix, "runtime").await?;
        let projection:Value=sqlx::query_scalar("SELECT projection->'legacy_import' FROM assistant.assistant_runtime_thread_projections WHERE runtime_thread_id=$1").bind(thread).fetch_one(&mut *tx).await?;
        if projection["normalizer_version"] != 2
            || projection["tool_call_count"] != 1
            || projection["approval_receipt_count"] != usize::from(role_fixture)
        {
            return Err(refused("actual legacy import projection differs"));
        }
        role(&mut tx, prefix, "gateway").await?;
    }
    for (tenant, user, session) in [
        ("other", "fresh-closure-user", sid.as_str()),
        ("fresh-closure-tenant", "other", sid.as_str()),
        ("fresh-closure-tenant", "fresh-closure-user", "absent"),
    ] {
        deny(
            &mut tx,
            true,
            Uuid::new_v4(),
            tenant,
            user,
            session,
            "native",
            "42501",
        )
        .await?;
    }
    let mut changed = tx.begin().await?;
    sqlx::query("UPDATE assistant.sessions SET history='[]'::jsonb WHERE session_id=$1")
        .bind(&sid)
        .execute(&mut *changed)
        .await?;
    deny(
        &mut changed,
        true,
        thread,
        "fresh-closure-tenant",
        "fresh-closure-user",
        &sid,
        "native",
        "55000",
    )
    .await?;
    changed.rollback().await?;
    sqlx::raw_sql("SAVEPOINT fresh_extension_denial")
        .execute(&mut *tx)
        .await?;
    let denied = sqlx::query("SELECT public.digest('abc','sha256')")
        .fetch_one(&mut *tx)
        .await;
    if denied
        .err()
        .and_then(|e| {
            e.as_database_error()
                .and_then(|d| d.code())
                .map(|c| c.into_owned())
        })
        .as_deref()
        != Some("42501")
    {
        return Err(refused("Gateway obtained direct extension EXECUTE"));
    }
    sqlx::raw_sql(
        "ROLLBACK TO SAVEPOINT fresh_extension_denial;RELEASE SAVEPOINT fresh_extension_denial",
    )
    .execute(&mut *tx)
    .await?;
    if sqlx::query_scalar::<_, Value>("SELECT history FROM assistant.sessions WHERE session_id=$1")
        .bind(&sid)
        .fetch_one(&mut *tx)
        .await?
        != history
    {
        return Err(refused("legacy source history mutated"));
    }
    tx.rollback().await?;
    let absent:bool=sqlx::query_scalar("SELECT NOT EXISTS(SELECT 1 FROM assistant.sessions WHERE session_id=$1) AND NOT EXISTS(SELECT 1 FROM assistant.assistant_runtime_items WHERE runtime_thread_id=$2)").bind(&sid).bind(thread).fetch_one(&mut *conn).await?;
    if !absent {
        return Err(refused("actual Gateway business fixture did not roll back"));
    }
    Ok(
        json!({"actual_gateway_role":true,"history_tool_paths":true,"runtime_seeded_terminal_approval":role_fixture,"all_six_hash_sites_exercised":role_fixture,"session_scope_user_tenant_collision_inflight_changedsource_and_temp_shadowing":true,"event_byte_hashes_checked":rows.len(),"idempotent_import":true,"cross_tenant_denied":true,"direct_extension_execute_denied":true,"extension_execute_grants_added":false,"fixture_rolled_back":true}),
    )
}

async fn role(conn: &mut PgConnection, prefix: &str, principal: &str) -> Result<()> {
    crate::manifest::validate_identifier(&format!("{prefix}{principal}"))?;
    execution::execute(conn, &format!("SET LOCAL ROLE \"{prefix}{principal}\"")).await
}
async fn ensure(
    conn: &mut PgConnection,
    id: Uuid,
    tenant: &str,
    user: &str,
    sid: &str,
    kind: &str,
) -> Result<()> {
    sqlx::query("SELECT * FROM assistant.ensure_assistant_runtime_thread($1,$2::varchar,$3::varchar,$4::varchar,$5::varchar)").bind(id).bind(tenant).bind(user).bind(sid).bind(kind).fetch_one(conn).await?;
    Ok(())
}
async fn deny(
    conn: &mut PgConnection,
    import: bool,
    id: Uuid,
    tenant: &str,
    user: &str,
    sid: &str,
    kind: &str,
    code: &str,
) -> Result<()> {
    let mut tx = conn.begin().await?;
    let result = if import {
        sqlx::query("SELECT * FROM assistant.import_assistant_legacy_session($1,$2::varchar,$3::varchar,$4::varchar)").bind(id).bind(tenant).bind(user).bind(sid).fetch_one(&mut *tx).await.map(|_|())
    } else {
        sqlx::query("SELECT * FROM assistant.ensure_assistant_runtime_thread($1,$2::varchar,$3::varchar,$4::varchar,$5::varchar)").bind(id).bind(tenant).bind(user).bind(sid).bind(kind).fetch_one(&mut *tx).await.map(|_|())
    };
    let actual = result.err().and_then(|e| {
        e.as_database_error()
            .and_then(|d| d.code())
            .map(|c| c.into_owned())
    });
    tx.rollback().await?;
    if actual.as_deref() != Some(code) {
        return Err(refused(
            "business negative did not return exact expected SQLSTATE",
        ));
    }
    Ok(())
}
