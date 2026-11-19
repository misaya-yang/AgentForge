//! Actual low-privilege audit closure and bounded DELETE predicate permissions.
mod agent_studio_support;
use agent_studio_support::*;
use ai_platform_migration_authority::source_tools::gateway_automations_032 as correction;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};

fn principal(pool: &PgPool, role: &str) -> PgPool {
    PgPoolOptions::new().max_connections(2)
        .connect_lazy_with(pool.connect_options().as_ref().clone().username(role))
}
fn permission_denied<T>(result: std::result::Result<T,sqlx::Error>) {
    let error=match result {Ok(_)=>panic!("unexpected application privilege"),Err(error)=>error};
    assert_eq!(error.as_database_error().and_then(|e|e.code()).as_deref(),Some("42501"));
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL real audit trigger and function ACL contract"]
async fn real_agent_create_requires_reviewed_redactor_execute_and_redacts_nested_audit() -> Result<()> {
    fixture(|pool,repo|async move {
        user(&pool,"audit-owner","audit-tenant").await?;
        let actor=actor("audit-owner","audit-tenant");
        let app=application_pool(&pool);
        assert_eq!(sqlx::query_scalar::<_,String>("SELECT current_user::text").fetch_one(&app).await?,"ai_gateway_gateway");
        let public:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM pg_proc p CROSS JOIN LATERAL aclexplode(p.proacl) a WHERE p.oid='gateway.agent_audit_redact_jsonb(jsonb)'::regprocedure AND a.grantee=0)").fetch_one(&pool).await?;
        assert!(!public);
        // Deliberate negative only inside this UUID-owned test database.
        sqlx::raw_sql("REVOKE EXECUTE ON FUNCTION gateway.agent_audit_redact_jsonb(jsonb) FROM ai_gateway_gateway").execute(&pool).await?;
        error(repo.create(&actor,"Denied",None,"",&spec("answer")).await,503,"AGENT_STORAGE_UNAVAILABLE");
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM gateway.agents").fetch_one(&pool).await?,0);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM gateway.audit_logs").fetch_one(&pool).await?,0);
        canonical_fixture(&pool).await?;
        let created=repo.create(&actor,"Allowed",None,"",&spec("answer")).await?;
        let id=created["agent_id"].as_str().unwrap();
        sqlx::query("INSERT INTO gateway.audit_logs(event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,status) VALUES('agent_studio',$1,$2,'agent',$3,'audit_contract',$4,'success')")
            .bind(&actor.user_id).bind(&actor.tenant_id).bind(id)
            .bind(json!({"authorization":"synthetic-test-only","nested":{"password":"synthetic-test-only","safe":"kept"}})).execute(&app).await?;
        let audit:Value=sqlx::query_scalar("SELECT request_summary FROM gateway.audit_logs WHERE action='audit_contract'").fetch_one(&pool).await?;
        assert_eq!(audit,json!({"authorization":"[REDACTED]","nested":{"password":"[REDACTED]","safe":"kept"}}));
        for role in ["ai_gateway_knowledge_api","ai_gateway_knowledge_worker","ai_gateway_runtime","ai_gateway_capability_worker","ai_gateway_migrator"] {
            let allowed:bool=sqlx::query_scalar("SELECT has_function_privilege($1,'gateway.agent_audit_redact_jsonb(jsonb)','EXECUTE')").bind(role).fetch_one(&pool).await?;
            assert!(!allowed,"unrelated role gained redactor EXECUTE");
            let denied=principal(&pool,role);
            permission_denied(sqlx::query("SELECT gateway.agent_audit_redact_jsonb('{}'::jsonb)").execute(&denied).await);
            denied.close().await;
        }
        app.close().await;
        Ok(())
    }).await
}

#[tokio::test]
#[ignore = "explicit isolated PostgreSQL exact Knowledge API DELETE-predicate column ACL contract"]
async fn delete_predicates_allow_only_three_reviewed_columns_and_reject_acl_drift() -> Result<()> {
    fixture(|pool,_repo|async move {
        let source=std::fs::read_to_string(root().join("database/baselines/2026_08_post_kb_v1/init.sql"))?;
        for relation in ["gateway.version_retention_policies","knowledge.dataset_keyword_tables"] {
            let start=format!("CREATE TABLE {relation} (");
            let body=source.split_once(&start).ok_or("frozen table definition missing")?.1
                .split_once("\n);").ok_or("frozen table terminator missing")?.0;
            sqlx::raw_sql(&format!("{start}{body}\n);")).execute(&pool).await?;
        }
        let grants=std::fs::read_to_string(root().join("database/baselines/2026_08_post_kb_v1/grants.sql"))?;
        for line in grants.lines().filter(|line|line.starts_with("GRANT DELETE ON TABLE ")
            && line.ends_with(" TO \"ai_gateway_knowledge_api\";")
            && (line.contains("\"version_retention_policies\"") || line.contains("\"dataset_keyword_tables\""))) {
            sqlx::raw_sql(line).execute(&pool).await?;
        }
        let api=principal(&pool,"ai_gateway_knowledge_api");
        permission_denied(sqlx::query("DELETE FROM gateway.version_retention_policies WHERE dataset_id='ours' AND tenant_id='our-tenant'").execute(&api).await);
        permission_denied(sqlx::query("DELETE FROM knowledge.dataset_keyword_tables WHERE dataset_id='ours'").execute(&api).await);
        let reviewed=sql("2026_08_post_kb_v1/032_gateway_automations.sql");
        sqlx::raw_sql(correction::runtime_acl_correction_sql(&reviewed)?).execute(&pool).await?;
        for check in correction::runtime_acl_postconditions() {
            assert!(sqlx::query_scalar::<_,bool>(&check).fetch_one(&pool).await?);
        }
        sqlx::raw_sql("INSERT INTO gateway.version_retention_policies(tenant_id,dataset_id) VALUES('our-tenant','ours'),('other-tenant','other'); INSERT INTO knowledge.dataset_keyword_tables(dataset_id) VALUES('ours'),('other')").execute(&pool).await?;
        assert_eq!(sqlx::query("DELETE FROM gateway.version_retention_policies WHERE dataset_id='ours' AND tenant_id='our-tenant'").execute(&api).await?.rows_affected(),1);
        assert_eq!(sqlx::query("DELETE FROM knowledge.dataset_keyword_tables WHERE dataset_id='ours'").execute(&api).await?.rows_affected(),1);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM gateway.version_retention_policies WHERE dataset_id='other'").fetch_one(&pool).await?,1);
        assert_eq!(sqlx::query_scalar::<_,i64>("SELECT count(*) FROM knowledge.dataset_keyword_tables WHERE dataset_id='other'").fetch_one(&pool).await?,1);
        permission_denied(sqlx::query("SELECT * FROM gateway.version_retention_policies").execute(&api).await);
        permission_denied(sqlx::query("SELECT keyword_table FROM knowledge.dataset_keyword_tables").execute(&api).await);
        permission_denied(sqlx::query("INSERT INTO knowledge.dataset_keyword_tables(dataset_id) VALUES('denied')").execute(&api).await);
        let gateway=application_pool(&pool);
        permission_denied(sqlx::query("SELECT dataset_id FROM gateway.version_retention_policies").execute(&gateway).await);
        let exact=&correction::runtime_acl_postconditions()[1];
        sqlx::raw_sql("GRANT SELECT(keyword_table) ON knowledge.dataset_keyword_tables TO ai_gateway_knowledge_api").execute(&pool).await?;
        assert!(!sqlx::query_scalar::<_,bool>(exact).fetch_one(&pool).await?);
        sqlx::raw_sql("REVOKE SELECT(keyword_table) ON knowledge.dataset_keyword_tables FROM ai_gateway_knowledge_api; GRANT SELECT(dataset_id) ON knowledge.dataset_keyword_tables TO ai_gateway_knowledge_api WITH GRANT OPTION").execute(&pool).await?;
        assert!(!sqlx::query_scalar::<_,bool>(exact).fetch_one(&pool).await?);
        sqlx::raw_sql("REVOKE GRANT OPTION FOR SELECT(dataset_id) ON knowledge.dataset_keyword_tables FROM ai_gateway_knowledge_api").execute(&pool).await?;
        assert!(sqlx::query_scalar::<_,bool>(exact).fetch_one(&pool).await?);
        gateway.close().await;api.close().await;
        Ok(())
    }).await
}
