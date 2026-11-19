//! Closed, host-path-free frozen cleanup plans, inventories and receipts.
use super::{AgentError, spec, text};
use serde_json::{Value, json};
use std::collections::BTreeSet;
fn error(code: &str) -> AgentError {
    AgentError::code(409, code)
}
fn exact(v: &Value, keys: &[&str]) -> bool {
    v.as_object()
        .is_some_and(|m| m.len() == keys.len() && keys.iter().all(|k| m.contains_key(*k)))
}
fn hex(s: &str, prefix: &str, n: usize) -> bool {
    s.strip_prefix(prefix).is_some_and(|s| {
        s.len() == n
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}
pub fn principal(s: &str) -> bool {
    hex(s, "am_", 61) || hex(s, "agent-memory:", 64)
}
pub fn digest(value: &Value) -> String {
    let mut value = value.clone();
    if let Some(m) = value.as_object_mut() {
        for k in ["plan_digest", "inventory_digest", "receipt_digest"] {
            m.remove(k);
        }
    }
    format!("sha256:{}", spec::hash(&value))
}
fn stable(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.as_bytes()[0].is_ascii_lowercase()
        && s.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}
fn points(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 255
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
}
fn number(v: &Value, key: &str, n: usize) -> bool {
    v[key].as_u64() == Some(n as u64)
}
pub fn build_plan(
    id: &str,
    tenant: &str,
    agent: &str,
    scope: &str,
    subject: Option<&str>,
    cutoff: &str,
    handles: Vec<String>,
) -> Result<Value, AgentError> {
    let handles: BTreeSet<_> = handles.into_iter().collect();
    if handles.iter().any(|s| !principal(s)) {
        return Err(error("AGENT_RUNTIME_CLEANUP_PRINCIPAL_INVALID"));
    }
    let mut plan = json!({"schema_version":"agent-runtime-cleanup-plan/v1","deletion_id":id,"tenant_id":tenant,"agent_id":agent,"scope":scope,"subject_user_id":subject,"cutoff_at":cutoff,"principal_count":handles.len(),"principal_handles":handles});
    plan["plan_digest"] = digest(&plan).into();
    validate_plan(&plan)?;
    Ok(plan)
}
pub fn validate_plan(v: &Value) -> Result<(), AgentError> {
    let invalid = || error("AGENT_RUNTIME_CLEANUP_PLAN_INVALID");
    if !exact(
        v,
        &[
            "schema_version",
            "deletion_id",
            "tenant_id",
            "agent_id",
            "scope",
            "subject_user_id",
            "cutoff_at",
            "principal_handles",
            "principal_count",
            "plan_digest",
        ],
    ) || v["schema_version"] != "agent-runtime-cleanup-plan/v1"
    {
        return Err(invalid());
    }
    let handles = v["principal_handles"].as_array().ok_or_else(invalid)?;
    let mut previous = "";
    for item in handles {
        let s = item.as_str().ok_or_else(invalid)?;
        if !principal(s) || s <= previous {
            return Err(invalid());
        }
        previous = s;
    }
    if !number(v, "principal_count", handles.len())
        || ["deletion_id", "tenant_id", "agent_id", "scope", "cutoff_at"]
            .iter()
            .any(|k| text(v, k).trim().is_empty())
        || chrono::DateTime::parse_from_rfc3339(text(v, "cutoff_at")).is_err()
        || !matches!(text(v, "scope"), "retention" | "user" | "tenant")
        || ((v["scope"] == "user") != v["subject_user_id"].as_str().is_some_and(|s| !s.is_empty()))
    {
        return Err(invalid());
    }
    if !hex(text(v, "plan_digest"), "sha256:", 64) || v["plan_digest"] != digest(v) {
        return Err(error("AGENT_RUNTIME_CLEANUP_PLAN_DIGEST_INVALID"));
    }
    Ok(())
}
pub fn validate_inventory(v: &Value, plan: &Value) -> Result<(), AgentError> {
    validate_plan(plan)?;
    let invalid = || error("AGENT_RUNTIME_CLEANUP_INVENTORY_INVALID");
    let mismatch = || error("AGENT_RUNTIME_CLEANUP_INVENTORY_SCOPE_MISMATCH");
    if !exact(
        v,
        &[
            "schema_version",
            "deletion_id",
            "tenant_id",
            "agent_id",
            "plan_digest",
            "cutoff_at",
            "principal_count",
            "source_count",
            "vector_count",
            "principals",
            "inventory_digest",
        ],
    ) || v["schema_version"] != "agent-runtime-cleanup-inventory/v1"
    {
        return Err(invalid());
    }
    if [
        "deletion_id",
        "tenant_id",
        "agent_id",
        "plan_digest",
        "cutoff_at",
    ]
    .iter()
    .any(|k| v[*k] != plan[*k])
    {
        return Err(mismatch());
    }
    let expected: BTreeSet<_> = plan["principal_handles"]
        .as_array()
        .expect("validated")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let mut observed = BTreeSet::new();
    let mut total_sources = 0;
    let mut total_vectors = 0;
    let principals = v["principals"].as_array().ok_or_else(invalid)?;
    for p in principals {
        if !exact(
            p,
            &[
                "principal_id",
                "source_count",
                "sources",
                "vector_count",
                "vector_sets",
            ],
        ) {
            return Err(invalid());
        }
        let id = text(p, "principal_id");
        if !expected.contains(id) || !observed.insert(id) {
            return Err(mismatch());
        }
        let sources = p["sources"].as_array().ok_or_else(invalid)?;
        let mut handles = BTreeSet::new();
        let mut scopes = BTreeSet::new();
        for s in sources {
            if !exact(
                s,
                &[
                    "logical_source_scope_handle",
                    "source_handle",
                    "version_digest",
                    "source_type",
                ],
            ) || !hex(text(s, "source_handle"), "memsrc_", 32)
                || !hex(text(s, "logical_source_scope_handle"), "memscope_", 32)
                || !hex(text(s, "version_digest"), "sha256:", 64)
                || !stable(text(s, "source_type"))
                || !handles.insert(text(s, "source_handle"))
                || !scopes.insert(text(s, "logical_source_scope_handle"))
            {
                return Err(invalid());
            }
        }
        if !number(p, "source_count", sources.len()) {
            return Err(invalid());
        }
        total_sources += sources.len();
        let vectors = p["vector_sets"].as_array().ok_or_else(invalid)?;
        let mut collections = BTreeSet::new();
        let mut count = 0;
        for set in vectors {
            if !exact(
                set,
                &[
                    "collection_kind",
                    "collection_handle",
                    "point_count",
                    "points",
                ],
            ) || !matches!(text(set, "collection_kind"), "current" | "legacy")
                || !hex(text(set, "collection_handle"), "memvec_", 32)
                || !collections.insert(text(set, "collection_handle"))
            {
                return Err(invalid());
            }
            let values = set["points"].as_array().ok_or_else(invalid)?;
            let mut ids = BTreeSet::new();
            for point in values {
                if !exact(point, &["point_id", "version_digest"])
                    || !points(text(point, "point_id"))
                    || !hex(text(point, "version_digest"), "sha256:", 64)
                    || !ids.insert(text(point, "point_id"))
                {
                    return Err(invalid());
                }
            }
            if !number(set, "point_count", values.len()) {
                return Err(invalid());
            }
            count += values.len();
        }
        if !number(p, "vector_count", count) {
            return Err(invalid());
        }
        total_vectors += count;
    }
    if observed != expected {
        return Err(mismatch());
    }
    if !number(v, "principal_count", principals.len())
        || !number(v, "source_count", total_sources)
        || !number(v, "vector_count", total_vectors)
    {
        return Err(invalid());
    }
    if !hex(text(v, "inventory_digest"), "sha256:", 64) || v["inventory_digest"] != digest(v) {
        return Err(error("AGENT_RUNTIME_CLEANUP_INVENTORY_DIGEST_INVALID"));
    }
    Ok(())
}
pub fn validate_receipt(v: &Value, plan: &Value, inventory: &Value) -> Result<(), AgentError> {
    validate_inventory(inventory, plan)?;
    let invalid = || error("AGENT_RUNTIME_CLEANUP_RECEIPT_INVALID");
    let mismatch = || error("AGENT_RUNTIME_CLEANUP_RECEIPT_SCOPE_MISMATCH");
    if !exact(
        v,
        &[
            "schema_version",
            "deletion_id",
            "tenant_id",
            "agent_id",
            "plan_digest",
            "inventory_digest",
            "status",
            "completed",
            "retryable",
            "principals",
            "errors",
            "receipt_digest",
        ],
    ) || v["schema_version"] != "agent-runtime-cleanup-receipt/v1"
    {
        return Err(invalid());
    }
    if ["deletion_id", "tenant_id", "agent_id", "plan_digest"]
        .iter()
        .any(|k| v[*k] != plan[*k])
        || v["inventory_digest"] != inventory["inventory_digest"]
    {
        return Err(mismatch());
    }
    let expected: std::collections::BTreeMap<_, _> = inventory["principals"]
        .as_array()
        .expect("validated")
        .iter()
        .map(|p| (text(p, "principal_id"), p))
        .collect();
    let mut observed = BTreeSet::new();
    let mut errors = BTreeSet::new();
    let mut complete = true;
    for p in v["principals"].as_array().ok_or_else(invalid)? {
        if !exact(
            p,
            &[
                "principal_id",
                "status",
                "completed",
                "retryable",
                "source_count",
                "deleted_source_count",
                "vector_count",
                "deleted_vector_count",
                "idempotent_absent_count",
                "idempotent_absent_vector_count",
                "errors",
            ],
        ) {
            return Err(invalid());
        }
        let id = text(p, "principal_id");
        let frozen = expected.get(id).ok_or_else(mismatch)?;
        if !observed.insert(id) {
            return Err(mismatch());
        }
        let done = p["completed"].as_bool().ok_or_else(invalid)?;
        if p["retryable"].as_bool() != Some(!done)
            || p["status"] != if done { "completed" } else { "partial" }
        {
            return Err(invalid());
        }
        let n = |key: &str| p[key].as_u64().ok_or_else(invalid);
        let sc = n("source_count")?;
        let vc = n("vector_count")?;
        let sd = n("deleted_source_count")?
            .checked_add(n("idempotent_absent_count")?)
            .ok_or_else(invalid)?;
        let vd = n("deleted_vector_count")?
            .checked_add(n("idempotent_absent_vector_count")?)
            .ok_or_else(invalid)?;
        if frozen["source_count"].as_u64() != Some(sc)
            || frozen["vector_count"].as_u64() != Some(vc)
            || sd > sc
            || vd > vc
            || (done && (sd != sc || vd != vc))
        {
            return Err(invalid());
        }
        let err = p["errors"].as_array().ok_or_else(invalid)?;
        let mut seen = BTreeSet::new();
        for e in err {
            let e = e.as_str().ok_or_else(invalid)?;
            if !stable(e) || !seen.insert(e) {
                return Err(invalid());
            }
            errors.insert(e);
        }
        if done != err.is_empty() {
            return Err(invalid());
        }
        complete &= done;
    }
    if observed != expected.keys().copied().collect() {
        return Err(mismatch());
    }
    if v["status"] != if complete { "completed" } else { "partial" }
        || v["completed"].as_bool() != Some(complete)
        || v["retryable"].as_bool() != Some(!complete)
        || v["errors"] != json!(errors)
    {
        return Err(invalid());
    }
    if !hex(text(v, "receipt_digest"), "sha256:", 64) || v["receipt_digest"] != digest(v) {
        return Err(error("AGENT_RUNTIME_CLEANUP_RECEIPT_DIGEST_INVALID"));
    }
    Ok(())
}
pub fn completed(v: &Value) -> bool {
    v["status"] == "completed"
        && v["completed"] == true
        && v["retryable"] == false
        && v["principals"].as_array().is_some_and(|a| {
            a.iter().all(|p| {
                p["status"] == "completed" && p["completed"] == true && p["retryable"] == false
            })
        })
}
