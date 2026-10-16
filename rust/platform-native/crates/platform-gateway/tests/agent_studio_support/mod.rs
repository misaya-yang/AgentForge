#![allow(dead_code)]
use ai_platform_gateway::migration_agent::{
    Actor, AgentError, AgentFuture,
    studio::{AgentRepository, SealedCapabilities, StudioResources},
};
use futures_util::FutureExt;
use serde_json::{Value, json};
use sqlx::{
    PgPool,
    postgres::{PgConnectOptions, PgPoolOptions},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::Arc,
};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
pub fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_owned()
}
pub fn sql(file: &str) -> String {
    std::fs::read_to_string(root().join("database/migrations").join(file)).unwrap()
}
pub fn actor(user: &str, tenant: &str) -> Actor {
    Actor {
        user_id: user.into(),
        tenant_id: tenant.into(),
        ..Default::default()
    }
}
pub fn spec(instruction: &str) -> Value {
    json!({"schema_version":"agent-spec/v1","instructions":instruction,"model":{"model_id":"qwen3.7-plus"},"capabilities":[],"knowledge":[],"memory":{"mode":"session"}})
}
pub struct Resources {
    pub pool: PgPool,
}
impl StudioResources for Resources {
    fn authorize_knowledge<'a>(
        &'a self,
        actor: &'a Actor,
        ids: &'a [String],
    ) -> AgentFuture<'a, BTreeSet<String>> {
        Box::pin(async move {
            if ids.is_empty() {
                return Ok(BTreeSet::new());
            }
            Ok(sqlx::query_scalar::<_,String>("SELECT dataset_id FROM datasets WHERE tenant_id=$1 AND dataset_id=ANY($2::text[]) AND NOT is_deleted AND (created_by=$3 OR visibility='public' OR EXISTS(SELECT 1 FROM dataset_permissions p WHERE p.dataset_id=datasets.dataset_id AND p.subject_type='user' AND p.subject_id=$3))").bind(&actor.tenant_id).bind(ids).bind(&actor.user_id).fetch_all(&self.pool).await?.into_iter().collect())
        })
    }
    fn seal_capabilities<'a>(
        &'a self,
        _: &'a Actor,
        spec: &'a Value,
        _: bool,
    ) -> AgentFuture<'a, SealedCapabilities> {
        Box::pin(async move {
            let capabilities=spec["capabilities"].as_array().into_iter().flatten().map(|c|json!({"capability_type":c["type"],"resource_id":c["resource_id"],"resource_version":c["resource_version"],"schema_hash":c["schema_hash"],"config":c["config"]})).collect();
            Ok(SealedCapabilities {
                capabilities,
                skills: vec![],
            })
        })
    }
}
pub struct Fixture {
    pub pool: PgPool,
    admin: PgPool,
    schema: String,
}
impl Fixture {
    pub async fn new() -> Result<Self> {
        Self::custom(include_str!("base.sql"), &["071_agent_studio_domain.sql"]).await
    }
    pub async fn custom(base: &str, migrations: &[&str]) -> Result<Self> {
        let values = dotenvy::from_path_iter(root().join(".env"))?
            .collect::<std::result::Result<BTreeMap<_, _>, _>>()?;
        let get = |key: &str| {
            values
                .get(key)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("local PostgreSQL test configuration missing key: {key}"))
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
        let schema = format!("agent_studio_test_{}", uuid::Uuid::new_v4().simple());
        sqlx::raw_sql(&format!("CREATE SCHEMA \"{schema}\""))
            .execute(&admin)
            .await?;
        let path = format!("\"{schema}\",public");
        let pool = PgPoolOptions::new()
            .max_connections(4)
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
            sqlx::raw_sql(base).execute(&f.pool).await?;
            for file in migrations {
                for _ in 0..2 {
                    sqlx::raw_sql(&sql(file)).execute(&f.pool).await?;
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
    pub fn repo(&self) -> AgentRepository {
        AgentRepository::new(
            self.pool.clone(),
            Arc::new(Resources {
                pool: self.pool.clone(),
            }),
        )
    }
    pub async fn close(self) -> Result<()> {
        self.pool.close().await;
        sqlx::raw_sql(&format!("DROP SCHEMA \"{}\" CASCADE", self.schema))
            .execute(&self.admin)
            .await?;
        self.admin.close().await;
        Ok(())
    }
}
pub async fn fixture<F, Fut>(f: F) -> Result<()>
where
    F: FnOnce(PgPool, AgentRepository) -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let state = Fixture::new().await?;
    let pool = state.pool.clone();
    let repo = state.repo();
    let result = std::panic::AssertUnwindSafe(async move { f(pool, repo).await })
        .catch_unwind()
        .await;
    state.close().await?;
    match result {
        Ok(r) => r,
        Err(e) => std::panic::resume_unwind(e),
    }
}
pub async fn user(p: &PgPool, id: &str, tenant: &str) -> Result<()> {
    sqlx::query("INSERT INTO users(user_id,tenant_id) VALUES($1,$2) ON CONFLICT DO NOTHING")
        .bind(id)
        .bind(tenant)
        .execute(p)
        .await?;
    Ok(())
}
pub fn error<T>(r: std::result::Result<T, AgentError>, status: u16, code: &str) {
    let e = match r {
        Err(error) => error,
        Ok(_) => panic!("expected {status} {code}, operation unexpectedly succeeded"),
    };
    assert_eq!(e.status, status);
    assert_eq!(e.detail["code"], code);
}
pub fn state<T: std::fmt::Debug>(
    r: std::result::Result<T, sqlx::Error>,
    expected: &str,
    needle: &str,
) {
    let e = r.unwrap_err();
    assert_eq!(
        e.as_database_error().and_then(|e| e.code()).as_deref(),
        Some(expected)
    );
    assert!(e.to_string().contains(needle));
}
pub struct Graph {
    pub tenant: String,
    pub agent: String,
    pub draft: String,
    pub version: String,
    pub publication: String,
    pub dataset: String,
    pub owner: String,
}
pub async fn graph(p: &PgPool, tenant: &str) -> Result<Graph> {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let g = Graph {
        tenant: tenant.into(),
        agent: uuid::Uuid::new_v4().to_string(),
        draft: uuid::Uuid::new_v4().to_string(),
        version: uuid::Uuid::new_v4().to_string(),
        publication: uuid::Uuid::new_v4().to_string(),
        dataset: format!("dataset-{suffix}"),
        owner: format!("owner-{suffix}"),
    };
    let statement = format!(
        r#"BEGIN;
INSERT INTO datasets(dataset_id,tenant_id,name) VALUES('{dataset}','{tenant}','{dataset}');
INSERT INTO users(user_id,tenant_id) VALUES('{owner}','{tenant}');
INSERT INTO agents(tenant_id,agent_id,slug,name,owner_id,created_by,updated_by) VALUES('{tenant}','{agent}','agent-{suffix}','agent-{suffix}','{owner}','{owner}','{owner}');
INSERT INTO agent_drafts(tenant_id,draft_id,agent_id,revision,schema_version,spec,spec_hash,updated_by) VALUES('{tenant}','{draft}','{agent}',1,'agent-spec/v1','{{"schema_version":"agent-spec/v1","instructions":"test","model":{{"model_id":"qwen3.7-plus"}}}}','{hash}','{owner}');
INSERT INTO agent_members(tenant_id,agent_id,principal_type,principal_id,role,created_by) VALUES('{tenant}','{agent}','user','{owner}','owner','{owner}');
UPDATE agents SET current_draft_id='{draft}' WHERE tenant_id='{tenant}' AND agent_id='{agent}';
INSERT INTO agent_draft_knowledge_bindings(tenant_id,draft_id,dataset_id,retrieval_config) VALUES('{tenant}','{draft}','{dataset}','{{}}');
INSERT INTO agent_versions(tenant_id,agent_version_id,agent_id,version_number,schema_version,resolved_spec,spec_hash,source_draft_id,source_draft_revision,created_by) VALUES('{tenant}','{version}','{agent}',1,'agent-spec/v1','{{"schema_version":"agent-spec/v1","instructions":"test","model":{{"model_id":"qwen3.7-plus"}}}}','{hash}','{draft}',1,'{owner}');
INSERT INTO agent_version_capabilities(tenant_id,agent_version_id,capability_type,resource_id,config) VALUES('{tenant}','{version}','native','web_fetch','{{}}');
INSERT INTO agent_version_knowledge_bindings(tenant_id,agent_version_id,dataset_id,retrieval_config) VALUES('{tenant}','{version}','{dataset}','{{}}');
UPDATE agent_versions SET bindings_sealed=TRUE WHERE tenant_id='{tenant}' AND agent_version_id='{version}';
INSERT INTO agent_publications(tenant_id,publication_id,agent_id,channel,version_id,created_by,updated_by) VALUES('{tenant}','{publication}','{agent}','api','{version}','{owner}','{owner}');
INSERT INTO agent_publish_events(tenant_id,publication_id,agent_id,to_version_id,actor_id) VALUES('{tenant}','{publication}','{agent}','{version}','{owner}');
INSERT INTO agent_api_tokens(tenant_id,publication_id,token_hash,name,created_by) VALUES('{tenant}','{publication}','{hash}','test-token','{owner}');COMMIT;"#,
        tenant = g.tenant,
        agent = g.agent,
        draft = g.draft,
        version = g.version,
        publication = g.publication,
        dataset = g.dataset,
        owner = g.owner,
        hash = ai_platform_gateway::migration_agent::spec::hash(&json!(suffix))
    );
    sqlx::raw_sql(&statement).execute(p).await?;
    Ok(g)
}

pub mod state;
