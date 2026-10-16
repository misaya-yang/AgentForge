//! Publication DDL contract against a disposable database, never a shared schema.
mod agent_studio_support;
use agent_studio_support::*;
use futures_util::FutureExt;
use sqlx::{
    PgPool, Row,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::collections::{BTreeMap, BTreeSet};
#[test]
fn release_migrations_are_forward_only() {
    for file in [
        "077_agent_publication_eval.sql",
        "078_agent_release_lifecycle_hardening.sql",
    ] {
        let s = sql(file).to_uppercase();
        assert!(!s.contains("DROP TABLE"));
        assert!(!s.contains("TRUNCATE"));
    }
}
async fn release_fixture<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce(PgPool) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let env = dotenvy::from_path_iter(root().join(".env"))?
        .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
    let get = |k: &str| {
        env.get(k)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| format!("missing isolated PostgreSQL setting {k}"))
    };
    let options = PgConnectOptions::new()
        .host("127.0.0.1")
        .port(get("POSTGRES_PORT")?.parse()?)
        .username(get("POSTGRES_USER")?)
        .password(get("POSTGRES_PASSWORD")?)
        .database(get("POSTGRES_DB")?);
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(options.clone())
        .await?;
    let name = format!("agent_release_test_{}", uuid::Uuid::new_v4().simple());
    sqlx::raw_sql(&format!("CREATE DATABASE \"{name}\""))
        .execute(&admin)
        .await?;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .after_connect(|c, _| {
            Box::pin(async move {
                sqlx::query("SET search_path=gateway,public")
                    .execute(c)
                    .await?;
                Ok(())
            })
        })
        .connect_with(options.database(&name))
        .await?;
    let p = pool.clone();
    let outcome = std::panic::AssertUnwindSafe(async move {
        sqlx::raw_sql("CREATE SCHEMA gateway").execute(&p).await?;
        sqlx::raw_sql(include_str!("agent_studio_support/base.sql"))
            .execute(&p)
            .await?;
        sqlx::raw_sql(&sql("071_agent_studio_domain.sql"))
            .execute(&p)
            .await?;
        sqlx::raw_sql(include_str!("agent_studio_support/release_base.sql"))
            .execute(&p)
            .await?;
        for file in [
            "077_agent_publication_eval.sql",
            "078_agent_release_lifecycle_hardening.sql",
        ] {
            for _ in 0..2 {
                sqlx::raw_sql(&sql(file)).execute(&p).await?;
            }
        }
        sqlx::raw_sql(&sql("2026_08_post_kb_v1/007_agent_publication_expiry.sql"))
            .execute(&p)
            .await?;
        test(p).await
    })
    .catch_unwind()
    .await;
    pool.close().await;
    sqlx::raw_sql(&format!("DROP DATABASE \"{name}\""))
        .execute(&admin)
        .await?;
    admin.close().await;
    match outcome {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e),
    }
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL publication contract"]
async fn release_migration_is_idempotent_and_lifecycle_tenant_constraints_are_exact() -> Result<()>
{
    release_fixture(|p|async move{
 let tables=sqlx::query_scalar::<_,String>("SELECT table_name FROM information_schema.tables WHERE table_schema=current_schema() AND table_name LIKE 'agent_release%'").fetch_all(&p).await?.into_iter().collect::<BTreeSet<_>>();assert_eq!(tables,["agent_release_evaluations","agent_release_evaluation_events","agent_release_requests"].map(str::to_owned).into_iter().collect());
 let triggers=sqlx::query_scalar::<_,String>("SELECT tgname FROM pg_trigger WHERE NOT tgisinternal AND tgrelid IN ('agent_release_evaluations'::regclass,'agent_release_evaluation_events'::regclass,'agent_release_requests'::regclass)").fetch_all(&p).await?.into_iter().collect::<BTreeSet<_>>();assert_eq!(triggers,["agent_release_evaluations_immutable","agent_release_evaluation_events_immutable","agent_release_requests_immutable"].map(str::to_owned).into_iter().collect());
 let rows=sqlx::query("SELECT column_name,is_nullable,column_default FROM information_schema.columns WHERE table_schema=current_schema() AND table_name='agent_release_evaluations' AND column_name IN ('dataset_version','dataset_manifest_hash','evaluation_identity_hash','started_at','completed_at')").fetch_all(&p).await?;assert_eq!(rows.iter().map(|r|r.get::<String,_>("column_name")).collect::<BTreeSet<_>>(),["dataset_version","dataset_manifest_hash","evaluation_identity_hash","started_at","completed_at"].map(str::to_owned).into_iter().collect());let completed=rows.iter().find(|r|r.get::<String,_>("column_name")=="completed_at").unwrap();assert_eq!(completed.get::<String,_>("is_nullable"),"YES");assert!(completed.get::<Option<String>,_>("column_default").is_none());
 let constraints=sqlx::query("SELECT conname,pg_get_constraintdef(oid) AS definition FROM pg_constraint WHERE conrelid='agent_release_evaluations'::regclass AND conname IN ('agent_release_evaluations_status_check','agent_release_evaluations_dataset_tenant_fk','agent_release_evaluations_run_tenant_fk')").fetch_all(&p).await?.into_iter().map(|r|(r.get::<String,_>("conname"),r.get::<String,_>("definition"))).collect::<BTreeMap<_,_>>();assert_eq!(constraints.len(),3);for s in ["queued","running","cancelled"]{assert!(constraints["agent_release_evaluations_status_check"].contains(s));}assert!(constraints["agent_release_evaluations_dataset_tenant_fk"].contains("FOREIGN KEY (tenant_id, dataset_id)"));assert!(constraints["agent_release_evaluations_run_tenant_fk"].contains("FOREIGN KEY (tenant_id, experiment_run_id)"));Ok(())}).await
}

use agent_studio_support::state::TestState;
use ai_platform_gateway::migration_agent::{Actor, routes::AgentState};
use serde_json::{Value, json};
async fn release_state_fixture<F, Fut>(test: F) -> Result<()>
where
    F: FnOnce(PgPool, TestState) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    release_fixture(|pool| async move {
        let graph = agent_studio_support::state::state(pool.clone()).await?;
        let control = graph.control_pool.clone();
        let outcome = std::panic::AssertUnwindSafe(test(pool, graph))
            .catch_unwind()
            .await;
        control.close().await;
        match outcome {
            Ok(r) => r,
            Err(e) => std::panic::resume_unwind(e),
        }
    })
    .await
}
fn release_spec(text: &str) -> Value {
    let mut s = spec(text);
    s["model"]["provider_id"] = json!("dashscope");
    s
}
fn gate() -> Value {
    json!({"schema_version":"agent-release-gate/v1","status":"passed","profile_id":"offline_v1","profile_version":"2026-07-19","execution_scope":"provider_free_release_integrity","model_quality_evaluated":false,"blocking_findings":[],"non_blocking_findings":[],"metrics":{"critical_pass_rate":1.0}})
}
async fn create_release_agent(
    pool: &PgPool,
    state: &AgentState,
    suffix: &str,
) -> Result<(Actor, String)> {
    let a = actor(&format!("owner-{suffix}"), &format!("tenant-{suffix}"));
    user(pool, &a.user_id, &a.tenant_id).await?;
    sqlx::query("INSERT INTO llm_providers(tenant_id,provider_id) VALUES($1,'dashscope')")
        .bind(&a.tenant_id)
        .execute(pool)
        .await?;
    sqlx::query("INSERT INTO llm_models(tenant_id,provider_id,model_id) VALUES($1,'dashscope','qwen3.7-plus')").bind(&a.tenant_id).execute(pool).await?;
    let row = state
        .repository
        .create(
            &a,
            &format!("Release {suffix}"),
            Some(&format!("release-{suffix}")),
            "",
            &release_spec("revision one"),
        )
        .await?;
    Ok((
        a,
        row["agent_id"]
            .as_str()
            .ok_or("missing agent ID")?
            .to_owned(),
    ))
}
async fn record_evaluation(state: &AgentState, a: &Actor, id: &str) -> Result<(Value, Value)> {
    let d = state.repository.draft(a, id).await?;
    let candidate = state
        .resolve_release_candidate(
            a,
            id,
            d["revision"].as_i64().ok_or("missing revision")?,
            "api",
            "private",
            &json!({"attachments":false,"high_risk_tools":false,"allowed_origins":[]}),
            None,
        )
        .await?;
    let queued = state
        .create_release_evaluation(a, id, &candidate, &gate())
        .await?;
    let eid = queued["evaluation_id"]
        .as_str()
        .ok_or("missing evaluation ID")?;
    state
        .repository
        .transition_evaluation(a, id, eid, false)
        .await?;
    let terminal = state
        .complete_release_evaluation(a, id, eid, &candidate, &gate())
        .await?;
    Ok((terminal, candidate))
}
fn statuses(row: &Value) -> Vec<&str> {
    row["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["status"].as_str().unwrap())
        .collect()
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis publication contract"]
async fn release_evaluation_lifecycle_is_durable_cancellable_and_terminal() -> Result<()> {
    release_state_fixture(|p, g| async move {
        let (a, id) = create_release_agent(&p, &g.state, "lifecycle").await?;
        let (terminal, candidate) = record_evaluation(&g.state, &a, &id).await?;
        let queued = g
            .state
            .create_release_evaluation(&a, &id, &candidate, &gate())
            .await?;
        assert_eq!(queued["status"], "queued");
        assert!(queued["completed_at"].is_null());
        assert_eq!(statuses(&queued), ["queued"]);
        let eid = queued["evaluation_id"].as_str().unwrap();
        let running = g
            .state
            .repository
            .transition_evaluation(&a, &id, eid, false)
            .await?;
        assert_eq!(running["status"], "running");
        assert_eq!(running["execution_claimed"], true);
        assert_eq!(statuses(&running), ["queued", "running"]);
        let cancelled = g
            .state
            .repository
            .transition_evaluation(&a, &id, eid, true)
            .await?;
        let after = g
            .state
            .complete_release_evaluation(&a, &id, eid, &candidate, &gate())
            .await?;
        assert_eq!(cancelled["status"], "cancelled");
        assert_eq!(after["status"], "cancelled");
        assert_eq!(statuses(&cancelled), ["queued", "running", "cancelled"]);
        error(
            g.state
                .repository
                .transition_evaluation(&a, &id, terminal["evaluation_id"].as_str().unwrap(), true)
                .await,
            409,
            "AGENT_EVAL_TERMINAL",
        );
        Ok(())
    })
    .await
}
async fn publish(
    state: &AgentState,
    a: &Actor,
    id: &str,
    evaluation: &Value,
    key: &str,
) -> Result<Value> {
    Ok(state
        .publish_agent(
            a,
            id,
            key,
            &json!({"evaluation_id":evaluation["evaluation_id"],"reason":"release"}),
        )
        .await?)
}
async fn release_counts(pool: &PgPool, a: &Actor, id: &str) -> Result<Value> {
    Ok(sqlx::query_scalar("SELECT jsonb_build_object('versions',(SELECT count(*) FROM agent_versions WHERE tenant_id=$1 AND agent_id=$2::uuid),'publications',(SELECT count(*) FROM agent_publications WHERE tenant_id=$1 AND agent_id=$2::uuid),'events',(SELECT count(*) FROM agent_publish_events WHERE tenant_id=$1 AND agent_id=$2::uuid),'requests',(SELECT count(*) FROM agent_release_requests WHERE tenant_id=$1 AND agent_id=$2::uuid))").bind(&a.tenant_id).bind(id).fetch_one(pool).await?)
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis publication contract"]
async fn release_evidence_rows_reject_update_and_delete() -> Result<()> {
    release_state_fixture(|p,g|async move {
 let (a,id)=create_release_agent(&p,&g.state,"immutable").await?;
 let (evaluation,_)=record_evaluation(&g.state,&a,&id).await?;
 publish(&g.state,&a,&id,&evaluation,"publish-immutable-0001").await?;
 for (table,column) in [("agent_release_evaluations","profile_id"),("agent_release_evaluation_events","summary"),("agent_release_requests","request_hash")] {
  for statement in [format!("UPDATE {table} SET {column}={column} WHERE tenant_id=$1"),format!("DELETE FROM {table} WHERE tenant_id=$1")] {
   let err=sqlx::query(&statement).bind(&a.tenant_id).execute(&p).await.unwrap_err();assert!(err.to_string().contains("AGENT_RELEASE_EVIDENCE_IMMUTABLE"));
  }
 }
 let counts:Value=sqlx::query_scalar("SELECT jsonb_build_object('evaluations',(SELECT count(*) FROM agent_release_evaluations WHERE tenant_id=$1),'events',(SELECT count(*) FROM agent_release_evaluation_events WHERE tenant_id=$1),'requests',(SELECT count(*) FROM agent_release_requests WHERE tenant_id=$1))").bind(&a.tenant_id).fetch_one(&p).await?;
 assert_eq!(counts,json!({"evaluations":1,"events":3,"requests":1}));Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis publication contract"]
async fn publish_is_idempotent_and_event_failure_rolls_back_every_row() -> Result<()> {
    release_state_fixture(|p,g|async move {
 let (a,id)=create_release_agent(&p,&g.state,"atomic").await?;
 let (evaluation,_)=record_evaluation(&g.state,&a,&id).await?;
 let first=publish(&g.state,&a,&id,&evaluation,"publish-atomic-0001").await?;
 let replay=publish(&g.state,&a,&id,&evaluation,"publish-atomic-0001").await?;
 assert_eq!(replay["idempotent_replay"],true);assert_eq!(first["version"]["agent_version_id"],replay["version"]["agent_version_id"]);assert_eq!(first["event"]["event_id"],replay["event"]["event_id"]);
 g.state.repository.update_draft(&a,&id,1,&release_spec("changed after committed release"),&json!({})).await?;
 let durable=publish(&g.state,&a,&id,&evaluation,"publish-atomic-0001").await?;assert_eq!(durable["idempotent_replay"],true);assert_eq!(durable["event"]["event_id"],first["event"]["event_id"]);
 error(g.state.publish_agent(&a,&id,"publish-atomic-0001",&json!({"evaluation_id":evaluation["evaluation_id"],"reason":"different payload"})).await,409,"AGENT_RELEASE_IDEMPOTENCY_CONFLICT");
 let (b,bid)=create_release_agent(&p,&g.state,"event-failure").await?;let (be,_)=record_evaluation(&g.state,&b,&bid).await?;
 sqlx::raw_sql("CREATE FUNCTION reject_publish_event_for_atomicity() RETURNS trigger AS $$ BEGIN RAISE EXCEPTION 'synthetic publish event failure'; END; $$ LANGUAGE plpgsql; CREATE TRIGGER reject_publish_event_for_atomicity BEFORE INSERT ON agent_publish_events FOR EACH ROW EXECUTE FUNCTION reject_publish_event_for_atomicity();").execute(&p).await?;
 let failure=publish(&g.state,&b,&bid,&be,"publish-failure-0001").await;assert!(failure.is_err());
 assert_eq!(release_counts(&p,&b,&bid).await?,json!({"versions":0,"publications":0,"events":0,"requests":0}));
 sqlx::raw_sql("DROP TRIGGER reject_publish_event_for_atomicity ON agent_publish_events; DROP FUNCTION reject_publish_event_for_atomicity()").execute(&p).await?;
 Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis publication contract"]
async fn stale_evaluation_cannot_create_version_or_pointer() -> Result<()> {
    release_state_fixture(|p, g| async move {
        let (a, id) = create_release_agent(&p, &g.state, "stale").await?;
        let (e, _) = record_evaluation(&g.state, &a, &id).await?;
        g.state
            .repository
            .update_draft(&a, &id, 1, &release_spec("revision two"), &json!({}))
            .await?;
        let failed = g
            .state
            .publish_agent(
                &a,
                &id,
                "publish-stale-0001",
                &json!({"evaluation_id":e["evaluation_id"],"reason":"release"}),
            )
            .await;
        error(failed, 409, "AGENT_EVAL_STALE");
        assert_eq!(
            release_counts(&p, &a, &id).await?,
            json!({"versions":0,"publications":0,"events":0,"requests":0})
        );
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis publication contract"]
async fn publish_rechecks_model_and_provider_before_any_release_write() -> Result<()> {
    release_state_fixture(|p, g| async move {
        for (suffix, statement) in [
            (
                "model-disabled",
                "UPDATE llm_models SET is_enabled=false WHERE tenant_id=$1",
            ),
            (
                "provider-disabled",
                "UPDATE llm_providers SET is_enabled=false WHERE tenant_id=$1",
            ),
            (
                "model-access",
                "UPDATE llm_models SET access_level='admin' WHERE tenant_id=$1",
            ),
        ] {
            let (a, id) = create_release_agent(&p, &g.state, suffix).await?;
            let (e, _) = record_evaluation(&g.state, &a, &id).await?;
            sqlx::query(statement)
                .bind(&a.tenant_id)
                .execute(&p)
                .await?;
            let result = g
                .state
                .publish_agent(
                    &a,
                    &id,
                    &format!("publish-{suffix}-0001"),
                    &json!({"evaluation_id":e["evaluation_id"],"reason":"release"}),
                )
                .await;
            assert!(
                result.is_err(),
                "disabled/unauthorized model must not publish"
            );
            assert_eq!(
                release_counts(&p, &a, &id).await?,
                json!({"versions":0,"publications":0,"events":0,"requests":0})
            );
        }
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis publication contract"]
async fn publish_rechecks_provider_readiness_and_preserves_all_release_rows() -> Result<()> {
    release_state_fixture(|p, g| async move {
        let (a, id) = create_release_agent(&p, &g.state, "provider-readiness").await?;
        let (e, _) = record_evaluation(&g.state, &a, &id).await?;
        g.providers
            .configured
            .store(false, std::sync::atomic::Ordering::SeqCst);
        error(
            g.state
                .publish_agent(
                    &a,
                    &id,
                    "readiness-unavailable-0001",
                    &json!({"evaluation_id":e["evaluation_id"],"reason":"release"}),
                )
                .await,
            503,
            "AGENT_RUNTIME_PROVIDER_UNAVAILABLE",
        );
        assert_eq!(
            release_counts(&p, &a, &id).await?,
            json!({"versions":0,"publications":0,"events":0,"requests":0})
        );
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis publication contract"]
async fn eval_dataset_manifest_is_tenant_bound_and_content_stale() -> Result<()> {
    release_state_fixture(|p,g|async move {
 let(a,id)=create_release_agent(&p,&g.state,"eval-dataset").await?;
 let dataset=uuid::Uuid::new_v4().to_string();let example=uuid::Uuid::new_v4().to_string();
 sqlx::query("INSERT INTO eval_datasets(dataset_id,tenant_id,name,version,created_by) VALUES($1::uuid,$2,'Release cases','release-v3',$3)").bind(&dataset).bind(&a.tenant_id).bind(&a.user_id).execute(&p).await?;
 sqlx::query("INSERT INTO eval_examples(example_id,dataset_id,tenant_id,input,expected_output,metadata,created_by) VALUES($1::uuid,$2::uuid,$3,'{\"message\":\"classify\"}','{\"queue\":\"billing\"}','{\"case_id\":\"billing-001\"}',$4)").bind(&example).bind(&dataset).bind(&a.tenant_id).bind(&a.user_id).execute(&p).await?;
 let policy=json!({"attachments":false,"high_risk_tools":false,"allowed_origins":[]});
 let candidate=g.state.resolve_release_candidate(&a,&id,1,"api","private",&policy,Some(&dataset)).await?;
 assert_eq!(candidate["dataset_version"],"release-v3");assert_eq!(candidate["dataset_manifest_hash"].as_str().unwrap().len(),64);
 let queued=g.state.create_release_evaluation(&a,&id,&candidate,&gate()).await?;assert_eq!(queued["dataset_id"],dataset);assert_eq!(queued["dataset_manifest_hash"],candidate["dataset_manifest_hash"]);
 let(b,bid)=create_release_agent(&p,&g.state,"foreign-dataset").await?;
 assert!(g.state.resolve_release_candidate(&b,&bid,1,"api","private",&policy,Some(&dataset)).await.is_err());
 sqlx::query("UPDATE eval_examples SET expected_output='{\"queue\":\"security\"}' WHERE tenant_id=$1 AND example_id=$2::uuid").bind(&a.tenant_id).bind(&example).execute(&p).await?;
 let changed=g.state.resolve_release_candidate(&a,&id,1,"api","private",&policy,Some(&dataset)).await?;assert_ne!(changed["dataset_manifest_hash"],candidate["dataset_manifest_hash"]);
 let eid=queued["evaluation_id"].as_str().unwrap();g.state.repository.transition_evaluation(&a,&id,eid,false).await?;
 error(g.state.complete_release_evaluation(&a,&id,eid,&candidate,&gate()).await,409,"AGENT_EVAL_DATASET_STALE");
 let persisted=g.state.repository.release_evaluation(&a,&id,eid,"owner").await?;assert_eq!(persisted["status"],"running");Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis publication contract"]
async fn same_tenant_key_serializes_across_different_agents() -> Result<()> {
    release_state_fixture(|p, g| async move {
        let (a, first) = create_release_agent(&p, &g.state, "global-key").await?;
        let second = g
            .state
            .repository
            .create(
                &a,
                "Second release target",
                Some("second-release-target"),
                "",
                &release_spec("second Agent"),
            )
            .await?;
        let second = second["agent_id"].as_str().unwrap();
        let (e1, _) = record_evaluation(&g.state, &a, &first).await?;
        let (e2, _) = record_evaluation(&g.state, &a, second).await?;
        let payload1 = json!({"evaluation_id":e1["evaluation_id"],"reason":"release"});
        let payload2 = json!({"evaluation_id":e2["evaluation_id"],"reason":"release"});
        let (r1, r2) = tokio::join!(
            g.state
                .publish_agent(&a, &first, "tenant-global-key-0001", &payload1),
            g.state
                .publish_agent(&a, second, "tenant-global-key-0001", &payload2)
        );
        assert_eq!(usize::from(r1.is_ok()) + usize::from(r2.is_ok()), 1);
        let failure = if r1.is_err() { r1 } else { r2 };
        error(failure, 409, "AGENT_RELEASE_IDEMPOTENCY_CONFLICT");
        for table in [
            "agent_versions",
            "agent_publications",
            "agent_publish_events",
            "agent_release_requests",
        ] {
            let count: i64 =
                sqlx::query_scalar(&format!("SELECT count(*) FROM {table} WHERE tenant_id=$1"))
                    .bind(&a.tenant_id)
                    .fetch_one(&p)
                    .await?;
            assert_eq!(count, 1, "{table}");
        }
        Ok(())
    })
    .await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis publication contract"]
async fn rollback_rechecks_revocation_and_preserves_pointer_on_failure() -> Result<()> {
    release_state_fixture(|p,g|async move {
 let(a,id)=create_release_agent(&p,&g.state,"rollback").await?;let(e1,_)=record_evaluation(&g.state,&a,&id).await?;let first=publish(&g.state,&a,&id,&e1,"publish-rollback-0001").await?;
 g.state.repository.update_draft(&a,&id,1,&release_spec("revision two"),&json!({})).await?;let(e2,_)=record_evaluation(&g.state,&a,&id).await?;let second=publish(&g.state,&a,&id,&e2,"publish-rollback-0002").await?;
 let publication=second["publication"]["publication_id"].as_str().unwrap();let version1=first["version"]["agent_version_id"].as_str().unwrap();let version2=second["version"]["agent_version_id"].as_str().unwrap();
 let payload=json!({"target_version_id":version1,"reason":"known healthy"});
 let rollback=g.state.rollback_publication(&a,publication,"rollback-atomic-0001",&payload).await?;let replay=g.state.rollback_publication(&a,publication,"rollback-atomic-0001",&payload).await?;assert_eq!(rollback["event"]["operation"],"rollback");assert_eq!(replay["event"]["event_id"],rollback["event"]["event_id"]);assert_eq!(replay["idempotent_replay"],true);
 let repromoted=publish(&g.state,&a,&id,&e2,"publish-rollback-0003").await?;assert_eq!(repromoted["publication"]["version_id"],version2);
 sqlx::query("INSERT INTO agent_version_revocations(tenant_id,agent_version_id) VALUES($1,$2::uuid)").bind(&a.tenant_id).bind(version1).execute(&p).await?;let before=release_counts(&p,&a,&id).await?;
 assert!(g.state.rollback_publication(&a,publication,"rollback-atomic-0002",&json!({"target_version_id":version1,"reason":"must fail"})).await.is_err());
 let pointer:String=sqlx::query_scalar("SELECT version_id::text FROM agent_publications WHERE tenant_id=$1 AND publication_id=$2::uuid").bind(&a.tenant_id).bind(publication).fetch_one(&p).await?;assert_eq!(pointer,version2);assert_eq!(release_counts(&p,&a,&id).await?,before);Ok(())}).await
}
async fn management_request(
    g: &TestState,
    jwt: &str,
    method: &str,
    path: &str,
    payload: Value,
) -> Result<(u16, Value)> {
    let request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {jwt}"))
        .header("content-type", "application/json")
        .body(axum::body::Body::from(serde_json::to_vec(&payload)?))?;
    let response = g
        .state
        .public
        .as_ref()
        .unwrap()
        .dispatch(&g.state, Default::default(), request)
        .await;
    let status = response.status().as_u16();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
    Ok((status, serde_json::from_slice(&bytes)?))
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis authenticated HTTP contract"]
async fn management_token_create_list_rotate_revoke_and_past_expiry() -> Result<()> {
    release_state_fixture(|p,g|async move {
 sqlx::raw_sql(include_str!("agent_studio_support/auth_base.sql")).execute(&p).await?;
 sqlx::raw_sql(&sql("079_agent_channel_runtime.sql")).execute(&p).await?;
 let(a,id)=create_release_agent(&p,&g.state,"token-management").await?;
 sqlx::query("INSERT INTO tenants(tenant_id) VALUES($1)").bind(&a.tenant_id).execute(&p).await?;
 let(e,_)=record_evaluation(&g.state,&a,&id).await?;let publication=publish(&g.state,&a,&id,&e,"publish-management-0001").await?;
 let pid=publication["publication"]["publication_id"].as_str().unwrap();let path=format!("/api/v1/publications/{pid}/tokens");
 let(jwt,jti)=agent_studio_support::state::authenticate_fixture(&g,&a).await?;
 let result=std::panic::AssertUnwindSafe(async {
 let(status,created)=management_request(&g,&jwt,"POST",&path,json!({"name":"server","scopes":["chat:write","sessions:write"]})).await?;assert_eq!(status,201);let raw=created["token"].as_str().unwrap();assert!(raw.starts_with("agt_"));assert!(created["token_metadata"].get("token_hash").is_none());let token_id=created["token_metadata"]["token_id"].as_str().unwrap();
 use sha2::{Digest,Sha256};let expected=ai_platform_gateway::migration_agent::spec::hex(&Sha256::digest(raw.as_bytes()));let stored:String=sqlx::query_scalar("SELECT token_hash FROM agent_api_tokens WHERE token_id=$1::uuid").bind(token_id).fetch_one(&p).await?;assert_eq!(stored,expected);assert_ne!(stored,raw);
 let(status,listed)=management_request(&g,&jwt,"GET",&path,json!({})).await?;assert_eq!(status,200);assert!(!listed.to_string().contains(raw));assert!(!listed.to_string().contains("token_hash"));assert_eq!(listed["tokens"].as_array().unwrap().len(),1);
 let(status,rotated)=management_request(&g,&jwt,"POST",&format!("{path}/{token_id}/rotate"),json!({})).await?;assert_eq!(status,201);let new_raw=rotated["token"].as_str().unwrap();assert_ne!(new_raw,raw);assert_eq!(rotated["token_metadata"]["rotated_from_token_id"],token_id);
 let public=g.state.public.as_ref().unwrap();error(public.resolve_api(&g.state.repository,raw,pid,"chat:write",None).await,401,"AGENT_RUNTIME_TOKEN_INVALID");assert!(public.resolve_api(&g.state.repository,new_raw,pid,"chat:write",None).await.is_ok());
 let new_id=rotated["token_metadata"]["token_id"].as_str().unwrap();let(status,revoked)=management_request(&g,&jwt,"DELETE",&format!("{path}/{new_id}"),json!({})).await?;assert_eq!(status,200);assert!(!revoked["revoked_at"].is_null());assert!(!revoked.to_string().contains(new_raw));error(public.resolve_api(&g.state.repository,new_raw,pid,"chat:write",None).await,401,"AGENT_RUNTIME_TOKEN_INVALID");
 let(status,expired)=management_request(&g,&jwt,"POST",&path,json!({"name":"expired","scopes":["chat:write"],"expires_at":(chrono::Utc::now()-chrono::Duration::seconds(1)).to_rfc3339()})).await?;assert_eq!(status,422);assert_eq!(expired["detail"]["code"],"AGENT_RUNTIME_TOKEN_EXPIRY_INVALID");
 Ok::<(),Box<dyn std::error::Error+Send+Sync>>(())
 }).catch_unwind().await;
 agent_studio_support::state::revoke_fixture(&g,&a,&jti).await?;
 match result {Ok(r)=>r,Err(p)=>std::panic::resume_unwind(p)}
 }).await
}
async fn studio_request(
    g: &TestState,
    jwt: &str,
    method: &str,
    path: &str,
    payload: Value,
    key: Option<&str>,
) -> Result<(u16, Value)> {
    use tower::ServiceExt;
    let mut builder = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("authorization", format!("Bearer {jwt}"))
        .header("content-type", "application/json");
    if let Some(key) = key {
        builder = builder.header("idempotency-key", key);
    }
    let response = ai_platform_gateway::migration_agent::router(g.state.clone())
        .oneshot(builder.body(axum::body::Body::from(serde_json::to_vec(&payload)?))?)
        .await?;
    let status = response.status().as_u16();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
    Ok((status, serde_json::from_slice(&body)?))
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis authenticated release API contract"]
async fn release_http_server_owned_gate_lifecycle_publish_replay_and_viewer_denial() -> Result<()> {
    release_state_fixture(|p,g|async move {
 sqlx::raw_sql(include_str!("agent_studio_support/auth_base.sql")).execute(&p).await?;
 let(a,id)=create_release_agent(&p,&g.state,"http-release").await?;sqlx::query("INSERT INTO tenants(tenant_id) VALUES($1)").bind(&a.tenant_id).execute(&p).await?;
 let viewer=actor("release-viewer",&a.tenant_id);user(&p,&viewer.user_id,&viewer.tenant_id).await?;g.state.repository.upsert_member(&a,&id,"user",&viewer.user_id,"viewer").await?;
 let(jwt,jti)=agent_studio_support::state::authenticate_fixture(&g,&a).await?;let(vjwt,vjti)=agent_studio_support::state::authenticate_fixture(&g,&viewer).await?;
 let result=std::panic::AssertUnwindSafe(async {
 let base=format!("/api/v1/agents/{id}");let input=json!({"draft_revision":1,"channel":"api","auth_mode":"private","channel_policy":{"attachments":false,"high_risk_tools":false,"allowed_origins":[]}});
 for field in ["status","gate_snapshot","profile_id","runtime_fingerprint","model_authorization"] {let mut body=input.clone();body[field]=json!("client-owned");assert_eq!(studio_request(&g,&jwt,"POST",&format!("{base}/evals"),body,None).await?.0,422);}
 let(status,queued)=studio_request(&g,&jwt,"POST",&format!("{base}/evals"),input.clone(),None).await?;assert_eq!(status,201,"{queued}");assert_eq!(queued["status"],"queued");let eid=queued["evaluation_id"].as_str().unwrap();
 let(status,completed)=studio_request(&g,&jwt,"POST",&format!("{base}/evals/{eid}/execute"),json!({}),None).await?;assert_eq!(status,200,"{completed}");assert_eq!(completed["status"],"passed");assert_eq!(completed["gate_snapshot"]["model_quality_evaluated"],false);
 let(status,list)=studio_request(&g,&jwt,"GET",&format!("{base}/evals"),json!({}),None).await?;assert_eq!(status,200);assert_eq!(list["evaluations"].as_array().unwrap().len(),1);
 let payload=json!({"evaluation_id":eid,"reason":"release"});let(status,missing)=studio_request(&g,&jwt,"POST",&format!("{base}/publish"),payload.clone(),None).await?;assert_eq!(status,428,"{missing}");
 let(status,published)=studio_request(&g,&jwt,"POST",&format!("{base}/publish"),payload.clone(),Some("http-publish-key-0001")).await?;assert_eq!(status,200,"{published}");
 let(status,replay)=studio_request(&g,&jwt,"POST",&format!("{base}/publish"),payload.clone(),Some("http-publish-key-0001")).await?;assert_eq!(status,200);assert_eq!(replay["idempotent_replay"],true);assert_eq!(replay["event"]["event_id"],published["event"]["event_id"]);
 assert_eq!(studio_request(&g,&vjwt,"POST",&format!("{base}/evals"),input.clone(),None).await?.0,404);
 assert_eq!(studio_request(&g,&vjwt,"POST",&format!("{base}/publish"),payload,Some("viewer-publish-0001")).await?.0,404);
 let(status,queued)=studio_request(&g,&jwt,"POST",&format!("{base}/evals"),input,None).await?;assert_eq!(status,201);let cancelled_id=queued["evaluation_id"].as_str().unwrap();
 let(status,cancelled)=studio_request(&g,&jwt,"POST",&format!("{base}/evals/{cancelled_id}/cancel"),json!({}),None).await?;assert_eq!(status,200);assert_eq!(cancelled["status"],"cancelled");let(status,after)=studio_request(&g,&jwt,"POST",&format!("{base}/evals/{cancelled_id}/execute"),json!({}),None).await?;assert_eq!(status,200);assert_eq!(after["status"],"cancelled");
 Ok::<(),Box<dyn std::error::Error+Send+Sync>>(())
 }).catch_unwind().await;
 let owner_cleanup=agent_studio_support::state::revoke_fixture(&g,&a,&jti).await;let viewer_cleanup=agent_studio_support::state::revoke_fixture(&g,&viewer,&vjti).await;owner_cleanup?;viewer_cleanup?;
 match result{Ok(r)=>r,Err(e)=>std::panic::resume_unwind(e)}
 }).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis rollback contract"]
async fn rollback_rejects_version_never_in_channel_history() -> Result<()> {
    release_state_fixture(|p,g|async move {
 let(a,id)=create_release_agent(&p,&g.state,"rollback-unreleased").await?;let(e,_)=record_evaluation(&g.state,&a,&id).await?;let published=publish(&g.state,&a,&id,&e,"publish-unreleased-0001").await?;
 g.state.repository.update_draft(&a,&id,1,&release_spec("never promoted"),&json!({})).await?;let version=g.state.repository.create_version(&a,&id,2).await?;
 let pid=published["publication"]["publication_id"].as_str().unwrap();let before=release_counts(&p,&a,&id).await?;
 error(g.state.rollback_publication(&a,pid,"rollback-unreleased-0001",&json!({"target_version_id":version["agent_version_id"],"reason":"must not bypass evaluation"})).await,409,"AGENT_ROLLBACK_TARGET_NOT_HISTORICAL");
 let pointer:String=sqlx::query_scalar("SELECT version_id::text FROM agent_publications WHERE tenant_id=$1 AND publication_id=$2::uuid").bind(&a.tenant_id).bind(pid).fetch_one(&p).await?;assert_eq!(pointer,published["version"]["agent_version_id"].as_str().unwrap());assert_eq!(release_counts(&p,&a,&id).await?,before);Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis rollback readiness contract"]
async fn rollback_rechecks_provider_readiness_and_preserves_every_release_row() -> Result<()> {
    release_state_fixture(|p,g|async move {
 let(a,id)=create_release_agent(&p,&g.state,"rollback-readiness").await?;let(e1,_)=record_evaluation(&g.state,&a,&id).await?;let first=publish(&g.state,&a,&id,&e1,"readiness-first-0001").await?;
 g.state.repository.update_draft(&a,&id,1,&release_spec("second revision"),&json!({})).await?;let(e2,_)=record_evaluation(&g.state,&a,&id).await?;let second=publish(&g.state,&a,&id,&e2,"readiness-second-0001").await?;
 let pid=second["publication"]["publication_id"].as_str().unwrap();let before=release_counts(&p,&a,&id).await?;g.providers.configured.store(false,std::sync::atomic::Ordering::SeqCst);
 error(g.state.rollback_publication(&a,pid,"rollback-no-provider-0001",&json!({"target_version_id":first["version"]["agent_version_id"],"reason":"readiness revoked"})).await,503,"AGENT_RUNTIME_PROVIDER_UNAVAILABLE");
 let pointer:String=sqlx::query_scalar("SELECT version_id::text FROM agent_publications WHERE tenant_id=$1 AND publication_id=$2::uuid").bind(&a.tenant_id).bind(pid).fetch_one(&p).await?;assert_eq!(pointer,second["version"]["agent_version_id"].as_str().unwrap());assert_eq!(release_counts(&p,&a,&id).await?,before);Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL and Redis selected live-run contract"]
async fn selected_live_run_promotes_existing_version_and_preserves_limited_quality_evidence()
-> Result<()> {
    release_state_fixture(|p,g|async move {
 let(a,id)=create_release_agent(&p,&g.state,"selected-run").await?;let version=g.state.repository.create_version(&a,&id,1).await?;let vid=version["agent_version_id"].as_str().unwrap();
 let dataset=uuid::Uuid::new_v4().to_string();sqlx::query("INSERT INTO eval_datasets(dataset_id,tenant_id,name,created_by) VALUES($1::uuid,$2,'selected run dataset',$3)").bind(&dataset).bind(&a.tenant_id).bind(&a.user_id).execute(&p).await?;
 let candidate=g.state.resolve_release_candidate(&a,&id,1,"api","private",&json!({"attachments":false,"high_risk_tools":false,"allowed_origins":[]}),Some(&dataset)).await?;let queued=g.state.create_release_evaluation(&a,&id,&candidate,&gate()).await?;let eid=queued["evaluation_id"].as_str().unwrap();g.state.repository.transition_evaluation(&a,&id,eid,false).await?;g.state.complete_release_evaluation(&a,&id,eid,&candidate,&gate()).await?;
 let resolution=g.state.repository.resolve_version(&a,&id,vid).await?;let snapshot=g.state.build_snapshot(&a,&resolution,"preview").await?;let hash=format!("sha256:{}",ai_platform_gateway::migration_agent::spec::hash(&snapshot));let identity=json!({"agent_id":id,"agent_version_id":vid,"agent_spec_hash":version["spec_hash"],"agent_runtime_snapshot_hash":hash});let mut target=identity.clone();target["candidate_type"]=json!("agent_version");
 let metrics=json!({"attempted_trials":3,"completed_trials":3,"total_trials":3,"failed_trials":0,"execution_failed_trials":0,"judge_failed_trials":0,"unscored_trials":0,"unknown_side_effect_trial_count":0,"actual_fingerprint":identity,"gate":{"profile":"agent_version_task_suite","status":"warning","reason":"fixed_sample_only"}});let run=uuid::Uuid::new_v4().to_string();
 sqlx::query("INSERT INTO eval_experiment_runs(run_id,tenant_id,dataset_id,run_mode,status,target_snapshot,score_summary,metrics,dataset_manifest_hash,evaluator_suite_hash) VALUES($1::uuid,$2,$3::uuid,'live_candidate','succeeded',$4,'{\"critical_failed_count\":0,\"excluded_case_count\":0}',$5,$6,$7)").bind(&run).bind(&a.tenant_id).bind(&dataset).bind(&target).bind(&metrics).bind(candidate["dataset_manifest_hash"].as_str().unwrap()).bind("8".repeat(64)).execute(&p).await?;
 let payload=json!({"evaluation_id":eid,"experiment_run_id":run,"reason":"selected fixed Version"});let result=g.state.publish_agent(&a,&id,"publish-selected-run-0001",&payload).await?;assert_eq!(result["version"]["agent_version_id"],vid);assert_eq!(result["publication"]["version_id"],vid);let audit=&result["event"]["validation_snapshot"];assert_eq!(audit["experiment_run_id"],run);assert_eq!(audit["quality_gate_status"],"warning");assert_eq!(audit["quality_evidence_scope"],"fixed_sample_only");assert_eq!(audit["general_quality_proven"],false);assert_eq!(release_counts(&p,&a,&id).await?["versions"],1);
 let replay=g.state.publish_agent(&a,&id,"publish-selected-run-0001",&payload).await?;assert_eq!(replay["idempotent_replay"],true);
 let mut conflicting=payload.clone();conflicting["experiment_run_id"]=json!(uuid::Uuid::new_v4().to_string());error(g.state.publish_agent(&a,&id,"publish-selected-run-0001",&conflicting).await,409,"AGENT_RELEASE_IDEMPOTENCY_CONFLICT");
 for (label,path,replacement) in [("unknown-effect",vec!["unknown_side_effect_trial_count"],json!(1)),("failed-gate",vec!["gate"],json!({"profile":"agent_version_task_suite","status":"fail","reason":"known_hard_failure"}))] {
 let bad=uuid::Uuid::new_v4().to_string();sqlx::query("INSERT INTO eval_experiment_runs(run_id,tenant_id,dataset_id,run_mode,status,target_snapshot,score_summary,metrics,dataset_manifest_hash,evaluator_suite_hash) SELECT $1::uuid,tenant_id,dataset_id,run_mode,status,target_snapshot,score_summary,jsonb_set(metrics,$3::text[],$4),dataset_manifest_hash,evaluator_suite_hash FROM eval_experiment_runs WHERE run_id=$2::uuid").bind(&bad).bind(&run).bind(path).bind(replacement).execute(&p).await?;
 let mut body=payload.clone();body["experiment_run_id"]=json!(bad);error(g.state.publish_agent(&a,&id,&format!("selected-run-{label}"),&body).await,409,"AGENT_EVAL_RUN_HARD_FAILURE");
 }
 assert_eq!(release_counts(&p,&a,&id).await?["events"],1);Ok(())}).await
}
