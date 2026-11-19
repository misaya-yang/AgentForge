use super::*;
pub(super) fn expected_acl_additions() -> BTreeSet<String> {
    let mut expected = BTreeSet::new();
    for (schema, table) in TABLES {
        expected.insert(format!("owner:{schema}.{table}:owner"));
        expected.insert(format!("acl:{schema}.{table}:owner:DELETE,INSERT,REFERENCES,SELECT,TRIGGER,TRUNCATE,UPDATE:grantopt=0"));
    }
    for (table, privileges) in [
        ("task_batch_inputs", "SELECT"),
        ("task_batch_contract", "SELECT"),
        ("task_provider_batches", "INSERT,SELECT"),
        ("task_reconciliation_audit", "INSERT,SELECT"),
        ("task_batch_quota_holds", "INSERT,SELECT"),
    ] {
        expected.insert(format!(
            "acl:gateway.{table}:gateway:{privileges}:grantopt=0"
        ));
    }
    for column in ["provider_job_id", "confirmed_at"] {
        expected.insert(format!(
            "column_acl:gateway.task_provider_batches.{column}:gateway:UPDATE:grantopt=0"
        ));
    }
    for column in ["state", "settled_at", "released_at"] {
        expected.insert(format!(
            "column_acl:gateway.task_batch_quota_holds.{column}:gateway:UPDATE:grantopt=0"
        ));
    }
    for (schema, name, args) in FUNCTIONS {
        expected.insert(format!("function_owner:{schema}.{name}({args}):owner"));
        expected.insert(format!(
            "function_acl:{schema}.{name}({args}):owner:EXECUTE:grantopt=0"
        ));
    }
    for (schema, name, args) in &FUNCTIONS[..4] {
        expected.insert(format!(
            "function_acl:{schema}.{name}({args}):gateway:EXECUTE:grantopt=0"
        ));
    }
    expected
}

pub(super) fn verify_exact_fingerprints(previous: &Value, current: &Value) -> Result<()> {
    let old_lines: Vec<String> =
        serde_json::from_value(previous["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("029 predecessor ACL lines are malformed"))?;
    let new_lines: Vec<String> =
        serde_json::from_value(current["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("029 measured ACL lines are malformed"))?;
    let old = old_lines.iter().cloned().collect::<BTreeSet<_>>();
    let new = new_lines.iter().cloned().collect::<BTreeSet<_>>();
    let mut expected = old.clone();
    expected.extend(expected_acl_additions());
    if old.len() != old_lines.len()
        || new.len() != new_lines.len()
        || new != expected
        || previous["acl_compatibility"]["delta_line"] != ACL_DELTA
        || current["acl_compatibility"]["delta_line"] != ACL_DELTA
        || previous["acl_compatibility"]["revision"] != current["acl_compatibility"]["revision"]
        || previous["acl_compatibility"]["manifest_sha256"]
            != current["acl_compatibility"]["manifest_sha256"]
        || previous["acl_compatibility"]["removed_delta_count"]
            != current["acl_compatibility"]["removed_delta_count"]
        || previous["acl_compatibility"]["base_acl_sha256"]
            == current["acl_compatibility"]["base_acl_sha256"]
        || previous["acl_compatibility"]["effective_acl_sha256"]
            == current["acl_compatibility"]["effective_acl_sha256"]
        || previous["raw_fingerprints"]["acl"] == current["raw_fingerprints"]["acl"]
        || previous["fingerprints"]["acl"] == current["fingerprints"]["acl"]
    {
        return Err(refused(
            "029 raw ACL delta differs from the fixed allowlist",
        ));
    }
    for key in ["extensions", "reference_data"] {
        if previous["raw_fingerprints"][key] != current["raw_fingerprints"][key]
            || previous["fingerprints"][key] != current["fingerprints"][key]
        {
            return Err(refused(
                "029 changed extension or reference-data fingerprints",
            ));
        }
    }
    if previous["raw_fingerprints"]["structural"] == current["raw_fingerprints"]["structural"] {
        return Err(refused(
            "029 measurement did not change the structural fingerprint",
        ));
    }
    Ok(())
}
