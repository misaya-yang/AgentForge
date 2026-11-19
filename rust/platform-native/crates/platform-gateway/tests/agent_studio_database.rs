//! Live database source contract matrix; explicit --include-ignored only.
mod agent_studio_support;
use agent_studio_support::*;
use sqlx::PgPool;
const TABLES: [&str; 10] = [
    "agents",
    "agent_members",
    "agent_drafts",
    "agent_draft_knowledge_bindings",
    "agent_versions",
    "agent_version_capabilities",
    "agent_version_knowledge_bindings",
    "agent_publications",
    "agent_publish_events",
    "agent_api_tokens",
];
#[test]
fn migration_is_additive() {
    let sql = sql("071_agent_studio_domain.sql").to_uppercase();
    for forbidden in ["DROP TABLE", "TRUNCATE", "ALTER COLUMN SERVICE_ID"] {
        assert!(!sql.contains(forbidden));
    }
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn migration_is_reentrant_and_all_tables_are_tenant_scoped() -> Result<()> {
    fixture(|p,_|async move{let mut tables=sqlx::query_scalar::<_,String>("SELECT table_name FROM information_schema.columns WHERE table_schema=current_schema() AND column_name='tenant_id' AND table_name=ANY($1::text[])").bind(TABLES.to_vec()).fetch_all(&p).await?;tables.sort();let mut expected=TABLES.map(str::to_owned).to_vec();expected.sort();assert_eq!(tables,expected);let n:i64=sqlx::query_scalar("SELECT COUNT(*) FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE t.tgname='agent_versions_immutable' AND NOT t.tgisinternal AND n.nspname=current_schema()").fetch_one(&p).await?;assert_eq!(n,1);Ok(())}).await
}
async fn rejected(p: &PgPool, sql: &str, code: &str, message: &str) -> Result<()> {
    let mut tx = p.begin().await?;
    let result = match sqlx::raw_sql(sql).execute(&mut *tx).await {
        Ok(_) => {
            sqlx::query("SET CONSTRAINTS ALL IMMEDIATE")
                .execute(&mut *tx)
                .await
        }
        Err(e) => Err(e),
    };
    state(result, code, message);
    tx.rollback().await?;
    Ok(())
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn composite_foreign_keys_reject_every_cross_tenant_child() -> Result<()> {
    fixture(|p,_|async move{let a=graph(&p,"tenant-a").await?;let b=graph(&p,"tenant-b").await?;let queries=vec![
format!("INSERT INTO agent_members(tenant_id,agent_id,principal_type,principal_id,role,created_by) VALUES('tenant-b','{}','user','cross-user','viewer','cross-user')",a.agent),
format!("INSERT INTO agent_drafts(tenant_id,draft_id,agent_id,revision,spec,spec_hash,updated_by) VALUES('tenant-b',gen_random_uuid(),'{}',1,'{{}}','{}','cross-user')",a.agent,"b".repeat(64)),
format!("UPDATE agents SET current_draft_id='{}' WHERE tenant_id='tenant-b' AND agent_id='{}'",a.draft,b.agent),
format!("INSERT INTO agent_draft_knowledge_bindings(tenant_id,draft_id,dataset_id,retrieval_config) VALUES('tenant-b','{}','{}','{{}}')",a.draft,a.dataset),
format!("INSERT INTO agent_versions(tenant_id,agent_version_id,agent_id,version_number,resolved_spec,spec_hash,source_draft_id,source_draft_revision,created_by) VALUES('tenant-b',gen_random_uuid(),'{}',2,'{{}}','{}','{}',1,'cross-user')",a.agent,"b".repeat(64),a.draft),
format!("INSERT INTO agent_version_capabilities(tenant_id,agent_version_id,capability_type,resource_id) VALUES('tenant-b','{}','native','cross-tool')",a.version),
format!("INSERT INTO agent_version_knowledge_bindings(tenant_id,agent_version_id,dataset_id) VALUES('tenant-b','{}','{}')",a.version,a.dataset),
format!("INSERT INTO agent_publications(tenant_id,publication_id,agent_id,channel,version_id,created_by,updated_by) VALUES('tenant-b',gen_random_uuid(),'{}','hosted','{}','cross-user','cross-user')",a.agent,a.version),
format!("INSERT INTO agent_publish_events(tenant_id,publication_id,agent_id,to_version_id,actor_id) VALUES('tenant-b','{}','{}','{}','cross-user')",a.publication,a.agent,a.version),
format!("INSERT INTO agent_api_tokens(tenant_id,publication_id,token_hash,name,created_by) VALUES('tenant-b','{}','{}','cross-token','cross-user')",a.publication,"b".repeat(64))];for q in queries{rejected(&p,&q,"23503","").await?;}Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn version_bindings_immutable_and_publish_events_append_only() -> Result<()> {
    fixture(|p,_|async move{let g=graph(&p,"tenant-immutable").await?;sqlx::query("INSERT INTO datasets(dataset_id,tenant_id,name) VALUES('extra','tenant-immutable','extra')").execute(&p).await?;let queries=vec![format!("UPDATE agent_versions SET version_number=9 WHERE tenant_id='tenant-immutable' AND agent_version_id='{}'",g.version),format!("DELETE FROM agent_version_capabilities WHERE tenant_id='tenant-immutable' AND agent_version_id='{}'",g.version),format!("DELETE FROM agent_version_knowledge_bindings WHERE tenant_id='tenant-immutable' AND agent_version_id='{}'",g.version),format!("INSERT INTO agent_version_capabilities(tenant_id,agent_version_id,capability_type,resource_id) VALUES('tenant-immutable','{}','native','late-tool')",g.version),format!("INSERT INTO agent_version_knowledge_bindings(tenant_id,agent_version_id,dataset_id) VALUES('tenant-immutable','{}','extra')",g.version),format!("DELETE FROM agent_publish_events WHERE tenant_id='tenant-immutable' AND publication_id='{}'",g.publication)];for q in queries{rejected(&p,&q,"55000","AGENT_VERSION_IMMUTABLE").await?;}let q=format!("INSERT INTO agent_versions(tenant_id,agent_version_id,agent_id,version_number,schema_version,resolved_spec,spec_hash,source_draft_id,source_draft_revision,created_by) VALUES('tenant-immutable',gen_random_uuid(),'{}',2,'agent-spec/v1','{{}}','{}','{}',1,'{}')",g.agent,"b".repeat(64),g.draft,g.owner);rejected(&p,&q,"23514","AGENT_VERSION_NOT_SEALED").await?;Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn last_owner_cannot_be_removed_or_demoted() -> Result<()> {
    fixture(|p,_|async move{let g=graph(&p,"tenant-owner").await?;for q in [format!("DELETE FROM agent_members WHERE tenant_id='tenant-owner' AND agent_id='{}' AND principal_id='{}'",g.agent,g.owner),format!("UPDATE agent_members SET role='editor' WHERE tenant_id='tenant-owner' AND agent_id='{}' AND principal_id='{}'",g.agent,g.owner)]{rejected(&p,&q,"23514","AGENT_LAST_OWNER").await?;}user(&p,"owner-second","tenant-owner").await?;sqlx::query("INSERT INTO agent_members(tenant_id,agent_id,principal_type,principal_id,role,created_by) VALUES('tenant-owner',$1::uuid,'user','owner-second','owner',$2)").bind(&g.agent).bind(&g.owner).execute(&p).await?;assert_eq!(sqlx::query("DELETE FROM agent_members WHERE tenant_id='tenant-owner' AND agent_id=$1::uuid AND principal_id=$2").bind(&g.agent).bind(&g.owner).execute(&p).await?.rows_affected(),1);assert_eq!(sqlx::query_scalar::<_,String>("SELECT owner_id FROM agents WHERE tenant_id='tenant-owner' AND agent_id=$1::uuid").bind(&g.agent).fetch_one(&p).await?,"owner-second");Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn tenant_principals_and_owner_consistency() -> Result<()> {
    fixture(|p,_|async move{let g=graph(&p,"tenant-principal").await?;user(&p,"foreign-user","tenant-foreign").await?;user(&p,"replacement","tenant-principal").await?;rejected(&p,&format!("INSERT INTO agent_members(tenant_id,agent_id,principal_type,principal_id,role,created_by) VALUES('tenant-principal','{}','user','foreign-user','viewer','{}')",g.agent,g.owner),"23503","").await?;rejected(&p,&format!("INSERT INTO agent_members(tenant_id,agent_id,principal_type,principal_id,role,created_by) VALUES('tenant-principal','{}','group','group-without-registry','viewer','{}')",g.agent,g.owner),"23514","").await?;rejected(&p,&format!("UPDATE agent_members SET principal_id='replacement' WHERE tenant_id='tenant-principal' AND agent_id='{}' AND principal_id='{}'",g.agent,g.owner),"55000","AGENT_MEMBER_IDENTITY_IMMUTABLE").await?;rejected(&p,"INSERT INTO agents(tenant_id,agent_id,slug,name,owner_id,created_by,updated_by) VALUES('tenant-principal',gen_random_uuid(),'no-owner','no-owner','replacement','replacement','replacement')","23514","AGENT_OWNER_INVARIANT").await?;sqlx::query("INSERT INTO agent_members(tenant_id,agent_id,principal_type,principal_id,role,created_by) VALUES('tenant-principal',$1::uuid,'user','replacement','viewer',$2)").bind(&g.agent).bind(&g.owner).execute(&p).await?;rejected(&p,&format!("UPDATE agents SET owner_id='replacement' WHERE tenant_id='tenant-principal' AND agent_id='{}'",g.agent),"23514","AGENT_OWNER_INVARIANT").await?;assert_eq!(sqlx::query_scalar::<_,String>("SELECT owner_id FROM agents WHERE tenant_id='tenant-principal' AND agent_id=$1::uuid").bind(&g.agent).fetch_one(&p).await?,g.owner);Ok(())}).await
}
#[tokio::test]
#[ignore = "explicit isolated PostgreSQL contract"]
async fn repository_draft_revision_metadata_validation_and_storage_are_atomic() -> Result<()> {
    fixture(|p,repo|async move{let owner=actor("owner-repo","tenant-repo");user(&p,"owner-repo","tenant-repo").await?;let first=repo.create(&owner,"Repository Agent",None,"",&spec("Repository contract")).await?;let id=first["agent_id"].as_str().unwrap();let mut foreign=actor("owner-repo","tenant-other");foreign.roles=vec!["admin".into()];error(repo.get(&foreign,id).await,404,"AGENT_NOT_FOUND");let changes=serde_json::json!({"name":"Atomic Repository Agent","description":"Saved with revision two"});let draft=repo.update_draft(&owner,id,1,&spec("Revision two"),&changes).await?;assert_eq!(draft["revision"],2);let changed=serde_json::json!({"name":"Must not survive conflict","description":"Must not survive conflict"});let conflict=repo.update_draft(&owner,id,1,&spec("Stale"),&changed).await.unwrap_err();assert_eq!(conflict.detail["code"],"AGENT_DRAFT_CONFLICT");assert_eq!(conflict.detail["current_revision"],2);let mut invalid=spec("Missing dataset");invalid["knowledge"]=serde_json::json!([{"dataset_id":"missing-dataset","retrieval_config":{"mode":"auto"}}]);error(repo.update_draft(&owner,id,2,&invalid,&changed).await,422,"AGENT_SPEC_INVALID");sqlx::raw_sql("CREATE FUNCTION reject_synthetic_draft_storage_failure() RETURNS trigger AS $$ BEGIN IF NEW.spec->>'instructions'='force storage failure' THEN RAISE EXCEPTION 'synthetic draft storage failure'; END IF; RETURN NEW; END; $$ LANGUAGE plpgsql; CREATE TRIGGER reject_synthetic_draft_storage_failure BEFORE UPDATE ON agent_drafts FOR EACH ROW EXECUTE FUNCTION reject_synthetic_draft_storage_failure();").execute(&p).await?;assert!(repo.update_draft(&owner,id,2,&spec("force storage failure"),&changed).await.is_err());let after=repo.get(&owner,id).await?;assert_eq!(after["name"],"Atomic Repository Agent");assert_eq!(after["description"],"Saved with revision two");assert_eq!(repo.draft(&owner,id).await?["revision"],2);Ok(())}).await
}
