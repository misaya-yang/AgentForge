//! Fixed 024 -> 025 metadata-only successor for the Gateway turn-persistence function.
//! No function body, argument, owner, ACL, role, table privilege, or caller fingerprint
//! is supplied by the caller. Measurement uses the native locked one-step runner.
use super::{measure_successors, next_successor as safe, next_successor_024};
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

pub const BASELINE: &str = "2026_08_post_kb_v1";
pub const PREDECESSOR_EPOCH: i32 = 24;
pub const CANDIDATE_EPOCH: i32 = 25;
/// Exact, activated024 source pins supplied by the integration owner.
pub const APPROVED_024_LOCK_SHA256: &str =
    "624e5b9b833bcc1d5667fb93bb43c424a7c4d14f14171d4822e7ebbd22f50643";
pub const APPROVED_024_MANIFEST_SHA256: &str =
    "5f71e17c48675c8f86c300afff8f12a46386c1f63a486a95a9101a2d2646e17f";
/// Hash of the unchanged `prosrc` body from the frozen canonical baseline.
pub const ISSUE_FUNCTION_BODY_MD5: &str = "159e51eac39d08f2dc99a046212422c9";
pub const ISSUE_FUNCTION_BODY_SHA256: &str =
    "c80c3483ca744c93f2b5f0f9533cf28528764768b638688084121a9d1c3098b7";

pub const PROPOSAL: &str =
    "docs/design/assistant-runtime-turn-persistence-security-definer.sql.proposal";
pub const SQL_SHA256: &str = "ce6c2c8f0894c976107037dd2da2d7d59d2ef6af0c129a694e25cbb12602cc5d";
const SQL: &str = "025_assistant_runtime_turn_security_definer.sql";
const REFERENCE: &str = "025_verification.json";
const MEASUREMENT: &str = "SUCCESSOR_025_MEASUREMENT.json";
const ACTIVATION: &str = "SUCCESSOR_025_ACTIVATION.json";
const MARKER: &str = "PREVIEW_025_NOT_ACTIVATED.json";
const LOCK: &str = "database/authority/package-authority-lock.json";
const MANIFEST: &str = "database/migrations/2026_08_post_kb_v1/manifest.yml";
const ARCHIVED_LOCK: &str =
    "docs/design/canonical-schema-successor-025-approved24-package-lock.json";
const ARCHIVED_MANIFEST: &str =
    "docs/design/canonical-schema-successor-025-approved24-manifest.yml";
const ACL_DELTA: &str = "schema_acl:public:migrator:USAGE:grantopt=0";
const TOOL: &str = "rust/platform-native/crates/platform-migration-authority/src/source_tools/next_successor_025.rs";
const PROVENANCE: &str = "Explicit PostgreSQL 16 measurement of only the 024 -> 025 SECURITY DEFINER and fixed search_path metadata for assistant.issue_assistant_runtime_turn against the exact activated epoch-24 source chain; function body, identity, owner, ACL, roles and other fingerprints are preserved.";
const ISSUE_FUNCTION_REGPROCEDURE: &str = "assistant.issue_assistant_runtime_turn(uuid, uuid, uuid, uuid, character varying, character varying, character varying, character varying, character varying, jsonb, character, bigint, character varying, character varying, character varying, character varying, character varying, character, integer, bigint, bigint, bigint, timestamp with time zone, text)";

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
        .map_err(|_| refused("025 malformed source JSON"))
}
fn lock_files(lock: &Value) -> Result<&serde_json::Map<String, Value>> {
    if lock["schema"] != "migration-authority/source-distribution/v1"
        || lock["baseline_id"] != BASELINE
    {
        return Err(refused("025 predecessor lock identity is invalid"));
    }
    lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("025 predecessor lock has no inventory"))
}
fn digest(value: &Value) -> Result<String> {
    Ok(safe::sha256_bytes(
        &serde_json::to_vec(value).map_err(|_| refused("025 policy serialization failed"))?,
    ))
}
fn require_proposal(root: &Path) -> Result<PathBuf> {
    let path = safe::source_file(root, PROPOSAL)?;
    if manifest::file_sha256(&path)? != SQL_SHA256 {
        return Err(refused(
            "025 SQL differs from the exact reviewed metadata-only proposal",
        ));
    }
    reject_embedded_transactions(
        &fs::read_to_string(&path)?,
        "assistant_runtime_turn_security_definer",
    )?;
    Ok(path)
}
fn function_acl_check() -> String {
    format!(
        "(SELECT count(*)=2 AND bool_and(a.privilege_type='EXECUTE' AND NOT a.is_grantable) AND bool_or(pg_get_userbyid(a.grantee)=current_user) AND bool_or(pg_get_userbyid(a.grantee)=(left(current_user,length(current_user)-5)||'gateway')) FROM aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a)"
    )
}
fn owner_is_nologin() -> &'static str {
    "EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.oid=p.proowner AND NOT r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreaterole AND NOT r.rolcreatedb AND NOT r.rolreplication AND NOT r.rolbypassrls)"
}
fn change() -> manifest::ChangeSpec {
    let signature = format!("to_regprocedure('{ISSUE_FUNCTION_REGPROCEDURE}')");
    let acl_check = function_acl_check();
    let owner_check = owner_is_nologin();
    manifest::ChangeSpec {
        sequence: CANDIDATE_EPOCH,
        name: "assistant_runtime_turn_security_definer".into(),
        file: SQL.into(),
        sha256: SQL_SHA256.into(),
        owner: "owner".into(),
        transaction_mode: manifest::TransactionMode::Transactional,
        rollback_class: manifest::RollbackClass::ForwardFixOnly,
        preconditions: vec![
            format!("SELECT count(*)=1 AND bool_and(p.prokind='f' AND p.pronargs=24 AND p.prorettype='void'::regtype AND NOT p.prosecdef AND p.proconfig IS NULL AND pg_catalog.md5(p.prosrc)='{ISSUE_FUNCTION_BODY_MD5}' AND p.proowner=(SELECT nspowner FROM pg_catalog.pg_namespace WHERE nspname='assistant') AND pg_get_userbyid(p.proowner)=current_user AND {owner_check} AND {acl_check}) FROM pg_catalog.pg_proc p WHERE p.oid={signature}"),
        ],
        postconditions: vec![
            format!("SELECT count(*)=1 AND bool_and(p.prokind='f' AND p.pronargs=24 AND p.prorettype='void'::regtype AND p.prosecdef AND p.proconfig=ARRAY['search_path=pg_catalog, assistant, gateway, pg_temp']::text[] AND pg_catalog.md5(p.prosrc)='{ISSUE_FUNCTION_BODY_MD5}' AND p.proowner=(SELECT nspowner FROM pg_catalog.pg_namespace WHERE nspname='assistant') AND pg_get_userbyid(p.proowner)=current_user AND {owner_check}) FROM pg_catalog.pg_proc p WHERE p.oid={signature}"),
            format!("SELECT count(*)=2 AND bool_and(a.privilege_type='EXECUTE' AND NOT a.is_grantable AND a.grantor=p.proowner) AND bool_or(pg_get_userbyid(a.grantee)=current_user) AND bool_or(pg_get_userbyid(a.grantee)=(left(current_user,length(current_user)-5)||'gateway')) FROM pg_catalog.pg_proc p CROSS JOIN LATERAL aclexplode(COALESCE(p.proacl,acldefault('f',p.proowner))) a WHERE p.oid={signature}"),
        ],
        timeout_seconds: 30,
        lock_budget_seconds: 5,
        resume_handler: None,
        repair_handler: None,
        notes: "Change only SECURITY DEFINER and the fixed pg_catalog,assistant,gateway,pg_temp search_path for the existing turn-persistence function. Preserve prosrc, signature, NOLOGIN owner and exact owner/gateway EXECUTE ACL; add no grants or role changes.".into(),
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
            .any(|(i, c)| c.sequence != i as i32 + 1)
    {
        return Err(refused(
            "025 requires the exact complete activated epoch-24 predecessor",
        ));
    }
    Ok(())
}
/// Accept only the measured, activated024 source bundle named by the immutable pins.
pub fn validate_source(root: &Path) -> Result<Candidate> {
    let root = safe::canonical_directory(root, "025 source must be a nonsymlink directory")?;
    require_proposal(&root)?;
    next_successor_024::verify_activated_source(&root)?;
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
    if package_lock_sha256 != APPROVED_024_LOCK_SHA256
        || predecessor_manifest_sha256 != APPROVED_024_MANIFEST_SHA256
    {
        return Err(refused(
            "025 source differs from the exact activated024 pins",
        ));
    }
    let predecessor_measurement_sha256 = manifest::file_sha256(&safe::source_file(
        &root,
        &format!(
            "database/{}",
            epoch_relative("SUCCESSOR_024_MEASUREMENT.json")
        ),
    )?)?;
    let predecessor_activation_sha256 = manifest::file_sha256(&safe::source_file(
        &root,
        &format!(
            "database/{}",
            epoch_relative("SUCCESSOR_024_ACTIVATION.json")
        ),
    )?)?;
    for name in [SQL, REFERENCE, MEASUREMENT, ACTIVATION] {
        match fs::symlink_metadata(epoch_file(&root, name)) {
            Ok(_) => {
                return Err(refused(
                    "025 candidate is already present in its predecessor source",
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
        .ok_or_else(|| refused("025 predecessor manifest has no change list"))?
        .push(serde_json::to_value(&spec).map_err(|_| refused("025 change serialization failed"))?);
    let plan = json!({"schema_version":"ai-platform/canonical-fixed-successor-policy/v1","baseline_id":BASELINE,"predecessor_epoch":PREDECESSOR_EPOCH,"candidate_epoch":CANDIDATE_EPOCH,"predecessor_manifest_sha256":predecessor_manifest_sha256,"approved_predecessor_package_lock_sha256":package_lock_sha256,"baseline_manifest_sha256":baseline_sha256,"predecessor_measurement_sha256":predecessor_measurement_sha256,"predecessor_activation_sha256":predecessor_activation_sha256,"proposal":PROPOSAL,"successor":{"change":spec},"database_actions_authorized":false,"fingerprint_status":"not_measured"});
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
        Ok(_) => return Err(refused("025 output must be new")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
        Err(e) => return Err(e.into()),
    }
    let parent = absolute
        .parent()
        .ok_or_else(|| refused("025 output parent is missing"))?
        .canonicalize()?;
    let resolved = parent.join(
        absolute
            .file_name()
            .ok_or_else(|| refused("025 output name is missing"))?,
    );
    if absolute != resolved
        || absolute.starts_with(source)
        || source.starts_with(&absolute)
        || preview.is_some_and(|p| absolute.starts_with(p) || p.starts_with(&absolute))
    {
        return Err(refused(
            "025 output must be external and disjoint from source/preview",
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
            "025 source contains missing or unexpected authority inputs",
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
                .ok_or_else(|| refused("025 invalid predecessor digest"))?
        {
            return Err(refused("025 changed a predecessor source input"));
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
            "025 is not the exact single reviewed source extension",
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
            .map_err(|_| refused("025 invalid preview name"))?;
        if kind.is_symlink()
            || !matches!(name.as_str(), "database" | MARKER)
            || (name == "database") != kind.is_dir()
            || (name == MARKER && !kind.is_file())
        {
            return Err(refused("025 preview has unexpected or unsafe entries"));
        }
        names.insert(name);
    }
    if names != BTreeSet::from(["database".into(), MARKER.into()]) {
        return Err(refused("025 preview inventory is incomplete"));
    }
    Ok(())
}
pub fn materialize(source: &Path, output: &Path) -> Result<Value> {
    let source = safe::canonical_directory(source, "025 source must be a nonsymlink directory")?;
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
                .map_err(|_| refused("025 manifest serialization failed"))?
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
/// Preview verification always revalidates the actual activated024 predecessor.
/// A measured preview is a distinct stage; it cannot reenter measurement.
pub fn verify_preview(source: &Path, preview: &Path, measured: bool) -> Result<Candidate> {
    let source = safe::canonical_directory(source, "025 source must be a nonsymlink directory")?;
    let preview = safe::canonical_directory(preview, "025 preview must be a nonsymlink directory")?;
    if preview.starts_with(&source) || source.starts_with(&preview) {
        return Err(refused("025 preview must be external"));
    }
    verify_preview_root(&preview)?;
    let candidate = validate_source(&source)?;
    verify_inputs(&preview, &candidate, measured, false)?;
    if manifest::file_sha256(&safe::source_file(&preview, LOCK)?)? != candidate.package_lock_sha256
    {
        return Err(refused("025 preview modified its predecessor package lock"));
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
            "025 preview marker is not bound to the reviewed candidate",
        ));
    }
    if measured {
        verify_measured_files(&source, &preview, &candidate)?;
    }
    Ok(candidate)
}

fn reference_value(
    candidate: &Candidate,
    evidence: &measure_successors::SingleStepEvidence,
) -> Value {
    let mut fingerprints = evidence.raw_fingerprints.clone();
    fingerprints.insert("acl".into(), evidence.base_acl_sha256.clone());
    let changes = evidence
        .applied
        .iter()
        .map(|(sequence, checksum)| (sequence.to_string(), json!(checksum)))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "baseline_id": BASELINE,
        "sequence": CANDIDATE_EPOCH,
        "baseline_manifest_sha256": candidate.baseline_sha256,
        "changes": changes,
        "fingerprints": fingerprints,
        "raw_fingerprints": evidence.raw_fingerprints,
        "acl_compatibility": {
            "revision": crate::ledger_acl::REVISION,
            "manifest_sha256": crate::ledger_acl::MANIFEST_SHA256,
            "delta_line": ACL_DELTA,
            "base_acl_sha256": evidence.base_acl_sha256,
            "effective_acl_sha256": evidence.raw_acl_sha256,
            "removed_delta_count": 1,
            "raw_acl_lines": evidence.raw_acl_lines,
            "base_acl_lines": evidence.base_acl_lines
        },
        "provenance": PROVENANCE,
        "provenance_details": {
            "tool": TOOL,
            "predecessor_epoch": PREDECESSOR_EPOCH,
            "approved_predecessor_package_lock_sha256": candidate.package_lock_sha256,
            "predecessor_manifest_sha256": candidate.plan["predecessor_manifest_sha256"],
            "source_plan_sha256": candidate.plan_sha256,
            "candidate_sql_sha256": SQL_SHA256,
            "predecessor_measurement_receipt_sha256": candidate.predecessor_measurement_sha256,
            "predecessor_activation_receipt_sha256": candidate.predecessor_activation_sha256,
            "git_baseline_provenance_verified": false
        }
    })
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn verify_unchanged_fingerprints(previous: &Value, current: &Value) -> Result<()> {
    let previous_raw_acl_lines: Vec<String> =
        serde_json::from_value(previous["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("025 predecessor raw ACL lines are malformed"))?;
    let current_raw_acl_lines: Vec<String> =
        serde_json::from_value(current["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("025 current raw ACL lines are malformed"))?;
    let previous_base_acl_lines: Vec<String> =
        serde_json::from_value(previous["acl_compatibility"]["base_acl_lines"].clone())
            .map_err(|_| refused("025 predecessor base ACL lines are malformed"))?;
    let current_base_acl_lines: Vec<String> =
        serde_json::from_value(current["acl_compatibility"]["base_acl_lines"].clone())
            .map_err(|_| refused("025 current base ACL lines are malformed"))?;
    if current_raw_acl_lines != previous_raw_acl_lines
        || current_base_acl_lines != previous_base_acl_lines
        || current["acl_compatibility"]["delta_line"] != previous["acl_compatibility"]["delta_line"]
        || current["acl_compatibility"]["revision"] != previous["acl_compatibility"]["revision"]
        || current["acl_compatibility"]["manifest_sha256"]
            != previous["acl_compatibility"]["manifest_sha256"]
        || current["acl_compatibility"]["base_acl_sha256"]
            != previous["acl_compatibility"]["base_acl_sha256"]
        || current["acl_compatibility"]["effective_acl_sha256"]
            != previous["acl_compatibility"]["effective_acl_sha256"]
        || current["raw_fingerprints"]["acl"] != previous["raw_fingerprints"]["acl"]
        || current["fingerprints"]["acl"] != previous["fingerprints"]["acl"]
    {
        return Err(refused("025 changed the measured ACL or role inventory"));
    }
    for key in ["extensions", "reference_data"] {
        if current["raw_fingerprints"][key] != previous["raw_fingerprints"][key]
            || current["fingerprints"][key] != previous["fingerprints"][key]
        {
            return Err(refused("025 changed an unrelated canonical fingerprint"));
        }
    }
    Ok(())
}

fn verify_reference_value(source: &Path, candidate: &Candidate, reference: &Value) -> Result<()> {
    let raw: BTreeMap<String, String> =
        serde_json::from_value(reference["raw_fingerprints"].clone())
            .map_err(|_| refused("025 raw fingerprints are malformed"))?;
    if raw.keys().ne(candidate.baseline.fingerprints().keys())
        || raw.values().any(|hash| !valid_hash(hash))
    {
        return Err(refused(
            "025 raw fingerprints must contain all four exact hash classes",
        ));
    }
    let lines: Vec<String> =
        serde_json::from_value(reference["acl_compatibility"]["raw_acl_lines"].clone())
            .map_err(|_| refused("025 raw ACL evidence is missing"))?;
    let (base_acl_sha256, raw_acl_sha256, base_acl_lines) =
        measure_successors::acl_reference(&raw, &lines)?;
    let applied = candidate
        .epoch
        .changes
        .iter()
        .map(|change| (change.sequence, change.sha256.clone()))
        .collect();
    let evidence = measure_successors::SingleStepEvidence {
        applied,
        raw_fingerprints: raw.clone(),
        base_acl_sha256,
        raw_acl_sha256,
        raw_acl_lines: lines.clone(),
        base_acl_lines: base_acl_lines.clone(),
        partition_catalog: None,
    };
    if reference != &reference_value(candidate, &evidence) {
        return Err(refused(
            "025 reference does not bind the complete prefix, native fingerprints and exact source provenance",
        ));
    }
    let previous_path = safe::source_file(
        source,
        &format!("database/{}", epoch_relative("024_verification.json")),
    )?;
    if lock_files(&candidate.package_lock)?.get(&epoch_relative("024_verification.json"))
        != Some(&json!(manifest::file_sha256(&previous_path)?))
    {
        return Err(refused(
            "025 predecessor reference differs from its activated024 lock",
        ));
    }
    let previous: Value = serde_json::from_slice(&fs::read(previous_path)?)
        .map_err(|_| refused("025 predecessor reference is malformed"))?;
    verify_unchanged_fingerprints(&previous, reference)?;
    if reference["raw_fingerprints"]["structural"] == previous["raw_fingerprints"]["structural"] {
        return Err(refused(
            "025 did not measure the target function security metadata change",
        ));
    }
    Ok(())
}
fn verify_measured_files(source: &Path, preview: &Path, candidate: &Candidate) -> Result<Value> {
    let reference = source_json(preview, &format!("database/{}", epoch_relative(REFERENCE)))?;
    verify_reference_value(source, candidate, &reference)?;
    let receipt = source_json(
        preview,
        &format!("database/{}", epoch_relative(MEASUREMENT)),
    )?;
    let database = receipt["expected_database"]
        .as_str()
        .ok_or_else(|| refused("025 measurement lacks its named isolated database"))?;
    measure_successors::validate_database_name(database)?;
    let reference_sha = manifest::file_sha256(&epoch_file(preview, REFERENCE))?;
    if receipt
        != common_receipt(
            candidate,
            &manifest::file_sha256(&preview.join(MANIFEST))?,
            "measured_external_preview",
            Some(database),
            Some(&reference_sha),
            None,
        )
    {
        return Err(refused(
            "025 measurement receipt is not bound to the exact preview",
        ));
    }
    Ok(receipt)
}
#[derive(Clone, Debug)]
pub struct MeasureOptions {
    pub source_root: PathBuf,
    pub preview_root: PathBuf,
    pub expected_database: String,
    pub run_approved: bool,
}
pub async fn measure(options: MeasureOptions) -> Result<()> {
    if !options.run_approved {
        return Err(refused(
            "025 measurement requires --run-approved-isolated-measurements",
        ));
    }
    measure_successors::validate_database_name(&options.expected_database)?;
    let candidate = verify_preview(&options.source_root, &options.preview_root, false)?;
    let source = safe::canonical_directory(
        &options.source_root,
        "025 source must be a nonsymlink directory",
    )?;
    let preview = safe::canonical_directory(
        &options.preview_root,
        "025 preview must be a nonsymlink directory",
    )?;
    measure_successors::measure_single_step(
        &preview.join("database"),
        &options.expected_database,
        PREDECESSOR_EPOCH,
        CANDIDATE_EPOCH,
        &candidate.baseline,
        &candidate.baseline_sha256,
        &candidate.epoch,
        options.run_approved,
        |evidence| {
            let reference = reference_value(&candidate, evidence);
            verify_reference_value(&source, &candidate, &reference)?;
            safe::write_reference(&epoch_file(&preview, REFERENCE), &reference)
        },
    )
    .await?;
    let rechecked = validate_source(&source)?;
    if rechecked.plan_sha256 != candidate.plan_sha256 {
        return Err(refused("025 predecessor source changed during measurement"));
    }
    let reference_sha = manifest::file_sha256(&epoch_file(&preview, REFERENCE))?;
    let receipt = common_receipt(
        &candidate,
        &manifest::file_sha256(&preview.join(MANIFEST))?,
        "measured_external_preview",
        Some(&options.expected_database),
        Some(&reference_sha),
        None,
    );
    safe::write_reference(&epoch_file(&preview, MEASUREMENT), &receipt)?;
    verify_preview(&source, &preview, true)?;
    Ok(())
}
fn write_relative(root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let path = root.join(relative);
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| refused("025 invalid bundle path"))?,
    )?;
    safe::write_new_bytes(&path, bytes)
}
/// Self-contained source overlay plus the exact activated024 archives. No canonical
/// file, distribution release pin or database is changed by bundle preparation.
pub fn activation_bundle(source: &Path, preview: &Path, output: &Path) -> Result<Value> {
    let source = safe::canonical_directory(source, "025 source must be a nonsymlink directory")?;
    let preview = safe::canonical_directory(preview, "025 preview must be a nonsymlink directory")?;
    let candidate = verify_preview(&source, &preview, true)?;
    let output = external_destination(&source, Some(&preview), output)?;
    let measurement = verify_measured_files(&source, &preview, &candidate)?;
    let manifest_sha = manifest::file_sha256(&preview.join(MANIFEST))?;
    let reference_sha = manifest::file_sha256(&epoch_file(&preview, REFERENCE))?;
    let measurement_sha = manifest::file_sha256(&epoch_file(&preview, MEASUREMENT))?;
    let database = measurement["expected_database"]
        .as_str()
        .ok_or_else(|| refused("025 measurement has no database identity"))?;
    let activation = common_receipt(
        &candidate,
        &manifest_sha,
        "activated_source_chain",
        Some(database),
        Some(&reference_sha),
        Some(&measurement_sha),
    );
    let mut activation_bytes = serde_json::to_vec_pretty(&activation)
        .map_err(|_| refused("025 activation serialization failed"))?;
    activation_bytes.push(b'\n');
    let mut package_lock = candidate.package_lock.clone();
    let files = package_lock["files_sha256"]
        .as_object_mut()
        .ok_or_else(|| refused("025 invalid predecessor lock"))?;
    for (name, hash) in [
        ("manifest.yml", manifest_sha.clone()),
        (SQL, SQL_SHA256.into()),
        (REFERENCE, reference_sha),
        (MEASUREMENT, measurement_sha),
        (ACTIVATION, safe::sha256_bytes(&activation_bytes)),
    ] {
        files.insert(epoch_relative(name), json!(hash));
    }
    let mut lock_bytes = serde_json::to_vec_pretty(&package_lock)
        .map_err(|_| refused("025 package lock serialization failed"))?;
    lock_bytes.push(b'\n');
    fs::create_dir(&output)?;
    let mut overlays = BTreeMap::new();
    for relative in [
        MANIFEST.to_owned(),
        format!("database/{}", epoch_relative(SQL)),
        format!("database/{}", epoch_relative(REFERENCE)),
        format!("database/{}", epoch_relative(MEASUREMENT)),
    ] {
        let bytes = fs::read(safe::source_file(&preview, &relative)?)?;
        write_relative(&output, &relative, &bytes)?;
        overlays.insert(relative, safe::sha256_bytes(&bytes));
    }
    for (relative, bytes) in [
        (
            format!("database/{}", epoch_relative(ACTIVATION)),
            activation_bytes,
        ),
        (LOCK.into(), lock_bytes),
        (
            ARCHIVED_LOCK.into(),
            fs::read(safe::source_file(&source, LOCK)?)?,
        ),
        (
            ARCHIVED_MANIFEST.into(),
            fs::read(safe::source_file(&source, MANIFEST)?)?,
        ),
    ] {
        write_relative(&output, &relative, &bytes)?;
        overlays.insert(relative, safe::sha256_bytes(&bytes));
    }
    // Recheck approved source after copying, before declaring a reviewable bundle.
    if validate_source(&source)?.plan_sha256 != candidate.plan_sha256 {
        return Err(refused("025 source changed during bundle preparation"));
    }
    let summary = json!({"schema":"ai-platform/canonical-fixed-successor-bundle/v1","status":"ready_for_primary_activation","database_actions":false,"source_activation":false,"predecessor_epoch":PREDECESSOR_EPOCH,"candidate_epoch":CANDIDATE_EPOCH,"source_plan_sha256":candidate.plan_sha256,"compiled_distribution_lock_sha256_to_review":overlays[LOCK],"files_sha256":overlays});
    safe::write_new_json(&output.join("ACTIVATION_025_REVIEW.json"), &summary)?;
    Ok(summary)
}

/// Rebuild the exact archived024 source view in owned external scratch, then run
/// its full unchanged verifier. No invented receipt or fingerprint is substituted.
struct PredecessorView(PathBuf);
impl Drop for PredecessorView {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn predecessor_view(root: &Path) -> Result<PredecessorView> {
    let parent = std::env::temp_dir().canonicalize()?;
    let directory = parent.join(format!(
        "agentforge-025-predecessor-{}",
        uuid::Uuid::new_v4()
    ));
    fs::create_dir(&directory)?;
    let view = PredecessorView(directory);
    safe::copy_tree(&root.join("database"), &view.0.join("database"))?;
    fs::create_dir(view.0.join("docs"))?;
    safe::copy_tree(&root.join("docs/design"), &view.0.join("docs/design"))?;
    fs::write(
        view.0.join(LOCK),
        fs::read(safe::source_file(root, ARCHIVED_LOCK)?)?,
    )?;
    fs::write(
        view.0.join(MANIFEST),
        fs::read(safe::source_file(root, ARCHIVED_MANIFEST)?)?,
    )?;
    for name in [SQL, REFERENCE, MEASUREMENT, ACTIVATION] {
        fs::remove_file(epoch_file(&view.0, name))?;
    }
    Ok(view)
}
pub fn verify_activated_source(root: &Path) -> Result<Value> {
    let root = safe::canonical_directory(root, "025 source must be a nonsymlink directory")?;
    // Current release must explicitly pin25 and accept the real24 predecessor;
    // this module never edits or bypasses those independently approved pins.
    distribution::verify(&root.join("database"), BASELINE)?;
    let epoch = manifest::load_epoch_manifest(&root.join(MANIFEST))?;
    if epoch.epoch != CANDIDATE_EPOCH {
        return Err(refused("025 canonical source is not activated"));
    }
    let view = predecessor_view(&root)?;
    let candidate = validate_source(&view.0)?;
    verify_inputs(&root, &candidate, true, true)?;
    let active_lock = source_json(&root, LOCK)?;
    let files = lock_files(&active_lock)?;
    if files.keys().cloned().collect::<BTreeSet<_>>() != inventory(&candidate, true, true)? {
        return Err(refused("025 activated lock coverage is invalid"));
    }
    // distribution::verify binds every current byte. This also checks the exact
    // unchanged24 predecessor before trusting new source receipts below.
    for (key, hash) in lock_files(&candidate.package_lock)? {
        if key != &epoch_relative("manifest.yml") && files.get(key) != Some(hash) {
            return Err(refused("025 activated lock changed predecessor terms"));
        }
    }
    let measurement = verify_measured_files(&root, &root, &candidate)?;
    let activation = source_json(&root, &format!("database/{}", epoch_relative(ACTIVATION)))?;
    let reference_sha = manifest::file_sha256(&epoch_file(&root, REFERENCE))?;
    let measurement_sha = manifest::file_sha256(&epoch_file(&root, MEASUREMENT))?;
    let database = measurement["expected_database"]
        .as_str()
        .ok_or_else(|| refused("025 measurement database is missing"))?;
    if activation
        != common_receipt(
            &candidate,
            &manifest::file_sha256(&root.join(MANIFEST))?,
            "activated_source_chain",
            Some(database),
            Some(&reference_sha),
            Some(&measurement_sha),
        )
    {
        return Err(refused(
            "025 activation does not bind the measured complete source chain",
        ));
    }
    Ok(activation)
}

#[cfg(test)]
#[path = "next_successor_025_tests.rs"]
mod tests;
