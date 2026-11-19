//! Validate catalog exports or observe them through a service's mTLS connection.
//! Policy activation and SQL-layer DML isolation acceptance remain separate.
use super::{Result, ensure};
use ai_platform_service_primitives::data_connection::{ConnectionConfig, DatabaseRole};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const CATALOG_EXPORT: &str =
    include_str!("../../../../../../docs/design/proposals/data-platform/tenant-rls-catalog.sql");

/// Observe the real catalog using the selected service's verified mTLS identity.
/// The transaction is read-only and always rolled back, including on rejection.
pub async fn observe(config: &ConnectionConfig) -> Result<Value> {
    let pool = config.connect().await.map_err(|e| e.to_string())?;
    let result = async {
        let mut tx = pool
            .begin()
            .await
            .map_err(|_| "RLS observation begin failed")?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await
            .map_err(|_| "RLS read-only transaction failed")?;
        sqlx::query("SET LOCAL search_path=pg_catalog")
            .execute(&mut *tx)
            .await
            .map_err(|_| "RLS catalog search path failed")?;
        sqlx::query("SET LOCAL statement_timeout='10s'")
            .execute(&mut *tx)
            .await
            .map_err(|_| "RLS observation timeout setup failed")?;
        // The checked-in export is also executable by the authority. Here SQLx
        // owns its transaction, so only its single SELECT is reused.
        let start = CATALOG_EXPORT
            .find("\nSELECT jsonb_build_object(")
            .ok_or("RLS catalog query missing")?
            + 1;
        let query = CATALOG_EXPORT[start..]
            .trim()
            .strip_suffix("ROLLBACK;")
            .ok_or("RLS catalog export transaction boundary missing")?
            .trim();
        let raw: Value = sqlx::query_scalar(query)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| "RLS catalog observation failed")?;
        let catalog: Catalog =
            serde_json::from_value(raw).map_err(|_| "RLS catalog observation shape invalid")?;
        let mut report = verify(&catalog)?;
        tx.rollback()
            .await
            .map_err(|_| "RLS observation rollback failed")?;
        report["observed_application_role"] = json!(config.role.name());
        report["read_only_observation"] = json!(true);
        Ok(report)
    }
    .await;
    pool.close().await;
    result
}

const CONTROL_POLICY: &[(&str, &str, &str, &str)] = &[
    (
        "assistant.runtime_thread_scope_v1(uuid,boolean,boolean)",
        "ai_gateway_runtime",
        "assistant",
        "s",
    ),
    (
        "assistant.runtime_run_scope_v1(uuid)",
        "ai_gateway_runtime",
        "assistant",
        "s",
    ),
    (
        "assistant.recoverable_capability_executions_v1()",
        "ai_gateway_capability_worker",
        "assistant",
        "s",
    ),
    (
        "knowledge.discover_pipeline_work_v1()",
        "ai_gateway_knowledge_worker",
        "knowledge",
        "v",
    ),
    (
        "knowledge.claim_pipeline_work_v1(text,text,double precision)",
        "ai_gateway_knowledge_worker",
        "knowledge",
        "v",
    ),
    (
        "knowledge.document_ingest_scope_v1(text,text)",
        "ai_gateway_knowledge_worker",
        "knowledge",
        "s",
    ),
    (
        "knowledge.stuck_documents_v1(integer)",
        "ai_gateway_knowledge_worker",
        "knowledge",
        "s",
    ),
    (
        "knowledge.unfinished_publications_v1()",
        "ai_gateway_knowledge_worker",
        "knowledge",
        "s",
    ),
    (
        "knowledge.queued_documents_v1(bigint,text,double precision,text[])",
        "ai_gateway_knowledge_worker",
        "knowledge",
        "s",
    ),
    (
        "knowledge.claim_document_batch_item_v1(text,double precision)",
        "ai_gateway_knowledge_worker",
        "knowledge",
        "v",
    ),
    (
        "assistant.gateway_run_scope_v1(uuid)",
        "ai_gateway_gateway",
        "assistant",
        "s",
    ),
    (
        "assistant.gateway_model_lease_scope_v1(uuid)",
        "ai_gateway_gateway",
        "assistant",
        "s",
    ),
    (
        "assistant.gateway_model_call_scope_v1(uuid)",
        "ai_gateway_gateway",
        "assistant",
        "s",
    ),
    (
        "knowledge.next_confluence_acl_refresh_v1()",
        "ai_gateway_knowledge_api",
        "knowledge",
        "s",
    ),
];
pub const TABLES: &[&str] = &[
    "assistant.agent_runtime_attachments",
    "assistant.agent_runtime_feedback",
    "assistant.agent_runtime_idempotency",
    "assistant.artifact_shares",
    "assistant.artifacts",
    "assistant.assistant_audit_events",
    "assistant.assistant_capability_events",
    "assistant.assistant_capability_executions",
    "assistant.assistant_command_queue",
    "assistant.assistant_context_breakdown",
    "assistant.assistant_memory_chunks",
    "assistant.assistant_memory_reflections",
    "assistant.assistant_memory_sources",
    "assistant.assistant_run_checkpoints",
    "assistant.assistant_runs",
    "assistant.assistant_runtime_items",
    "assistant.assistant_runtime_model_calls",
    "assistant.assistant_runtime_model_leases",
    "assistant.assistant_runtime_snapshot_revocations",
    "assistant.assistant_runtime_snapshots",
    "assistant.assistant_runtime_thread_members",
    "assistant.assistant_runtime_thread_projections",
    "assistant.assistant_runtime_threads",
    "assistant.assistant_scheduler_jobs",
    "assistant.assistant_session_runtime_assignments",
    "assistant.assistant_skill_runs",
    "assistant.assistant_skill_version_revocations",
    "assistant.assistant_skill_versions",
    "assistant.assistant_skills",
    "assistant.assistant_tool_approvals",
    "assistant.conversation_shares",
    "assistant.exams",
    "assistant.image_tasks",
    "assistant.quizzes",
    "assistant.session_memory",
    "assistant.sessions",
    "assistant.user_memory",
    "knowledge.child_chunks",
    "knowledge.confluence_connections",
    "knowledge.confluence_space_bindings",
    "knowledge.dataset_collection_bindings",
    "knowledge.dataset_query_feedback",
    "knowledge.datasets",
    "knowledge.kb_bm25_v2_lifecycle",
    "knowledge.kb_document_batch_operations",
    "knowledge.kb_parsing_ir",
    "knowledge.kb_parsing_page_cache",
    "knowledge.kb_segment_attachment_bindings",
    "knowledge.documents",
    "knowledge.segments",
];
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema: String,
    pub database: String,
    pub roles: Vec<Role>,
    pub tables: Vec<Table>,
    pub controls: Vec<Control>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Control {
    pub signature: String,
    pub owner: String,
    pub security_definer: bool,
    pub volatility: String,
    pub configuration: Vec<String>,
    pub execute_roles: Vec<String>,
    pub grantable: bool,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Role {
    pub name: String,
    pub superuser: bool,
    pub bypassrls: bool,
    pub inherits_owner: bool,
    pub inherits_privileged: bool,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Table {
    pub relation: String,
    pub owner: String,
    pub enabled: bool,
    pub forced: bool,
    pub policies: Vec<Policy>,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub name: String,
    pub permissive: bool,
    pub command: String,
    pub roles: Vec<String>,
    pub using_expression: String,
    pub check_expression: String,
}
pub fn predicate(relation: &str) -> Result<String> {
    ensure(TABLES.contains(&relation), "unknown RLS table")?;
    if matches!(relation, "knowledge.documents" | "knowledge.segments") {
        Ok(format!(
            "EXISTS (SELECT 1 FROM knowledge.datasets AS tenant_dataset WHERE tenant_dataset.dataset_id={}.dataset_id AND tenant_dataset.tenant_id=NULLIF(pg_catalog.current_setting('app.tenant_id',true),''))",
            relation.split('.').next_back().unwrap()
        ))
    } else {
        Ok("tenant_id=NULLIF(pg_catalog.current_setting('app.tenant_id',true),'')".into())
    }
}
// pg_get_expr is server-parsed SQL. Ignore harmless render parentheses, catalog
// qualification and text casts only. No OR/NOT/function/subquery is accepted
// unless it is already in the exact admitted predicate token sequence. Quoted
// literal contents remain opaque. Export must run with search_path=pg_catalog.
fn canonical(sql: &str) -> Result<Vec<String>> {
    let b = sql.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        if b[i].is_ascii_whitespace() || matches!(b[i], b'(' | b')') {
            i += 1;
            continue;
        }
        let start = i;
        if matches!(b[i], b'\'' | b'"') {
            let quote = b[i];
            i += 1;
            loop {
                ensure(i < b.len(), "unclosed RLS catalog expression")?;
                if b[i] == quote {
                    i += 1;
                    if i < b.len() && b[i] == quote {
                        i += 1;
                    } else {
                        break;
                    }
                } else {
                    i += 1;
                }
            }
            out.push(sql[start..i].to_string());
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'_' {
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(sql[start..i].to_ascii_lowercase());
        } else {
            ensure(b[i].is_ascii(), "unsupported RLS catalog expression")?;
            i += 1;
            out.push(sql[start..i].to_string());
        }
    }
    let mut result = Vec::new();
    let mut i = 0;
    while i < out.len() {
        if out[i] == "as" {
            i += 1;
            continue;
        }
        if out[i] == "pg_catalog" && out.get(i + 1).is_some_and(|t| t == ".") {
            i += 2;
            continue;
        }
        if out[i] == ":"
            && out.get(i + 1).is_some_and(|t| t == ":")
            && out.get(i + 2).is_some_and(|t| t == "text")
        {
            i += 3;
            continue;
        }
        result.push(out[i].clone());
        i += 1;
    }
    Ok(result)
}
pub fn verify(c: &Catalog) -> Result<Value> {
    ensure(
        c.schema == "agentforge-tenant-rls-catalog/v1" && !c.database.is_empty(),
        "RLS catalog identity missing",
    )?;
    let controls = CONTROL_POLICY
        .iter()
        .map(|(signature, role, namespace, volatility)| {
            (*signature, (*role, *namespace, *volatility))
        })
        .collect::<BTreeMap<_, _>>();
    ensure(
        c.controls.len() == controls.len()
            && c.controls
                .iter()
                .map(|control| control.signature.as_str())
                .collect::<BTreeSet<_>>()
                == controls.keys().copied().collect(),
        "RLS control entrypoint inventory mismatch",
    )?;
    for control in &c.controls {
        let (role, namespace, volatility) = controls[control.signature.as_str()];
        ensure(
            control.owner == "ai_gateway_owner"
                && control.security_definer
                && control.volatility == volatility
                && !control.grantable
                && control.configuration.len() == 1
                && control.configuration[0].replace(' ', "")
                    == format!("search_path=pg_catalog,{namespace},pg_temp")
                && control.execute_roles.len() == 2
                && control
                    .execute_roles
                    .iter()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>()
                    == BTreeSet::from(["ai_gateway_owner", role]),
            "RLS control entrypoint broadens authority",
        )?;
    }
    let roles = DatabaseRole::ALL
        .map(|r| r.name().to_string())
        .into_iter()
        .collect::<BTreeSet<_>>();
    ensure(
        c.roles.len() == roles.len()
            && c.roles
                .iter()
                .map(|r| r.name.clone())
                .collect::<BTreeSet<_>>()
                == roles,
        "RLS role inventory mismatch",
    )?;
    ensure(
        c.roles
            .iter()
            .all(|r| !r.superuser && !r.bypassrls && !r.inherits_owner && !r.inherits_privileged),
        "application bypasses FORCE RLS",
    )?;
    ensure(
        c.tables.len() == TABLES.len()
            && c.tables
                .iter()
                .map(|t| t.relation.as_str())
                .collect::<BTreeSet<_>>()
                == TABLES.iter().copied().collect(),
        "RLS table inventory mismatch",
    )?;
    for t in &c.tables {
        ensure(
            t.owner == "ai_gateway_owner" && t.enabled && t.forced,
            "RLS owner/ENABLE/FORCE mismatch",
        )?;
        let policies = t
            .policies
            .iter()
            .map(|p| (p.name.as_str(), p))
            .collect::<BTreeMap<_, _>>();
        ensure(
            t.policies.len() == 3 && policies.len() == 3,
            "ambiguous RLS policies",
        )?;
        let expected = canonical(&predicate(&t.relation)?)?;
        for (name, permissive) in [("tenant_allow", true), ("tenant_guard", false)] {
            let p = policies.get(name).ok_or("required RLS policy absent")?;
            ensure(
                p.permissive == permissive
                    && p.command == "*"
                    && p.roles.len() == roles.len()
                    && p.roles.iter().cloned().collect::<BTreeSet<_>>() == roles,
                "RLS policy authority mismatch",
            )?;
            ensure(
                canonical(&p.using_expression)? == expected
                    && canonical(&p.check_expression)? == expected,
                "RLS USING/WITH CHECK predicate mismatch",
            )?;
        }
        let p = policies
            .get("authority_maintenance")
            .ok_or("owner maintenance policy missing")?;
        ensure(
            p.permissive
                && p.command == "*"
                && p.roles == ["ai_gateway_owner"]
                && canonical(&p.using_expression)? == ["true"]
                && canonical(&p.check_expression)? == ["true"],
            "maintenance role/policy broadened",
        )?;
    }
    Ok(
        json!({"status":"passed","scope":"operator_exported_catalog_structure_only","tables":TABLES.len(),"application_roles":roles.len(),"runtime_acceptance":false,"sql_layer_cross_tenant_dml":"not_executed","query_plan_regressions":"not_measured","effects":false}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Catalog {
        Catalog {
            controls: CONTROL_POLICY
                .iter()
                .map(|(signature, role, namespace, volatility)| Control {
                    signature: (*signature).into(),
                    owner: "ai_gateway_owner".into(),
                    security_definer: true,
                    volatility: (*volatility).into(),
                    configuration: vec![format!("search_path=pg_catalog, {namespace}, pg_temp")],
                    execute_roles: vec!["ai_gateway_owner".into(), (*role).into()],
                    grantable: false,
                })
                .collect(),
            schema: "agentforge-tenant-rls-catalog/v1".into(),
            database: "isolated-fixture".into(),
            roles: DatabaseRole::ALL
                .into_iter()
                .map(|r| Role {
                    name: r.name().into(),
                    superuser: false,
                    bypassrls: false,
                    inherits_owner: false,
                    inherits_privileged: false,
                })
                .collect(),
            tables: TABLES
                .iter()
                .map(|t| Table {
                    relation: t.to_string(),
                    owner: "ai_gateway_owner".into(),
                    enabled: true,
                    forced: true,
                    policies: vec![
                        Policy {
                            name: "tenant_allow".into(),
                            permissive: true,
                            command: "*".into(),
                            roles: DatabaseRole::ALL.map(|r| r.name().into()).into(),
                            using_expression: predicate(t).unwrap(),
                            check_expression: predicate(t).unwrap(),
                        },
                        Policy {
                            name: "tenant_guard".into(),
                            permissive: false,
                            command: "*".into(),
                            roles: DatabaseRole::ALL.map(|r| r.name().into()).into(),
                            using_expression: predicate(t).unwrap(),
                            check_expression: predicate(t).unwrap(),
                        },
                        Policy {
                            name: "authority_maintenance".into(),
                            permissive: true,
                            command: "*".into(),
                            roles: vec!["ai_gateway_owner".into()],
                            using_expression: "true".into(),
                            check_expression: "true".into(),
                        },
                    ],
                })
                .collect(),
        }
    }
    #[test]
    fn full_first_stage_policy_and_pg_catalog_rendering_are_checked() {
        let mut c = fixture();
        let p =
            "((tenant_id)::text = NULLIF(current_setting('app.tenant_id'::text, true), ''::text))";
        c.tables[0].policies[0].using_expression = p.into();
        let document = c
            .tables
            .iter_mut()
            .find(|t| t.relation == "knowledge.documents")
            .unwrap();
        document.policies[1].check_expression="(EXISTS ( SELECT 1 FROM knowledge.datasets tenant_dataset WHERE (((tenant_dataset.dataset_id)::text = (documents.dataset_id)::text) AND ((tenant_dataset.tenant_id)::text = NULLIF(current_setting('app.tenant_id'::text, true), ''::text)))))".into();
        assert_eq!(verify(&c).unwrap()["runtime_acceptance"], false);
    }
    #[test]
    fn force_bypass_membership_and_missing_tables_are_refused() {
        for n in 0..5 {
            let mut c = fixture();
            match n {
                0 => c.tables[0].forced = false,
                1 => c.roles[0].bypassrls = true,
                2 => c.roles[0].inherits_owner = true,
                3 => c.roles[0].inherits_privileged = true,
                _ => {
                    c.tables.pop();
                }
            }
            assert!(verify(&c).is_err());
        }
    }
    #[test]
    fn control_entrypoints_refuse_public_grants_and_changed_execution_identity() {
        for index in 0..5 {
            let mut c = fixture();
            match index {
                0 => c.controls[0].execute_roles.push("PUBLIC".into()),
                1 => c.controls[0].owner = "ai_gateway_runtime".into(),
                2 => c.controls[0].configuration.push("row_security=off".into()),
                3 => c.controls[0].grantable = true,
                _ => {
                    c.controls.pop();
                }
            }
            assert!(verify(&c).is_err());
        }
    }
    #[test]
    fn permissive_escape_write_check_drift_and_foreign_parent_are_refused() {
        for sql in [
            "true",
            "tenant_id=NULLIF(current_setting('app.tenant_id',true),'') OR true",
            "tenant_id=NULLIF(current_setting('app.tenant_id',true),'tenant-b')",
            "tenant_id=NULLIF(evil.current_setting('app.tenant_id',true),'')",
        ] {
            let mut c = fixture();
            c.tables[0].policies[1].check_expression = sql.into();
            assert!(verify(&c).is_err());
        }
        let mut c = fixture();
        c.tables
            .iter_mut()
            .find(|t| t.relation == "knowledge.documents")
            .unwrap()
            .policies[0]
            .using_expression = predicate("knowledge.segments").unwrap();
        assert!(verify(&c).is_err());
    }
}
