use super::*;

pub(super) fn expected_acl_additions() -> BTreeSet<String> {
    let mut expected = BTreeSet::new();
    for (schema, table) in TABLES {
        expected.insert(format!("owner:{schema}.{table}:owner"));
        expected.insert(format!(
            "acl:{schema}.{table}:owner:DELETE,INSERT,REFERENCES,SELECT,TRIGGER,TRUNCATE,UPDATE:grantopt=0"
        ));
    }
    let table_grants = [
        ("knowledge_model_calls", "SELECT"),
        ("knowledge_model_workload_nonces", "INSERT"),
        ("dispatch_token_reservations", "INSERT,SELECT"),
        ("enterprise_identity_links", "INSERT,SELECT"),
        ("enterprise_scim_credentials", "INSERT,SELECT,UPDATE"),
        ("enterprise_scim_users", "INSERT,SELECT,UPDATE"),
        ("enterprise_scim_groups", "DELETE,INSERT,SELECT,UPDATE"),
        ("enterprise_scim_members", "DELETE,INSERT,SELECT"),
        ("enterprise_scim_role_map", "SELECT"),
        ("enterprise_security_state", "INSERT,SELECT,UPDATE"),
        ("enterprise_passkeys", "DELETE,INSERT,SELECT,UPDATE"),
        ("enterprise_recovery_codes", "DELETE,INSERT,SELECT,UPDATE"),
        ("enterprise_session_assurance", "INSERT,SELECT"),
        ("enterprise_tenant_policy", "SELECT"),
        ("routing_policy_versions", "INSERT,SELECT"),
        ("routing_policy_active", "INSERT,SELECT,UPDATE"),
        ("routing_decisions", "INSERT,SELECT"),
    ];
    for (table, privileges) in table_grants {
        expected.insert(format!(
            "acl:gateway.{table}:gateway:{privileges}:grantopt=0"
        ));
    }
    for column in [
        "state",
        "dispatched_at",
        "provider_request_id",
        "error_code",
        "observation",
    ] {
        expected.insert(format!(
            "column_acl:gateway.knowledge_model_calls.{column}:gateway:UPDATE:grantopt=0"
        ));
    }
    for column in [
        "state",
        "candidate_receipt_id",
        "settlement_candidate",
        "settlement_observation",
        "updated_at",
    ] {
        expected.insert(format!(
            "column_acl:gateway.dispatch_token_reservations.{column}:gateway:UPDATE:grantopt=0"
        ));
    }
    for (schema, name, arguments) in FUNCTIONS {
        expected.insert(format!("function_owner:{schema}.{name}({arguments}):owner"));
        expected.insert(format!(
            "function_acl:{schema}.{name}({arguments}):owner:EXECUTE:grantopt=0"
        ));
    }
    for (schema, name, arguments) in &FUNCTIONS[..2] {
        expected.insert(format!(
            "function_acl:{schema}.{name}({arguments}):gateway:EXECUTE:grantopt=0"
        ));
    }
    expected
}

pub(super) fn verify_exact_fingerprints(previous: &Value, current: &Value) -> Result<()> {
    let old_lines: Vec<String> =
        serde_json::from_value(previous["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("027 predecessor ACL lines are malformed"))?;
    let new_lines: Vec<String> =
        serde_json::from_value(current["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("027 measured ACL lines are malformed"))?;
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
            "027 raw ACL delta differs from the fixed allowlist",
        ));
    }
    for key in ["extensions", "reference_data"] {
        if previous["raw_fingerprints"][key] != current["raw_fingerprints"][key]
            || previous["fingerprints"][key] != current["fingerprints"][key]
        {
            return Err(refused(
                "027 changed extension or reference-data fingerprints",
            ));
        }
    }
    Ok(())
}
