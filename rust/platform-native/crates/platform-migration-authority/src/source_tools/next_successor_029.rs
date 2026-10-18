//! Fixed 028 -> 029 source factory for Gateway worker leases and gated provider Batch.
//! All predecessor/proposal inputs are hash-pinned; only external previews can be generated.
use super::{measure_successors, next_successor as safe, next_successor_028};
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

#[path = "next_successor_029/acl.rs"]
mod acl;
#[path = "next_successor_029/activation.rs"]
mod activation;
#[path = "next_successor_029/measurement.rs"]
mod measurement;

pub use activation::{activation_bundle, verify_activated_source};
use measurement::verify_measured_files;
pub use measurement::{MeasureOptions, measure};

pub const BASELINE: &str = "2026_08_post_kb_v1";
pub const PREDECESSOR_EPOCH: i32 = 28;
pub const CANDIDATE_EPOCH: i32 = 29;

// Filled only from the real, measured and activated 028 package. None is an
// intentional fail-closed state, never a synthetic hash or inferred identity.
pub const APPROVED_028_LOCK_SHA256: Option<&str> =
    Some("1d45dc6faae144ab72cbb64eaa4b4eb849fc4c649acbdd74ad9e2921aed38f01");
pub const APPROVED_028_MANIFEST_SHA256: Option<&str> =
    Some("988f445e7faf70e604938d98eeaf798c1ac99a242c0c22ded6c478108e81fab9");
pub const APPROVED_028_REFERENCE_SHA256: Option<&str> =
    Some("a0ae5dee780ca0fc429b8523502bd8c1f337f5e5d3c08d65ad33c54070c8372a");
pub const APPROVED_028_MEASUREMENT_SHA256: Option<&str> =
    Some("c2c8e2b2552480a0c8fae3a8558f8133a127bde3f52e73f2daa5718039745851");
pub const APPROVED_028_ACTIVATION_SHA256: Option<&str> =
    Some("2d045bb350077ed94e0027527ca5af3e7f4dddaa0f0b6a68f83eeb8f30a5ee1b");
pub const APPROVED_028_SOURCE_PLAN_SHA256: Option<&str> =
    Some("404e6f581fa90974d85d8872dcb9dc13187a69e8a1bd2d2e70a7a422364fc55c");

const SQL: &str = "029_gateway_workers_and_batch.sql";
const PROPOSAL: &str = "docs/design/proposals/gateway-workers-and-batch-029.sql";
const REFERENCE: &str = "029_verification.json";
const MEASUREMENT: &str = "SUCCESSOR_029_MEASUREMENT.json";
const ACTIVATION: &str = "SUCCESSOR_029_ACTIVATION.json";
const MARKER: &str = "PREVIEW_029_NOT_ACTIVATED.json";
const LOCK: &str = "database/authority/package-authority-lock.json";
const MANIFEST: &str = "database/migrations/2026_08_post_kb_v1/manifest.yml";
const ARCHIVED_LOCK: &str =
    "docs/design/canonical-schema-successor-029-approved28-package-lock.json";
const ARCHIVED_MANIFEST: &str =
    "docs/design/canonical-schema-successor-029-approved28-manifest.yml";
const TOOL: &str = "rust/platform-native/crates/platform-migration-authority/src/source_tools/next_successor_029.rs";
const SQL_SHA256: &str = "8652f2b46e760a6c37947f5e56540219037a0b90eb456a73d438fc7f9ce96745";
const ACL_DELTA: &str = "schema_acl:public:migrator:USAGE:grantopt=0";
const PROVENANCE: &str = "Fixed two-source 028 -> 029 bundle: Gateway singleton worker leases and gated provider Batch reconciliation. The only source transformation replaces the worker proposal's final hardcoded Gateway EXECUTE grant with an exact owner-derived quoted principal. Batch marker remains empty. All predecessor objects, extensions and reference data remain; measured ACL changes must equal only the listed owner, Gateway table/column and typed function additions.";
const HEADER: &str = "-- UNACTIVATED fixed 028 -> 029 source bundle.\n-- Exact worker lease and provider Batch proposal inputs, in dependency order.\n-- Only the terminal worker EXECUTE grant is deterministically prefix-rendered.\n-- Batch contract marker remains empty pending actual contract acceptance.\n-- Native authority owns the single transaction; no activation is included.\n";
const OWNER_GUARD: &str = r#"
DO $successor_029_owner$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner'));
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$'
    OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
    OR (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)=current_user) IS NOT TRUE
    OR EXISTS(SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls))
 THEN RAISE EXCEPTION 'canonical unprivileged schema owner required'; END IF;
END
$successor_029_owner$;
"#;
const WORKER_GRANT_ORIGINAL: &str = r#"GRANT EXECUTE ON FUNCTION gateway.acquire_worker_lease(text,uuid,bigint),
    gateway.renew_worker_lease(text,uuid,bigint,bigint),
    gateway.assert_worker_lease(text,uuid,bigint),
    gateway.release_worker_lease(text,uuid,bigint) TO ai_gateway_gateway;
"#;
const WORKER_GRANT_DERIVED: &str = r#"DO $successor_029_worker_grants$
DECLARE prefix text:=left(current_user,length(current_user)-length('owner')); principal text;
BEGIN
 IF current_user !~ '^[a-z][a-z0-9_]{0,20}_owner$' OR prefix !~ '^[a-z][a-z0-9_]{0,20}_$'
 THEN RAISE EXCEPTION 'invalid canonical owner role'; END IF;
 principal:=prefix||'gateway';
 EXECUTE format('GRANT EXECUTE ON FUNCTION gateway.acquire_worker_lease(text,uuid,bigint), gateway.renew_worker_lease(text,uuid,bigint,bigint), gateway.assert_worker_lease(text,uuid,bigint), gateway.release_worker_lease(text,uuid,bigint) TO %I',principal);
END
$successor_029_worker_grants$;
"#;
#[derive(Clone, Copy)]
struct ProposalInput {
    label: &'static str,
    path: &'static str,
    sha256: &'static str,
}
const PROPOSAL_INPUTS: [ProposalInput; 2] = [
    ProposalInput {
        label: "gateway-singleton-worker-leases",
        path: "docs/design/proposals/gateway-singleton-worker-leases.sql",
        sha256: "1f45d0020ecf7ef80aab5c4967972fe2955f4021016dcadf3d3a124eeef9dbf7",
    },
    ProposalInput {
        label: "generic-task-batch-reconciliation",
        path: "docs/design/proposals/generic-task-batch-reconciliation.sql",
        sha256: "e34b2704f094d2a5bdf15c4646b87e8eada38085737e9fb7a5dd22c8b834ba4e",
    },
];
const TABLES: [(&str, &str); 6] = [
    ("gateway", "singleton_worker_leases"),
    ("gateway", "task_batch_inputs"),
    ("gateway", "task_provider_batches"),
    ("gateway", "task_reconciliation_audit"),
    ("gateway", "task_batch_contract"),
    ("gateway", "task_batch_quota_holds"),
];
const FUNCTIONS: [(&str, &str, &str); 6] = [
    (
        "gateway",
        "acquire_worker_lease",
        "p_job text, p_owner uuid, p_ttl_ms bigint",
    ),
    (
        "gateway",
        "renew_worker_lease",
        "p_job text, p_owner uuid, p_fence bigint, p_ttl_ms bigint",
    ),
    (
        "gateway",
        "assert_worker_lease",
        "p_job text, p_owner uuid, p_fence bigint",
    ),
    (
        "gateway",
        "release_worker_lease",
        "p_job text, p_owner uuid, p_fence bigint",
    ),
    ("gateway", "task_batch_binding", ""),
    ("gateway", "task_batch_hold_binding", ""),
];

const RETAINED_TABLES: [&str; 17] = [
    "gateway.knowledge_model_calls",
    "gateway.knowledge_model_workload_nonces",
    "gateway.dispatch_token_reservations",
    "gateway.enterprise_tenant_policy",
    "gateway.enterprise_identity_links",
    "gateway.enterprise_scim_credentials",
    "gateway.enterprise_scim_users",
    "gateway.enterprise_scim_groups",
    "gateway.enterprise_scim_members",
    "gateway.enterprise_scim_role_map",
    "gateway.enterprise_security_state",
    "gateway.enterprise_passkeys",
    "gateway.enterprise_recovery_codes",
    "gateway.enterprise_session_assurance",
    "gateway.routing_policy_versions",
    "gateway.routing_policy_active",
    "gateway.routing_decisions",
];
const RETAINED_FUNCTIONS: [&str; 6] = [
    "gateway.reserve_knowledge_model_call",
    "gateway.settle_knowledge_model_call",
    "gateway.enterprise_user_epoch",
    "gateway.enterprise_assignment_epoch",
    "gateway.enterprise_role_policy_epoch",
    "gateway.enterprise_tenant_mfa_epoch",
];

#[derive(Clone, Copy)]
struct Approved028 {
    lock_sha256: &'static str,
    manifest_sha256: &'static str,
    reference_sha256: &'static str,
    measurement_sha256: &'static str,
    activation_sha256: &'static str,
    source_plan_sha256: &'static str,
}

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
    pub predecessor_reference_sha256: String,
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
        .map_err(|_| refused("029 source JSON is malformed"))
}
fn lock_files(lock: &Value) -> Result<&serde_json::Map<String, Value>> {
    if lock["schema"] != "migration-authority/source-distribution/v1"
        || lock["baseline_id"] != BASELINE
    {
        return Err(refused("029 predecessor lock identity is invalid"));
    }
    lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("029 predecessor lock has no inventory"))
}
fn digest(value: &Value) -> Result<String> {
    Ok(safe::sha256_bytes(
        &serde_json::to_vec(value).map_err(|_| refused("029 plan serialization failed"))?,
    ))
}
fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn approved_028() -> Result<Approved028> {
    let pins = Approved028 {
        lock_sha256: APPROVED_028_LOCK_SHA256
            .ok_or_else(|| refused("029 is blocked until the real measured 028 lock is pinned"))?,
        manifest_sha256: APPROVED_028_MANIFEST_SHA256.ok_or_else(|| {
            refused("029 is blocked until the real measured 028 manifest is pinned")
        })?,
        reference_sha256: APPROVED_028_REFERENCE_SHA256.ok_or_else(|| {
            refused("029 is blocked until the real 028 verification reference is pinned")
        })?,
        measurement_sha256: APPROVED_028_MEASUREMENT_SHA256.ok_or_else(|| {
            refused("029 is blocked until the real 028 measurement receipt is pinned")
        })?,
        activation_sha256: APPROVED_028_ACTIVATION_SHA256.ok_or_else(|| {
            refused("029 is blocked until the real 028 activation receipt is pinned")
        })?,
        source_plan_sha256: APPROVED_028_SOURCE_PLAN_SHA256
            .ok_or_else(|| refused("029 is blocked until the real measured 028 plan is pinned"))?,
    };
    if [
        pins.lock_sha256,
        pins.manifest_sha256,
        pins.reference_sha256,
        pins.measurement_sha256,
        pins.activation_sha256,
        pins.source_plan_sha256,
    ]
    .iter()
    .any(|hash| !valid_sha256(hash))
    {
        return Err(refused(
            "029 measured 028 identity contains an invalid SHA-256",
        ));
    }
    Ok(pins)
}
fn transform_worker_grant(source: &str) -> Result<String> {
    if source.matches(WORKER_GRANT_ORIGINAL).count() != 1
        || !source.ends_with(WORKER_GRANT_ORIGINAL)
    {
        return Err(refused("029 exact terminal worker grant anchor drifted"));
    }
    Ok(source.replacen(WORKER_GRANT_ORIGINAL, WORKER_GRANT_DERIVED, 1))
}
fn expected_sql(root: &Path) -> Result<Vec<u8>> {
    let mut expected = HEADER.as_bytes().to_vec();
    expected.extend_from_slice(OWNER_GUARD.as_bytes());
    for (index, input) in PROPOSAL_INPUTS.iter().enumerate() {
        let source = fs::read(safe::source_file(root, input.path)?)?;
        if safe::sha256_bytes(&source) != input.sha256 {
            return Err(refused("029 frozen proposal input SHA-256 drifted"));
        }
        let text =
            std::str::from_utf8(&source).map_err(|_| refused("029 proposal is not UTF-8"))?;
        let body = if index == 0 {
            transform_worker_grant(text)?
        } else {
            text.to_owned()
        };
        reject_embedded_transactions(&body, "platform_upgrade_gateway_workers_batch_029")?;
        expected.extend_from_slice(
            format!(
                "\n-- BEGIN FIXED SOURCE {}: {}\n-- Frozen source SHA-256: {}\n",
                input.label, input.path, input.sha256
            )
            .as_bytes(),
        );
        expected.extend_from_slice(body.as_bytes());
        if !body.ends_with('\n') {
            expected.push(b'\n');
        }
        expected.extend_from_slice(format!("-- END FIXED SOURCE {}\n", input.label).as_bytes());
    }
    Ok(expected)
}
fn require_proposal(root: &Path) -> Result<Vec<u8>> {
    let expected = expected_sql(root)?;
    let bytes = fs::read(safe::source_file(root, PROPOSAL)?)?;
    if bytes != expected || safe::sha256_bytes(&bytes) != SQL_SHA256 {
        return Err(refused(
            "029 fixed aggregate differs from its exact source inputs or approved digest",
        ));
    }
    reject_embedded_transactions(
        std::str::from_utf8(&bytes).map_err(|_| refused("029 bundle is not UTF-8"))?,
        "platform_upgrade_gateway_workers_batch_029",
    )?;
    Ok(bytes)
}

fn predecessor_owner_precondition() -> &'static str {
    "SELECT current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)<>current_user) AND (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge'])) AND NOT EXISTS(SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls))"
}
fn dependencies_present() -> &'static str {
    "SELECT pg_catalog.to_regclass('gateway.task_deliveries') IS NOT NULL AND pg_catalog.to_regclass('gateway.dispatch_token_reservations') IS NOT NULL AND pg_catalog.to_regclass('gateway.price_book_v2_usage_receipts') IS NOT NULL AND pg_catalog.to_regclass('gateway.price_book_v2_accounting_checkpoints') IS NOT NULL AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_usage_receipts'::pg_catalog.regclass AND conname='price_book_v2_usage_receipts_request_v2_check' AND convalidated) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.price_book_v2_accounting_checkpoints'::pg_catalog.regclass AND conname='price_book_v2_accounting_checkpoints_stage_check' AND convalidated) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.dispatch_token_reservations'::pg_catalog.regclass AND conname='dispatch_token_reservations_request_type_check' AND convalidated) AND EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='gateway.usage_records'::pg_catalog.regclass AND conname='usage_records_meter_v1' AND convalidated)"
}
fn target_objects_absent() -> String {
    let tables = TABLES
        .iter()
        .map(|(schema, table)| format!("pg_catalog.to_regclass('{schema}.{table}') IS NULL"));
    let functions=FUNCTIONS.iter().map(|(schema,name,_)|format!("NOT EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='{schema}' AND p.proname='{name}')"));
    tables.chain(functions).collect::<Vec<_>>().join(" AND ")
}
fn objects_postcondition() -> String {
    let tables=TABLES.iter().map(|(schema, table)|format!("EXISTS(SELECT 1 FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname='{schema}' AND c.relname='{table}' AND c.relkind='r' AND pg_catalog.pg_get_userbyid(c.relowner)=current_user)"));
    let functions=FUNCTIONS.iter().map(|(schema,name,args)|format!("EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='{schema}' AND p.proname='{name}' AND pg_catalog.pg_get_function_identity_arguments(p.oid)='{args}' AND pg_catalog.pg_get_userbyid(p.proowner)=current_user)"));
    format!(
        "SELECT {}",
        tables.chain(functions).collect::<Vec<_>>().join(" AND ")
    )
}
fn state_postcondition() -> &'static str {
    "SELECT NOT EXISTS(SELECT 1 FROM gateway.task_batch_contract) AND (SELECT count(*)=4 AND bool_and(p.prosecdef AND p.proconfig=ARRAY['search_path=pg_catalog']::text[]) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='gateway' AND p.proname IN ('acquire_worker_lease','renew_worker_lease','assert_worker_lease','release_worker_lease')) AND (SELECT count(*)=2 AND bool_and(NOT p.prosecdef AND p.proconfig=ARRAY['search_path=pg_catalog, pg_temp']::text[]) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname='gateway' AND p.proname IN ('task_batch_binding','task_batch_hold_binding')) AND (SELECT count(*)=2 FROM pg_catalog.pg_trigger WHERE NOT tgisinternal AND tgenabled='O' AND tgtype=23 AND ((tgrelid='gateway.task_provider_batches'::pg_catalog.regclass AND tgname='task_batch_bound' AND tgfoid='gateway.task_batch_binding()'::pg_catalog.regprocedure) OR (tgrelid='gateway.task_batch_quota_holds'::pg_catalog.regclass AND tgname='task_batch_hold_bound' AND tgfoid='gateway.task_batch_hold_binding()'::pg_catalog.regprocedure))) AND EXISTS(SELECT 1 FROM pg_catalog.pg_attribute WHERE attrelid='gateway.task_batch_inputs'::pg_catalog.regclass AND attname='request_count' AND atttypid='bigint'::pg_catalog.regtype AND attnotnull AND NOT attisdropped)"
}
fn constraints_postcondition() -> String {
    let checks = [
        (
            "gateway.price_book_v2_usage_receipts",
            "price_book_v2_batch_receipt_check",
        ),
        (
            "gateway.price_book_v2_accounting_checkpoints",
            "price_book_v2_batch_stage_check",
        ),
        (
            "gateway.dispatch_token_reservations",
            "dispatch_token_reservations_batch_type_check",
        ),
        ("gateway.usage_records", "usage_records_batch_meter_v1"),
    ];
    let dropped = [
        (
            "gateway.price_book_v2_usage_receipts",
            "price_book_v2_usage_receipts_request_v2_check",
        ),
        (
            "gateway.price_book_v2_accounting_checkpoints",
            "price_book_v2_accounting_checkpoints_stage_check",
        ),
        (
            "gateway.dispatch_token_reservations",
            "dispatch_token_reservations_request_type_check",
        ),
        ("gateway.usage_records", "usage_records_meter_v1"),
    ];
    let validated=checks.iter().map(|(table,name)|format!("EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='{table}'::pg_catalog.regclass AND conname='{name}' AND convalidated)"));
    let absent=dropped.iter().map(|(table,name)|format!("NOT EXISTS(SELECT 1 FROM pg_catalog.pg_constraint WHERE conrelid='{table}'::pg_catalog.regclass AND conname='{name}')"));
    format!(
        "SELECT {}",
        validated.chain(absent).collect::<Vec<_>>().join(" AND ")
    )
}

fn retained_feature_postcondition() -> String {
    let tables = RETAINED_TABLES
        .iter()
        .map(|table| format!("pg_catalog.to_regclass('{table}') IS NOT NULL"));
    let functions = RETAINED_FUNCTIONS.iter().map(|name| {
        format!("EXISTS(SELECT 1 FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname||'.'||p.proname='{name}')")
    });
    format!(
        "SELECT {}",
        tables.chain(functions).collect::<Vec<_>>().join(" AND ")
    )
}
fn target_artifacts_absent(root: &Path) -> Result<()> {
    for name in [SQL, REFERENCE, MEASUREMENT, ACTIVATION] {
        match fs::symlink_metadata(epoch_file(root, name)) {
            Ok(_) => {
                return Err(refused(
                    "029 candidate artifact already exists in its source",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    for relative in [ARCHIVED_LOCK, ARCHIVED_MANIFEST] {
        match fs::symlink_metadata(root.join(relative)) {
            Ok(_) => {
                return Err(refused(
                    "029 predecessor archive already exists in its source",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn change() -> manifest::ChangeSpec {
    manifest::ChangeSpec {
        sequence:CANDIDATE_EPOCH,name:"platform_upgrade_gateway_workers_batch_029".into(),file:SQL.into(),sha256:SQL_SHA256.into(),
        owner:"owner".into(),transaction_mode:manifest::TransactionMode::Transactional,rollback_class:manifest::RollbackClass::ForwardFixOnly,
        preconditions:vec![predecessor_owner_precondition().into(),dependencies_present().into(),format!("SELECT {}",target_objects_absent())],
        postconditions:vec![predecessor_owner_precondition().into(),objects_postcondition(),state_postcondition().into(),constraints_postcondition(),retained_feature_postcondition()],
        timeout_seconds:600,lock_budget_seconds:5,resume_handler:None,repair_handler:None,
        notes:"Fixed two-source 028 -> 029 Gateway worker lease and gated provider Batch extension. Minimal exact Gateway table/column/typed function ACL only; Runtime/Worker/Knowledge/public receive none. Retain the exact token/request branches while validating the proposal's named Batch expansions before dropping only their prior narrower checks. Empty Batch marker is mandatory; actual schema/grants/price contract acceptance precedes any separate marker activation. No role/schema/extension/reference-data changes.".into(),
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
            "029 requires the exact complete activated 028 predecessor",
        ));
    }
    Ok(())
}

pub fn validate_source(root: &Path) -> Result<Candidate> {
    let pins = approved_028()?;
    let root = safe::canonical_directory(root, "029 source must be a nonsymlink directory")?;
    let sql = require_proposal(&root)?;
    let predecessor_activation = next_successor_028::verify_activated_source(&root)?;
    distribution::verify(&root.join("database"), BASELINE)?;
    let mut epoch = manifest::load_epoch_manifest(&root.join(MANIFEST))?;
    require_predecessor(&epoch)?;
    let package_lock = source_json(&root, LOCK)?;
    let package_lock_sha256 = manifest::file_sha256(&safe::source_file(&root, LOCK)?)?;
    let predecessor_manifest_sha256 = manifest::file_sha256(&safe::source_file(&root, MANIFEST)?)?;
    let reference_sha256 = manifest::file_sha256(&epoch_file(&root, "028_verification.json"))?;
    let measurement_sha256 =
        manifest::file_sha256(&epoch_file(&root, "SUCCESSOR_028_MEASUREMENT.json"))?;
    let activation_sha256 =
        manifest::file_sha256(&epoch_file(&root, "SUCCESSOR_028_ACTIVATION.json"))?;
    if package_lock_sha256 != pins.lock_sha256
        || predecessor_manifest_sha256 != pins.manifest_sha256
        || reference_sha256 != pins.reference_sha256
        || measurement_sha256 != pins.measurement_sha256
        || activation_sha256 != pins.activation_sha256
        || predecessor_activation["source_plan_sha256"] != pins.source_plan_sha256
        || predecessor_activation["verification_ref_sha256"] != pins.reference_sha256
        || predecessor_activation["measurement_receipt_sha256"] != pins.measurement_sha256
    {
        return Err(refused(
            "029 predecessor differs from the exact measured 028 identity pins",
        ));
    }
    target_artifacts_absent(&root)?;
    let (baseline, baseline_sha256) = commands::load_baseline_with_mode(
        &AuthorityPaths {
            database_dir: root.join("database"),
        },
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    let mut manifest_value = source_json(&root, MANIFEST)?;
    let spec = change();
    manifest_value["epoch"] = json!(CANDIDATE_EPOCH);
    manifest_value["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("029 predecessor manifest has no change list"))?
        .push(serde_json::to_value(&spec).map_err(|_| refused("029 change serialization failed"))?);
    let plan = json!({
        "schema_version": "ai-platform/canonical-fixed-successor-policy/v1",
        "phase": "029_gateway_worker_leases_and_gated_batch",
        "baseline_id": BASELINE, "predecessor_epoch": PREDECESSOR_EPOCH, "candidate_epoch": CANDIDATE_EPOCH,
        "predecessor_package_lock_sha256": package_lock_sha256,
        "predecessor_manifest_sha256": predecessor_manifest_sha256,
        "predecessor_verification_reference_sha256": reference_sha256,
        "predecessor_measurement_receipt_sha256": measurement_sha256,
        "predecessor_activation_receipt_sha256": activation_sha256,
        "predecessor_source_plan_sha256": pins.source_plan_sha256,
        "baseline_manifest_sha256": baseline_sha256,
        "proposal_path": PROPOSAL, "proposal_sha256": SQL_SHA256,
        "proposal_inputs": PROPOSAL_INPUTS.iter().map(|i| json!({"label":i.label,"path":i.path,"sha256":i.sha256})).collect::<Vec<_>>(),
        "dependency_order": ["gateway-singleton-worker-leases", "generic-task-batch-reconciliation"],
        "fixed_transformation": "Replace exactly the worker proposal's terminal hardcoded Gateway GRANT with owner-prefix-derived quoted role grant; all other proposal bytes retained",
        "new_tables": TABLES, "new_functions": FUNCTIONS,
        "acl_additions": acl::expected_acl_additions(),
        "batch_activation": "marker table remains empty; actual contract acceptance is a separate operator gate",
        "database_actions_authorized": false, "fingerprint_status": "not_measured",
        "successor": {"change": spec}
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
        predecessor_reference_sha256: reference_sha256,
        predecessor_measurement_sha256: measurement_sha256,
        predecessor_activation_sha256: activation_sha256,
    })
}

fn external_destination(source: &Path, preview: Option<&Path>, output: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(output)?;
    match fs::symlink_metadata(&absolute) {
        Ok(_) => return Err(refused("029 output must be new")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("029 output parent is missing"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("029 output name is missing"))?,
    );
    if absolute != resolved
        || absolute.starts_with(source)
        || source.starts_with(&absolute)
        || preview.is_some_and(|path| absolute.starts_with(path) || path.starts_with(&absolute))
    {
        return Err(refused("029 output must be external and disjoint"));
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
        "predecessor_reference_sha256": candidate.predecessor_reference_sha256,
        "predecessor_measurement_sha256": candidate.predecessor_measurement_sha256,
        "predecessor_activation_sha256": candidate.predecessor_activation_sha256,
        "candidate_manifest_sha256": manifest_sha,
        "baseline_manifest_sha256": candidate.baseline_sha256,
        "candidate_sql_sha256": SQL_SHA256,
        "expected_database": database,
        "verification_ref_sha256": reference,
        "measurement_receipt_sha256": measurement,
        "post_migration_noop": if matches!(status, "measured_external_preview" | "activated_source_chain") { Some(true) } else { None },
        "batch_submission_activated": false
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
            "029 preview has missing or unexpected database inputs",
        ));
    }
    for (relative, hash) in lock_files(&candidate.package_lock)? {
        if relative == &epoch_relative("manifest.yml") {
            continue;
        }
        let path = safe::source_file(root, &format!("database/{relative}"))?;
        if manifest::file_sha256(&path)? != hash.as_str().unwrap_or_default() {
            return Err(refused("029 changed an activated028 source input"));
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
            "029 preview is not the exact fixed worker and gated Batch extension",
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
            .map_err(|_| refused("029 preview name is invalid"))?;
        if kind.is_symlink()
            || !matches!(name.as_str(), "database" | MARKER)
            || (name == "database") != kind.is_dir()
            || (name == MARKER && !kind.is_file())
        {
            return Err(refused(
                "029 preview contains an unexpected or unsafe entry",
            ));
        }
        names.insert(name);
    }
    if names != BTreeSet::from(["database".into(), MARKER.into()]) {
        return Err(refused("029 preview inventory is incomplete"));
    }
    Ok(())
}

pub fn materialize(source: &Path, output: &Path) -> Result<Value> {
    let source = safe::canonical_directory(source, "029 source must be a nonsymlink directory")?;
    let candidate = validate_source(&source)?;
    let output = external_destination(&source, None, output)?;
    fs::create_dir(&output)?;
    safe::copy_tree(&source.join("database"), &output.join("database"))?;
    safe::write_new_bytes(&epoch_file(&output, SQL), &require_proposal(&source)?)?;
    fs::write(
        output.join(MANIFEST),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&candidate.manifest_value)
                .map_err(|_| refused("029 manifest serialization failed"))?
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
    let source = safe::canonical_directory(source, "029 source must be a real directory")?;
    let preview = safe::canonical_directory(preview, "029 preview must be a real directory")?;
    if preview.starts_with(&source) || source.starts_with(&preview) {
        return Err(refused("029 preview must be external to its source"));
    }
    verify_preview_root(&preview)?;
    let candidate = validate_source(&source)?;
    verify_inputs(&preview, &candidate, measured, false)?;
    if manifest::file_sha256(&safe::source_file(&preview, LOCK)?)? != candidate.package_lock_sha256
    {
        return Err(refused(
            "029 preview modified the 028 predecessor package lock",
        ));
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
            "029 preview marker is not bound to the exact source",
        ));
    }
    if measured {
        measurement::verify_measured_files(&source, &preview, &candidate)?;
    }
    Ok(candidate)
}

#[cfg(test)]
#[path = "next_successor_029_tests.rs"]
mod tests;
