//! Tenant-scoped Agent repository. ACL denials intentionally collapse to 404.
//! All compound mutations retain one PostgreSQL transaction and row locks.
use super::{Actor, AgentError, AgentFuture, spec, text, uuid};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool, Row};
use std::{collections::BTreeSet, sync::Arc};

/// The host wires the already migrated Knowledge/MCP/Skill resource authorities.
/// This is mandatory, including when creating versions with no resource bindings.
/// Each returned capability is a sealed {capability_type,resource_id,
/// resource_version,schema_hash,config}; skill records carry exact artifact IDs.
pub trait StudioResources: Send + Sync {
    fn authorize_knowledge<'a>(
        &'a self,
        actor: &'a Actor,
        ids: &'a [String],
    ) -> AgentFuture<'a, BTreeSet<String>>;
    fn seal_capabilities<'a>(
        &'a self,
        actor: &'a Actor,
        spec: &'a Value,
        seal_version: bool,
    ) -> AgentFuture<'a, SealedCapabilities>;
    fn seal_capabilities_in<'a>(
        &'a self,
        actor: &'a Actor,
        spec: &'a Value,
        seal_version: bool,
        _conn: &'a mut PgConnection,
    ) -> AgentFuture<'a, SealedCapabilities> {
        self.seal_capabilities(actor, spec, seal_version)
    }
}
#[derive(Clone, Default)]
pub struct SealedCapabilities {
    pub capabilities: Vec<Value>,
    pub skills: Vec<Value>,
}
#[derive(Clone)]
pub struct AgentRepository {
    pub pool: PgPool,
    pub resources: Arc<dyn StudioResources>,
}
#[derive(Default)]
pub struct ListAgents {
    pub limit: i64,
    pub cursor: Option<String>,
    pub status: Option<String>,
    pub owner_id: Option<String>,
    pub search: Option<String>,
    pub channel: Option<String>,
}
fn rank(role: &str) -> u8 {
    match role {
        "viewer" => 1,
        "editor" => 2,
        "owner" => 3,
        _ => 0,
    }
}
fn slug(value: &str) -> String {
    let mut s = String::new();
    let mut dash = false;
    for c in value.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c);
            dash = false;
        } else if !dash && !s.is_empty() {
            s.push('-');
            dash = true;
        }
    }
    {
        let s = s.trim_matches('-').chars().take(128).collect::<String>();
        if s.is_empty() { "agent".into() } else { s }
    }
}
fn not_found() -> AgentError {
    AgentError::code(404, "AGENT_NOT_FOUND")
}
fn active(agent: &Value) -> Result<(), AgentError> {
    if matches!(text(agent, "status"), "archived" | "deleted") {
        Err(AgentError::code(409, "AGENT_ARCHIVED"))
    } else {
        Ok(())
    }
}
pub(crate) async fn authorized(
    conn: &mut PgConnection,
    actor: &Actor,
    id: &str,
    role: &str,
    lock: bool,
) -> Result<Value, AgentError> {
    let id = uuid(id)?;
    let sql = format!(
        "SELECT to_jsonb(a)||jsonb_build_object('caller_role',CASE WHEN $4 THEN 'owner' ELSE m.role END) FROM agents a LEFT JOIN agent_members m ON m.tenant_id=a.tenant_id AND m.agent_id=a.agent_id AND m.principal_type='user' AND m.principal_id=$3 WHERE a.tenant_id=$1 AND a.agent_id=$2::uuid AND a.deleted_at IS NULL AND ($4::boolean OR m.principal_id IS NOT NULL) {}",
        if lock { "FOR UPDATE OF a" } else { "" }
    );
    let row: Value = sqlx::query_scalar(&sql)
        .bind(&actor.tenant_id)
        .bind(id)
        .bind(&actor.user_id)
        .bind(actor.is_admin())
        .fetch_optional(conn)
        .await?
        .ok_or_else(not_found)?;
    if rank(text(&row, "caller_role")) < rank(role) {
        return Err(not_found());
    }
    Ok(row)
}
pub(crate) async fn audit(
    conn: &mut PgConnection,
    actor: &Actor,
    id: &str,
    action: &str,
    summary: Value,
) -> Result<(), AgentError> {
    sqlx::query("INSERT INTO audit_logs(event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,status) VALUES('agent_studio',$1,$2,'agent',$3,$4,$5::jsonb,'success')").bind(&actor.user_id).bind(&actor.tenant_id).bind(id).bind(action).bind(spec::redact(&summary)).execute(conn).await?;
    Ok(())
}
fn knowledge(spec: &Value) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    spec["knowledge"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| {
            let id = v["dataset_id"].as_str()?;
            if id.is_empty() || !seen.insert(id) {
                return None;
            }
            Some(json!({"dataset_id":id,"retrieval_config":super::object(&v["retrieval_config"])}))
        })
        .collect()
}
async fn binding_audit(
    conn: &mut PgConnection,
    actor: &Actor,
    id: &str,
    old: &Value,
    new: &Value,
) -> Result<(), AgentError> {
    fn bindings(v: &Value) -> BTreeSet<(String, String, String, String)> {
        v["capabilities"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| matches!(text(b, "type"), "mcp" | "connector" | "skill"))
            .map(|b| {
                (
                    text(b, "type").into(),
                    text(b, "resource_id").into(),
                    text(b, "resource_version").into(),
                    text(b, "schema_hash").into(),
                )
            })
            .collect()
    }
    let a = bindings(old);
    let b = bindings(new);
    for (action, set) in [
        ("remove", a.difference(&b).cloned().collect::<Vec<_>>()),
        ("add", b.difference(&a).cloned().collect::<Vec<_>>()),
    ] {
        for (t, r, v, h) in set {
            let mut summary = json!({"capability_type":t,"resource_id":r});
            if !v.is_empty() {
                summary["resource_version"] = v.into();
            }
            if !h.is_empty() {
                summary["schema_hash"] = h.into();
            }
            audit(conn, actor, id, &format!("{t}_binding_{action}"), summary).await?;
        }
    }
    let ids = |s: &Value| {
        knowledge(s)
            .iter()
            .map(|v| text(v, "dataset_id").to_owned())
            .collect::<BTreeSet<_>>()
    };
    let a = ids(old);
    let b = ids(new);
    for (action, set) in [
        ("remove", a.difference(&b).cloned().collect::<Vec<_>>()),
        ("add", b.difference(&a).cloned().collect::<Vec<_>>()),
    ] {
        for dataset in set {
            audit(
                conn,
                actor,
                id,
                &format!("knowledge_binding_{action}"),
                json!({"dataset_id":dataset}),
            )
            .await?;
        }
    }
    Ok(())
}
impl AgentRepository {
    pub fn new(pool: PgPool, resources: Arc<dyn StudioResources>) -> Self {
        Self { pool, resources }
    }
    async fn replace_bindings(
        &self,
        conn: &mut PgConnection,
        actor: &Actor,
        draft_id: &str,
        value: &Value,
    ) -> Result<(), AgentError> {
        let bindings = knowledge(value);
        let ids: Vec<_> = bindings
            .iter()
            .map(|v| text(v, "dataset_id").to_owned())
            .collect();
        if !ids.is_empty()
            && self.resources.authorize_knowledge(actor, &ids).await?
                != ids.iter().cloned().collect()
        {
            return Err(AgentError::validation(vec![
                json!({"field":"knowledge","code":"AGENT_RESOURCE_NOT_FOUND","message":"one or more Dataset bindings are unavailable"}),
            ]));
        }
        sqlx::query(
            "DELETE FROM agent_draft_knowledge_bindings WHERE tenant_id=$1 AND draft_id=$2::uuid",
        )
        .bind(&actor.tenant_id)
        .bind(draft_id)
        .execute(&mut *conn)
        .await?;
        for b in bindings {
            sqlx::query("INSERT INTO agent_draft_knowledge_bindings(tenant_id,draft_id,dataset_id,retrieval_config,bound_by,authorization_checked_at) VALUES($1,$2::uuid,$3,$4::jsonb,$5,NOW())").bind(&actor.tenant_id).bind(draft_id).bind(text(&b,"dataset_id")).bind(&b["retrieval_config"]).bind(&actor.user_id).execute(&mut *conn).await?;
        }
        let sealed = self
            .resources
            .seal_capabilities_in(actor, value, false, conn)
            .await?;
        let exists: bool =
            sqlx::query_scalar("SELECT TO_REGCLASS('agent_draft_skill_bindings') IS NOT NULL")
                .fetch_one(&mut *conn)
                .await?;
        if !exists {
            if sealed.skills.is_empty() {
                return Ok(());
            }
            return Err(AgentError::code(503, "AGENT_SKILL_STORAGE_UNAVAILABLE"));
        }
        sqlx::query(
            "DELETE FROM agent_draft_skill_bindings WHERE tenant_id=$1 AND draft_id=$2::uuid",
        )
        .bind(&actor.tenant_id)
        .bind(draft_id)
        .execute(&mut *conn)
        .await?;
        for b in sealed.skills {
            sqlx::query("INSERT INTO agent_draft_skill_bindings(tenant_id,draft_id,skill_id,skill_version_id,skill_name,bound_by) VALUES($1,$2::uuid,$3::uuid,$4::uuid,$5,$6)").bind(&actor.tenant_id).bind(draft_id).bind(text(&b,"skill_id")).bind(text(&b,"skill_version_id")).bind(text(&b,"skill_name")).bind(&actor.user_id).execute(&mut *conn).await?;
        }
        Ok(())
    }
    pub async fn create(
        &self,
        actor: &Actor,
        name: &str,
        requested_slug: Option<&str>,
        description: &str,
        value: &Value,
    ) -> Result<Value, AgentError> {
        let errors = spec::safety_errors(value);
        if !errors.is_empty() {
            return Err(AgentError::validation(errors));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let draft = uuid::Uuid::new_v4().to_string();
        let mut tx = self.pool.begin().await?;
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id=$1 AND user_id=$2 AND status='active')").bind(&actor.tenant_id).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        if !exists {
            return Err(AgentError::code(404, "AGENT_PRINCIPAL_NOT_FOUND"));
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('agent-count:'||$1,0))")
            .bind(&actor.tenant_id)
            .execute(&mut *tx)
            .await?;
        let policy: bool =
            sqlx::query_scalar("SELECT TO_REGCLASS('agent_governance_policies') IS NOT NULL")
                .fetch_one(&mut *tx)
                .await?;
        let limit: i32 = if policy {
            sqlx::query_scalar("SELECT COALESCE(MIN(max_agents_per_tenant),100)::int FROM agent_governance_policies WHERE tenant_id=$1").bind(&actor.tenant_id).fetch_one(&mut *tx).await?
        } else {
            100
        };
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agents WHERE tenant_id=$1 AND deleted_at IS NULL",
        )
        .bind(&actor.tenant_id)
        .fetch_one(&mut *tx)
        .await?;
        if count >= i64::from(limit) {
            return Err(AgentError::code(409, "AGENT_TENANT_AGENT_QUOTA_EXCEEDED"));
        }
        sqlx::query("INSERT INTO agents(tenant_id,agent_id,slug,name,description,owner_id,created_by,updated_by) VALUES($1,$2::uuid,$3,$4,$5,$6,$6,$6)").bind(&actor.tenant_id).bind(&id).bind(slug(requested_slug.unwrap_or(name))).bind(name).bind(description).bind(&actor.user_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO agent_drafts(tenant_id,draft_id,agent_id,revision,schema_version,spec,spec_hash,updated_by) VALUES($1,$2::uuid,$3::uuid,1,$4,$5::jsonb,$6,$7)").bind(&actor.tenant_id).bind(&draft).bind(&id).bind(spec::SCHEMA).bind(value).bind(spec::hash(value)).bind(&actor.user_id).execute(&mut *tx).await?;
        self.replace_bindings(&mut tx, actor, &draft, value).await?;
        sqlx::query("INSERT INTO agent_members(tenant_id,agent_id,principal_type,principal_id,role,created_by) VALUES($1,$2::uuid,'user',$3,'owner',$3)").bind(&actor.tenant_id).bind(&id).bind(&actor.user_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE agents SET current_draft_id=$3::uuid,updated_by=$4 WHERE tenant_id=$1 AND agent_id=$2::uuid").bind(&actor.tenant_id).bind(&id).bind(&draft).bind(&actor.user_id).execute(&mut *tx).await?;
        audit(&mut tx, actor, &id, "create", json!({"draft_revision":1})).await?;
        binding_audit(&mut tx, actor, &id, &json!({}), value).await?;
        let row:Value=sqlx::query_scalar("SELECT to_jsonb(a)||jsonb_build_object('caller_role','owner','draft_revision',d.revision) FROM agents a JOIN agent_drafts d ON d.tenant_id=a.tenant_id AND d.draft_id=a.current_draft_id WHERE a.tenant_id=$1 AND a.agent_id=$2::uuid").bind(&actor.tenant_id).bind(&id).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(row)
    }
    /// Copy the mutable authoring spec only. Published bindings, credentials and
    /// delivery state are deliberately not inherited from the source Agent.
    pub async fn copy(
        &self,
        actor: &Actor,
        source_id: &str,
        name: Option<&str>,
        requested_slug: Option<&str>,
    ) -> Result<Value, AgentError> {
        let id = uuid::Uuid::new_v4().to_string();
        let draft = uuid::Uuid::new_v4().to_string();
        let mut tx = self.pool.begin().await?;
        let source = authorized(&mut tx, actor, source_id, "owner", false).await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('agent-count:'||$1,0))")
            .bind(&actor.tenant_id)
            .execute(&mut *tx)
            .await?;
        let has_policy: bool =
            sqlx::query_scalar("SELECT TO_REGCLASS('agent_governance_policies') IS NOT NULL")
                .fetch_one(&mut *tx)
                .await?;
        let limit: i32 = if has_policy {
            sqlx::query_scalar("SELECT COALESCE(MIN(max_agents_per_tenant),100)::int FROM agent_governance_policies WHERE tenant_id=$1")
                .bind(&actor.tenant_id).fetch_one(&mut *tx).await?
        } else {
            100
        };
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM agents WHERE tenant_id=$1 AND deleted_at IS NULL",
        )
        .bind(&actor.tenant_id)
        .fetch_one(&mut *tx)
        .await?;
        if count >= i64::from(limit) {
            return Err(AgentError::code(409, "AGENT_TENANT_AGENT_QUOTA_EXCEEDED"));
        }
        let original: Value = sqlx::query_scalar(
            "SELECT spec FROM agent_drafts WHERE tenant_id=$1 AND agent_id=$2::uuid",
        )
        .bind(&actor.tenant_id)
        .bind(source_id)
        .fetch_one(&mut *tx)
        .await?;
        let copied = spec::copy_spec(&original);
        let name = name
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{} Copy", text(&source, "name")));
        let copied_slug = slug(
            &requested_slug
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .unwrap_or_else(|| format!("{}-copy-{}", text(&source, "slug"), &id[..8])),
        );
        sqlx::query("INSERT INTO agents(tenant_id,agent_id,slug,name,description,owner_id,created_by,updated_by) VALUES($1,$2::uuid,$3,$4,$5,$6,$6,$6)")
            .bind(&actor.tenant_id).bind(&id).bind(copied_slug).bind(name).bind(text(&source,"description")).bind(&actor.user_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO agent_drafts(tenant_id,draft_id,agent_id,revision,schema_version,spec,spec_hash,updated_by) VALUES($1,$2::uuid,$3::uuid,1,$4,$5::jsonb,$6,$7)")
            .bind(&actor.tenant_id).bind(&draft).bind(&id).bind(spec::SCHEMA).bind(&copied).bind(spec::hash(&copied)).bind(&actor.user_id).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO agent_members(tenant_id,agent_id,principal_type,principal_id,role,created_by) VALUES($1,$2::uuid,'user',$3,'owner',$3)")
            .bind(&actor.tenant_id).bind(&id).bind(&actor.user_id).execute(&mut *tx).await?;
        let mut row: Value = sqlx::query_scalar("UPDATE agents a SET current_draft_id=$3::uuid,updated_by=$4 WHERE tenant_id=$1 AND agent_id=$2::uuid RETURNING to_jsonb(a)||jsonb_build_object('caller_role','owner')")
            .bind(&actor.tenant_id).bind(&id).bind(&draft).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        audit(
            &mut tx,
            actor,
            &id,
            "copy",
            json!({"source_agent_id":source_id}),
        )
        .await?;
        tx.commit().await?;
        row["draft_revision"] = 1.into();
        Ok(row)
    }
    pub async fn list(&self, actor: &Actor, query: ListAgents) -> Result<Value, AgentError> {
        let limit = query.limit.clamp(1, 100);
        let (time, id) = match query.cursor {
            Some(c) => {
                let bytes = URL_SAFE_NO_PAD
                    .decode(c)
                    .map_err(|_| AgentError::code(400, "AGENT_CURSOR_INVALID"))?;
                let raw = String::from_utf8(bytes)
                    .map_err(|_| AgentError::code(400, "AGENT_CURSOR_INVALID"))?;
                let (t, id) = raw
                    .rsplit_once('|')
                    .ok_or_else(|| AgentError::code(400, "AGENT_CURSOR_INVALID"))?;
                (
                    Some(
                        DateTime::parse_from_rfc3339(t)
                            .map_err(|_| AgentError::code(400, "AGENT_CURSOR_INVALID"))?
                            .with_timezone(&Utc),
                    ),
                    Some(uuid(id)?),
                )
            }
            None => (None, None),
        };
        let mut rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(a)||jsonb_build_object('caller_role',COALESCE(m.role,'owner'),'draft_revision',d.revision,'published_channels',ARRAY(SELECT DISTINCT p.channel FROM agent_publications p WHERE p.tenant_id=a.tenant_id AND p.agent_id=a.agent_id AND p.status='active' ORDER BY p.channel)) FROM agents a LEFT JOIN agent_members m ON m.tenant_id=a.tenant_id AND m.agent_id=a.agent_id AND m.principal_type='user' AND m.principal_id=$2 JOIN agent_drafts d ON d.tenant_id=a.tenant_id AND d.draft_id=a.current_draft_id WHERE a.tenant_id=$1 AND a.deleted_at IS NULL AND ($3::boolean OR m.principal_id IS NOT NULL) AND ($4::text IS NULL OR a.status=$4) AND ($5::text IS NULL OR a.owner_id=$5) AND ($6::text IS NULL OR a.name ILIKE $6 OR a.slug ILIKE $6) AND ($7::text IS NULL OR EXISTS(SELECT 1 FROM agent_publications p WHERE p.tenant_id=a.tenant_id AND p.agent_id=a.agent_id AND p.channel=$7 AND p.status='active')) AND ($8::timestamptz IS NULL OR (a.updated_at,a.agent_id)<($8,$9::uuid)) ORDER BY a.updated_at DESC,a.agent_id DESC LIMIT $10")
            .bind(&actor.tenant_id).bind(&actor.user_id).bind(actor.is_admin()).bind(query.status).bind(query.owner_id).bind(query.search.map(|s|format!("%{s}%"))).bind(query.channel).bind(time).bind(id).bind(limit+1).fetch_all(&self.pool).await?;
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let cursor = if more {
            rows.last().map(|v| {
                URL_SAFE_NO_PAD
                    .encode(format!("{}|{}", text(v, "updated_at"), text(v, "agent_id")).as_bytes())
            })
        } else {
            None
        };
        Ok(json!({"items":rows,"next_cursor":cursor}))
    }
    pub async fn get(&self, actor: &Actor, id: &str) -> Result<Value, AgentError> {
        let mut c = self.pool.acquire().await?;
        let mut row = authorized(&mut c, actor, id, "viewer", false).await?;
        row["draft"]=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('revision',revision,'schema_version',schema_version,'spec_hash',spec_hash,'updated_at',updated_at) FROM agent_drafts WHERE tenant_id=$1 AND agent_id=$2::uuid").bind(&actor.tenant_id).bind(id).fetch_optional(&mut *c).await?.unwrap_or(json!({}));
        Ok(row)
    }
    pub async fn update(
        &self,
        actor: &Actor,
        id: &str,
        changes: &Value,
    ) -> Result<Value, AgentError> {
        if !["name", "slug", "description"]
            .iter()
            .any(|k| changes.get(k).is_some())
        {
            return self.get(actor, id).await;
        }
        let mut tx = self.pool.begin().await?;
        let auth = authorized(&mut tx, actor, id, "editor", true).await?;
        let mut row:Value=sqlx::query_scalar("UPDATE agents a SET name=COALESCE($4,name),description=COALESCE($5,description),slug=COALESCE($6,slug),updated_by=$3,updated_at=NOW() WHERE tenant_id=$1 AND agent_id=$2::uuid AND deleted_at IS NULL RETURNING to_jsonb(a)").bind(&actor.tenant_id).bind(id).bind(&actor.user_id).bind(changes.get("name").and_then(Value::as_str)).bind(changes.get("description").and_then(Value::as_str)).bind(changes.get("slug").and_then(Value::as_str).map(slug)).fetch_one(&mut *tx).await?;
        let fields: Vec<_> = ["description", "name", "slug"]
            .into_iter()
            .filter(|k| changes.get(*k).is_some())
            .collect();
        audit(&mut tx, actor, id, "update", json!({"fields":fields})).await?;
        tx.commit().await?;
        row["caller_role"] = auth["caller_role"].clone();
        Ok(row)
    }
    pub async fn draft(&self, actor: &Actor, id: &str) -> Result<Value, AgentError> {
        let mut c = self.pool.acquire().await?;
        authorized(&mut c, actor, id, "viewer", false).await?;
        let mut row: Value = sqlx::query_scalar(
            "SELECT to_jsonb(d) FROM agent_drafts d WHERE tenant_id=$1 AND agent_id=$2::uuid",
        )
        .bind(&actor.tenant_id)
        .bind(id)
        .fetch_optional(&mut *c)
        .await?
        .ok_or_else(not_found)?;
        row["spec"] = spec::redact_for_read(&row["spec"]);
        Ok(row)
    }
    pub async fn update_draft(
        &self,
        actor: &Actor,
        id: &str,
        revision: i64,
        value: &Value,
        changes: &Value,
    ) -> Result<Value, AgentError> {
        let mut tx = self.pool.begin().await?;
        active(&authorized(&mut tx, actor, id, "editor", true).await?)?;
        let errors = spec::safety_errors(value);
        if !errors.is_empty() {
            return Err(AgentError::validation(errors));
        }
        let draft:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_drafts d WHERE tenant_id=$1 AND agent_id=$2::uuid FOR UPDATE").bind(&actor.tenant_id).bind(id).fetch_optional(&mut *tx).await?.ok_or_else(not_found)?;
        let current = draft["revision"].as_i64().unwrap_or(0);
        if current != revision {
            return Err(AgentError::conflict(current));
        }
        self.replace_bindings(&mut tx, actor, text(&draft, "draft_id"), value)
            .await?;
        if changes.get("name").is_some() || changes.get("description").is_some() {
            sqlx::query("UPDATE agents SET name=COALESCE($4,name),description=COALESCE($5,description),updated_by=$3,updated_at=NOW() WHERE tenant_id=$1 AND agent_id=$2::uuid AND deleted_at IS NULL").bind(&actor.tenant_id).bind(id).bind(&actor.user_id).bind(changes.get("name").and_then(Value::as_str)).bind(changes.get("description").and_then(Value::as_str)).execute(&mut *tx).await?;
        }
        let row:Value=sqlx::query_scalar("UPDATE agent_drafts d SET revision=$4,schema_version=$5,spec=$6::jsonb,spec_hash=$7,updated_by=$8,updated_at=NOW() WHERE tenant_id=$1 AND agent_id=$2::uuid AND draft_id=$3::uuid RETURNING to_jsonb(d)").bind(&actor.tenant_id).bind(id).bind(text(&draft,"draft_id")).bind(revision+1).bind(spec::SCHEMA).bind(value).bind(spec::hash(value)).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        let fields: Vec<_> = ["description", "name"]
            .into_iter()
            .filter(|k| changes.get(*k).is_some())
            .collect();
        audit(
            &mut tx,
            actor,
            id,
            "draft_update",
            json!({"from_revision":revision,"to_revision":revision+1,"agent_fields":fields}),
        )
        .await?;
        binding_audit(&mut tx, actor, id, &draft["spec"], value).await?;
        tx.commit().await?;
        Ok(row)
    }
    pub async fn validate_draft(&self, actor: &Actor, id: &str) -> Result<Value, AgentError> {
        let draft = self.draft(actor, id).await?;
        let errors = spec::validate(&draft["spec"]);
        Ok(
            json!({"valid":errors.is_empty(),"revision":draft["revision"],"spec_hash":draft["spec_hash"],"errors":errors}),
        )
    }
    pub async fn create_version(
        &self,
        actor: &Actor,
        id: &str,
        revision: i64,
    ) -> Result<Value, AgentError> {
        let mut tx = self.pool.begin().await?;
        active(&authorized(&mut tx, actor, id, "owner", true).await?)?;
        let draft:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_drafts d WHERE tenant_id=$1 AND agent_id=$2::uuid FOR UPDATE").bind(&actor.tenant_id).bind(id).fetch_optional(&mut *tx).await?.ok_or_else(not_found)?;
        if draft["revision"].as_i64() != Some(revision) {
            return Err(AgentError::conflict(
                draft["revision"].as_i64().unwrap_or(0),
            ));
        }
        let row = self
            .insert_version(&mut tx, actor, id, &draft, None, None)
            .await?;
        audit(&mut tx,actor,id,"version_create",json!({"agent_version_id":row["agent_version_id"],"draft_revision":revision,"version_number":row["version_number"]})).await?;
        tx.commit().await?;
        Ok(row)
    }
    pub(crate) async fn insert_version(
        &self,
        conn: &mut PgConnection,
        actor: &Actor,
        id: &str,
        draft: &Value,
        evaluation: Option<&str>,
        identity: Option<&str>,
    ) -> Result<Value, AgentError> {
        let value = &draft["spec"];
        let errors = spec::validate(value);
        if !errors.is_empty() {
            return Err(AgentError::validation(errors));
        }
        let sealed = self
            .resources
            .seal_capabilities_in(actor, value, true, conn)
            .await?;
        let knowledge:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('dataset_id',dataset_id,'retrieval_config',retrieval_config) FROM agent_draft_knowledge_bindings WHERE tenant_id=$1 AND draft_id=$2::uuid ORDER BY dataset_id").bind(&actor.tenant_id).bind(text(draft,"draft_id")).fetch_all(&mut *conn).await?;
        let ids: Vec<_> = knowledge
            .iter()
            .map(|v| text(v, "dataset_id").to_owned())
            .collect();
        if !ids.is_empty()
            && self.resources.authorize_knowledge(actor, &ids).await?
                != ids.iter().cloned().collect()
        {
            return Err(AgentError::code(422, "AGENT_KNOWLEDGE_UNAVAILABLE"));
        }
        let number:i64=sqlx::query_scalar("SELECT (COALESCE(MAX(version_number),0)+1)::bigint FROM agent_versions WHERE tenant_id=$1 AND agent_id=$2::uuid").bind(&actor.tenant_id).bind(id).fetch_one(&mut *conn).await?;
        let version = uuid::Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO agent_versions(tenant_id,agent_version_id,agent_id,version_number,schema_version,resolved_spec,spec_hash,source_draft_id,source_draft_revision,created_by,release_evaluation_id,release_identity_hash) VALUES($1,$2::uuid,$3::uuid,$4,$5,$6::jsonb,$7,$8::uuid,$9,$10,$11::uuid,$12)").bind(&actor.tenant_id).bind(&version).bind(id).bind(number).bind(spec::SCHEMA).bind(value).bind(spec::hash(value)).bind(text(draft,"draft_id")).bind(draft["revision"].as_i64().unwrap_or(0)).bind(&actor.user_id).bind(evaluation).bind(identity).execute(&mut *conn).await?;
        for b in sealed.capabilities {
            sqlx::query("INSERT INTO agent_version_capabilities(tenant_id,agent_version_id,capability_type,resource_id,resource_version,schema_hash,config) VALUES($1,$2::uuid,$3,$4,$5,$6,$7::jsonb)").bind(&actor.tenant_id).bind(&version).bind(text(&b,"capability_type")).bind(text(&b,"resource_id")).bind(b["resource_version"].as_str()).bind(b["schema_hash"].as_str()).bind(super::object(&b["config"])).execute(&mut *conn).await?;
        }
        for b in sealed.skills {
            sqlx::query("INSERT INTO agent_version_skill_bindings(tenant_id,agent_version_id,skill_id,skill_version_id,skill_name,content_hash) VALUES($1,$2::uuid,$3::uuid,$4::uuid,$5,$6)").bind(&actor.tenant_id).bind(&version).bind(text(&b,"skill_id")).bind(text(&b,"skill_version_id")).bind(text(&b,"skill_name")).bind(text(&b,"content_hash")).execute(&mut *conn).await?;
        }
        for b in knowledge {
            sqlx::query("INSERT INTO agent_version_knowledge_bindings(tenant_id,agent_version_id,dataset_id,retrieval_config,bound_by,authorization_checked_at,content_mode,historical_replayable) VALUES($1,$2::uuid,$3,$4::jsonb,$5,NOW(),'live_latest',FALSE)").bind(&actor.tenant_id).bind(&version).bind(text(&b,"dataset_id")).bind(&b["retrieval_config"]).bind(&actor.user_id).execute(&mut *conn).await?;
        }
        Ok(sqlx::query_scalar("UPDATE agent_versions v SET bindings_sealed=TRUE WHERE tenant_id=$1 AND agent_version_id=$2::uuid RETURNING to_jsonb(v)").bind(&actor.tenant_id).bind(&version).fetch_one(conn).await?)
    }
    pub async fn versions(&self, actor: &Actor, id: &str) -> Result<Vec<Value>, AgentError> {
        let mut c = self.pool.acquire().await?;
        authorized(&mut c, actor, id, "viewer", false).await?;
        Ok(sqlx::query_scalar("SELECT to_jsonb(v) FROM agent_versions v WHERE tenant_id=$1 AND agent_id=$2::uuid ORDER BY version_number DESC").bind(&actor.tenant_id).bind(id).fetch_all(&mut *c).await?)
    }
    pub async fn members(&self, actor: &Actor, id: &str) -> Result<Vec<Value>, AgentError> {
        let mut c = self.pool.acquire().await?;
        authorized(&mut c, actor, id, "viewer", false).await?;
        Ok(sqlx::query_scalar("SELECT to_jsonb(m) FROM agent_members m WHERE tenant_id=$1 AND agent_id=$2::uuid ORDER BY role,principal_type,principal_id").bind(&actor.tenant_id).bind(id).fetch_all(&mut *c).await?)
    }
    pub async fn upsert_member(
        &self,
        actor: &Actor,
        id: &str,
        kind: &str,
        principal: &str,
        role: &str,
    ) -> Result<Value, AgentError> {
        if kind != "user" {
            return Err(AgentError::code(404, "AGENT_PRINCIPAL_NOT_FOUND"));
        }
        if rank(role) == 0 {
            return Err(AgentError::code(422, "AGENT_ROLE_INVALID"));
        }
        let mut tx = self.pool.begin().await?;
        authorized(&mut tx, actor, id, "owner", true).await?;
        let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE tenant_id=$1 AND user_id=$2 AND status='active')").bind(&actor.tenant_id).bind(principal).fetch_one(&mut *tx).await?;
        if !exists {
            return Err(AgentError::code(404, "AGENT_PRINCIPAL_NOT_FOUND"));
        }
        let row:Value=sqlx::query_scalar("INSERT INTO agent_members AS m(tenant_id,agent_id,principal_type,principal_id,role,created_by) VALUES($1,$2::uuid,$3,$4,$5,$6) ON CONFLICT(tenant_id,agent_id,principal_type,principal_id) DO UPDATE SET role=EXCLUDED.role,updated_at=NOW() RETURNING to_jsonb(m)").bind(&actor.tenant_id).bind(id).bind(kind).bind(principal).bind(role).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        audit(
            &mut tx,
            actor,
            id,
            "member_upsert",
            json!({"principal_type":kind,"principal_id":principal,"role":role}),
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }
    pub async fn remove_member(
        &self,
        actor: &Actor,
        id: &str,
        kind: &str,
        principal: &str,
    ) -> Result<(), AgentError> {
        let mut tx = self.pool.begin().await?;
        authorized(&mut tx, actor, id, "owner", true).await?;
        let result=sqlx::query("DELETE FROM agent_members WHERE tenant_id=$1 AND agent_id=$2::uuid AND principal_type=$3 AND principal_id=$4").bind(&actor.tenant_id).bind(id).bind(kind).bind(principal).execute(&mut *tx).await?;
        if result.rows_affected() != 1 {
            return Err(AgentError::code(404, "AGENT_MEMBER_NOT_FOUND"));
        }
        audit(
            &mut tx,
            actor,
            id,
            "member_remove",
            json!({"principal_type":kind,"principal_id":principal}),
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn archive(
        &self,
        actor: &Actor,
        id: &str,
        disable_publications: bool,
    ) -> Result<Value, AgentError> {
        let mut tx = self.pool.begin().await?;
        authorized(&mut tx, actor, id, "owner", true).await?;
        let row:Value=sqlx::query_scalar("UPDATE agents a SET status='archived',archived_at=NOW(),updated_by=$3 WHERE tenant_id=$1 AND agent_id=$2::uuid AND deleted_at IS NULL RETURNING to_jsonb(a)").bind(&actor.tenant_id).bind(id).bind(&actor.user_id).fetch_one(&mut *tx).await?;
        if disable_publications {
            sqlx::query("UPDATE agent_publications SET status='disabled',updated_by=$3 WHERE tenant_id=$1 AND agent_id=$2::uuid").bind(&actor.tenant_id).bind(id).bind(&actor.user_id).execute(&mut *tx).await?;
        }
        audit(
            &mut tx,
            actor,
            id,
            "archive",
            json!({"disable_publications":disable_publications}),
        )
        .await?;
        tx.commit().await?;
        Ok(row)
    }
    pub async fn soft_delete(&self, actor: &Actor, id: &str) -> Result<(), AgentError> {
        let mut tx = self.pool.begin().await?;
        authorized(&mut tx, actor, id, "owner", true).await?;
        sqlx::query("UPDATE agents SET status='deleted',deleted_at=NOW(),updated_by=$3 WHERE tenant_id=$1 AND agent_id=$2::uuid AND deleted_at IS NULL").bind(&actor.tenant_id).bind(id).bind(&actor.user_id).execute(&mut *tx).await?;
        sqlx::query("UPDATE agent_publications SET status='disabled',updated_by=$3 WHERE tenant_id=$1 AND agent_id=$2::uuid").bind(&actor.tenant_id).bind(id).bind(&actor.user_id).execute(&mut *tx).await?;
        audit(&mut tx, actor, id, "soft_delete", json!({})).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn resolve_preview(
        &self,
        actor: &Actor,
        id: &str,
        revision: i64,
    ) -> Result<Value, AgentError> {
        let mut c = self.pool.acquire().await?;
        let agent = authorized(&mut c, actor, id, "viewer", false).await?;
        active(&agent)?;
        let draft:Value=sqlx::query_scalar("SELECT to_jsonb(d) FROM agent_drafts d WHERE tenant_id=$1 AND agent_id=$2::uuid AND revision=$3").bind(&actor.tenant_id).bind(id).bind(revision).fetch_optional(&mut *c).await?.ok_or_else(||AgentError::code(409,"AGENT_PREVIEW_REVISION_STALE"))?;
        if !spec::validate(&draft["spec"]).is_empty() {
            return Err(AgentError::code(409, "AGENT_RUNTIME_SPEC_INVALID"));
        }
        let sealed = self
            .resources
            .seal_capabilities_in(actor, &draft["spec"], true, &mut c)
            .await?;
        let knowledge:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('dataset_id',dataset_id,'retrieval_config',retrieval_config) FROM agent_draft_knowledge_bindings WHERE tenant_id=$1 AND draft_id=$2::uuid ORDER BY dataset_id").bind(&actor.tenant_id).bind(text(&draft,"draft_id")).fetch_all(&mut *c).await?;
        Ok(
            json!({"agent":agent,"caller_role":agent["caller_role"],"draft":draft,"spec":draft["spec"],"capabilities":sealed.capabilities,"knowledge":knowledge,"publication":null}),
        )
    }
    pub async fn resolve_version(
        &self,
        actor: &Actor,
        id: &str,
        version: &str,
    ) -> Result<Value, AgentError> {
        let mut c = self.pool.acquire().await?;
        let agent = authorized(&mut c, actor, id, "viewer", false).await?;
        self.version_material(
            &mut c,
            &actor.tenant_id,
            &agent,
            version,
            "viewer",
            Value::Null,
        )
        .await
    }
    pub(crate) async fn version_material(
        &self,
        c: &mut PgConnection,
        tenant: &str,
        agent: &Value,
        version: &str,
        role: &str,
        publication: Value,
    ) -> Result<Value, AgentError> {
        active(agent)?;
        let version = uuid(version)?;
        let v:Value=sqlx::query_scalar("SELECT to_jsonb(v)||jsonb_build_object('revoked',EXISTS(SELECT 1 FROM agent_version_revocations r WHERE r.tenant_id=v.tenant_id AND r.agent_version_id=v.agent_version_id)) FROM agent_versions v WHERE v.tenant_id=$1 AND v.agent_id=$2::uuid AND v.agent_version_id=$3::uuid").bind(tenant).bind(text(agent,"agent_id")).bind(&version).fetch_optional(&mut *c).await?.ok_or_else(||AgentError::code(409,"AGENT_VERSION_REVOKED"))?;
        if v["revoked"] == true {
            return Err(AgentError::code(409, "AGENT_VERSION_REVOKED"));
        }
        if !spec::validate(&v["resolved_spec"]).is_empty() {
            return Err(AgentError::code(409, "AGENT_RUNTIME_SPEC_INVALID"));
        }
        let caps:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('capability_type',capability_type,'resource_id',resource_id,'resource_version',resource_version,'schema_hash',schema_hash,'config',config) FROM agent_version_capabilities WHERE tenant_id=$1 AND agent_version_id=$2::uuid ORDER BY capability_type,resource_id").bind(tenant).bind(&version).fetch_all(&mut *c).await?;
        let kb:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('dataset_id',dataset_id,'retrieval_config',retrieval_config) FROM agent_version_knowledge_bindings WHERE tenant_id=$1 AND agent_version_id=$2::uuid ORDER BY dataset_id").bind(tenant).bind(&version).fetch_all(&mut *c).await?;
        Ok(
            json!({"agent":agent,"caller_role":role,"version":v,"spec":v["resolved_spec"],"capabilities":caps,"knowledge":kb,"publication":publication}),
        )
    }
    pub async fn publications(&self, actor: &Actor, id: &str) -> Result<Vec<Value>, AgentError> {
        let mut c = self.pool.acquire().await?;
        authorized(&mut c, actor, id, "viewer", false).await?;
        Ok(sqlx::query_scalar("SELECT to_jsonb(p) FROM agent_publications p WHERE tenant_id=$1 AND agent_id=$2::uuid ORDER BY channel").bind(&actor.tenant_id).bind(id).fetch_all(&mut *c).await?)
    }
    pub async fn resolve_publication(
        &self,
        actor: &Actor,
        publication_id: &str,
        pin: Option<&str>,
    ) -> Result<Value, AgentError> {
        let mut c = self.pool.acquire().await?;
        let p:Value=sqlx::query_scalar("SELECT to_jsonb(p) FROM agent_publications p WHERE tenant_id=$1 AND publication_id=$2::uuid").bind(&actor.tenant_id).bind(uuid(publication_id)?).fetch_optional(&mut *c).await?.ok_or_else(||AgentError::code(409,"PUBLICATION_DISABLED"))?;
        require_active_publication(&p)?;
        let a = authorized(&mut c, actor, text(&p, "agent_id"), "viewer", false).await?;
        let version = pin.unwrap_or(text(&p, "version_id")).to_owned();
        self.version_material(
            &mut c,
            &actor.tenant_id,
            &a,
            &version,
            text(&a, "caller_role"),
            p,
        )
        .await
    }
}
pub fn require_active_publication(p: &Value) -> Result<(), AgentError> {
    if text(p, "status") != "active" {
        return Err(AgentError::code(409, "PUBLICATION_DISABLED"));
    }
    if let Some(t) = p["expires_at"].as_str() {
        if DateTime::parse_from_rfc3339(t)
            .map_err(|_| AgentError::code(409, "PUBLICATION_EXPIRED"))?
            .with_timezone(&Utc)
            <= Utc::now()
        {
            return Err(AgentError::code(409, "PUBLICATION_EXPIRED"));
        }
    }
    Ok(())
}
pub fn require_public_expiry(
    auth_mode: &str,
    policy: &Value,
) -> Result<Option<DateTime<Utc>>, AgentError> {
    if auth_mode != "public" {
        return Ok(None);
    }
    let t = DateTime::parse_from_rfc3339(text(policy, "expires_at"))
        .map_err(|_| AgentError::code(409, "AGENT_PUBLIC_EXPIRY_REQUIRED"))?
        .with_timezone(&Utc);
    if t <= Utc::now() {
        return Err(AgentError::code(409, "AGENT_PUBLIC_EXPIRY_EXPIRED"));
    }
    Ok(Some(t))
}
