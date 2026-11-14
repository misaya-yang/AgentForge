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
/// An explicitly selected private JSON fixture never falls back to dotenv.
pub fn database_values() -> Result<BTreeMap<String, String>> {
    let Some(path) = std::env::var_os("AGENTFORGE_STUDIO_TEST_CONFIG_JSON") else {
        return Ok(dotenvy::from_path_iter(root().join(".env"))?
            .collect::<std::result::Result<BTreeMap<_, _>, _>>()?);
    };
    use std::io::Read;
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err("isolated database JSON fixture must use an absolute path".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options
        .open(&path)
        .map_err(|_| "isolated database JSON fixture unavailable")?;
    let metadata = file
        .metadata()
        .map_err(|_| "isolated database JSON fixture unavailable")?;
    let symlink = std::fs::symlink_metadata(&path)
        .map_err(|_| "isolated database JSON fixture unavailable")?;
    if !metadata.is_file() || symlink.file_type().is_symlink() || metadata.len() > 65536 {
        return Err("isolated database JSON fixture must be a bounded regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if !matches!(metadata.permissions().mode() & 0o777, 0o400 | 0o600) {
            return Err("isolated database JSON fixture must have private permissions".into());
        }
    }
    let mut bytes = Vec::new();
    file.take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "isolated database JSON fixture unavailable")?;
    if bytes.len() > 65536 {
        return Err("isolated database JSON fixture exceeds its size limit".into());
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| "isolated database JSON fixture must contain string settings".into())
}
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
const FIXTURE_PATH: &str = "gateway,assistant,knowledge,public";

/// Keep owner-only setup separate from the actual Gateway login used by repositories.
pub fn application_pool(owner: &PgPool) -> PgPool {
    let options = owner
        .connect_options()
        .as_ref()
        .clone()
        .username("ai_gateway_gateway");
    PgPoolOptions::new()
        .max_connections(4)
        .after_connect(|connection, _| Box::pin(async move {
            sqlx::query("SELECT set_config('search_path',$1,false)")
                .bind(FIXTURE_PATH).execute(connection).await?;
            Ok(())
        }))
        .connect_lazy_with(options)
}

/// Place legacy migration fixtures in the frozen source's real domains and
/// copy only its existing Gateway grants for objects present in this case.
pub async fn canonical_fixture(pool: &PgPool) -> Result<()> {
    // Custom cases may install only one domain; canonical schema grants still
    // describe all three domains, without adding any objects or table rights.
    sqlx::raw_sql("CREATE SCHEMA IF NOT EXISTS gateway; CREATE SCHEMA IF NOT EXISTS assistant; CREATE SCHEMA IF NOT EXISTS knowledge;")
        .execute(pool).await?;
    let baseline = root().join("database/baselines/2026_08_post_kb_v1");
    let source = std::fs::read_to_string(baseline.join("init.sql"))?;
    let tables = regex::Regex::new(
        r"(?m)^CREATE TABLE (assistant|gateway|knowledge)\.([a-z_][a-z0-9_]*)\s*\(",
    )?;
    for capture in tables.captures_iter(&source) {
        let domain = &capture[1];
        let table = &capture[2];
        if domain == "gateway" { continue; }
        let exists: bool = sqlx::query_scalar("SELECT to_regclass($1) IS NOT NULL")
            .bind(format!("gateway.{table}")).fetch_one(pool).await?;
        if exists {
            sqlx::query(&format!("ALTER TABLE gateway.\"{table}\" SET SCHEMA \"{domain}\""))
                .execute(pool).await?;
        }
    }
    let functions = regex::Regex::new(
        r"(?m)^CREATE FUNCTION (assistant|gateway|knowledge)\.([a-z_][a-z0-9_]*)\(",
    )?;
    for capture in functions.captures_iter(&source) {
        let domain = &capture[1];
        let name = &capture[2];
        if domain == "gateway" { continue; }
        let signatures: Vec<String> = sqlx::query_scalar("SELECT pg_get_function_identity_arguments(p.oid) FROM pg_proc p JOIN pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='gateway' AND p.proname=$1")
            .bind(name).fetch_all(pool).await?;
        for signature in signatures {
            sqlx::query(&format!("ALTER FUNCTION gateway.\"{name}\"({signature}) SET SCHEMA \"{domain}\""))
                .execute(pool).await?;
        }
    }
    let grants = std::fs::read_to_string(baseline.join("grants.sql"))?;
    // PostgreSQL grants PUBLIC EXECUTE on new functions by default. Frozen
    // authority explicitly removes it; leaving it here masks nested-call bugs.
    sqlx::raw_sql("REVOKE ALL ON ALL FUNCTIONS IN SCHEMA gateway,assistant,knowledge FROM PUBLIC; REVOKE ALL ON ALL FUNCTIONS IN SCHEMA gateway,assistant,knowledge FROM ai_gateway_gateway;")
        .execute(pool).await?;
    let relations = regex::Regex::new(
        r#"^GRANT [A-Z, ]+ ON (?:TABLE|SEQUENCE) "([a-z_]+)"\."([a-z_][a-z0-9_]*)" TO "ai_gateway_gateway";$"#,
    )?;
    let routines = regex::Regex::new(
        r#"^GRANT EXECUTE ON FUNCTION (.+) TO "ai_gateway_gateway";$"#,
    )?;
    for line in grants.lines() {
        let present = if let Some(capture) = relations.captures(line) {
            sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
                .bind(format!("{}.{}", &capture[1], &capture[2])).fetch_one(pool).await?
        } else if let Some(capture) = routines.captures(line) {
            sqlx::query_scalar::<_, bool>("SELECT to_regprocedure($1) IS NOT NULL")
                .bind(&capture[1]).fetch_one(pool).await?
        } else {
            line.starts_with("GRANT USAGE ON SCHEMA ")
        };
        if present { sqlx::raw_sql(line).execute(pool).await?; }
    }
    let redactor: bool = sqlx::query_scalar("SELECT to_regprocedure('gateway.agent_audit_redact_jsonb(jsonb)') IS NOT NULL")
        .fetch_one(pool).await?;
    if redactor {
        let correction = sql("2026_08_post_kb_v1/032_gateway_automations.sql");
        let reviewed = ai_platform_migration_authority::source_tools::gateway_automations_032::audit_redactor_acl_sql(&correction)?;
        sqlx::raw_sql(reviewed).execute(pool).await?;
    }
    Ok(())
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
/// Complete 081 audit/redactor/trigger and governance definitions. The later
/// retention section requires separate runtime fixtures, outside these cases.
pub async fn install_audit_fixture(pool: &PgPool) -> Result<()> {
    let source=sql("081_agent_studio_operations_governance.sql");
    let prefix=source.split_once("CREATE TABLE IF NOT EXISTS agent_data_deletion_requests (")
        .ok_or("audit/governance source boundary missing")?.0;
    sqlx::raw_sql(&format!("{prefix}\nCOMMIT;")).execute(pool).await?;
    canonical_fixture(pool).await
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
    database: String,
}
impl Fixture {
    pub async fn new() -> Result<Self> {
        // The current repository writes the forward release-identity columns
        // even for an unpublished version. Keep their real migrations present.
        let release = include_str!("release_base.sql")
            .split("CREATE TABLE agent_version_revocations")
            .next().expect("release fixture SQL");
        let base = format!("{}\n{release}", include_str!("base.sql"));
        let fixture=Self::custom(&base, &[
            "071_agent_studio_domain.sql", "077_agent_publication_eval.sql",
            "078_agent_release_lifecycle_hardening.sql",
        ]).await?;
        if let Err(error)=install_audit_fixture(&fixture.pool).await {
            fixture.close().await?;return Err(error);
        }
        Ok(fixture)
    }
    pub async fn custom(base: &str, migrations: &[&str]) -> Result<Self> {
        let values = database_values()?;
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
        let database = format!("agent_studio_test_{}", uuid::Uuid::new_v4().simple());
        sqlx::raw_sql(&format!("CREATE DATABASE \"{database}\" WITH OWNER ai_gateway_owner TEMPLATE template0"))
            .execute(&admin)
            .await?;
        let pool = PgPoolOptions::new()
            .max_connections(4)
            .after_connect(|conn, _| {
                Box::pin(async move {
                    sqlx::query("SET ROLE ai_gateway_owner").execute(&mut *conn).await?;
                    sqlx::query("SELECT set_config('search_path',$1,false)")
                        .bind(FIXTURE_PATH)
                        .execute(conn)
                        .await?;
                    Ok(())
                })
            })
            .connect_with(options.database(&database))
            .await?;
        let f = Self {
            pool,
            admin,
            database,
        };
        let setup: Result<()> = async {
            sqlx::raw_sql("CREATE SCHEMA gateway; CREATE SCHEMA assistant; CREATE SCHEMA knowledge; CREATE EXTENSION pgcrypto WITH SCHEMA public; REVOKE ALL ON SCHEMA public FROM PUBLIC;")
                .execute(&f.pool).await?;
            sqlx::raw_sql(base).execute(&f.pool).await?;
            // 081's real audit trigger redacts both payload columns.
            sqlx::raw_sql("ALTER TABLE IF EXISTS gateway.audit_logs ADD COLUMN IF NOT EXISTS response_summary jsonb")
                .execute(&f.pool).await?;
            for file in migrations {
                for _ in 0..2 {
                    sqlx::raw_sql(&sql(file)).execute(&f.pool).await?;
                }
            }
            canonical_fixture(&f.pool).await?;
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
            application_pool(&self.pool),
            Arc::new(Resources {
                pool: self.pool.clone(),
            }),
        )
    }
    pub async fn close(self) -> Result<()> {
        self.pool.close().await;
        sqlx::raw_sql(&format!("DROP DATABASE \"{}\" WITH (FORCE)", self.database))
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
