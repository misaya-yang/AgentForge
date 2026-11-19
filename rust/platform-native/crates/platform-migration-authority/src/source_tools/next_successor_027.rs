//! Fixed, unactivated 026 -> 027 source factory for five reviewed proposals.
//! It cannot allocate a release, accept caller SQL, or activate canonical files.
use super::{measure_successors, next_successor as safe, next_successor_026};
use crate::{
    AuthorityError, Result, commands, distribution, manifest, runner::AuthorityPaths,
    sql::reject_embedded_transactions,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

#[path = "next_successor_027/acl.rs"]
mod acl;
#[path = "next_successor_027/activation.rs"]
mod activation;
#[path = "next_successor_027/measurement.rs"]
mod measurement;

pub use activation::{activation_bundle, verify_activated_source};
use measurement::verify_measured_files;
pub use measurement::{MeasureOptions, measure};

pub const BASELINE: &str = "2026_08_post_kb_v1";
pub const PREDECESSOR_EPOCH: i32 = 26;
pub const CANDIDATE_EPOCH: i32 = 27;
pub const APPROVED_026_LOCK_SHA256: &str =
    "412f111e43c8bce04096a84beaff05a8c139762ff335e8138f09696936cc711a";
pub const APPROVED_026_MANIFEST_SHA256: &str =
    "c7224df7a5ba48f8a061c416b04928e84f5758de67b941093f8e036bf8b744b7";

const SQL: &str = "027_native_ai_platform_contracts.sql";
const REFERENCE: &str = "027_verification.json";
const MEASUREMENT: &str = "SUCCESSOR_027_MEASUREMENT.json";
const ACTIVATION: &str = "SUCCESSOR_027_ACTIVATION.json";
const MARKER: &str = "PREVIEW_027_NOT_ACTIVATED.json";
const LOCK: &str = "database/authority/package-authority-lock.json";
const MANIFEST: &str = "database/migrations/2026_08_post_kb_v1/manifest.yml";
const ARCHIVED_LOCK: &str =
    "docs/design/canonical-schema-successor-027-approved26-package-lock.json";
const ARCHIVED_MANIFEST: &str =
    "docs/design/canonical-schema-successor-027-approved26-manifest.yml";
const ACL_DELTA: &str = "schema_acl:public:migrator:USAGE:grantopt=0";
const TERMS_REQUEST_EXPANSION_CHECK: &str = "price_book_v2_terms_component_request_v2_check";
const RECEIPTS_REQUEST_EXPANSION_CHECK: &str = "price_book_v2_usage_receipts_request_v2_check";
const USAGE_RECORDS_METER_CHECK: &str = "usage_records_meter_v1";
const AUTH_EPOCH_EXPANSION_CHECK: &str = "enterprise_users_auth_epoch_v2_check";
const CONTRACT_028_PROPOSAL: &str = "database/proposals/price-book-v2-contract-028.sql";
const CONTRACT_028_SHA256: &str =
    "d6b47a4abd6427f2a788f9447750a0201787dc9e64bcb574dca2deaecf2ca3f4";
const TOOL: &str = "rust/platform-native/crates/platform-migration-authority/src/source_tools/next_successor_027.rs";
const PROVENANCE: &str = "Fixed five-source 026 -> 027 v2 bundle measured as one native successor. This is expansion-only: the two original price-book checks remain installed while separately named expanded checks are added NOT VALID and validated; their exact drops are deferred to the unnumbered 028 contract, and request-metered model entries remain gated until that contract completes. Request metering adds its CHECK NOT VALID then validates it. Enterprise auth_epoch is added with DEFAULT 0, guarded by a validated non-null/nonnegative CHECK before SET NOT NULL. The complete request-metering, knowledge-model, dispatch, enterprise-identity and routing functionality remains present; only Gateway receives the listed minimum access.";
const HEADER: &str = "-- UNACTIVATED, unnumbered fixed source bundle for canonical successor 027 v2.\n-- Expansion-only: retain both original 026 price-book CHECKs; the exact two drops belong to 028.\n-- Generated from five exact proposal inputs in the recorded dependency order.\n-- Two source-level BEGIN/COMMIT wrappers are removed before the single native transaction.\n-- No distribution pin, numbered migration, database action, or activation is included.\n";
const OWNER_GUARD: &str = "\n-- Native-authority owner boundary for every input in this fixed bundle.\nDO $successor_027_owner$\nDECLARE prefix text:=left(current_user,length(current_user)-length('owner'));\nBEGIN\n IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'\n    OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'\n    OR (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)=current_user) IS NOT TRUE\n    OR EXISTS(SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls))\n THEN RAISE EXCEPTION 'canonical unprivileged schema owner required'; END IF;\nEND\n$successor_027_owner$;\n";
const KNOWLEDGE_MODEL_GRANTS: &str = "\n-- The Gateway is the only caller of the typed knowledge-model boundary.\nDO $knowledge_model_grants$\nDECLARE prefix text:=left(current_user,length(current_user)-length('owner')); principal text;\nBEGIN\n IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN RAISE EXCEPTION 'invalid canonical owner role'; END IF;\n principal:=prefix||'gateway';\n EXECUTE format('GRANT SELECT ON gateway.knowledge_model_calls TO %I',principal);\n EXECUTE format('GRANT UPDATE(state,dispatched_at,provider_request_id,error_code,observation) ON gateway.knowledge_model_calls TO %I',principal);\n EXECUTE format('GRANT INSERT ON gateway.knowledge_model_workload_nonces TO %I',principal);\n EXECUTE format('GRANT EXECUTE ON FUNCTION gateway.reserve_knowledge_model_call(text,text,text,text,uuid,jsonb,jsonb,jsonb,jsonb,bigint,bigint,bigint) TO %I',principal);\n EXECUTE format('GRANT EXECUTE ON FUNCTION gateway.settle_knowledge_model_call(uuid,text,text,bigint,bigint,bigint,bigint,jsonb,jsonb) TO %I',principal);\nEND\n$knowledge_model_grants$;\n";
const PROPOSAL_INPUTS: [ProposalInput; 5] = [
    ProposalInput {
        label: "request-meter-v1",
        path: "database/proposals/request-meter-v1.sql",
        sha256: "3a2ab7661a2ce25f00118c894746d8da58d084695eae62316d0f467a29788ae6",
        description: "Extends 026 price-book and usage-receipt checks with request metering and typed N/A token facts.",
        strip_transaction_wrapper: true,
    },
    ProposalInput {
        label: "knowledge-model-plane",
        path: "database/proposals/knowledge-model-plane.sql",
        sha256: "ff5900f75ba8ebb93b20926736651a5c99abd50a57fff7cb0236efcec1cc5ad2",
        description: "Adds Gateway-owned, quota-locked model-call reservations and idempotent settlement on request-meter facts.",
        strip_transaction_wrapper: true,
    },
    ProposalInput {
        label: "dispatch-token-reservations",
        path: "database/proposals/dispatch-token-reservations.sql",
        sha256: "94daf9090175a42edf630af9ac75be4ef81482da7f7ae0ae0830a17f618423f0",
        description: "Adds a minimal Gateway dispatch reservation ledger using the 026 quota accounting checkpoint.",
        strip_transaction_wrapper: false,
    },
    ProposalInput {
        label: "enterprise-identity",
        path: "database/proposals/enterprise-identity.sql",
        sha256: "839102b978eec643828a0e841bb98124dc4838ebd4461f8bca875b7db0721f5f",
        description: "Adds tenant-scoped identity, SCIM, MFA, passkey and session-assurance state on existing Gateway identities.",
        strip_transaction_wrapper: false,
    },
    ProposalInput {
        label: "routing-policy",
        path: "database/proposals/routing-policy.sql",
        sha256: "769b8d971da17d2caa151c4895e4a7110c2887ad6752cabe04ca5cc6be520f6a",
        description: "Adds versioned tenant routing policies and append-only routing decisions without provider or budget authority.",
        strip_transaction_wrapper: false,
    },
];

#[derive(Clone, Copy)]
struct ProposalInput {
    label: &'static str,
    path: &'static str,
    sha256: &'static str,
    description: &'static str,
    strip_transaction_wrapper: bool,
}

const TABLES: [(&str, &str); 17] = [
    ("gateway", "knowledge_model_calls"),
    ("gateway", "knowledge_model_workload_nonces"),
    ("gateway", "dispatch_token_reservations"),
    ("gateway", "enterprise_tenant_policy"),
    ("gateway", "enterprise_identity_links"),
    ("gateway", "enterprise_scim_credentials"),
    ("gateway", "enterprise_scim_users"),
    ("gateway", "enterprise_scim_groups"),
    ("gateway", "enterprise_scim_members"),
    ("gateway", "enterprise_scim_role_map"),
    ("gateway", "enterprise_security_state"),
    ("gateway", "enterprise_passkeys"),
    ("gateway", "enterprise_recovery_codes"),
    ("gateway", "enterprise_session_assurance"),
    ("gateway", "routing_policy_versions"),
    ("gateway", "routing_policy_active"),
    ("gateway", "routing_decisions"),
];

const FUNCTIONS: [(&str, &str, &str); 6] = [
    (
        "gateway",
        "reserve_knowledge_model_call",
        "p_tenant text, p_user text, p_call text, p_hash text, p_lease uuid, p_token jsonb, p_body jsonb, p_snapshot jsonb, p_price jsonb, p_input bigint, p_output bigint, p_cost bigint",
    ),
    (
        "gateway",
        "settle_knowledge_model_call",
        "p_lease uuid, p_tenant text, p_user text, p_input bigint, p_output bigint, p_input_cost bigint, p_output_cost bigint, p_result jsonb, p_record jsonb",
    ),
    ("gateway", "enterprise_user_epoch", ""),
    ("gateway", "enterprise_assignment_epoch", ""),
    ("gateway", "enterprise_role_policy_epoch", ""),
    ("gateway", "enterprise_tenant_mfa_epoch", ""),
];

#[derive(Debug, Clone)]
pub struct Candidate {
    pub plan: Value,
    pub manifest_value: Value,
    pub epoch: manifest::EpochManifest,
    pub baseline: manifest::BaselineManifest,
    pub baseline_sha256: String,
    pub package_lock: Value,
    pub package_lock_sha256: String,
    pub plan_sha256: String,
    pub sql_sha256: String,
    pub predecessor_measurement_sha256: String,
    pub predecessor_activation_sha256: String,
}

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}
fn epoch_relative(name: &str) -> String {
    format!("migrations/{BASELINE}/{name}")
}
fn epoch_file(root: &Path, name: &str) -> PathBuf {
    root.join("database").join(epoch_relative(name))
}
fn source_json(root: &Path, relative: &str) -> Result<Value> {
    serde_json::from_slice(&fs::read(safe::source_file(root, relative)?)?)
        .map_err(|_| refused("027 malformed source JSON"))
}
fn lock_files(lock: &Value) -> Result<&serde_json::Map<String, Value>> {
    if lock["schema"] != "migration-authority/source-distribution/v1"
        || lock["baseline_id"] != BASELINE
    {
        return Err(refused("027 predecessor lock identity is invalid"));
    }
    lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("027 predecessor lock has no inventory"))
}
fn digest(value: &Value) -> Result<String> {
    Ok(safe::sha256_bytes(
        &serde_json::to_vec(value).map_err(|_| refused("027 plan serialization failed"))?,
    ))
}
fn outer_body(bytes: &[u8], input: ProposalInput) -> Result<Vec<u8>> {
    if !input.strip_transaction_wrapper {
        return Ok(bytes.to_vec());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| refused("027 proposal is not UTF-8"))?;
    let mut lines = text.split_inclusive('\n').collect::<Vec<_>>();
    let begin = lines.iter().position(|line| line.trim() == "BEGIN;");
    let commit = lines.iter().rposition(|line| line.trim() == "COMMIT;");
    if begin.is_none()
        || commit.is_none()
        || begin == commit
        || lines.iter().filter(|line| line.trim() == "BEGIN;").count() != 1
        || lines.iter().filter(|line| line.trim() == "COMMIT;").count() != 1
    {
        return Err(refused(
            "027 transaction envelope differs from the fixed proposal",
        ));
    }
    let begin = begin.unwrap();
    let commit = commit.unwrap();
    if begin >= commit
        || lines[commit + 1..]
            .iter()
            .any(|line| !line.trim().is_empty())
    {
        return Err(refused("027 transaction envelope is not outermost"));
    }
    lines.drain(commit..);
    lines.remove(begin);
    Ok(lines.concat().into_bytes())
}
fn replace_fixed_once(source: &str, old: &str, new: &str) -> Result<String> {
    if source.matches(old).count() != 1 {
        return Err(refused("027 v2 fixed source transformation anchor drifted"));
    }
    Ok(source.replacen(old, new, 1))
}
fn replace_statement_once(source: &str, prefix: &str, replacement: &str) -> Result<String> {
    let mut output = String::with_capacity(source.len() + replacement.len());
    let mut found = 0;
    for line in source.split_inclusive('\n') {
        if line.starts_with(prefix) {
            found += 1;
            output.push_str(replacement);
        } else {
            output.push_str(line);
        }
    }
    if found != 1 {
        return Err(refused("027 v2 fixed statement anchor drifted"));
    }
    Ok(output)
}
fn expand_named_check(source: &str, table: &str, old_name: &str, new_name: &str) -> Result<String> {
    let prefix = format!("ALTER TABLE {table} ADD CONSTRAINT {old_name} ");
    let line = source
        .split_inclusive('\n')
        .find(|line| line.starts_with(&prefix))
        .ok_or_else(|| refused("027 v2 expansion CHECK is missing"))?;
    let statement = line.trim_end_matches('\n').trim_end_matches('\r');
    let renamed = statement.replacen(
        &format!("CONSTRAINT {old_name} "),
        &format!("CONSTRAINT {new_name} "),
        1,
    );
    let body = renamed
        .strip_suffix(';')
        .ok_or_else(|| refused("027 v2 expansion CHECK terminator changed"))?;
    let replacement =
        format!("{body} NOT VALID;\nALTER TABLE {table} VALIDATE CONSTRAINT {new_name};\n");
    replace_statement_once(source, &prefix, &replacement)
}
fn transform_request_meter_v2(source: &str) -> Result<String> {
    let mut body = replace_fixed_once(
        source,
        "ALTER TABLE gateway.price_book_v2_terms DROP CONSTRAINT price_book_v2_terms_component_check;\n",
        "-- Retain the 026 component CHECK; contract it only in the reviewed 028 successor.\n",
    )?;
    body = replace_fixed_once(
        &body,
        "ALTER TABLE gateway.price_book_v2_usage_receipts DROP CONSTRAINT price_book_v2_usage_receipts_check;\n",
        "-- Retain the 026 receipt CHECK; contract it only in the reviewed 028 successor.\n",
    )?;
    body = expand_named_check(
        &body,
        "gateway.price_book_v2_terms",
        "price_book_v2_terms_component_check",
        TERMS_REQUEST_EXPANSION_CHECK,
    )?;
    body = expand_named_check(
        &body,
        "gateway.price_book_v2_usage_receipts",
        "price_book_v2_usage_receipts_check",
        RECEIPTS_REQUEST_EXPANSION_CHECK,
    )?;
    body = replace_fixed_once(
        &body,
        "\n) IS TRUE);\n",
        "\n) IS TRUE) NOT VALID;\nALTER TABLE gateway.usage_records VALIDATE CONSTRAINT usage_records_meter_v1;\n",
    )?;
    Ok(body)
}
fn transform_enterprise_identity_v2(source: &str) -> Result<String> {
    replace_fixed_once(
        source,
        "ALTER TABLE gateway.users ADD COLUMN auth_epoch bigint NOT NULL DEFAULT 0 CHECK(auth_epoch>=0);\n",
        "-- Add the constant default first; validate the explicit proof before setting NOT NULL.\nALTER TABLE gateway.users ADD COLUMN auth_epoch bigint DEFAULT 0;\nALTER TABLE gateway.users ADD CONSTRAINT enterprise_users_auth_epoch_v2_check CHECK(auth_epoch IS NOT NULL AND auth_epoch>=0) NOT VALID;\nALTER TABLE gateway.users VALIDATE CONSTRAINT enterprise_users_auth_epoch_v2_check;\nALTER TABLE gateway.users ALTER COLUMN auth_epoch SET NOT NULL;\n",
    )
}
fn transform_input_v2(input: ProposalInput, source: &[u8]) -> Result<Vec<u8>> {
    let body = outer_body(source, input)?;
    let text = std::str::from_utf8(&body).map_err(|_| refused("027 proposal is not UTF-8"))?;
    let transformed = match input.label {
        "request-meter-v1" => transform_request_meter_v2(text)?,
        "enterprise-identity" => transform_enterprise_identity_v2(text)?,
        _ => text.to_owned(),
    };
    Ok(transformed.into_bytes())
}
fn expected_sql(root: &Path) -> Result<Vec<u8>> {
    let mut expected = HEADER.as_bytes().to_vec();
    expected.extend_from_slice(OWNER_GUARD.as_bytes());
    for input in PROPOSAL_INPUTS {
        let source = fs::read(safe::source_file(root, input.path)?)?;
        if safe::sha256_bytes(&source) != input.sha256 {
            return Err(refused(
                "027 fixed proposal input differs from its approved SHA-256",
            ));
        }
        let body = transform_input_v2(input, &source)?;
        expected.extend_from_slice(
            format!(
                "\n-- BEGIN FIXED SOURCE {}: {}\n-- {}\n-- Frozen source SHA-256: {}\n",
                input.label, input.path, input.description, input.sha256
            )
            .as_bytes(),
        );
        expected.extend_from_slice(&body);
        if !body.ends_with(b"\n") {
            expected.push(b'\n');
        }
        expected.extend_from_slice(format!("-- END FIXED SOURCE {}\n", input.label).as_bytes());
    }
    expected.extend_from_slice(KNOWLEDGE_MODEL_GRANTS.as_bytes());
    Ok(expected)
}

// Filled from the exact deterministic expansion-only v2 bytes generated by expected_sql.
pub const SQL_SHA256: &str = "e22a46ef53f48f7839d6b06f121f8c57e3fa18a8065ffcc4e6b2af8760c9ed7f";
fn require_sql(root: &Path) -> Result<Vec<u8>> {
    let bytes = expected_sql(root)?;
    if safe::sha256_bytes(&bytes) != SQL_SHA256 {
        return Err(refused(
            "027 fixed five-source aggregate differs from its approved digest",
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| refused("027 SQL is not UTF-8"))?;
    reject_embedded_transactions(text, "platform_upgrade_native_contracts_027")?;
    Ok(bytes)
}

fn owner_precondition() -> String {
    "SELECT current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)<>current_user) AND (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge'])) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls)) AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'gateway') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'runtime') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_api') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_worker')".into()
}
fn dependencies_present() -> &'static str {
    "SELECT pg_catalog.to_regclass('gateway.price_book_v2_terms') IS NOT NULL AND pg_catalog.to_regclass('gateway.price_book_v2_usage_receipts') IS NOT NULL AND pg_catalog.to_regclass('gateway.usage_records') IS NOT NULL AND pg_catalog.to_regclass('gateway.users') IS NOT NULL AND pg_catalog.to_regclass('gateway.tenants') IS NOT NULL AND pg_catalog.to_regclass('gateway.user_quotas') IS NOT NULL AND pg_catalog.to_regclass('gateway.rbac_roles') IS NOT NULL AND pg_catalog.to_regclass('gateway.user_roles') IS NOT NULL AND pg_catalog.to_regclass('gateway.user_permissions') IS NOT NULL AND pg_catalog.to_regclass('gateway.role_permissions') IS NOT NULL AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_terms'::pg_catalog.regclass AND conname='price_book_v2_terms_component_check' AND convalidated AND position('cached_input' in pg_catalog.pg_get_constraintdef(oid))>0 AND position('request' in pg_catalog.pg_get_constraintdef(oid))=0) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_check' AND convalidated AND position('per_1k_tokens' in pg_catalog.pg_get_constraintdef(oid))>0 AND position('per_request' in pg_catalog.pg_get_constraintdef(oid))=0) AND (SELECT count(*)=2 FROM pg_catalog.pg_attribute WHERE attrelid='gateway.usage_records'::pg_catalog.regclass AND attname IN ('input_tokens','output_tokens') AND attnotnull)"
}
fn target_objects_absent() -> String {
    let tables = TABLES
        .iter()
        .map(|(schema, table)| format!("pg_catalog.to_regclass('{schema}.{table}') IS NULL"));
    let functions = FUNCTIONS.iter().map(|(schema, name, _)| {
        format!("NOT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='{schema}' AND p.proname='{name}')")
    });
    let absent_columns = [
        "gateway.users.auth_epoch",
        "gateway.usage_records.meter_kind",
        "gateway.usage_records.measured_units",
        "gateway.usage_records.request_cost_nanos",
        "gateway.usage_records.token_count_state",
    ]
    .iter()
    .map(|column| {
        let (relation, name) = column.rsplit_once('.').unwrap();
        format!("NOT EXISTS(SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid='{relation}'::pg_catalog.regclass AND attname='{name}' AND NOT attisdropped)")
    });
    let absent_constraints = [
        ("gateway.usage_records", USAGE_RECORDS_METER_CHECK),
        ("gateway.price_book_v2_terms", TERMS_REQUEST_EXPANSION_CHECK),
        (
            "gateway.price_book_v2_usage_receipts",
            RECEIPTS_REQUEST_EXPANSION_CHECK,
        ),
        ("gateway.users", AUTH_EPOCH_EXPANSION_CHECK),
    ]
    .iter()
    .map(|(table, name)| {
        format!(
            "NOT EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='{table}'::pg_catalog.regclass AND conname='{name}')"
        )
    });
    tables
        .chain(functions)
        .chain(absent_columns)
        .chain(absent_constraints)
        .collect::<Vec<_>>()
        .join(" AND ")
}
fn owner_postcondition() -> String {
    let relations = TABLES
        .iter()
        .map(|(schema, table)| format!("('{schema}','{table}')"))
        .collect::<Vec<_>>()
        .join(",");
    let function_names = FUNCTIONS
        .iter()
        .map(|(schema, name, _)| format!("'{schema}.{name}'"))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "SELECT current_user ~ '^[a-z][a-z0-9_]*_owner$' AND NOT EXISTS(SELECT 1 FROM (VALUES {relations}) expected(schema_name,relation_name) JOIN pg_catalog.pg_namespace n ON n.nspname=expected.schema_name JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname=expected.relation_name WHERE pg_catalog.pg_get_userbyid(c.relowner)<>current_user) AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname||'.'||p.proname=ANY(ARRAY[{function_names}]) AND pg_catalog.pg_get_userbyid(p.proowner)<>current_user)"
    )
}
fn objects_present() -> String {
    let tables = TABLES
        .iter()
        .map(|(schema, table)| format!("pg_catalog.to_regclass('{schema}.{table}') IS NOT NULL"));
    let functions = FUNCTIONS.iter().map(|(schema, name, _)| {
        format!("EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='{schema}' AND p.proname='{name}')")
    });
    tables.chain(functions).collect::<Vec<_>>().join(" AND ")
}
fn state_postcondition() -> &'static str {
    "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_attrdef d ON d.adrelid=a.attrelid AND d.adnum=a.attnum WHERE a.attrelid='gateway.users'::pg_catalog.regclass AND a.attname='auth_epoch' AND a.atttypid='bigint'::pg_catalog.regtype AND a.attnotnull AND a.attgenerated='' AND pg_catalog.pg_get_expr(d.adbin,d.adrelid)='0') AND (SELECT count(*)=4 FROM pg_catalog.pg_attribute WHERE attrelid='gateway.usage_records'::pg_catalog.regclass AND attname=ANY(ARRAY['meter_kind','measured_units','request_cost_nanos','token_count_state']) AND NOT attisdropped AND attgenerated='s') AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.usage_records'::pg_catalog.regclass AND conname='usage_records_meter_v1' AND convalidated) AND (SELECT count(*)=6 FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='gateway' AND t.tgname IN ('enterprise_user_epoch','enterprise_role_assignment_epoch','enterprise_direct_permission_epoch','enterprise_role_permission_epoch','enterprise_role_definition_epoch','enterprise_tenant_mfa_epoch') AND NOT t.tgisinternal)"
}
fn expansion_constraints_postcondition() -> &'static str {
    "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_terms'::pg_catalog.regclass AND conname='price_book_v2_terms_component_check' AND convalidated AND position('request' in pg_catalog.pg_get_constraintdef(oid))=0) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_check' AND convalidated AND position('per_request' in pg_catalog.pg_get_constraintdef(oid))=0) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_terms'::pg_catalog.regclass AND conname='price_book_v2_terms_component_request_v2_check' AND convalidated AND position('request' in pg_catalog.pg_get_constraintdef(oid))>0) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_request_v2_check' AND convalidated AND position('per_request' in pg_catalog.pg_get_constraintdef(oid))>0) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.users'::pg_catalog.regclass AND conname='enterprise_users_auth_epoch_v2_check' AND convalidated AND position('IS NOT NULL' in upper(pg_catalog.pg_get_constraintdef(oid)))>0 AND position('>= 0' in pg_catalog.pg_get_constraintdef(oid))>0)"
}
fn change() -> manifest::ChangeSpec {
    let owner = owner_precondition();
    manifest::ChangeSpec {
        sequence: CANDIDATE_EPOCH,
        name: "platform_upgrade_native_contracts_027".into(),
        file: SQL.into(),
        sha256: SQL_SHA256.into(),
        owner: "owner".into(),
        transaction_mode: manifest::TransactionMode::Transactional,
        rollback_class: manifest::RollbackClass::ForwardFixOnly,
        preconditions: vec![owner, dependencies_present().into(), format!("SELECT {}", target_objects_absent())],
        postconditions: vec![
            owner_postcondition(),
            format!("SELECT {}", objects_present()),
            state_postcondition().into(),
            expansion_constraints_postcondition().into(),
            "SELECT bool_and(p.prosecdef AND p.proconfig=ARRAY['search_path=pg_catalog, gateway']::text[]) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='gateway' AND p.proname IN ('reserve_knowledge_model_call','settle_knowledge_model_call')".into(),
            "SELECT count(*)=4 AND bool_and(NOT p.prosecdef AND p.proconfig=ARRAY['search_path=pg_catalog, gateway, pg_temp']::text[]) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='gateway' AND p.proname IN ('enterprise_user_epoch','enterprise_assignment_epoch','enterprise_role_policy_epoch','enterprise_tenant_mfa_epoch')".into(),
        ],
        timeout_seconds: 600,
        lock_budget_seconds: 5,
        resume_handler: None,
        repair_handler: None,
        notes: "Single transactional 026 -> 027 v2 expansion-only extension from the original five hash-pinned unnumbered proposals. Both original price-book checks remain beside independently named NOT VALID then validated expanded checks; their exact drops are deferred to the unnumbered 028 contract and request-metered model enablement remains gated until then. Usage-meter and auth_epoch checks use NOT VALID then VALIDATE, with auth_epoch SET NOT NULL only after validation. Request metering, knowledge-model, dispatch, enterprise-identity and routing functionality remain complete. Exact measured ACL additions grant only Gateway the fixed table, column, and typed-function privileges. No owner, role, schema, extension, or reference-data changes are permitted.".into(),
    }
}

fn require_predecessor(epoch: &manifest::EpochManifest) -> Result<()> {
    if epoch.baseline_id != BASELINE
        || epoch.epoch != PREDECESSOR_EPOCH
        || epoch.changes.len() != PREDECESSOR_EPOCH as usize
        || epoch
            .changes
            .iter()
            .enumerate()
            .any(|(index, change)| change.sequence != index as i32 + 1)
    {
        return Err(refused(
            "027 requires the exact complete activated epoch-26 predecessor",
        ));
    }
    Ok(())
}

/// Accepts only the fully verified 026 distribution and the five pinned inputs.
pub fn validate_source(root: &Path) -> Result<Candidate> {
    let root = safe::canonical_directory(root, "027 source must be a nonsymlink directory")?;
    let sql = require_sql(&root)?;
    next_successor_026::verify_activated_source(&root)?;
    distribution::verify(&root.join("database"), BASELINE)?;
    let mut epoch = manifest::load_epoch_manifest(&root.join(MANIFEST))?;
    require_predecessor(&epoch)?;
    let (baseline, baseline_sha256) = commands::load_baseline_with_mode(
        &AuthorityPaths {
            database_dir: root.join("database"),
        },
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    let package_lock = source_json(&root, LOCK)?;
    let package_lock_sha256 = manifest::file_sha256(&safe::source_file(&root, LOCK)?)?;
    let predecessor_manifest_sha256 = manifest::file_sha256(&safe::source_file(&root, MANIFEST)?)?;
    if package_lock_sha256 != APPROVED_026_LOCK_SHA256
        || predecessor_manifest_sha256 != APPROVED_026_MANIFEST_SHA256
    {
        return Err(refused(
            "027 predecessor differs from the exact activated026 pins",
        ));
    }
    let predecessor_measurement_sha256 = manifest::file_sha256(&safe::source_file(
        &root,
        &format!(
            "database/{}",
            epoch_relative("SUCCESSOR_026_MEASUREMENT.json")
        ),
    )?)?;
    let predecessor_activation_sha256 = manifest::file_sha256(&safe::source_file(
        &root,
        &format!(
            "database/{}",
            epoch_relative("SUCCESSOR_026_ACTIVATION.json")
        ),
    )?)?;
    for name in [SQL, REFERENCE, MEASUREMENT, ACTIVATION] {
        match fs::symlink_metadata(epoch_file(&root, name)) {
            Ok(_) => {
                return Err(refused(
                    "027 candidate artifacts already exist in the source",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    let mut manifest_value = source_json(&root, MANIFEST)?;
    let spec = change();
    manifest_value["epoch"] = json!(CANDIDATE_EPOCH);
    manifest_value["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("027 predecessor manifest has no change list"))?
        .push(serde_json::to_value(&spec).map_err(|_| refused("027 change serialization failed"))?);
    let input_values = PROPOSAL_INPUTS
        .iter()
        .map(|input| {
            json!({
                "label": input.label,
                "path": input.path,
                "sha256": input.sha256,
                "description": input.description,
                "transaction_wrapper_removed": input.strip_transaction_wrapper
            })
        })
        .collect::<Vec<_>>();
    let plan = json!({
        "schema_version": "ai-platform/canonical-fixed-successor-policy/v1",
        "baseline_id": BASELINE,
        "predecessor_epoch": PREDECESSOR_EPOCH,
        "candidate_epoch": CANDIDATE_EPOCH,
        "predecessor_manifest_sha256": predecessor_manifest_sha256,
        "approved_predecessor_package_lock_sha256": package_lock_sha256,
        "baseline_manifest_sha256": baseline_sha256,
        "predecessor_measurement_sha256": predecessor_measurement_sha256,
        "predecessor_activation_sha256": predecessor_activation_sha256,
        "candidate_sql_sha256": SQL_SHA256,
        "deferred_028_contract_sha256": CONTRACT_028_SHA256,
        "dependency_order": [
            "request-meter-v1",
            "knowledge-model-plane",
            "dispatch-token-reservations",
            "enterprise-identity",
            "routing-policy"
        ],
        "dependency_edges": [
            ["activated-026", "request-meter-v1"],
            ["request-meter-v1", "knowledge-model-plane"],
            ["activated-026 quota checkpoint", "dispatch-token-reservations"],
            ["existing Gateway identities and tenants", "enterprise-identity"],
            ["existing Gateway tenants", "routing-policy"]
        ],
        "migration_policy": {
            "phase": "027_expansion_only_v2",
            "retained_price_book_checks": [
                "gateway.price_book_v2_terms.price_book_v2_terms_component_check",
                "gateway.price_book_v2_usage_receipts.price_book_v2_usage_receipts_check"
            ],
            "validated_expansion_checks": [
                format!("gateway.price_book_v2_terms.{TERMS_REQUEST_EXPANSION_CHECK}"),
                format!("gateway.price_book_v2_usage_receipts.{RECEIPTS_REQUEST_EXPANSION_CHECK}"),
                format!("gateway.usage_records.{USAGE_RECORDS_METER_CHECK}"),
                format!("gateway.users.{AUTH_EPOCH_EXPANSION_CHECK}")
            ],
            "deferred_028_contract": {
                "proposal": CONTRACT_028_PROPOSAL,
                "proposal_sha256": CONTRACT_028_SHA256,
                "operations": [
                    "DROP gateway.price_book_v2_terms.price_book_v2_terms_component_check",
                    "DROP gateway.price_book_v2_usage_receipts.price_book_v2_usage_receipts_check"
                ],
                "precondition": "both original checks remain present and both expanded price-book checks are validated",
                "request_metered_model_enablement": "remain gated until the 028 contract completes"
            }
        },
        "proposal_inputs": input_values,
        "existing_table_deltas": {
            "gateway.price_book_v2_terms": "retain the 026 component check and add a separately named request expansion CHECK as NOT VALID then VALIDATE",
            "gateway.price_book_v2_usage_receipts": "retain the 026 receipt check and add a separately named per_request expansion CHECK as NOT VALID then VALIDATE",
            "gateway.usage_records": "make input/output token facts nullable; add four stored generated request-meter columns and add usage_records_meter_v1 NOT VALID then VALIDATE",
            "gateway.users": "add auth_epoch bigint DEFAULT 0; add and validate a non-null/nonnegative CHECK before SET NOT NULL",
            "new_tables": TABLES.iter().map(|(schema, table)| format!("{schema}.{table}")).collect::<Vec<_>>(),
            "new_functions": FUNCTIONS.iter().map(|(schema, name, _)| format!("{schema}.{name}")).collect::<Vec<_>>()
        },
        "gateway_privileges": {
            "knowledge_model_calls": ["SELECT", "UPDATE(state,dispatched_at,provider_request_id,error_code,observation)"],
            "knowledge_model_workload_nonces": ["INSERT"],
            "knowledge_model_functions": ["EXECUTE reserve_knowledge_model_call", "EXECUTE settle_knowledge_model_call"],
            "dispatch_token_reservations": ["SELECT", "INSERT", "UPDATE(state,candidate_receipt_id,settlement_candidate,settlement_observation,updated_at)"],
            "enterprise_identity": [
                "enterprise_identity_links INSERT,SELECT",
                "enterprise_scim_credentials INSERT,SELECT,UPDATE",
                "enterprise_scim_users INSERT,SELECT,UPDATE",
                "enterprise_scim_groups DELETE,INSERT,SELECT,UPDATE",
                "enterprise_scim_members DELETE,INSERT,SELECT",
                "enterprise_scim_role_map SELECT",
                "enterprise_security_state INSERT,SELECT,UPDATE",
                "enterprise_passkeys DELETE,INSERT,SELECT,UPDATE",
                "enterprise_recovery_codes DELETE,INSERT,SELECT,UPDATE",
                "enterprise_session_assurance INSERT,SELECT",
                "enterprise_tenant_policy SELECT"
            ],
            "routing_policy": "versions/decisions SELECT+INSERT; active policy SELECT+INSERT+UPDATE",
            "runtime_worker_knowledge": []
        },
        "successor": {"change": spec},
        "database_actions_authorized": false,
        "fingerprint_status": "not_measured"
    });
    let plan_sha256 = digest(&plan)?;
    epoch.epoch = CANDIDATE_EPOCH;
    epoch.changes.push(change());
    Ok(Candidate {
        plan,
        manifest_value,
        epoch,
        baseline,
        baseline_sha256,
        package_lock,
        package_lock_sha256,
        plan_sha256,
        sql_sha256: safe::sha256_bytes(&sql),
        predecessor_measurement_sha256,
        predecessor_activation_sha256,
    })
}

fn external_destination(source: &Path, preview: Option<&Path>, output: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(output)?;
    match fs::symlink_metadata(&absolute) {
        Ok(_) => return Err(refused("027 output must be new")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("027 output parent is missing"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("027 output name is missing"))?,
    );
    if absolute != resolved
        || absolute.starts_with(source)
        || source.starts_with(&absolute)
        || preview.is_some_and(|path| absolute.starts_with(path) || path.starts_with(&absolute))
    {
        return Err(refused("027 output must be external and disjoint"));
    }
    Ok(absolute)
}
fn common_receipt(
    candidate: &Candidate,
    manifest_sha: &str,
    status: &str,
    database: Option<&str>,
    reference: Option<&str>,
    measurement: Option<&str>,
) -> Value {
    json!({
        "schema": "ai-platform/canonical-fixed-successor-receipt/v1",
        "status": status,
        "baseline_id": BASELINE,
        "predecessor_epoch": PREDECESSOR_EPOCH,
        "candidate_epoch": CANDIDATE_EPOCH,
        "database_actions": status == "measured_external_preview",
        "source_activation": false,
        "source_plan_sha256": candidate.plan_sha256,
        "approved_predecessor_package_lock_sha256": candidate.package_lock_sha256,
        "predecessor_manifest_sha256": candidate.plan["predecessor_manifest_sha256"],
        "candidate_manifest_sha256": manifest_sha,
        "baseline_manifest_sha256": candidate.baseline_sha256,
        "candidate_sql_sha256": SQL_SHA256,
        "predecessor_measurement_sha256": candidate.predecessor_measurement_sha256,
        "predecessor_activation_sha256": candidate.predecessor_activation_sha256,
        "expected_database": database,
        "verification_ref_sha256": reference,
        "measurement_receipt_sha256": measurement,
        "post_migration_noop": true
    })
}
fn inventory(candidate: &Candidate, measured: bool, bundled: bool) -> Result<BTreeSet<String>> {
    let mut files = lock_files(&candidate.package_lock)?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    files.insert(epoch_relative(SQL));
    if measured || bundled {
        files.insert(epoch_relative(REFERENCE));
        files.insert(epoch_relative(MEASUREMENT));
    }
    if bundled {
        files.insert(epoch_relative(ACTIVATION));
    }
    Ok(files)
}
fn verify_inputs(root: &Path, candidate: &Candidate, measured: bool, bundled: bool) -> Result<()> {
    if safe::collect_distribution_inputs(&root.join("database"))?
        != inventory(candidate, measured, bundled)?
    {
        return Err(refused(
            "027 preview has missing or unexpected database inputs",
        ));
    }
    for (relative, hash) in lock_files(&candidate.package_lock)? {
        if relative == &epoch_relative("manifest.yml") {
            continue;
        }
        let path = safe::source_file(root, &format!("database/{relative}"))?;
        if manifest::file_sha256(&path)? != hash.as_str().unwrap_or_default() {
            return Err(refused("027 changed an activated026 source input"));
        }
    }
    if source_json(root, MANIFEST)? != candidate.manifest_value
        || manifest::load_epoch_manifest(&root.join(MANIFEST))?.epoch != CANDIDATE_EPOCH
        || manifest::file_sha256(&safe::source_file(
            root,
            &format!("database/{}", epoch_relative(SQL)),
        )?)? != SQL_SHA256
    {
        return Err(refused(
            "027 preview is not the exact fixed source extension",
        ));
    }
    Ok(())
}
fn verify_preview_root(preview: &Path) -> Result<()> {
    let mut names = BTreeSet::new();
    for entry in fs::read_dir(preview)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| refused("027 preview name is invalid"))?;
        if kind.is_symlink()
            || !matches!(name.as_str(), "database" | MARKER)
            || (name == "database") != kind.is_dir()
            || (name == MARKER && !kind.is_file())
        {
            return Err(refused(
                "027 preview contains an unexpected or unsafe entry",
            ));
        }
        names.insert(name);
    }
    if names != BTreeSet::from(["database".into(), MARKER.into()]) {
        return Err(refused("027 preview inventory is incomplete"));
    }
    Ok(())
}
pub fn materialize(source: &Path, output: &Path) -> Result<Value> {
    let source = safe::canonical_directory(source, "027 source must be a nonsymlink directory")?;
    let candidate = validate_source(&source)?;
    let output = external_destination(&source, None, output)?;
    fs::create_dir(&output)?;
    safe::copy_tree(&source.join("database"), &output.join("database"))?;
    safe::write_new_bytes(&epoch_file(&output, SQL), &require_sql(&source)?)?;
    fs::write(
        output.join(MANIFEST),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&candidate.manifest_value)
                .map_err(|_| refused("027 manifest serialization failed"))?
        ),
    )?;
    let marker = common_receipt(
        &candidate,
        &manifest::file_sha256(&output.join(MANIFEST))?,
        "inactive_external_preview_only",
        None,
        None,
        None,
    );
    safe::write_new_json(&output.join(MARKER), &marker)?;
    verify_preview(&source, &output, false)?;
    Ok(marker)
}
pub fn verify_preview(source: &Path, preview: &Path, measured: bool) -> Result<Candidate> {
    let source = safe::canonical_directory(source, "027 source must be a nonsymlink directory")?;
    let preview = safe::canonical_directory(preview, "027 preview must be a nonsymlink directory")?;
    if preview.starts_with(&source) || source.starts_with(&preview) {
        return Err(refused("027 preview must be external to its source"));
    }
    verify_preview_root(&preview)?;
    let candidate = validate_source(&source)?;
    verify_inputs(&preview, &candidate, measured, false)?;
    if manifest::file_sha256(&safe::source_file(&preview, LOCK)?)? != candidate.package_lock_sha256
    {
        return Err(refused("027 preview modified the predecessor package lock"));
    }
    let manifest_sha = manifest::file_sha256(&preview.join(MANIFEST))?;
    if source_json(&preview, MARKER)?
        != common_receipt(
            &candidate,
            &manifest_sha,
            "inactive_external_preview_only",
            None,
            None,
            None,
        )
    {
        return Err(refused(
            "027 preview marker is not bound to the exact source",
        ));
    }
    if measured {
        verify_measured_files(&source, &preview, &candidate)?;
    }
    Ok(candidate)
}

#[cfg(test)]
#[path = "next_successor_027_tests.rs"]
mod tests;
