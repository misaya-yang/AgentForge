//! Connector source ACLs intersect dataset permissions. Missing, stale and
//! unresolved imported ACLs fail closed; platform admin does not override source grants.
use super::types::UserContext;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Clause {
    #[serde(default)]
    pub users: Vec<String>,
    #[serde(default)]
    pub roles: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acl {
    pub schema: String,
    pub verified: bool,
    pub tenant_id: String,
    pub connection_id: String,
    pub source_revision: String,
    pub expires_at: i64,
    pub clauses: Vec<Clause>,
}
pub fn allowed(source: &str, metadata: &Value, actor: Option<&UserContext>) -> bool {
    let Some(raw) = metadata.get("_connector_acl") else {
        return source != "confluence";
    };
    let Ok(acl) = serde_json::from_value::<Acl>(raw.clone()) else {
        return false;
    };
    let Some(actor) = actor else { return false };
    acl.schema == "connector-acl/v1"
        && acl.verified
        && !actor.anonymous()
        && actor.tenant_id == acl.tenant_id
        && acl.expires_at > chrono::Utc::now().timestamp()
        && !acl.clauses.is_empty()
        && acl.clauses.len() <= 128
        && acl.clauses.iter().all(|c| {
            c.users.contains(&actor.user_id) || c.roles.iter().any(|r| actor.roles.contains(r))
        })
}
fn quote(v: &str) -> String {
    format!(
        "E'{}'",
        v.replace(char::from(92), "\\\\")
            .replace(char::from(39), "''")
    )
}
/// This returns an SQL expression, not a statement. The only interpolated actor
/// fields use PostgreSQL string-literal escaping; column aliases are fixed here.
pub fn predicate(actor: Option<&UserContext>) -> String {
    let base = "(COALESCE(d.source_type,'')<>'confluence' AND NOT(COALESCE(d.metadata,'{}'::jsonb) ? '_connector_acl'))";
    let Some(actor) = actor.filter(|a| !a.anonymous() && !a.tenant_id.is_empty()) else {
        return format!(" AND ({base}) ");
    };
    let roles = actor
        .roles
        .iter()
        .map(|r| quote(r))
        .collect::<Vec<_>>()
        .join(",");
    let roles = if roles.is_empty() {
        "ARRAY[]::text[]".into()
    } else {
        format!("ARRAY[{roles}]::text[]")
    };
    // CASE prevents malformed JSON from raising errors or acting as an empty grant.
    let clauses = "CASE WHEN jsonb_typeof(d.metadata->'_connector_acl'->'clauses')='array' THEN d.metadata->'_connector_acl'->'clauses' ELSE '[{}]'::jsonb END";
    format!(
        " AND ({base} OR (d.metadata->'_connector_acl'->>'schema'='connector-acl/v1' AND d.metadata->'_connector_acl'->'verified'='true'::jsonb AND d.metadata->'_connector_acl'->>'tenant_id'={} AND COALESCE(d.metadata->'_connector_acl'->>'expires_at','') ~ '^[0-9]{{1,12}}$' AND CASE WHEN COALESCE(d.metadata->'_connector_acl'->>'expires_at','') ~ '^[0-9]{{1,12}}$' THEN (d.metadata->'_connector_acl'->>'expires_at')::bigint > extract(epoch from clock_timestamp()) ELSE FALSE END AND jsonb_array_length({clauses}) BETWEEN 1 AND 128 AND NOT EXISTS(SELECT 1 FROM jsonb_array_elements({clauses}) c WHERE NOT(COALESCE((jsonb_typeof(c->'users')='array' AND c->'users' ? {}) OR (jsonb_typeof(c->'roles')='array' AND c->'roles' ?| {roles}),FALSE))))) ",
        quote(&actor.tenant_id),
        quote(&actor.user_id)
    )
}
pub fn current_predicate() -> String {
    predicate(super::model_plane::verified_actor_scope().as_ref())
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn inherited_restrictions_intersect_and_never_admin_bypass() {
        let mut a = UserContext {
            user_id: "u".into(),
            tenant_id: "t".into(),
            roles: vec!["staff".into(), "admin".into()],
            user_tier: "normal".into(),
            user_type: "user".into(),
            is_authenticated: true,
        };
        let mut m = json!({"_connector_acl":{"schema":"connector-acl/v1","verified":true,"tenant_id":"t","connection_id":"c","source_revision":"1","expires_at":chrono::Utc::now().timestamp()+60,"clauses":[{"users":["u"]},{"roles":["staff"]}]}});
        assert!(allowed("confluence", &m, Some(&a)));
        a.roles = vec!["admin".into()];
        assert!(!allowed("confluence", &m, Some(&a)));
        m["_connector_acl"]["verified"] = json!(false);
        assert!(!allowed("confluence", &m, Some(&a)));
        assert!(!allowed("confluence", &json!({}), Some(&a)));
    }
    #[test]
    fn sql_actor_strings_are_literal_escaped() {
        let a = UserContext {
            user_id: "x' OR TRUE --".into(),
            tenant_id: "t".into(),
            user_tier: "normal".into(),
            user_type: "user".into(),
            is_authenticated: true,
            roles: vec![],
        };
        assert!(predicate(Some(&a)).contains("E'x'' OR TRUE --'"));
        assert!(!allowed("confluence", &serde_json::json!({}), None));
    }
}
