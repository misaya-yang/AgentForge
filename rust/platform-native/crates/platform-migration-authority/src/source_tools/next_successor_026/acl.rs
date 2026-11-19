use super::*;

fn owner_precondition() -> String {
    "SELECT current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)<>current_user) AND (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge'])) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls)) AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'gateway') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'runtime') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_api') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_worker')".into()
}
fn target_objects_absent() -> String {
    let tables = TABLES
        .iter()
        .map(|(schema, table)| format!("pg_catalog.to_regclass('{schema}.{table}') IS NULL"))
        .collect::<Vec<_>>();
    let functions = [
        "gateway.price_book_v2_validate_publication()",
        "gateway.price_book_v2_checkpoint_receipt()",
        "gateway.price_book_v2_reject_mutation()",
        "assistant.runtime_cache_settlement_v2_immutable()",
        "assistant.assert_capability_witness_binding()",
        "assistant.complete_assistant_runtime_model_call_v2(uuid,character varying,bigint,bigint,bigint,character varying,jsonb)",
    ];
    tables
        .into_iter()
        .chain(
            functions
                .into_iter()
                .map(|function| format!("pg_catalog.to_regprocedure('{function}') IS NULL")),
        )
        .collect::<Vec<_>>()
        .join(" AND ")
}
fn dependencies_present() -> &'static str {
    "SELECT pg_catalog.to_regclass('assistant.assistant_runtime_model_calls') IS NOT NULL AND pg_catalog.to_regclass('assistant.assistant_runtime_snapshots') IS NOT NULL AND pg_catalog.to_regclass('assistant.assistant_sdk_runs') IS NOT NULL AND pg_catalog.to_regclass('assistant.assistant_runs') IS NOT NULL AND pg_catalog.to_regprocedure('assistant.complete_assistant_runtime_model_call(uuid,bigint,bigint,bigint,character varying)') IS NOT NULL AND pg_catalog.to_regclass('gateway.local_node_channels') IS NOT NULL AND pg_catalog.to_regclass('gateway.local_node_devices') IS NOT NULL AND pg_catalog.to_regclass('gateway.local_node_device_credentials') IS NOT NULL"
}
fn owner_postcondition() -> String {
    let relations = TABLES
        .iter()
        .map(|(schema, table)| format!("('{schema}','{table}')"))
        .collect::<Vec<_>>()
        .join(",");
    "SELECT current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)<>current_user) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls)) AND NOT EXISTS (SELECT 1 FROM (VALUES ".to_owned()
        + &relations
        + ") AS expected(schema_name,relation_name) JOIN pg_catalog.pg_namespace n ON n.nspname=expected.schema_name JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname=expected.relation_name WHERE pg_catalog.pg_get_userbyid(c.relowner)<>current_user) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname||'.'||p.proname=ANY(ARRAY['gateway.price_book_v2_validate_publication','gateway.price_book_v2_checkpoint_receipt','gateway.price_book_v2_reject_mutation','assistant.runtime_cache_settlement_v2_immutable','assistant.assert_capability_witness_binding','assistant.complete_assistant_runtime_model_call_v2']) AND pg_catalog.pg_get_userbyid(p.proowner)<>current_user)"
}
fn target_objects_present() -> String {
    let tables = TABLES
        .iter()
        .map(|(schema, table)| format!("pg_catalog.to_regclass('{schema}.{table}') IS NOT NULL"))
        .collect::<Vec<_>>();
    let functions = [
        "gateway.price_book_v2_validate_publication()",
        "gateway.price_book_v2_checkpoint_receipt()",
        "gateway.price_book_v2_reject_mutation()",
        "assistant.runtime_cache_settlement_v2_immutable()",
        "assistant.assert_capability_witness_binding()",
        "assistant.complete_assistant_runtime_model_call_v2(uuid,character varying,bigint,bigint,bigint,character varying,jsonb)",
    ];
    tables
        .into_iter()
        .chain(
            functions
                .into_iter()
                .map(|function| format!("pg_catalog.to_regprocedure('{function}') IS NOT NULL")),
        )
        .collect::<Vec<_>>()
        .join(" AND ")
}
fn expected_relation_acl() -> String {
    let names = TABLES
        .iter()
        .map(|(schema, table)| format!("('{schema}','{table}')"))
        .collect::<Vec<_>>()
        .join(",");
    "SELECT NOT EXISTS (SELECT 1 FROM (VALUES ".to_owned()
        + &names
        + ") AS expected(schema_name,relation_name) JOIN pg_catalog.pg_namespace n ON n.nspname=expected.schema_name JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname=expected.relation_name CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) a WHERE a.grantee=0 OR (pg_catalog.pg_get_userbyid(a.grantee)=left(current_user,length(current_user)-length('owner'))||'runtime' AND NOT (n.nspname='assistant' AND c.relname='assistant_runtime_capability_witnesses' AND a.privilege_type='SELECT' AND NOT a.is_grantable AND pg_catalog.pg_get_userbyid(a.grantor)=current_user)))"
}
fn expected_function_acl() -> &'static str {
    "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) a WHERE n.nspname||'.'||p.proname=ANY(ARRAY['gateway.price_book_v2_validate_publication','gateway.price_book_v2_checkpoint_receipt','gateway.price_book_v2_reject_mutation','assistant.runtime_cache_settlement_v2_immutable','assistant.assert_capability_witness_binding','assistant.complete_assistant_runtime_model_call_v2']) AND (a.grantee=0 OR pg_catalog.pg_get_userbyid(a.grantee)=left(current_user,length(current_user)-length('owner'))||'runtime')) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) a WHERE n.nspname||'.'||p.proname IN ('gateway.price_book_v2_validate_publication','gateway.price_book_v2_checkpoint_receipt','gateway.price_book_v2_reject_mutation','assistant.runtime_cache_settlement_v2_immutable','assistant.assert_capability_witness_binding') AND a.grantee<>p.proowner)"
}
fn gateway_settlement_function_acl() -> &'static str {
    "SELECT count(*)=2 AND bool_and(a.privilege_type='EXECUTE' AND NOT a.is_grantable AND a.grantor=p.proowner) AND bool_or(pg_catalog.pg_get_userbyid(a.grantee)=current_user) AND bool_or(pg_catalog.pg_get_userbyid(a.grantee)=left(current_user,length(current_user)-length('owner'))||'gateway') FROM pg_catalog.pg_proc p CROSS JOIN LATERAL pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) a WHERE p.oid=pg_catalog.to_regprocedure('assistant.complete_assistant_runtime_model_call_v2(uuid,character varying,bigint,bigint,bigint,character varying,jsonb)')"
}
fn expected_function_security_modes() -> &'static str {
    "SELECT count(*)=6 AND count(*) FILTER (WHERE p.prosecdef)=2 AND bool_and((p.proname='complete_assistant_runtime_model_call_v2' AND p.prosecdef AND p.proconfig=ARRAY['search_path=pg_catalog, assistant, pg_temp']::text[]) OR (p.proname='assert_capability_witness_binding' AND p.prosecdef AND p.proconfig=ARRAY['search_path=pg_catalog, assistant, pg_temp']::text[]) OR (p.proname NOT IN ('complete_assistant_runtime_model_call_v2','assert_capability_witness_binding') AND NOT p.prosecdef AND p.proconfig=ARRAY['search_path=pg_catalog, pg_temp']::text[])) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE (n.nspname='gateway' AND p.proname IN ('price_book_v2_validate_publication','price_book_v2_checkpoint_receipt','price_book_v2_reject_mutation')) OR (n.nspname='assistant' AND p.proname IN ('runtime_cache_settlement_v2_immutable','assert_capability_witness_binding','complete_assistant_runtime_model_call_v2'))"
}
pub(super) fn change() -> manifest::ChangeSpec {
    manifest::ChangeSpec {
        sequence: CANDIDATE_EPOCH,
        name: "platform_upgrade_native_contracts_026".into(),
        file: SQL.into(),
        sha256: SQL_SHA256.into(),
        owner: "owner".into(),
        transaction_mode: manifest::TransactionMode::Transactional,
        rollback_class: manifest::RollbackClass::ForwardFixOnly,
        preconditions: vec![owner_precondition(), format!("SELECT {}", target_objects_absent()), dependencies_present().into()],
        postconditions: vec![
            owner_postcondition(),
            format!("SELECT {}", target_objects_present()),
            expected_relation_acl(),
            expected_function_acl().into(),
            gateway_settlement_function_acl().into(),
            expected_function_security_modes().into(),
            "SELECT pg_catalog.to_regprocedure('assistant.complete_assistant_runtime_model_call_v2(uuid,character varying,bigint,bigint,bigint,character varying,jsonb)') IS NOT NULL AND EXISTS (SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='assistant' AND p.proname='complete_assistant_runtime_model_call_v2' AND p.prosecdef AND p.proconfig=ARRAY['search_path=pg_catalog, assistant, pg_temp']::text[])".into(),
        ],
        timeout_seconds: 300,
        lock_budget_seconds: 5,
        resume_handler: None,
        repair_handler: None,
        notes: "Single transactional 025 to 026 source bundle from four hash-pinned proposals in explicit dependency order. No caller-supplied SQL. Preserve the shared NOLOGIN owner, roles, extensions, and reference rows; measured raw ACL additions must exactly match the fixed table/function/column grant allowlist, including Gateway-only runtime settlement EXECUTE and Knowledge API/Worker column privileges.".into(),
    }
}
pub(super) fn expected_acl_additions() -> BTreeSet<String> {
    let mut expected = BTreeSet::new();
    for (schema, table) in TABLES {
        expected.insert(format!("owner:{schema}.{table}:owner"));
        expected.insert(format!(
            "acl:{schema}.{table}:owner:DELETE,INSERT,REFERENCES,SELECT,TRIGGER,TRUNCATE,UPDATE:grantopt=0"
        ));
    }
    for table in [
        "price_book_v2_terms",
        "price_book_v2_publications",
        "price_book_v2_routes",
        "price_book_v2_activation_audit",
        "price_book_v2_usage_receipts",
        "price_book_v2_accounting_checkpoints",
        "price_book_v2_import_audit",
        "local_node_control_commands",
        "local_node_control_deliveries",
        "local_node_request_keys",
        "local_node_request_nonces",
    ] {
        expected.insert(format!(
            "acl:gateway.{table}:gateway:INSERT,SELECT:grantopt=0"
        ));
    }
    expected.insert(
        "acl:assistant.assistant_runtime_capability_witnesses:gateway:INSERT,SELECT:grantopt=0"
            .into(),
    );
    expected.insert(
        "acl:assistant.assistant_runtime_capability_witnesses:runtime:SELECT:grantopt=0".into(),
    );
    expected.insert(
        "acl:assistant.assistant_runtime_capability_witnesses:runtime:SELECT:grantopt=0".into(),
    );
    expected.insert("acl:knowledge.vector_mutation_jobs:knowledge_api:SELECT:grantopt=0".into());
    expected.insert("acl:knowledge.vector_mutation_jobs:knowledge_worker:SELECT:grantopt=0".into());
    for column in [
        "model_version",
        "book_version",
        "revision",
        "updated_by",
        "updated_at",
    ] {
        expected.insert(format!(
            "column_acl:gateway.price_book_v2_routes.{column}:gateway:UPDATE:grantopt=0"
        ));
    }
    for column in ["observation", "updated_at"] {
        expected.insert(format!(
            "column_acl:gateway.price_book_v2_accounting_checkpoints.{column}:gateway:UPDATE:grantopt=0"
        ));
    }
    for column in ["acknowledged_at", "acknowledged_channel_id"] {
        expected.insert(format!(
            "column_acl:gateway.local_node_control_commands.{column}:gateway:UPDATE:grantopt=0"
        ));
    }
    for column in ["nonce", "issued_at", "expires_at", "consumed_at"] {
        expected.insert(format!(
            "column_acl:gateway.local_node_request_nonces.{column}:gateway:UPDATE:grantopt=0"
        ));
    }
    for column in [
        "job_id",
        "tenant_id",
        "dataset_id",
        "requested_by",
        "expected_generation",
        "payload",
        "expected_revision",
    ] {
        expected.insert(format!(
            "column_acl:knowledge.vector_mutation_jobs.{column}:knowledge_api:INSERT:grantopt=0"
        ));
    }
    for column in ["state", "error_class", "updated_at"] {
        expected.insert(format!(
            "column_acl:knowledge.vector_mutation_jobs.{column}:knowledge_api:UPDATE:grantopt=0"
        ));
    }
    for column in ["state", "attempts", "result", "error_class", "updated_at"] {
        expected.insert(format!(
            "column_acl:knowledge.vector_mutation_jobs.{column}:knowledge_worker:UPDATE:grantopt=0"
        ));
    }

    let functions = [
        ("gateway", "price_book_v2_validate_publication", ""),
        ("gateway", "price_book_v2_checkpoint_receipt", ""),
        ("gateway", "price_book_v2_reject_mutation", ""),
        ("assistant", "runtime_cache_settlement_v2_immutable", ""),
        ("assistant", "assert_capability_witness_binding", ""),
        (
            "assistant",
            "complete_assistant_runtime_model_call_v2",
            "p_call_id uuid, p_tenant_id character varying, p_input_tokens bigint, p_output_tokens bigint, p_cost_microusd bigint, p_provider_request_id character varying, p_receipt jsonb",
        ),
    ];
    for (schema, function, arguments) in functions {
        expected.insert(format!(
            "function_owner:{schema}.{function}({arguments}):owner"
        ));
        expected.insert(format!(
            "function_acl:{schema}.{function}({arguments}):owner:EXECUTE:grantopt=0"
        ));
    }
    expected.insert("function_acl:assistant.complete_assistant_runtime_model_call_v2(p_call_id uuid, p_tenant_id character varying, p_input_tokens bigint, p_output_tokens bigint, p_cost_microusd bigint, p_provider_request_id character varying, p_receipt jsonb):gateway:EXECUTE:grantopt=0".into());
    expected
}

pub(super) fn verify_exact_fingerprints(previous: &Value, current: &Value) -> Result<()> {
    let previous_raw_acl_lines: Vec<String> =
        serde_json::from_value(previous["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("026 predecessor raw ACL lines are malformed"))?;
    let current_raw_acl_lines: Vec<String> =
        serde_json::from_value(current["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("026 current raw ACL lines are malformed"))?;
    let previous_set = previous_raw_acl_lines
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let current_set = current_raw_acl_lines
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut expected = previous_set.clone();
    expected.extend(expected_acl_additions());
    if previous_set.len() != previous_raw_acl_lines.len()
        || current_set.len() != current_raw_acl_lines.len()
        || current_set != expected
        || current["acl_compatibility"]["delta_line"] != ACL_DELTA
        || previous["acl_compatibility"]["delta_line"] != ACL_DELTA
        || current["acl_compatibility"]["revision"] != previous["acl_compatibility"]["revision"]
        || current["acl_compatibility"]["manifest_sha256"]
            != previous["acl_compatibility"]["manifest_sha256"]
        || current["acl_compatibility"]["removed_delta_count"]
            != previous["acl_compatibility"]["removed_delta_count"]
        || current["acl_compatibility"]["base_acl_sha256"]
            == previous["acl_compatibility"]["base_acl_sha256"]
        || current["acl_compatibility"]["effective_acl_sha256"]
            == previous["acl_compatibility"]["effective_acl_sha256"]
        || current["raw_fingerprints"]["acl"] == previous["raw_fingerprints"]["acl"]
        || current["fingerprints"]["acl"] == previous["fingerprints"]["acl"]
    {
        return Err(refused(
            "026 raw ACL delta is not the exact fixed new-object grant set",
        ));
    }
    for key in ["extensions", "reference_data"] {
        if current["raw_fingerprints"][key] != previous["raw_fingerprints"][key]
            || current["fingerprints"][key] != previous["fingerprints"][key]
        {
            return Err(refused("026 changed an unrelated canonical fingerprint"));
        }
    }
    Ok(())
}
