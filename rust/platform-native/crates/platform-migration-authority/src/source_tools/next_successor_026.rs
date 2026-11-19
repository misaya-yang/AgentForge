//! Fixed 025 -> 026 source bundle for five reviewed native contracts.
//! Callers cannot supply SQL, source hashes, roles, grants, or fingerprint deltas.
use super::{measure_successors, next_successor as safe, next_successor_025};
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

#[path = "next_successor_026/acl.rs"]
mod acl;
#[path = "next_successor_026/activation.rs"]
mod activation;
#[path = "next_successor_026/measurement.rs"]
mod measurement;

use acl::change;
#[cfg(test)]
use acl::{expected_acl_additions, verify_exact_fingerprints};
pub use activation::{activation_bundle, verify_activated_source};
use measurement::verify_measured_files;
#[cfg(test)]
use measurement::verify_reference_value;
pub use measurement::{MeasureOptions, measure};

pub const BASELINE: &str = "2026_08_post_kb_v1";
pub const PREDECESSOR_EPOCH: i32 = 25;
pub const CANDIDATE_EPOCH: i32 = 26;
/// Exact activated025 source pins supplied by the integration owner.
pub const APPROVED_025_LOCK_SHA256: &str =
    "9eda03e81caf8c9fb2757b4a675daeafa33febe4a397202cac0746e4d05a3b3e";
pub const APPROVED_025_MANIFEST_SHA256: &str =
    "a7e327f282f8a3e0b5535be32af0be2737df15a3037ca48ae10467e5e91c7489";

pub const PROPOSAL: &str = "docs/design/platform-upgrade-native-contracts-026.sql.proposal";
pub const SQL_SHA256: &str = "8d716380f32c5ea3ff815e9228153d6161ae9a94bc68949f7dab02abfaf87b07";
const SQL: &str = "026_platform_upgrade_native_contracts.sql";
const REFERENCE: &str = "026_verification.json";
const MEASUREMENT: &str = "SUCCESSOR_026_MEASUREMENT.json";
const ACTIVATION: &str = "SUCCESSOR_026_ACTIVATION.json";
const MARKER: &str = "PREVIEW_026_NOT_ACTIVATED.json";
const LOCK: &str = "database/authority/package-authority-lock.json";
const MANIFEST: &str = "database/migrations/2026_08_post_kb_v1/manifest.yml";
const ARCHIVED_LOCK: &str =
    "docs/design/canonical-schema-successor-026-approved25-package-lock.json";
const ARCHIVED_MANIFEST: &str =
    "docs/design/canonical-schema-successor-026-approved25-manifest.yml";
const ACL_DELTA: &str = "schema_acl:public:migrator:USAGE:grantopt=0";
const TOOL: &str = "rust/platform-native/crates/platform-migration-authority/src/source_tools/next_successor_026.rs";
const PROVENANCE: &str = "Fixed 025 -> 026 bundle from five exact proposal inputs: price-book v2, its dependent runtime-cache settlement boundary, least-privilege Runtime attenuation witnesses, local-node emergency-stop/PoP4, and Knowledge vector mutation jobs. Full PostgreSQL 16 single-step measurement binds the exact 025 source chain; exact new-object ACL delta is required and roles, extensions, reference data and prior rows must remain unchanged.";
const PROPOSAL_HEADER: &str = "-- UNACTIVATED, unnumbered fixed source bundle for canonical successor 026.\n-- Generated only from the five named proposal sources below, in this order.\n-- The Runtime attenuation-witness table receives a fixed definer path and authority-resolved Gateway/Runtime grants.\n-- No arbitrary SQL input, database action, release pin, or activation is included.\n";
const WITNESS_AUTHORITY_POSTLUDE: &str = "\n-- Keep caller TEMP objects behind trusted schemas for this SECURITY DEFINER trigger.\nALTER FUNCTION assistant.assert_capability_witness_binding() SET search_path=pg_catalog,assistant,pg_temp;\nDO $witness_grants$\nDECLARE prefix text:=left(current_user,length(current_user)-length('owner'));\nBEGIN\n    IF prefix !~ '^[a-z][a-z0-9_]{0,20}_$' THEN\n        RAISE EXCEPTION 'invalid canonical role prefix';\n    END IF;\n    EXECUTE format(\n        'GRANT INSERT,SELECT ON assistant.assistant_runtime_capability_witnesses TO %I',\n        prefix||'gateway'\n    );\n    EXECUTE format(\n        'GRANT SELECT ON assistant.assistant_runtime_capability_witnesses TO %I',\n        prefix||'runtime'\n    );\nEND\n$witness_grants$;\n";

#[derive(Clone, Copy)]
struct ProposalInput {
    label: &'static str,
    path: &'static str,
    sha256: &'static str,
    description: &'static str,
}

const PROPOSAL_INPUTS: [ProposalInput; 5] = [
    ProposalInput {
        label: "price-book-v2",
        path: "database/proposals/price-book-v2.sql",
        sha256: "e384ba326a3d28f251f65ac74ae6a5fcec6877ffa7972ccf88298c12a246dd28",
        description: "Foundation: immutable price-book terms, publications, routing, receipts and audit.",
    },
    ProposalInput {
        label: "runtime-cache-settlement-v2",
        path: "database/proposals/runtime-cache-settlement-v2.sql",
        sha256: "64e857cd925c3f0c6dfc566ee78cf6bac8bd4dc67f900879db03945a985a84a7",
        description: "Depends on price-book v2; adds the Gateway-only SECURITY DEFINER settlement boundary.",
    },
    ProposalInput {
        label: "runtime-capability-witness-store",
        path: "docs/design/proposals/runtime-capability-witness-store.sql",
        sha256: "5e396ac9ebdd4d845da92ce0a47b993a849547cbd40a5474825394c64fbcc0fe",
        description: "Adds managed and SDK attenuation-v3 witnesses with owner-verified snapshot binding and no Worker/Knowledge/PUBLIC access.",
    },
    ProposalInput {
        label: "local-node-owner-emergency-stop-pop4",
        path: "docs/design/local-node-owner-emergency-stop.sql.proposal",
        sha256: "e2f47ab2fbd7f5e21654a5e18418275b0a4a12ad4f82c33f63bcc731362d83ff",
        description: "Adds owner-bound emergency stop and PoP4 request proof records.",
    },
    ProposalInput {
        label: "knowledge-vector-mutation-jobs",
        path: "database/proposals/knowledge-vector-mutation-jobs.sql",
        sha256: "314b56d76e39ddc286ce63d0267bdda8d4a1b6411d1d6c39f6244b8c10dd2408",
        description: "Adds the API-to-worker durable vector mutation queue with least-privilege grants.",
    },
];

const TABLES: [(&str, &str); 14] = [
    ("gateway", "price_book_v2_terms"),
    ("gateway", "price_book_v2_publications"),
    ("gateway", "price_book_v2_routes"),
    ("gateway", "price_book_v2_activation_audit"),
    ("gateway", "price_book_v2_usage_receipts"),
    ("gateway", "price_book_v2_accounting_checkpoints"),
    ("gateway", "price_book_v2_import_audit"),
    ("assistant", "runtime_cache_settlement_v2"),
    ("assistant", "assistant_runtime_capability_witnesses"),
    ("gateway", "local_node_control_commands"),
    ("gateway", "local_node_control_deliveries"),
    ("gateway", "local_node_request_keys"),
    ("gateway", "local_node_request_nonces"),
    ("knowledge", "vector_mutation_jobs"),
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
    pub proposal_sha256: String,
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
        .map_err(|_| refused("026 malformed source JSON"))
}
fn lock_files(lock: &Value) -> Result<&serde_json::Map<String, Value>> {
    if lock["schema"] != "migration-authority/source-distribution/v1"
        || lock["baseline_id"] != BASELINE
    {
        return Err(refused("026 predecessor lock identity is invalid"));
    }
    lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("026 predecessor lock has no inventory"))
}
fn digest(value: &Value) -> Result<String> {
    Ok(safe::sha256_bytes(
        &serde_json::to_vec(value).map_err(|_| refused("026 policy serialization failed"))?,
    ))
}
fn expected_proposal_bytes(root: &Path) -> Result<Vec<u8>> {
    let mut expected = PROPOSAL_HEADER.as_bytes().to_vec();
    for input in PROPOSAL_INPUTS {
        let source = fs::read(safe::source_file(root, input.path)?)?;
        if safe::sha256_bytes(&source) != input.sha256 {
            return Err(refused(
                "026 fixed proposal input differs from its approved SHA-256",
            ));
        }
        expected.extend_from_slice(
            format!(
                "\n-- BEGIN FIXED SOURCE {}: {}\n-- {}\n-- Frozen source SHA-256: {}\n",
                input.label, input.path, input.description, input.sha256
            )
            .as_bytes(),
        );
        expected.extend_from_slice(&source);
        if !source.ends_with(b"\n") {
            expected.push(b'\n');
        }
        expected.extend_from_slice(format!("-- END FIXED SOURCE {}\n", input.label).as_bytes());
        if input.label == "runtime-capability-witness-store" {
            expected.extend_from_slice(WITNESS_AUTHORITY_POSTLUDE.as_bytes());
        }
    }
    Ok(expected)
}
fn require_proposal(root: &Path) -> Result<PathBuf> {
    let path = safe::source_file(root, PROPOSAL)?;
    let expected = expected_proposal_bytes(root)?;
    let actual = fs::read(&path)?;
    if actual != expected || safe::sha256_bytes(&actual) != SQL_SHA256 {
        return Err(refused(
            "026 bundle differs from the fixed five-source proposal and approved digest",
        ));
    }
    let sql = String::from_utf8(actual).map_err(|_| refused("026 bundle is not UTF-8 SQL"))?;
    reject_embedded_transactions(&sql, "platform_upgrade_native_contracts_026")?;
    Ok(path)
}
fn require_predecessor(epoch: &manifest::EpochManifest) -> Result<()> {
    if epoch.baseline_id != BASELINE
        || epoch.epoch != PREDECESSOR_EPOCH
        || epoch.changes.len() != PREDECESSOR_EPOCH as usize
        || epoch
            .changes
            .iter()
            .enumerate()
            .any(|(i, c)| c.sequence != i as i32 + 1)
    {
        return Err(refused(
            "026 requires the exact complete activated epoch-25 predecessor",
        ));
    }
    Ok(())
}
/// Accept only the measured, activated025 source bundle named by the immutable pins.
pub fn validate_source(root: &Path) -> Result<Candidate> {
    let root = safe::canonical_directory(root, "026 source must be a nonsymlink directory")?;
    require_proposal(&root)?;
    next_successor_025::verify_activated_source(&root)?;
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
    if package_lock_sha256 != APPROVED_025_LOCK_SHA256
        || predecessor_manifest_sha256 != APPROVED_025_MANIFEST_SHA256
    {
        return Err(refused(
            "026 source differs from the exact activated025 pins",
        ));
    }
    let predecessor_measurement_sha256 = manifest::file_sha256(&safe::source_file(
        &root,
        &format!(
            "database/{}",
            epoch_relative("SUCCESSOR_025_MEASUREMENT.json")
        ),
    )?)?;
    let predecessor_activation_sha256 = manifest::file_sha256(&safe::source_file(
        &root,
        &format!(
            "database/{}",
            epoch_relative("SUCCESSOR_025_ACTIVATION.json")
        ),
    )?)?;
    for name in [SQL, REFERENCE, MEASUREMENT, ACTIVATION] {
        match fs::symlink_metadata(epoch_file(&root, name)) {
            Ok(_) => {
                return Err(refused(
                    "026 candidate is already present in its predecessor source",
                ));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    let mut manifest_value = source_json(&root, MANIFEST)?;
    let spec = change();
    manifest_value["epoch"] = json!(CANDIDATE_EPOCH);
    manifest_value["changes"]
        .as_array_mut()
        .ok_or_else(|| refused("026 predecessor manifest has no change list"))?
        .push(serde_json::to_value(&spec).map_err(|_| refused("026 change serialization failed"))?);
    let proposal_inputs = PROPOSAL_INPUTS
        .iter()
        .map(|input| {
            json!({
                "label": input.label,
                "path": input.path,
                "sha256": input.sha256,
                "description": input.description
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
        "proposal": PROPOSAL,
        "proposal_sha256": SQL_SHA256,
        "dependency_order": [
            "price-book-v2",
            "runtime-cache-settlement-v2",
            "local-node-owner-emergency-stop-pop4",
            "knowledge-vector-mutation-jobs"
        ],
        "proposal_inputs": proposal_inputs,
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
        proposal_sha256: SQL_SHA256.into(),
        predecessor_measurement_sha256,
        predecessor_activation_sha256,
    })
}
fn external_destination(source: &Path, preview: Option<&Path>, output: &Path) -> Result<PathBuf> {
    let absolute = std::path::absolute(output)?;
    match fs::symlink_metadata(&absolute) {
        Ok(_) => return Err(refused("026 output must be new")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("026 output parent is missing"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("026 output name is missing"))?,
    );
    if absolute != resolved
        || absolute.starts_with(source)
        || source.starts_with(&absolute)
        || preview.is_some_and(|p| absolute.starts_with(p) || p.starts_with(&absolute))
    {
        return Err(refused(
            "026 output must be external and disjoint from source/preview",
        ));
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
    json!({"schema":"ai-platform/canonical-fixed-successor-receipt/v1","status":status,"baseline_id":BASELINE,"predecessor_epoch":PREDECESSOR_EPOCH,"candidate_epoch":CANDIDATE_EPOCH,"database_actions":status=="measured_external_preview","source_activation":status=="activated_source_chain","source_plan_sha256":candidate.plan_sha256,"approved_predecessor_package_lock_sha256":candidate.package_lock_sha256,"predecessor_manifest_sha256":candidate.plan["predecessor_manifest_sha256"],"candidate_manifest_sha256":manifest_sha,"baseline_manifest_sha256":candidate.baseline_sha256,"candidate_sql_sha256":SQL_SHA256,"predecessor_measurement_sha256":candidate.predecessor_measurement_sha256,"predecessor_activation_sha256":candidate.predecessor_activation_sha256,"expected_database":database,"verification_ref_sha256":reference,"measurement_receipt_sha256":measurement,"post_migration_noop":status!="inactive_external_preview_only"})
}
fn inventory(candidate: &Candidate, measured: bool, activated: bool) -> Result<BTreeSet<String>> {
    let mut paths = lock_files(&candidate.package_lock)?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    paths.insert(epoch_relative(SQL));
    if measured || activated {
        paths.insert(epoch_relative(REFERENCE));
        paths.insert(epoch_relative(MEASUREMENT));
    }
    if activated {
        paths.insert(epoch_relative(ACTIVATION));
    }
    Ok(paths)
}
fn verify_inputs(
    root: &Path,
    candidate: &Candidate,
    measured: bool,
    activated: bool,
) -> Result<()> {
    if safe::collect_distribution_inputs(&root.join("database"))?
        != inventory(candidate, measured, activated)?
    {
        return Err(refused(
            "026 source contains missing or unexpected authority inputs",
        ));
    }
    for (relative, expected) in lock_files(&candidate.package_lock)? {
        if relative == &epoch_relative("manifest.yml") {
            continue;
        }
        let file = safe::source_file(root, &format!("database/{relative}"))?;
        if manifest::file_sha256(&file)?
            != expected
                .as_str()
                .ok_or_else(|| refused("026 invalid predecessor digest"))?
        {
            return Err(refused("026 changed a predecessor source input"));
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
            "026 is not the exact single reviewed source extension",
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
            .map_err(|_| refused("026 invalid preview name"))?;
        if kind.is_symlink()
            || !matches!(name.as_str(), "database" | MARKER)
            || (name == "database") != kind.is_dir()
            || (name == MARKER && !kind.is_file())
        {
            return Err(refused("026 preview has unexpected or unsafe entries"));
        }
        names.insert(name);
    }
    if names != BTreeSet::from(["database".into(), MARKER.into()]) {
        return Err(refused("026 preview inventory is incomplete"));
    }
    Ok(())
}
pub fn materialize(source: &Path, output: &Path) -> Result<Value> {
    let source = safe::canonical_directory(source, "026 source must be a nonsymlink directory")?;
    let candidate = validate_source(&source)?;
    let output = external_destination(&source, None, output)?;
    fs::create_dir(&output)?;
    safe::copy_tree(&source.join("database"), &output.join("database"))?;
    safe::write_new_bytes(
        &epoch_file(&output, SQL),
        &fs::read(require_proposal(&source)?)?,
    )?;
    fs::write(
        output.join(MANIFEST),
        format!(
            "{}\n",
            serde_json::to_string_pretty(&candidate.manifest_value)
                .map_err(|_| refused("026 manifest serialization failed"))?
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
/// Preview verification always revalidates the actual activated025 predecessor.
/// A measured preview is a distinct stage; it cannot reenter measurement.
pub fn verify_preview(source: &Path, preview: &Path, measured: bool) -> Result<Candidate> {
    let source = safe::canonical_directory(source, "026 source must be a nonsymlink directory")?;
    let preview = safe::canonical_directory(preview, "026 preview must be a nonsymlink directory")?;
    if preview.starts_with(&source) || source.starts_with(&preview) {
        return Err(refused("026 preview must be external"));
    }
    verify_preview_root(&preview)?;
    let candidate = validate_source(&source)?;
    verify_inputs(&preview, &candidate, measured, false)?;
    if manifest::file_sha256(&safe::source_file(&preview, LOCK)?)? != candidate.package_lock_sha256
    {
        return Err(refused("026 preview modified its predecessor package lock"));
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
            "026 preview marker is not bound to the reviewed candidate",
        ));
    }
    if measured {
        verify_measured_files(&source, &preview, &candidate)?;
    }
    Ok(candidate)
}

#[cfg(test)]
#[path = "next_successor_026_tests.rs"]
mod tests;
