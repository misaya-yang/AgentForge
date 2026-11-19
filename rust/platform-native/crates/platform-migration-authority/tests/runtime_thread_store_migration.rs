//! Source-equivalent thread-store contracts. Live tests require explicit --include-ignored.
//! Each test creates and drops only a uniquely named schema; no shared tables are reset.
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::path::PathBuf;
use uuid::Uuid;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_owned()
}
fn migration(n: &str) -> String {
    std::fs::read_to_string(root().join("database/migrations").join(n)).unwrap()
}
const MIGRATIONS: [&str; 5] = [
    "089_agent_runtime_thread_store.sql",
    "090_agent_runtime_model_leases.sql",
    "092_agent_runtime_legacy_import.sql",
    "094_agent_runtime_legacy_import_normalization.sql",
    "095_agent_runtime_single_kernel.sql",
];
#[test]
fn migration_is_additive_and_has_atomic_append_contract() {
    let checks: [(&str, &[&str]); 6] = [
        (
            MIGRATIONS[0],
            &[
                "CREATE OR REPLACE FUNCTION APPEND_ASSISTANT_RUNTIME_ITEM",
                "FOR UPDATE",
                "ASSISTANT_RUNTIME_EVENT_KEY_CONFLICT",
                "ASSISTANT_RUNTIME_THREAD_SCOPE_MISMATCH",
                "ASSISTANT_RUNTIME_MEMBER_SCOPE_MISMATCH",
                "ASSISTANT_RUNTIME_ASSIGNMENT_IMMUTABLE",
            ],
        ),
        (
            MIGRATIONS[1],
            &[
                "CREATE OR REPLACE FUNCTION ISSUE_ASSISTANT_RUNTIME_TURN",
                "CREATE OR REPLACE FUNCTION RESERVE_ASSISTANT_RUNTIME_MODEL_CALL",
                "ASSISTANT_RUNTIME_MODEL_CALL_REPLAYED",
                "ASSISTANT_RUNTIME_LEASE_BUDGET_EXHAUSTED",
            ],
        ),
        (
            MIGRATIONS[2],
            &[
                "CREATE OR REPLACE FUNCTION IMPORT_ASSISTANT_LEGACY_SESSION",
                "ASSISTANT_RUNTIME_IMPORT_IN_FLIGHT",
                "ASSISTANT_RUNTIME_SOURCE_KIND_INVALID",
                "DYNAMIC_TOOL_FINGERPRINT",
                "FOR UPDATE",
            ],
        ),
        (
            "093_agent_runtime_assistant_session_fks.sql",
            &[
                "REFERENCES ASSISTANT.SESSIONS",
                "GATEWAY.ASSISTANT_SESSION_RUNTIME_ASSIGNMENTS",
                "GATEWAY.ASSISTANT_RUNTIME_THREADS",
            ],
        ),
        (
            MIGRATIONS[3],
            &[
                "CREATE OR REPLACE FUNCTION IMPORT_ASSISTANT_LEGACY_SESSION",
                "TO_REGCLASS('ASSISTANT.SESSIONS')",
                "SET SEARCH_PATH FROM CURRENT",
                "ASSISTANT_RUNTIME_IMPORT_TOOL_PAIRING_INVALID",
                "ASSISTANT_RUNTIME_IMPORT_SOURCE_CHANGED",
                "''BLOCKED''",
                "AGENT-RUNTIME-LEGACY-APPROVAL/V1",
            ],
        ),
        (
            MIGRATIONS[4],
            &[
                "RUNTIME_OWNER = 'AGENT_RUNTIME'",
                "PYTHON_CONTROL",
                "CODEX_CANDIDATE",
                "P.PRONAMESPACE = TO_REGNAMESPACE(CURRENT_SCHEMA())",
            ],
        ),
    ];
    for (name, needles) in checks {
        let sql = migration(name).to_uppercase();
        for needle in needles {
            assert!(sql.contains(needle), "{name}: {needle}");
        }
        if name != MIGRATIONS[4] {
            assert!(!sql.contains("DROP TABLE"));
            assert!(!sql.contains("TRUNCATE"));
        } else {
            assert!(!sql.contains("UPDATE ASSISTANT_RUNTIME_SNAPSHOTS"));
            assert!(!sql.contains("UPDATE ASSISTANT_RUNTIME_ITEMS"));
        }
    }
}
struct Fixture {
    pool: PgPool,
    admin: PgPool,
    schema: String,
}
impl Fixture {
    async fn new() -> Result<Self> {
        let env_path = std::env::var_os("ENV_FILE")
            .filter(|x| !x.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| root().join(".env"));
        let file_values = if env_path.exists() {
            dotenvy::from_path_iter(env_path)?
                .collect::<std::result::Result<std::collections::BTreeMap<String, String>, _>>()?
        } else {
            Default::default()
        };
        let get = |k: &str| {
            std::env::var(k)
                .ok()
                .filter(|x| !x.is_empty())
                .or_else(|| file_values.get(k).cloned())
                .filter(|x| !x.is_empty())
                .ok_or_else(|| format!("local PostgreSQL test configuration missing key: {k}"))
        };
        let options = PgConnectOptions::new()
            .host("127.0.0.1")
            .port(get("POSTGRES_PORT")?.parse()?)
            .username(&get("POSTGRES_USER")?)
            .password(&get("POSTGRES_PASSWORD")?)
            .database(&get("POSTGRES_DB")?);
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await?;
        let schema = format!("agent_runtime_test_{}", Uuid::new_v4().simple());
        sqlx::raw_sql(&format!("CREATE SCHEMA \"{schema}\""))
            .execute(&admin)
            .await?;
        let path = format!("\"{schema}\",public");
        let pool = PgPoolOptions::new()
            .min_connections(1)
            .max_connections(8)
            .after_connect(move |conn, _| {
                let path = path.clone();
                Box::pin(async move {
                    sqlx::query("SELECT set_config('search_path',$1,false)")
                        .bind(path)
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(options)
            .await?;
        let f = Self {
            pool,
            admin,
            schema,
        };
        let setup: Result<()> = async {
            sqlx::raw_sql(include_str!("fixtures/runtime_thread_store_base.sql"))
                .execute(&f.pool)
                .await?;
            for name in MIGRATIONS {
                let sql = migration(name);
                for _ in 0..2 {
                    sqlx::raw_sql(&sql).execute(&f.pool).await?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(e) = setup {
            f.close().await?;
            return Err(e);
        }
        Ok(f)
    }
    async fn close(self) -> Result<()> {
        self.pool.close().await;
        sqlx::raw_sql(&format!("DROP SCHEMA \"{}\" CASCADE", self.schema))
            .execute(&self.admin)
            .await?;
        self.admin.close().await;
        Ok(())
    }
}
// Catch assertion panics in a spawned task so cleanup still runs on test failure.
async fn fixture<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce(PgPool) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let f = Fixture::new().await?;
    let p = f.pool.clone();
    let outcome = tokio::spawn(async move { test(p).await }).await;
    f.close().await?;
    match outcome {
        Ok(r) => r,
        Err(e) if e.is_panic() => std::panic::resume_unwind(e.into_panic()),
        Err(e) => Err(e.into()),
    }
}
fn state<T: std::fmt::Debug>(result: std::result::Result<T, sqlx::Error>, code: &str) {
    let e = result.unwrap_err();
    assert_eq!(
        e.as_database_error().and_then(|x| x.code()).as_deref(),
        Some(code)
    );
}
fn message<T: std::fmt::Debug>(result: std::result::Result<T, sqlx::Error>, needle: &str) {
    assert!(result.unwrap_err().to_string().contains(needle));
}
async fn session(p: &PgPool, t: &str, u: &str, h: Value) -> Result<String> {
    let s = format!("session-{}", Uuid::new_v4());
    sqlx::query("INSERT INTO sessions(session_id,service_id,user_id,tenant_id,history) VALUES($1,'__builtin_assistant__',$2,$3,$4)").bind(&s).bind(u).bind(t).bind(h).execute(p).await?;
    Ok(s)
}
async fn import(
    p: &PgPool,
    id: Uuid,
    t: &str,
    u: &str,
    s: &str,
) -> std::result::Result<sqlx::postgres::PgRow, sqlx::Error> {
    sqlx::query("SELECT * FROM import_assistant_legacy_session($1,$2,$3,$4)")
        .bind(id)
        .bind(t)
        .bind(u)
        .bind(s)
        .fetch_one(p)
        .await
}
async fn count(p: &PgPool, table: &str, id: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {table} WHERE runtime_thread_id=$1"
    ))
    .bind(id)
    .fetch_one(p)
    .await?)
}
async fn seed(p: &PgPool) -> Result<(Uuid, String)> {
    let s = session(p, "tenant-a", "user-a", json!([])).await?;
    let id = Uuid::new_v4();
    sqlx::query("INSERT INTO assistant_runtime_threads(runtime_thread_id,tenant_id,user_id,session_id) VALUES($1,'tenant-a','user-a',$2)").bind(id).bind(&s).execute(p).await?;
    sqlx::query("INSERT INTO assistant_runtime_thread_members(kernel_thread_id,runtime_thread_id,kernel_session_id,relation_kind,tenant_id,user_id,session_id) VALUES($1,$1,$1,'root','tenant-a','user-a',$2)").bind(id).bind(&s).execute(p).await?;
    Ok((id, s))
}
fn hash(v: &Value) -> String {
    format!("{:x}", Sha256::digest(serde_json::to_vec(v).unwrap()))
}
async fn append(
    p: &PgPool,
    id: Uuid,
    s: &str,
    t: &str,
    event: Uuid,
    key: &str,
    payload: Value,
) -> std::result::Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT append_assistant_runtime_item($1,$1,$2,'user-a',$3,$4,$5,'turn-1','item-1','item/started','agent_message','in_progress',$6,$7)").bind(id).bind(t).bind(s).bind(event).bind(key).bind(&payload).bind(hash(&payload)).fetch_one(p).await
}
#[tokio::test]
#[ignore = "isolated PostgreSQL contract; run explicitly"]
async fn session_runtime_assignment_is_scope_bound_and_immutable() -> Result<()> {
    fixture(|p|async move{
 let s=session(&p,"tenant-assignment","user-assignment",json!([])).await?;
 sqlx::query("INSERT INTO assistant_session_runtime_assignments(tenant_id,user_id,session_id,runtime_owner,kernel_revision) VALUES('tenant-assignment','user-assignment',$1,'agent_runtime','kernel-1')").bind(&s).execute(&p).await?;
 let row=sqlx::query("SELECT runtime_owner,kernel_revision FROM assistant_session_runtime_assignments WHERE session_id=$1").bind(&s).fetch_one(&p).await?;
 assert_eq!(row.get::<String,_>("runtime_owner"),"agent_runtime");assert_eq!(row.get::<String,_>("kernel_revision"),"kernel-1");
 state(sqlx::query("UPDATE assistant_session_runtime_assignments SET runtime_owner='agent_runtime',kernel_revision='fork-sha' WHERE session_id=$1").bind(&s).execute(&p).await,"55000");
 let invalid=session(&p,"tenant-assignment","user-assignment",json!([])).await?;
 state(sqlx::query("INSERT INTO assistant_session_runtime_assignments(tenant_id,user_id,session_id,runtime_owner) VALUES('tenant-assignment','user-assignment',$1,'python_control')").bind(invalid).execute(&p).await,"23514");Ok(())}).await
}
#[tokio::test]
#[ignore = "isolated PostgreSQL contract; run explicitly"]
async fn legacy_import_is_idempotent_and_cross_tenant_scoped() -> Result<()> {
    fixture(|p|async move{
 let history=json!([{"role":"user","content":"hello"},{"role":"assistant","content":"hi"}]);let s=session(&p,"legacy-tenant","legacy-user",history.clone()).await?;let id=Uuid::new_v4();
 for _ in 0..2 {let r=import(&p,id,"legacy-tenant","legacy-user",&s).await?;assert_eq!(r.get::<String,_>("import_status"),"ready");assert_eq!(r.get::<i32,_>("source_history_count"),2);}
 assert_eq!(count(&p,"assistant_runtime_items",id).await?,2);
 let r=sqlx::query("SELECT event_type,payload FROM assistant_runtime_items WHERE runtime_thread_id=$1 ORDER BY sequence LIMIT 1").bind(id).fetch_one(&p).await?;assert_eq!(r.get::<String,_>("event_type"),"rollout/item");let v:Value=r.get("payload");assert_eq!(v["type"],"response_item");assert_eq!(v["payload"]["type"],"message");
 assert_eq!(sqlx::query_scalar::<_,Value>("SELECT history FROM sessions WHERE session_id=$1").bind(&s).fetch_one(&p).await?,history);
 state(import(&p,Uuid::new_v4(),"other-tenant","legacy-user",&s).await,"42501");
 let (a,b)=tokio::join!(import(&p,id,"legacy-tenant","legacy-user",&s),import(&p,id,"legacy-tenant","legacy-user",&s));for r in [a?,b?]{assert_eq!(r.get::<String,_>("import_status"),"ready");}Ok(())}).await
}
#[tokio::test]
#[ignore = "isolated PostgreSQL contract; run explicitly"]
async fn legacy_import_refuses_in_flight_run_and_unconsumed_approval() -> Result<()> {
    fixture(|p|async move{
 for status in ["running","blocked","approved"] {let s=session(&p,"running-tenant","running-user",json!([])).await?;
 if status=="approved" {sqlx::query("INSERT INTO assistant_tool_approvals(approval_id,tenant_id,user_id,session_id,tool_name,status) VALUES($1,'running-tenant','running-user',$2,'write_tool','approved')").bind(Uuid::new_v4()).bind(&s).execute(&p).await?;}else{sqlx::query("INSERT INTO assistant_runs(run_id,tenant_id,user_id,session_id,status) VALUES($1,'running-tenant','running-user',$2,$3)").bind(Uuid::new_v4()).bind(&s).bind(status).execute(&p).await?;}
 state(import(&p,Uuid::new_v4(),"running-tenant","running-user",&s).await,"55000");}Ok(())}).await
}
#[tokio::test]
#[ignore = "isolated PostgreSQL contract; run explicitly"]
async fn legacy_import_preserves_paired_tool_history_and_hashed_approval_receipt() -> Result<()> {
    fixture(|p|async move{
 let history=json!([{"role":"user","content":"look up the account"},{"role":"assistant","content":"The account is active.","metadata":{"tool_calls":[{"id":"call-1","name":"account_lookup","arguments":{"account_id":"acct-1"},"status":"completed"}],"tool_results":[{"tool_call_id":"call-1","name":"account_lookup","result":{"active":true},"error":null}]}}]);
 let s=session(&p,"legacy-tenant","legacy-user",history.clone()).await?;let id=Uuid::new_v4();let approval=Uuid::new_v4();
 sqlx::query("INSERT INTO assistant_tool_approvals(approval_id,tenant_id,user_id,session_id,run_id,tool_name,arguments,status,approved_by,approved_at) VALUES($1,'legacy-tenant','legacy-user',$2,$3,'account_lookup','{\"account_id\":\"acct-1\"}'::jsonb,'consumed','legacy-user',NOW())").bind(approval).bind(&s).bind(Uuid::new_v4()).execute(&p).await?;
 assert_eq!(import(&p,id,"legacy-tenant","legacy-user",&s).await?.get::<String,_>("import_status"),"ready");
 let payloads=sqlx::query_scalar::<_,Value>("SELECT payload FROM assistant_runtime_items WHERE runtime_thread_id=$1 AND event_type='rollout/item' ORDER BY sequence").bind(id).fetch_all(&p).await?;
 assert_eq!(payloads.iter().map(|x|x["payload"]["type"].as_str().unwrap()).collect::<Vec<_>>(),vec!["message","function_call","function_call_output","message"]);
 assert_eq!(payloads[1]["payload"],json!({"type":"function_call","name":"account_lookup","arguments":"{\"account_id\": \"acct-1\"}","call_id":"call-1"}));assert_eq!(payloads[2]["payload"]["call_id"],"call-1");assert_eq!(serde_json::from_str::<Value>(payloads[2]["payload"]["output"].as_str().unwrap())?,json!({"active":true}));
 let receipt=sqlx::query_scalar::<_,Value>("SELECT payload FROM assistant_runtime_items WHERE runtime_thread_id=$1 AND event_type='agent-runtime/legacy_approval'").bind(id).fetch_one(&p).await?;assert_eq!(receipt["approval_id"],approval.to_string());assert!(!receipt["arguments_sha256"].as_str().unwrap().is_empty());assert!(receipt.get("arguments").is_none());
 let projection=sqlx::query_scalar::<_,Value>("SELECT projection->'legacy_import' FROM assistant_runtime_thread_projections WHERE runtime_thread_id=$1").bind(id).fetch_one(&p).await?;assert_eq!(projection["normalizer_version"],2);assert_eq!(projection["tool_call_count"],1);assert_eq!(projection["approval_receipt_count"],1);
 assert_eq!(sqlx::query_scalar::<_,Value>("SELECT history FROM sessions WHERE session_id=$1").bind(&s).fetch_one(&p).await?,history);Ok(())}).await
}
#[tokio::test]
#[ignore = "isolated PostgreSQL contract; run explicitly"]
async fn ready_legacy_import_rejects_source_history_drift() -> Result<()> {
    fixture(|p|async move{
 let s=session(&p,"legacy-tenant","legacy-user",json!([{"role":"user","content":"before"}])).await?;let id=Uuid::new_v4();let first=import(&p,id,"legacy-tenant","legacy-user",&s).await?;
 sqlx::query("UPDATE sessions SET history=history||'[{\"role\":\"assistant\",\"content\":\"after\"}]'::jsonb WHERE session_id=$1").bind(&s).execute(&p).await?;
 message(import(&p,id,"legacy-tenant","legacy-user",&s).await,"IMPORT_SOURCE_CHANGED");let r=sqlx::query("SELECT import_status,source_history_count,source_history_sha256 FROM assistant_runtime_threads WHERE runtime_thread_id=$1").bind(id).fetch_one(&p).await?;assert_eq!(r.get::<String,_>("import_status"),"ready");assert_eq!(r.get::<i32,_>("source_history_count"),1);assert_eq!(r.get::<String,_>("source_history_sha256"),first.get::<String,_>("source_history_sha256"));assert_eq!(count(&p,"assistant_runtime_items",id).await?,1);Ok(())}).await
}
#[tokio::test]
#[ignore = "isolated PostgreSQL contract; run explicitly"]
async fn legacy_import_rejects_unpaired_tool_history_atomically() -> Result<()> {
    fixture(|p|async move{
 for (calls,results) in [(json!([{"id":"orphan-call","name":"lookup","arguments":{}}]),json!([])),(json!([]),json!([{"tool_call_id":"orphan-result","name":"lookup","result":"x"}]))]{let s=session(&p,"legacy-tenant","legacy-user",json!([{"role":"assistant","content":"","metadata":{"tool_calls":calls,"tool_results":results}}])).await?;let id=Uuid::new_v4();message(import(&p,id,"legacy-tenant","legacy-user",&s).await,"IMPORT_TOOL_PAIRING_INVALID");assert_eq!(count(&p,"assistant_runtime_threads",id).await?,0);assert_eq!(count(&p,"assistant_runtime_items",id).await?,0);}Ok(())}).await
}
#[tokio::test]
#[ignore = "isolated PostgreSQL contract; run explicitly"]
async fn atomic_append_is_idempotent_gap_free_and_scope_bound() -> Result<()> {
    fixture(|p|async move{
 let (id,s)=seed(&p).await?;let event=Uuid::new_v4();for _ in 0..2{assert_eq!(append(&p,id,&s,"tenant-a",event,"event-1",json!({"text":"hello"})).await?,1);}
 state(append(&p,id,&s,"tenant-a",Uuid::new_v4(),"event-1",json!({"text":"different"})).await,"23505");state(append(&p,id,&s,"tenant-b",Uuid::new_v4(),"cross-tenant",json!({"text":"hello"})).await,"42501");
 let mut tasks=tokio::task::JoinSet::new();for i in 0..20 {let p=p.clone();let s=s.clone();tasks.spawn(async move {append(&p,id,&s,"tenant-a",Uuid::new_v4(),&format!("parallel-{i}"),json!({"index":i})).await});}let mut seq=vec![];while let Some(r)=tasks.join_next().await{seq.push(r??);}seq.sort();assert_eq!(seq,(2..22).collect::<Vec<i64>>());
 assert_eq!(sqlx::query_scalar::<_,i64>("SELECT sequence FROM assistant_runtime_items WHERE runtime_thread_id=$1 ORDER BY sequence").bind(id).fetch_all(&p).await?,(1..22).collect::<Vec<i64>>());assert_eq!(sqlx::query_scalar::<_,i64>("SELECT last_sequence FROM assistant_runtime_threads WHERE runtime_thread_id=$1").bind(id).fetch_one(&p).await?,21);Ok(())}).await
}
#[tokio::test]
#[ignore = "isolated PostgreSQL contract; run explicitly"]
async fn snapshots_items_and_run_identity_fail_closed() -> Result<()> {
    fixture(|p|async move{
 let (id,s)=seed(&p).await?;let run=Uuid::new_v4();let snapshot=Uuid::new_v4();let v=json!({"schema_version":"agent-runtime-snapshot/v1","permissions":{}});let mut tx=p.begin().await?;
 sqlx::query("INSERT INTO assistant_runtime_snapshots(snapshot_id,runtime_thread_id,run_id,tenant_id,user_id,session_id,schema_version,snapshot,snapshot_sha256) VALUES($1,$2,$3,'tenant-a','user-a',$4,'agent-runtime-snapshot/v1',$5,$6)").bind(snapshot).bind(id).bind(run).bind(&s).bind(&v).bind(hash(&v)).execute(&mut *tx).await?;
 sqlx::query("INSERT INTO assistant_runs(run_id,tenant_id,user_id,session_id,engine,harness_thread_id,harness_turn_id,runtime_snapshot_id,kernel_revision,capability_revision) VALUES($1,'tenant-a','user-a',$2,'agent_runtime',$3,'turn-1',$4,'fork-sha',1)").bind(run).bind(&s).bind(id).bind(snapshot).execute(&mut *tx).await?;tx.commit().await?;
 sqlx::query("INSERT INTO assistant_runtime_snapshot_revocations(snapshot_id,tenant_id,user_id,session_id,reason_code,revoked_by) VALUES($1,'tenant-a','user-a',$2,'lease_revoked','test-operator')").bind(snapshot).bind(&s).execute(&p).await?;
 state(sqlx::query("UPDATE assistant_runtime_snapshot_revocations SET reason_code='changed' WHERE snapshot_id=$1").bind(snapshot).execute(&p).await,"55000");state(sqlx::query("DELETE FROM assistant_runtime_snapshots WHERE snapshot_id=$1").bind(snapshot).execute(&p).await,"55000");
 state(sqlx::query("INSERT INTO assistant_runs(run_id,tenant_id,user_id,session_id,engine) VALUES($1,'tenant-a','user-a',$2,'agent_runtime')").bind(Uuid::new_v4()).bind(&s).execute(&p).await,"23514");
 let event=Uuid::new_v4();append(&p,id,&s,"tenant-a",event,"immutable-item",json!({"text":"immutable"})).await?;state(sqlx::query("UPDATE assistant_runtime_items SET status='completed' WHERE event_id=$1").bind(event).execute(&p).await,"55000");Ok(())}).await
}
#[tokio::test]
#[ignore = "isolated PostgreSQL contract; run explicitly"]
async fn runtime_model_lease_is_atomic_bounded_and_replay_safe() -> Result<()> {
    fixture(|p|async move{
 let (id,s)=seed(&p).await?;let run=Uuid::new_v4();let snapshot=Uuid::new_v4();let lease=Uuid::new_v4();let v=json!({"schema_version":"agent-runtime-snapshot/v1","model":{"id":"model-a","provider_id":"provider-a"},"capability_revision":3});let expires=chrono::Utc::now()+chrono::Duration::minutes(5);
 sqlx::query("INSERT INTO assistant_session_runtime_assignments(tenant_id,user_id,session_id,runtime_owner,kernel_revision) VALUES('tenant-a','user-a',$1,'agent_runtime','fork-sha')").bind(&s).execute(&p).await?;
 sqlx::query("SELECT issue_assistant_runtime_turn($1,$2,$3,$4,'tenant-a','user-a',$5,'fork-sha','agent-runtime-snapshot/v1',$6,$7,3,'balanced','agent-runtime-model-lease/v1','provider-a','model-a','provider-revision-a',$8,2,1000,500,1000000,$9,'hello')").bind(snapshot).bind(lease).bind(run).bind(id).bind(&s).bind(&v).bind(hash(&v)).bind("a".repeat(64)).bind(expires).execute(&p).await?;
 let r=sqlx::query("SELECT engine,harness_thread_id,harness_turn_id,runtime_snapshot_id FROM assistant_runs WHERE run_id=$1").bind(run).fetch_one(&p).await?;assert_eq!(r.get::<String,_>("engine"),"agent_runtime");assert_eq!(r.get::<Uuid,_>("harness_thread_id"),id);assert_eq!(r.get::<String,_>("harness_turn_id"),run.to_string());assert_eq!(r.get::<Uuid,_>("runtime_snapshot_id"),snapshot);
 let call=Uuid::new_v4();sqlx::query("SELECT reserve_assistant_runtime_model_call($1,$2,$3,100,200,250000)").bind(call).bind(lease).bind("b".repeat(64)).execute(&p).await?;state(sqlx::query("SELECT reserve_assistant_runtime_model_call($1,$2,$3,100,200,250000)").bind(Uuid::new_v4()).bind(lease).bind("b".repeat(64)).execute(&p).await,"55000");
 sqlx::query("UPDATE assistant_runtime_model_calls SET status='dispatched',dispatched_at=NOW(),updated_at=NOW() WHERE call_id=$1 AND status='reserved'").bind(call).execute(&p).await?;sqlx::query("SELECT complete_assistant_runtime_model_call($1,90,50,120000,'provider-request')").bind(call).execute(&p).await?;
 let r=sqlx::query("SELECT calls_reserved,calls_completed,used_input_tokens,used_output_tokens,used_cost_microusd FROM assistant_runtime_model_leases WHERE lease_id=$1").bind(lease).fetch_one(&p).await?;assert_eq!(r.get::<i32,_>("calls_reserved"),1);assert_eq!(r.get::<i32,_>("calls_completed"),1);for (k,n) in [("used_input_tokens",90),("used_output_tokens",50),("used_cost_microusd",120000)]{assert_eq!(r.get::<i64,_>(k),n);}
 state(sqlx::query("SELECT issue_assistant_runtime_turn($1,$2,$3,$4,'tenant-b','user-a',$5,'fork-sha','agent-runtime-snapshot/v1','{}'::jsonb,$6,1,'auto','agent-runtime-model-lease/v1','provider-a','model-a','provider-revision-a',$7,1,100,100,1000,$8,'cross-tenant')").bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(Uuid::new_v4()).bind(id).bind(&s).bind(hash(&json!({}))).bind("c".repeat(64)).bind(expires).execute(&p).await,"42501");Ok(())}).await
}
