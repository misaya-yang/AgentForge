//! Application-owned, read-only schema admission. Every application role uses
//! the owner-installed metadata reader; permission failures never trigger a
//! privileged or direct-ledger fallback.
use crate::{
    AuthorityError, DEFAULT_BASELINE_ID, Result,
    manifest::{BASELINE_SCHEMA, BaselineManifest, ChangeSpec},
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use std::collections::BTreeSet;

const ADMISSION_SNAPSHOT_VERSION: &str = "ai-platform/schema-admission-snapshot/v1";
const MAX_SUPPORTED_SUCCESSOR_EPOCH: i32 = 28;
const BASELINE_MANIFEST_BYTES: &[u8] =
    include_bytes!("../../../../../database/baselines/2026_08_post_kb_v1/manifest.json");
const EPOCH_MANIFEST_BYTES: &[u8] =
    include_bytes!("../../../../../database/migrations/2026_08_post_kb_v1/manifest.yml");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationPrincipal {
    /// Default Gateway task delivery uses the in-memory queue.
    Gateway,
    /// Explicit durable delivery requires successor020.
    GatewayDurable,
    KnowledgeApi,
    KnowledgeWorker,
    Runtime,
    CapabilityWorker,
}

#[derive(Clone)]
struct ExpectedChange {
    sequence: i32,
    name: String,
    checksum_sha256: String,
}

#[derive(Clone)]
struct AdmissionContract {
    baseline_id: String,
    baseline_manifest_sha256: String,
    structural_sha256: String,
    acl_sha256: String,
    extensions_sha256: String,
    reference_data_sha256: String,
    source_git_sha: String,
    source_epoch: i32,
    changes: Vec<ExpectedChange>,
}

#[derive(Debug)]
enum ReaderFailure {
    Denied,
    Database(sqlx::Error),
}

fn refused(message: &str) -> AuthorityError {
    AuthorityError::Refused(message.into())
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn exact_keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    })
}

fn compiled_contract(baseline_bytes: &[u8], epoch_bytes: &[u8]) -> Result<AdmissionContract> {
    let baseline: BaselineManifest = serde_json::from_slice(baseline_bytes)
        .map_err(|_| refused("compiled baseline manifest is malformed"))?;
    if baseline.schema != BASELINE_SCHEMA
        || baseline.state != "frozen"
        || baseline.baseline_id != DEFAULT_BASELINE_ID
        || baseline.source_git_sha.len() != 40
        || !baseline
            .source_git_sha
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || ![
            baseline.structural_sha256.as_str(),
            baseline.acl_sha256.as_str(),
            baseline.extensions_sha256.as_str(),
            baseline.reference_data_sha256.as_str(),
        ]
        .into_iter()
        .all(valid_sha256)
    {
        return Err(refused("compiled frozen baseline identity is invalid"));
    }

    let epoch: Value = serde_yaml::from_slice(epoch_bytes)
        .map_err(|_| refused("compiled epoch manifest is malformed"))?;
    if !exact_keys(&epoch, &["baseline_id", "epoch", "changes"])
        || epoch["baseline_id"] != DEFAULT_BASELINE_ID
    {
        return Err(refused("compiled epoch manifest identity is invalid"));
    }
    let source_epoch = epoch["epoch"]
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .filter(|value| (20..=MAX_SUPPORTED_SUCCESSOR_EPOCH).contains(value))
        .ok_or_else(|| refused("compiled epoch is invalid"))?;
    let entries = epoch["changes"]
        .as_array()
        .ok_or_else(|| refused("compiled epoch changes are malformed"))?;
    if entries.len() != source_epoch as usize {
        return Err(refused("compiled epoch is not a complete manifest"));
    }
    let mut changes = Vec::with_capacity(entries.len());
    let mut names = BTreeSet::new();
    for (index, entry) in entries.iter().enumerate() {
        let change: ChangeSpec = serde_json::from_value(entry.clone())
            .map_err(|_| refused("compiled epoch change is malformed"))?;
        if change.sequence != index as i32 + 1
            || change.name.is_empty()
            || !names.insert(change.name.clone())
            || !valid_sha256(&change.sha256)
        {
            return Err(refused("compiled epoch changes are not an exact prefix"));
        }
        changes.push(ExpectedChange {
            sequence: change.sequence,
            name: change.name,
            checksum_sha256: change.sha256,
        });
    }

    Ok(AdmissionContract {
        baseline_id: baseline.baseline_id,
        baseline_manifest_sha256: format!("{:x}", Sha256::digest(baseline_bytes)),
        structural_sha256: baseline.structural_sha256,
        acl_sha256: baseline.acl_sha256,
        extensions_sha256: baseline.extensions_sha256,
        reference_data_sha256: baseline.reference_data_sha256,
        source_git_sha: baseline.source_git_sha,
        source_epoch,
        changes,
    })
}

/// Export the exact snapshot compiled from the frozen baseline and complete
/// activated successor manifest. No database, caller-supplied hashes or repair.
pub fn compiled_admission_snapshot() -> Result<Value> {
    let contract = compiled_contract(BASELINE_MANIFEST_BYTES, EPOCH_MANIFEST_BYTES)?;
    Ok(snapshot_value(&contract, contract.source_epoch))
}

fn snapshot_value(contract: &AdmissionContract, epoch: i32) -> Value {
    let changes = contract
        .changes
        .iter()
        .take(epoch as usize)
        .map(|change| {
            serde_json::json!({
                "baseline_id": contract.baseline_id,
                "sequence": change.sequence,
                "name": change.name,
                "checksum_sha256": change.checksum_sha256,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema_version": ADMISSION_SNAPSHOT_VERSION,
        "baselines": [{
            "baseline_id": contract.baseline_id,
            "manifest_sha256": contract.baseline_manifest_sha256,
            "structural_sha256": contract.structural_sha256,
            "acl_sha256": contract.acl_sha256,
            "extensions_sha256": contract.extensions_sha256,
            "reference_data_sha256": contract.reference_data_sha256,
            "source_git_sha": contract.source_git_sha,
        }],
        "epoch": epoch,
        "changes": changes,
        "has_attempts": false,
        "has_incomplete_attempts": false,
    })
}

fn window(principal: ApplicationPrincipal, source_epoch: i32) -> (i32, i32) {
    // Request-metered model contracts become usable after the measured028 contraction.
    if source_epoch >= 28 {
        return (28, source_epoch);
    }

    // Native model admission, request accounting and enterprise sessions use027.
    if source_epoch >= 27 {
        return (27, source_epoch);
    }

    // Current code uses the price-book, witnesses and durable vector jobs in026.
    // Historical compiled contracts retain their original windows below.
    if source_epoch >= 26 {
        return (26, source_epoch);
    }
    match principal {
        ApplicationPrincipal::Runtime | ApplicationPrincipal::CapabilityWorker => {
            (24, source_epoch)
        }
        // Current Gateway issues turns through the metadata-hardened025 function.
        ApplicationPrincipal::Gateway | ApplicationPrincipal::GatewayDurable
            if source_epoch >= 25 =>
        {
            (25, source_epoch)
        }
        ApplicationPrincipal::Gateway => (18, source_epoch),
        ApplicationPrincipal::GatewayDurable => (20, source_epoch),
        ApplicationPrincipal::KnowledgeApi => (19, source_epoch),
        ApplicationPrincipal::KnowledgeWorker if source_epoch >= 21 => (21, source_epoch),
        ApplicationPrincipal::KnowledgeWorker => (19, 20),
    }
}

fn validate_snapshot_value(
    snapshot: &Value,
    principal: ApplicationPrincipal,
    contract: &AdmissionContract,
) -> Result<()> {
    if !(20..=MAX_SUPPORTED_SUCCESSOR_EPOCH).contains(&contract.source_epoch) {
        return Err(refused(&format!(
            "application admission requires an exact measured source manifest at epoch 20..={MAX_SUPPORTED_SUCCESSOR_EPOCH}",
        )));
    }
    if !exact_keys(
        snapshot,
        &[
            "schema_version",
            "baselines",
            "epoch",
            "changes",
            "has_attempts",
            "has_incomplete_attempts",
        ],
    ) || snapshot["schema_version"] != ADMISSION_SNAPSHOT_VERSION
    {
        return Err(refused("schema admission snapshot shape is unsupported"));
    }

    let baselines = snapshot["baselines"]
        .as_array()
        .filter(|rows| rows.len() == 1)
        .ok_or_else(|| refused("schema admission requires exactly one baseline marker"))?;
    let baseline = &baselines[0];
    if !exact_keys(
        baseline,
        &[
            "baseline_id",
            "manifest_sha256",
            "structural_sha256",
            "acl_sha256",
            "extensions_sha256",
            "reference_data_sha256",
            "source_git_sha",
        ],
    ) || baseline["baseline_id"] != contract.baseline_id
        || baseline["manifest_sha256"] != contract.baseline_manifest_sha256
        || baseline["structural_sha256"] != contract.structural_sha256
        || baseline["acl_sha256"] != contract.acl_sha256
        || baseline["extensions_sha256"] != contract.extensions_sha256
        || baseline["reference_data_sha256"] != contract.reference_data_sha256
        || baseline["source_git_sha"] != contract.source_git_sha
    {
        return Err(refused(
            "adopted baseline marker differs from frozen source",
        ));
    }

    if snapshot["has_attempts"] != false || snapshot["has_incomplete_attempts"] != false {
        return Err(refused("schema authority contains migration attempt rows"));
    }
    let epoch = snapshot["epoch"]
        .as_i64()
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| refused("schema admission epoch is malformed"))?;
    let (minimum, maximum) = window(principal, contract.source_epoch);
    if epoch < minimum || epoch > maximum {
        return Err(refused("schema epoch is outside this application profile"));
    }

    let changes = snapshot["changes"]
        .as_array()
        .ok_or_else(|| refused("schema admission changes are malformed"))?;
    if changes.len() != epoch as usize || changes.len() > contract.changes.len() {
        return Err(refused("schema changes are sparse, truncated, or future"));
    }
    for (index, applied) in changes.iter().enumerate() {
        let expected = &contract.changes[index];
        if !exact_keys(
            applied,
            &["baseline_id", "sequence", "name", "checksum_sha256"],
        ) || applied["baseline_id"] != contract.baseline_id
            || applied["sequence"] != expected.sequence
            || applied["name"] != expected.name
            || applied["checksum_sha256"] != expected.checksum_sha256
        {
            return Err(refused(
                "schema change ledger differs from its manifest prefix",
            ));
        }
    }
    Ok(())
}

fn validate_reader_result(
    result: std::result::Result<Value, ReaderFailure>,
    principal: ApplicationPrincipal,
    contract: &AdmissionContract,
) -> Result<()> {
    match result {
        Ok(snapshot) => validate_snapshot_value(&snapshot, principal, contract),
        Err(ReaderFailure::Denied) => Err(refused("schema admission reader permission denied")),
        Err(ReaderFailure::Database(error)) => Err(AuthorityError::Database(error)),
    }
}

fn classify_reader_error(error: sqlx::Error) -> ReaderFailure {
    if error
        .as_database_error()
        .and_then(|database| database.code())
        .as_deref()
        == Some("42501")
    {
        ReaderFailure::Denied
    } else {
        ReaderFailure::Database(error)
    }
}

// This catalog query runs as the connected login, before SECURITY DEFINER.
// The reader's schema is chosen by code, never by the DSN or search_path.
const PRINCIPAL_IDENTITY_SQL: &str = r#"
SELECT pg_catalog.jsonb_build_object(
    'current_user', current_user, 'session_user', session_user,
    'login', caller.rolcanlogin, 'inherit', caller.rolinherit,
    'superuser', caller.rolsuper, 'createdb', caller.rolcreatedb,
    'createrole', caller.rolcreaterole, 'replication', caller.rolreplication,
    'bypassrls', caller.rolbypassrls, 'owner', owner.rolname,
    'canonical_owner', reader.proowner = namespace.nspowner
        AND NOT (owner.rolcanlogin OR owner.rolsuper OR owner.rolcreatedb
                 OR owner.rolcreaterole OR owner.rolreplication OR owner.rolbypassrls),
    'metadata_reader', reader.prosecdef AND reader.prokind = 'f'
        AND reader.prorettype = 'pg_catalog.jsonb'::pg_catalog.regtype)
FROM pg_catalog.pg_proc AS reader
JOIN pg_catalog.pg_namespace AS namespace ON namespace.oid = reader.pronamespace
JOIN pg_catalog.pg_roles AS owner ON owner.oid = reader.proowner
JOIN pg_catalog.pg_roles AS caller ON caller.rolname = current_user
WHERE namespace.nspname = $1
  AND reader.proname = 'platform_schema_admission_snapshot_v1'
  AND reader.pronargs = 0
"#;

fn principal_role_suffix(principal: ApplicationPrincipal) -> &'static str {
    match principal {
        ApplicationPrincipal::Gateway | ApplicationPrincipal::GatewayDurable => "gateway",
        ApplicationPrincipal::KnowledgeApi => "knowledge_api",
        ApplicationPrincipal::KnowledgeWorker => "knowledge_worker",
        ApplicationPrincipal::Runtime => "runtime",
        ApplicationPrincipal::CapabilityWorker => "capability_worker",
    }
}

fn validate_principal_identity(identity: &Value, principal: ApplicationPrincipal) -> Result<()> {
    let owner = identity["owner"].as_str().unwrap_or_default();
    let stem = owner.strip_suffix("_owner").unwrap_or_default();
    let valid_prefix = !stem.is_empty()
        && stem.len() <= 21
        && stem.as_bytes()[0].is_ascii_lowercase()
        && stem
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    let expected_role = format!("{stem}_{}", principal_role_suffix(principal));
    if !valid_prefix
        || identity["current_user"] != expected_role
        || identity["session_user"] != expected_role
        || identity["login"] != true
        || identity["canonical_owner"] != true
        || identity["metadata_reader"] != true
        || [
            "inherit",
            "superuser",
            "createdb",
            "createrole",
            "replication",
            "bypassrls",
        ]
        .iter()
        .any(|key| identity[*key] != false)
    {
        return Err(refused(
            "schema admission requires the canonical unprivileged service login role",
        ));
    }
    Ok(())
}

/// Run before exposing routes or starting background work, on the application's
/// own connection. This verifies login identity, then performs one EXECUTE-only
/// metadata read. It never reads
/// ledger tables directly or changes role after a function/permission failure.
pub async fn require_compatible_schema(
    conn: &mut PgConnection,
    principal: ApplicationPrincipal,
) -> Result<()> {
    let contract = compiled_contract(BASELINE_MANIFEST_BYTES, EPOCH_MANIFEST_BYTES)?;
    let reader_schema = match principal {
        ApplicationPrincipal::Runtime | ApplicationPrincipal::CapabilityWorker => "assistant",
        _ => "gateway",
    };
    let identity = sqlx::query_scalar::<_, Value>(PRINCIPAL_IDENTITY_SQL)
        .bind(reader_schema)
        .fetch_one(&mut *conn)
        .await
        .map_err(|_| refused("schema admission principal identity unavailable"))?;
    validate_principal_identity(&identity, principal)?;
    let reader = match principal {
        ApplicationPrincipal::Runtime | ApplicationPrincipal::CapabilityWorker => {
            "SELECT assistant.platform_schema_admission_snapshot_v1()"
        }
        _ => "SELECT gateway.platform_schema_admission_snapshot_v1()",
    };
    let snapshot = sqlx::query_scalar::<_, Value>(reader)
        .fetch_one(&mut *conn)
        .await
        .map_err(classify_reader_error);
    validate_reader_result(snapshot, principal, &contract)
}

#[cfg(test)]
#[path = "application_guard/tests.rs"]
mod tests;
