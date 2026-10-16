#![allow(dead_code)]
use serde_json::{Value, json};
use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use uuid::Uuid;
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_owned()
}
pub fn options() -> Result<PgConnectOptions> {
    let env: BTreeMap<String, String> =
        dotenvy::from_path_iter(".env")?.collect::<std::result::Result<_, _>>()?;
    let required = |k: &str| {
        env.get(k)
            .ok_or_else(|| format!("missing local database setting {k}"))
    };
    Ok(PgConnectOptions::new()
        .host("127.0.0.1")
        .port(
            env.get("POSTGRES_PORT")
                .filter(|x| !x.is_empty())
                .map(|x| x.parse())
                .transpose()?
                .unwrap_or(5432),
        )
        .username(required("POSTGRES_USER")?)
        .password(required("POSTGRES_PASSWORD")?)
        .database(
            env.get("POSTGRES_DB")
                .filter(|x| !x.is_empty())
                .map(String::as_str)
                .unwrap_or("gateway"),
        ))
}
pub async fn connect() -> Result<PgConnection> {
    Ok(PgConnection::connect_with(&options()?).await?)
}
pub fn write(path: &Path, value: &Value) -> Result<()> {
    if let Some(p) = path.parent() {
        if !p.as_os_str().is_empty() {
            std::fs::create_dir_all(p)?;
        }
    }
    std::fs::write(path, format!("{}\n", serde_json::to_string_pretty(value)?))?;
    Ok(())
}
pub fn core(items: &[Value], run: Uuid) -> Value {
    let mut active = false;
    let (mut started, mut complete, mut aborted) = (0, 0, 0);
    let mut calls = BTreeMap::<String, u64>::new();
    let mut outputs = BTreeMap::<String, u64>::new();
    for item in items {
        let p = &item["payload"];
        let kind = p["type"].as_str().unwrap_or("");
        if item["type"] == "event_msg" {
            if kind == "task_started" {
                active = p["turn_id"] == run.to_string();
                started += i32::from(active);
            }
            if p["turn_id"] == run.to_string() {
                complete += i32::from(kind == "task_complete");
                aborted += i32::from(kind == "turn_aborted");
            }
        } else if active && item["type"] == "response_item" {
            let call = p["call_id"].as_str().unwrap_or("null").to_string();
            if kind == "function_call" {
                *calls.entry(call.clone()).or_default() += 1;
            }
            if kind == "function_call_output" {
                *outputs.entry(call).or_default() += 1;
            }
        }
    }
    json!({"started":started,"complete":complete,"aborted":aborted,"calls":calls,"outputs":outputs})
}
pub async fn observe(run: Uuid, path: &Path) -> Result<Value> {
    let mut db = connect().await?;
    let result = observe_on(&mut db, run, path).await;
    db.close().await?;
    result
}
pub async fn observe_on(db: &mut PgConnection, run: Uuid, path: &Path) -> Result<Value> {
    let run_row:Value=sqlx::query_scalar("SELECT row_to_json(q) FROM (SELECT r.run_id,r.status,r.session_id,r.harness_thread_id,o.owner_id,o.fence,l.lease_id,l.snapshot_id,l.model_id,l.status AS lease_status FROM assistant.assistant_runs r JOIN assistant.assistant_runtime_execution_owners o ON o.run_id=r.run_id JOIN assistant.assistant_runtime_model_leases l ON l.run_id=r.run_id WHERE r.run_id=$1) q").bind(run).fetch_optional(&mut *db).await?.ok_or("Original durable run not found")?;
    let mut approvals=sqlx::query_scalar::<_,Value>("SELECT row_to_json(q) FROM (SELECT approval_id,status,tool_call_id,expires_at FROM assistant.assistant_tool_approvals WHERE run_id=$1 ORDER BY created_at) q").bind(run).fetch_all(&mut *db).await?;
    for row in &mut approvals {
        if let Some(raw) = row["expires_at"].as_str() {
            let dt = chrono::DateTime::parse_from_rfc3339(raw)?.with_timezone(&chrono::Utc);
            row["expires_at"] = Value::String(if dt.timestamp_subsec_micros() == 0 {
                dt.format("%Y-%m-%d %H:%M:%S+00:00").to_string()
            } else {
                dt.format("%Y-%m-%d %H:%M:%S%.6f+00:00").to_string()
            });
        }
    }
    let invocations=sqlx::query_scalar::<_,Value>("SELECT row_to_json(q) FROM (SELECT call_id,params->>'tool' AS tool,response IS NOT NULL AS has_response FROM assistant.assistant_runtime_invocations WHERE run_id=$1 ORDER BY created_at) q").bind(run).fetch_all(&mut *db).await?;
    let executions=sqlx::query_scalar::<_,Value>("SELECT row_to_json(q) FROM (SELECT execution_id,tool_call_id,attempt_id,capability_id,status,dispatch_fence,result_summary IS NOT NULL AS has_receipt FROM assistant.assistant_capability_executions WHERE run_id=$1 ORDER BY created_at) q").bind(run).fetch_all(&mut *db).await?;
    let items=sqlx::query_scalar::<_,Value>("SELECT payload FROM assistant.assistant_runtime_items WHERE kernel_thread_id=$1 AND event_type='rollout/item' ORDER BY sequence").bind(run_row["harness_thread_id"].as_str().ok_or("missing harness thread")?.parse::<Uuid>()?).fetch_all(&mut *db).await?;
    let facts = json!({"run":run_row,"approvals":approvals,"invocations":invocations,"executions":executions,"core":core(&items,run)});
    write(path, &facts)?;
    println!("{}", serde_json::to_string(&facts)?);
    Ok(facts)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn core_tracks_only_active_original_turn() {
        let id = Uuid::nil();
        let rows = vec![
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":id.to_string()}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"a"}}),
            json!({"type":"response_item","payload":{"type":"function_call_output","call_id":"a"}}),
            json!({"type":"event_msg","payload":{"type":"task_complete","turn_id":id.to_string()}}),
            json!({"type":"event_msg","payload":{"type":"task_started","turn_id":"other"}}),
            json!({"type":"response_item","payload":{"type":"function_call","call_id":"b"}}),
        ];
        assert_eq!(
            core(&rows, id),
            json!({"started":1,"complete":1,"aborted":0,"calls":{"a":1},"outputs":{"a":1}})
        );
    }
}
