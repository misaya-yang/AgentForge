//! Source-only U-DATA-12 migration safety gate and executed N-1 rehearsal entry.
//!
//! The static report is deliberately never a compatibility pass. A separate
//! action must execute a source-hashed N-1 driver against a named isolated DB.
use ai_platform_migration_authority::{
    AuthorityError, DEFAULT_BASELINE_ID, Result, manifest,
    manifest::{ChangeSpec, EpochManifest, TransactionMode},
};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[path = "migration_lint/approved_027_grants.rs"]
mod approved_027_grants;

#[path = "migration_lint/grants.rs"]
mod grants;
#[path = "migration_lint/n1.rs"]
mod n1;
#[path = "migration_lint/sql_lexer.rs"]
mod sql_lexer;
#[path = "migration_lint/sql_policy.rs"]
mod sql_policy;
#[path = "migration_lint/sql_safety.rs"]
mod sql_safety;

pub use n1::rehearse_n1;
#[cfg(test)]
use n1::{DRIVER_SCHEMA, DriverResult, DriverScenario, validate_driver_result};
use sql_policy::lint_sql;

const MAX_LOCK_TIMEOUT_SECONDS: i64 = 5;
const MAX_STATEMENT_TIMEOUT_SECONDS: i64 = 3_600;
const ROLE_PREFIX_REGEX: &str = "^[a-z][a-z0-9_]{0,20}_$";
const APPROVED_OWNER_PRECONDITION: &str = "SELECT current_user ~ '^[a-z][a-z0-9_]{0,20}_owner$' AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge']) AND pg_catalog.pg_get_userbyid(n.nspowner)<>current_user) AND (SELECT count(*)=3 FROM pg_catalog.pg_namespace n WHERE n.nspname=ANY(ARRAY['assistant','gateway','knowledge'])) AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.rolname=current_user AND (r.rolcanlogin OR r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls)) AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'gateway') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'runtime') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_api') AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles WHERE rolname=left(current_user,length(current_user)-length('owner'))||'knowledge_worker')";

// These are the only dynamic role grants accepted by the fixed 026 source
// bundle. format(%I) quotes the suffix-derived role identifier; no SQL text
// other than these reviewed constants may be supplied by migration input.
const APPROVED_ROLE_GRANTS: &[(&str, &str)] = &[
    (
        "gateway",
        "GRANT SELECT,INSERT ON gateway.price_book_v2_terms,gateway.price_book_v2_publications,gateway.price_book_v2_routes,gateway.price_book_v2_activation_audit,gateway.price_book_v2_usage_receipts,gateway.price_book_v2_accounting_checkpoints,gateway.price_book_v2_import_audit TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(model_version,book_version,revision,updated_by,updated_at) ON gateway.price_book_v2_routes TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(observation,updated_at) ON gateway.price_book_v2_accounting_checkpoints TO %I",
    ),
    (
        "gateway",
        "GRANT EXECUTE ON FUNCTION assistant.complete_assistant_runtime_model_call_v2(uuid,varchar,bigint,bigint,bigint,varchar,jsonb) TO %I",
    ),
    (
        "gateway",
        "GRANT SELECT,INSERT ON gateway.local_node_control_commands TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(acknowledged_at,acknowledged_channel_id) ON gateway.local_node_control_commands TO %I",
    ),
    (
        "gateway",
        "GRANT SELECT,INSERT ON gateway.local_node_control_deliveries TO %I",
    ),
    (
        "gateway",
        "GRANT SELECT,INSERT ON gateway.local_node_request_keys TO %I",
    ),
    (
        "gateway",
        "GRANT SELECT,INSERT ON gateway.local_node_request_nonces TO %I",
    ),
    (
        "gateway",
        "GRANT UPDATE(nonce,issued_at,expires_at,consumed_at) ON gateway.local_node_request_nonces TO %I",
    ),
    (
        "knowledge_api",
        "GRANT SELECT ON knowledge.vector_mutation_jobs TO %I",
    ),
    (
        "knowledge_api",
        "GRANT INSERT(job_id,tenant_id,dataset_id,requested_by,expected_generation,payload,expected_revision) ON knowledge.vector_mutation_jobs TO %I",
    ),
    (
        "knowledge_api",
        "GRANT UPDATE(state,error_class,updated_at) ON knowledge.vector_mutation_jobs TO %I",
    ),
    (
        "knowledge_worker",
        "GRANT SELECT ON knowledge.vector_mutation_jobs TO %I",
    ),
    (
        "knowledge_worker",
        "GRANT UPDATE(state,attempts,result,error_class,updated_at) ON knowledge.vector_mutation_jobs TO %I",
    ),
    (
        "gateway",
        "GRANT INSERT,SELECT ON assistant.assistant_runtime_capability_witnesses TO %I",
    ),
    (
        "runtime",
        "GRANT SELECT ON assistant.assistant_runtime_capability_witnesses TO %I",
    ),
];

const TRANSACTIONAL_TIMEOUT_RUNNER: &str = include_str!("runner.rs");
const NON_TRANSACTIONAL_TIMEOUT_RUNNER: &str = include_str!("recovery.rs");

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub severity: String,
    pub code: String,
    pub statement: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct LintReport {
    pub schema: &'static str,
    pub status: &'static str,
    pub baseline_id: String,
    pub successor_sequence: i32,
    pub change_name: String,
    pub sql_sha256: String,
    pub manifest_sha256: String,
    pub lock_timeout_ms: i64,
    pub statement_timeout_ms: i64,
    pub timeout_owner: &'static str,
    pub compatibility_phase: &'static str,
    pub n1_compatibility: &'static str,
    pub n1_rehearsal_required: bool,
    pub release_eligible: bool,
    pub findings: Vec<Finding>,
}

impl LintReport {
    fn has_blockers(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.severity == "blocked")
    }
}

#[derive(Clone, Debug)]
pub struct N1RunOptions {
    pub previous_release_id: String,
    pub previous_application: PathBuf,
    pub previous_application_sha256: String,
    pub driver: PathBuf,
    pub driver_sha256: String,
    pub expected_database: String,
    pub receipt_output: PathBuf,
}

#[derive(Clone, Debug, Serialize)]
pub struct N1Receipt {
    pub schema: &'static str,
    pub status: &'static str,
    pub baseline_id: String,
    pub successor_sequence: i32,
    pub candidate_sql_sha256: String,
    pub candidate_manifest_sha256: String,
    pub compatibility_phase: String,
    pub expected_database: String,
    pub previous_release_id: String,
    pub previous_application_sha256: String,
    pub driver_sha256: String,
    pub driver_stdout_sha256: String,
    pub scenario_ids: Vec<String>,
    pub passed_scenarios: usize,
    pub failed_scenarios: usize,
    pub driver_exit_code: i32,
    pub elapsed_ms: u128,
    pub executed_at_utc: String,
}

#[derive(Clone, Debug)]
struct CandidateSource {
    root: PathBuf,
    manifest: EpochManifest,
    change: ChangeSpec,
    sql_path: PathBuf,
    sql_sha256: String,
    manifest_sha256: String,
}

fn canonical_source(path: &Path) -> Result<PathBuf> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(refused(
            "migration lint source root must be a real directory",
        ));
    }
    Ok(path.canonicalize()?)
}

fn checked_source_file(root: &Path, path: &Path) -> Result<PathBuf> {
    if !path.starts_with(root) {
        return Err(refused("migration lint source escaped its root"));
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| refused("migration lint source escaped its root"))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        use std::path::Component;
        let Component::Normal(name) = component else {
            return Err(refused("migration lint source path is invalid"));
        };
        current.push(name);
        let metadata = fs::symlink_metadata(&current)?;
        if metadata.file_type().is_symlink() {
            return Err(refused("migration lint rejects source symlinks"));
        }
    }
    let metadata = fs::metadata(&current)?;
    if !metadata.is_file() {
        return Err(refused("migration lint input is not a regular file"));
    }
    Ok(current)
}

fn candidate_source(source_root: &Path, sequence: i32) -> Result<CandidateSource> {
    let root = canonical_source(source_root)?;
    let manifest_path = root
        .join("database/migrations")
        .join(DEFAULT_BASELINE_ID)
        .join("manifest.yml");
    let manifest_path = checked_source_file(&root, &manifest_path)?;
    let manifest = manifest::load_epoch_manifest(&manifest_path)?;
    if manifest.baseline_id != DEFAULT_BASELINE_ID
        || manifest.epoch != sequence
        || manifest.changes.len() != sequence as usize
        || manifest
            .changes
            .iter()
            .enumerate()
            .any(|(index, change)| change.sequence != index as i32 + 1)
    {
        return Err(refused(
            "migration lint accepts only the latest contiguous successor in the canonical baseline",
        ));
    }
    let change = manifest
        .changes
        .iter()
        .find(|change| change.sequence == sequence)
        .cloned()
        .ok_or_else(|| refused("successor change is absent from the manifest"))?;
    let sql_path = root
        .join("database/migrations")
        .join(&manifest.baseline_id)
        .join(&change.file);
    let sql_path = checked_source_file(&root, &sql_path)?;
    let sql_sha256 = manifest::file_sha256(&sql_path)?;
    if sql_sha256 != change.sha256 {
        return Err(refused("successor SQL differs from its manifest checksum"));
    }
    let manifest_sha256 = manifest::file_sha256(&manifest_path)?;
    Ok(CandidateSource {
        root,
        manifest,
        change,
        sql_path,
        sql_sha256,
        manifest_sha256,
    })
}

fn runner_owns_timeouts(mode: TransactionMode) -> bool {
    let transactional = TRANSACTIONAL_TIMEOUT_RUNNER
        .contains("set_config('statement_timeout',$1,true),set_config('lock_timeout',$2,true)")
        && TRANSACTIONAL_TIMEOUT_RUNNER
            .contains(".bind((spec.timeout_seconds * 1000).to_string())")
        && TRANSACTIONAL_TIMEOUT_RUNNER
            .contains(".bind((spec.lock_budget_seconds * 1000).to_string())");
    let non_transactional = NON_TRANSACTIONAL_TIMEOUT_RUNNER
        .contains("set_config('statement_timeout',$1,false),set_config('lock_timeout',$2,false)")
        && NON_TRANSACTIONAL_TIMEOUT_RUNNER
            .contains(".bind((spec.timeout_seconds * 1000).to_string())")
        && NON_TRANSACTIONAL_TIMEOUT_RUNNER
            .contains(".bind((spec.lock_budget_seconds * 1000).to_string())");
    match mode {
        TransactionMode::Transactional => transactional,
        TransactionMode::NonTransactional => non_transactional,
    }
}

pub fn lint_source(source_root: &Path, sequence: i32) -> Result<LintReport> {
    let candidate = candidate_source(source_root, sequence)?;
    let mut findings = Vec::new();
    if candidate.change.lock_budget_seconds < 1
        || candidate.change.lock_budget_seconds > MAX_LOCK_TIMEOUT_SECONDS
        || candidate.change.lock_budget_seconds > candidate.change.timeout_seconds
    {
        findings.push(Finding {
            severity: "blocked".into(),
            code: "lock_timeout_outside_controlled_budget".into(),
            statement: 0,
        });
    }
    if candidate.change.timeout_seconds < 1
        || candidate.change.timeout_seconds > MAX_STATEMENT_TIMEOUT_SECONDS
    {
        findings.push(Finding {
            severity: "blocked".into(),
            code: "statement_timeout_outside_controlled_budget".into(),
            statement: 0,
        });
    }
    if !runner_owns_timeouts(candidate.change.transaction_mode) {
        findings.push(Finding {
            severity: "blocked".into(),
            code: "authority_runner_timeout_contract_missing".into(),
            statement: 0,
        });
    }

    let sql = fs::read_to_string(&candidate.sql_path)?;
    let (mut ddl_findings, phase) = lint_sql(&sql, &candidate.change)?;
    findings.append(&mut ddl_findings);
    findings.sort_by(|a, b| {
        (&a.statement, &a.code, &a.severity).cmp(&(&b.statement, &b.code, &b.severity))
    });
    findings.dedup();
    let blocked = findings.iter().any(|finding| finding.severity == "blocked");
    Ok(LintReport {
        schema: "ai-platform/migration-source-lint/v1",
        status: if blocked {
            "rejected"
        } else {
            "static_lint_passed_n1_not_run"
        },
        baseline_id: candidate.manifest.baseline_id,
        successor_sequence: sequence,
        change_name: candidate.change.name,
        sql_sha256: candidate.sql_sha256,
        manifest_sha256: candidate.manifest_sha256,
        lock_timeout_ms: candidate.change.lock_budget_seconds.saturating_mul(1000),
        statement_timeout_ms: candidate.change.timeout_seconds.saturating_mul(1000),
        timeout_owner: "native_authority_runner_from_manifest",
        compatibility_phase: phase,
        n1_compatibility: "not_run",
        n1_rehearsal_required: true,
        release_eligible: false,
        findings,
    })
}

fn push_finding(findings: &mut Vec<Finding>, severity: &str, code: &str, statement: usize) {
    findings.push(Finding {
        severity: severity.into(),
        code: code.into(),
        statement,
    });
}

#[cfg(test)]
#[path = "migration_lint_tests.rs"]
mod tests;
