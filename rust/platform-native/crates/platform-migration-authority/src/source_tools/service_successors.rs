//! A closed, source-owned029 ->030..035 chain, not an arbitrary SQL importer.
//! The only admission inputs are the fixed catalog and actual pinned proposal bytes.
use super::{measure_successors, next_successor as safe, next_successor_029};
use crate::{AuthorityError, Result, distribution, manifest, runner::AuthorityPaths};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};
#[path = "service_successors/activation.rs"]
mod activation;
#[path = "service_successors/measurement.rs"]
mod measurement;
#[path = "service_successors/operator_stage.rs"]
pub mod operator_stage;
#[path = "service_successors/partition_catalog.rs"]
pub(crate) mod partition_catalog;
pub use activation::{activation_bundle, verify_activated_source, verify_anchor_029};
pub use measurement::{MeasureOptions, measure};
const BASELINE: &str = "2026_08_post_kb_v1";
const LOCK: &str = "database/authority/package-authority-lock.json";
const MANIFEST: &str = "database/migrations/2026_08_post_kb_v1/manifest.yml";
const CATALOG: &str = "docs/design/proposals/service-successors/catalog.json";
pub const CATALOG_SHA256: &str = "141024ba9914bf78ec9b3796966fb270001d786204296253bb23c7a904559a2c";
const CATALOG_BYTES: &[u8] =
    include_bytes!("../../../../../../docs/design/proposals/service-successors/catalog.json");
const MARKER: &str = "SERVICE_SUCCESSOR_PREVIEW.json";
fn activation_name(n: i32) -> String {
    format!("SERVICE_SUCCESSOR_{n:03}_ACTIVATION.json")
}
const ARCHIVED_MANIFEST: &str =
    "docs/design/canonical-schema-service-successors-approved29-manifest.yml";
const ARCHIVED_LOCK: &str =
    "docs/design/canonical-schema-service-successors-approved29-package-lock.json";
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    occurrences: usize,
    from: String,
    to: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Literal {
    literal: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SourcePiece {
    source: String,
    sha256: String,
    edits: Vec<Edit>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(untagged)]
enum Piece {
    Literal(Literal),
    Source(SourcePiece),
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Step {
    sequence: i32,
    pieces: Vec<Piece>,
    sql_source: String,
    change: manifest::ChangeSpec,
    acl_additions: Vec<String>,
    compatibility_phase: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    auxiliary_inputs_sha256: BTreeMap<String, String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: String,
    anchor_epoch: i32,
    anchor_files_sha256: BTreeMap<String, String>,
    proposal_inputs_sha256: BTreeMap<String, String>,
    steps: Vec<Step>,
    admission_requirements: BTreeMap<String, String>,
    admission_minimum_epochs: BTreeMap<String, i32>,
    deferred: String,
}
fn refused(s: &str) -> AuthorityError {
    AuthorityError::Refused(format!("service successor: {s}"))
}
fn ep(name: &str) -> String {
    format!("migrations/{BASELINE}/{name}")
}
fn file(root: &Path, name: &str) -> PathBuf {
    root.join("database").join(ep(name))
}
fn reference_name(n: i32) -> String {
    format!("{n:03}_verification.json")
}
fn receipt_name(n: i32) -> String {
    format!("SERVICE_SUCCESSOR_{n:03}_MEASUREMENT.json")
}
fn digest(value: &Value) -> Result<String> {
    fn canonical(v: &Value) -> Value {
        match v {
            Value::Object(o) => Value::Object(
                o.iter()
                    .collect::<BTreeMap<_, _>>()
                    .into_iter()
                    .map(|(k, v)| (k.clone(), canonical(v)))
                    .collect(),
            ),
            Value::Array(a) => Value::Array(a.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    Ok(safe::sha256_bytes(
        &serde_json::to_vec(&canonical(value)).map_err(|_| refused("JSON encoding failed"))?,
    ))
}

fn bytes(value: &Value) -> Result<Vec<u8>> {
    let mut b = serde_json::to_vec_pretty(value).map_err(|_| refused("JSON encoding failed"))?;
    b.push(b'\n');
    Ok(b)
}
fn read_json(root: &Path, relative: &str) -> Result<Value> {
    serde_json::from_slice(&fs::read(safe::source_file(root, relative)?)?)
        .map_err(|_| refused("malformed source JSON"))
}
fn catalog() -> Result<Catalog> {
    if safe::sha256_bytes(CATALOG_BYTES) != CATALOG_SHA256 {
        return Err(refused("compiled catalog pin drift"));
    }
    let c: Catalog =
        serde_json::from_slice(CATALOG_BYTES).map_err(|_| refused("compiled catalog invalid"))?;
    if c.schema != "ai-platform/source-owned-service-successors/v1"
        || c.anchor_epoch != 29
        || c.steps.iter().map(|s| s.sequence).ne(30..=35)
    {
        return Err(refused("unreviewed catalog allocation"));
    }
    Ok(c)
}
fn step(c: &Catalog, n: i32) -> Result<&Step> {
    c.steps
        .iter()
        .find(|s| s.sequence == n)
        .ok_or_else(|| refused("only fixed030..035 are supported"))
}
/// Used by delivery lint to scope new grant templates to the complete source-owned spec.
pub fn recognizes_change(change: &manifest::ChangeSpec) -> bool {
    catalog()
        .ok()
        .and_then(|c| {
            step(&c, change.sequence)
                .ok()
                .map(|s| serde_json::to_value(&s.change).ok() == serde_json::to_value(change).ok())
        })
        .unwrap_or(false)
}
fn render(root: &Path, c: &Catalog, s: &Step) -> Result<Vec<u8>> {
    for (path, hash) in &s.auxiliary_inputs_sha256 {
        if c.proposal_inputs_sha256.get(path) != Some(hash)
            || manifest::file_sha256(&safe::source_file(root, path)?)? != *hash
        {
            return Err(refused("source-owned auxiliary input changed"));
        }
    }
    let mut result = String::new();
    for p in &s.pieces {
        match p {
            Piece::Literal(p) => result.push_str(&p.literal),
            Piece::Source(p) => {
                if c.proposal_inputs_sha256.get(&p.source) != Some(&p.sha256) {
                    return Err(refused("proposal is outside the catalog"));
                }
                let raw = fs::read(safe::source_file(root, &p.source)?)?;
                if safe::sha256_bytes(&raw) != p.sha256 {
                    return Err(refused("original proposal bytes changed"));
                }
                let mut text =
                    String::from_utf8(raw).map_err(|_| refused("proposal must be UTF-8"))?;
                for e in &p.edits {
                    if e.from.is_empty()
                        || e.occurrences == 0
                        || text.matches(e.from.as_str()).count() != e.occurrences
                    {
                        return Err(refused("deterministic transformation anchor differs"));
                    }
                    text = text.replace(e.from.as_str(), e.to.as_str());
                }
                result.push_str(&text);
                result.push('\n');
            }
        }
    }
    let data = result.into_bytes();
    if safe::sha256_bytes(&data) != s.change.sha256
        || data != fs::read(safe::source_file(root, &s.sql_source)?)?
    {
        return Err(refused(
            "generated SQL differs from fixed source reconstruction",
        ));
    }
    crate::sql::reject_embedded_transactions(
        std::str::from_utf8(&data).map_err(|_| refused("SQL encoding failed"))?,
        &s.change.name,
    )?;
    Ok(data)
}
/// Retain selected original proposal files outside docs/database as well (the
/// Knowledge input is crate-owned). Anchors must be self-contained and never
/// resolve missing pinned inputs through a mutable live checkout fallback.
fn copy_selected_inputs(from: &Path, to: &Path, n: i32) -> Result<()> {
    let c = catalog()?;
    let mut paths = BTreeSet::new();
    for step in c.steps.iter().filter(|s| s.sequence <= n) {
        for piece in &step.pieces {
            if let Piece::Source(p) = piece {
                paths.insert(p.source.as_str());
            }
        }
        paths.extend(step.auxiliary_inputs_sha256.keys().map(String::as_str));
    }
    for relative in paths {
        let raw = fs::read(safe::source_file(from, relative)?)?;
        let output = to.join(relative);
        if output.exists() {
            if fs::read(safe::source_file(to, relative)?)? != raw {
                return Err(refused("anchor proposal input collision"));
            }
        } else {
            fs::create_dir_all(
                output
                    .parent()
                    .ok_or_else(|| refused("anchor input path invalid"))?,
            )?;
            safe::write_new_bytes(&output, &raw)?;
        }
    }
    Ok(())
}
struct Source {
    root: PathBuf,
    input_root: PathBuf,
    input_epoch: i32,
    _anchor: Option<activation::Anchor>,
    catalog: Catalog,
    base_manifest: Value,
    base_lock: Value,
    baseline: manifest::BaselineManifest,
    baseline_sha: String,
}
fn validate(source: &Path, n: i32) -> Result<Source> {
    let input_root = safe::canonical_directory(source, "service source must be real")?;
    let input_epoch = manifest::load_epoch_manifest(&input_root.join(MANIFEST))?.epoch;
    let anchor = if input_epoch == 29 {
        None
    } else {
        verify_activated_source(&input_root)?;
        Some(activation::restore_anchor(&input_root, input_epoch)?)
    };
    let root = anchor
        .as_ref()
        .map(|a| a.0.clone())
        .unwrap_or_else(|| input_root.clone());
    let c = catalog()?;
    step(&c, n)?;
    let actual_bytes = fs::read(safe::source_file(&root, CATALOG)?)?;
    // The current complete allocation is an exact compiled input. Historical
    // prefixes compare only their selected scope so future additions cannot
    // invalidate a frozen earlier plan; there is no unselected future at the tip.
    if c.steps.last().is_some_and(|last| last.sequence == n)
        && actual_bytes.as_slice() != CATALOG_BYTES
    {
        return Err(refused("complete source catalog identity changed"));
    }
    let actual: Catalog =
        serde_json::from_slice(&actual_bytes).map_err(|_| refused("source catalog malformed"))?;
    if actual.schema != c.schema || actual.anchor_epoch != 29 || scope(&actual, n)? != scope(&c, n)?
    {
        return Err(refused("source catalog selected prefix changed"));
    }
    for (relative, sha) in &c.anchor_files_sha256 {
        if manifest::file_sha256(&safe::source_file(&root, relative)?)? != *sha {
            return Err(refused("exact measured029 anchor changed"));
        }
    }
    next_successor_029::verify_activated_source(&root)?;
    for s in c.steps.iter().filter(|s| s.sequence <= n) {
        render(&root, &c, s)?;
    }
    let paths = AuthorityPaths {
        database_dir: root.join("database"),
    };
    let (baseline, baseline_sha) = crate::commands::load_baseline_with_mode(
        &paths,
        BASELINE,
        distribution::SourceMode::Distribution,
    )?;
    Ok(Source {
        input_root,
        input_epoch,
        _anchor: anchor,
        base_manifest: read_json(&root, MANIFEST)?,
        base_lock: read_json(&root, LOCK)?,
        root,
        catalog: c,
        baseline,
        baseline_sha,
    })
}
fn manifest_value(source: &Source, n: i32) -> Result<Value> {
    step(&source.catalog, n)?;
    let mut value = source.base_manifest.clone();
    value["epoch"] = json!(n);
    for s in source.catalog.steps.iter().filter(|s| s.sequence <= n) {
        value["changes"]
            .as_array_mut()
            .ok_or_else(|| refused("anchor change list invalid"))?
            .push(serde_json::to_value(&s.change).map_err(|_| refused("change encoding failed"))?);
    }
    Ok(value)
}
fn manifest_sha(source: &Source, n: i32) -> Result<String> {
    Ok(safe::sha256_bytes(&bytes(&manifest_value(source, n)?)?))
}
fn scope(c: &Catalog, n: i32) -> Result<Value> {
    step(c, n)?;
    let steps = c
        .steps
        .iter()
        .filter(|s| s.sequence <= n)
        .collect::<Vec<_>>();
    let paths = steps
        .iter()
        .flat_map(|s| s.pieces.iter())
        .filter_map(|p| match p {
            Piece::Source(s) => Some(s.source.as_str()),
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    let mut paths = paths;
    for s in &steps {
        paths.extend(s.auxiliary_inputs_sha256.keys().map(String::as_str));
    }
    let inputs = c
        .proposal_inputs_sha256
        .iter()
        .filter(|(key, _)| paths.contains(key.as_str()))
        .collect::<BTreeMap<_, _>>();
    let requirements = c
        .admission_requirements
        .iter()
        .filter(|(key, _)| {
            c.admission_minimum_epochs
                .get(*key)
                .is_some_and(|min| *min <= n)
        })
        .collect::<BTreeMap<_, _>>();
    let epochs = c
        .admission_minimum_epochs
        .iter()
        .filter(|(_, min)| **min <= n)
        .collect::<BTreeMap<_, _>>();
    Ok(
        json!({"schema":"ai-platform/source-owned-service-prefix/v1","anchor_files_sha256":c.anchor_files_sha256,"steps":steps,"proposal_inputs_sha256":inputs,"admission_requirements":requirements,"admission_minimum_epochs":epochs}),
    )
}
fn scope_sha(n: i32) -> Result<String> {
    digest(&scope(&catalog()?, n)?)
}
fn plan(n: i32) -> Result<Value> {
    Ok(
        json!({"schema":"ai-platform/source-owned-service-step/v1","catalog_sha256":scope_sha(n)?,"anchor_epoch":29,"candidate_epoch":n,"predecessor_epoch":n-1}),
    )
}
fn marker(n: i32) -> Result<Value> {
    Ok(
        json!({"schema":"ai-platform/source-owned-service-preview/v1","candidate_epoch":n,"catalog_sha256":scope_sha(n)?,"source_activation":false,"database_actions":false,"post_migration_noop":Value::Null,"batch_submission_activated":false}),
    )
}
fn inventory(source: &Source, n: i32, measured: bool) -> Result<BTreeSet<String>> {
    let mut files = source.base_lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("anchor lock invalid"))?
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    for s in source.catalog.steps.iter().filter(|s| s.sequence <= n) {
        files.insert(ep(&s.change.file));
        if s.sequence < n || measured {
            files.insert(ep(&reference_name(s.sequence)));
            files.insert(ep(&receipt_name(s.sequence)));
        }
    }
    Ok(files)
}
fn verify_inputs(source: &Source, root: &Path, n: i32, measured: bool) -> Result<()> {
    if safe::collect_distribution_inputs(&root.join("database"))? != inventory(source, n, measured)?
    {
        return Err(refused("preview input coverage differs"));
    }
    if fs::read(safe::source_file(root, LOCK)?)? != fs::read(source.root.join(LOCK))? {
        return Err(refused("preview changed anchor lock"));
    }
    for (relative, sha) in source.base_lock["files_sha256"]
        .as_object()
        .ok_or_else(|| refused("anchor inventory invalid"))?
    {
        if relative == &ep("manifest.yml") {
            continue;
        }
        if manifest::file_sha256(&safe::source_file(root, &format!("database/{relative}"))?)?
            != sha.as_str().unwrap_or("")
        {
            return Err(refused("preview changed a frozen029 input"));
        }
    }
    if fs::read(safe::source_file(root, MANIFEST)?)? != bytes(&manifest_value(source, n)?)? {
        return Err(refused(
            "preview manifest is not the exact ordered extension",
        ));
    }
    for s in source.catalog.steps.iter().filter(|s| s.sequence <= n) {
        if fs::read(file(root, &s.change.file))? != render(&source.root, &source.catalog, s)? {
            return Err(refused("preview SQL drift"));
        }
    }
    let epoch = manifest::load_epoch_manifest(&root.join(MANIFEST))?;
    if epoch.epoch != n || epoch.changes.len() != n as usize {
        return Err(refused("incomplete preview prefix"));
    }
    measurement::verify_chain(source, root, n, measured)?;
    verify_existing(source, root)?;
    Ok(())
}
fn verify_existing(source: &Source, preview: &Path) -> Result<()> {
    for n in 30..=source.input_epoch {
        let spec = step(&source.catalog, n)?;
        for name in [spec.change.file.clone(), reference_name(n), receipt_name(n)] {
            if fs::read(safe::source_file(
                &source.input_root,
                &format!("database/{}", ep(&name)),
            )?)? != fs::read(safe::source_file(
                preview,
                &format!("database/{}", ep(&name)),
            )?)? {
                return Err(refused(
                    "measured previous preview differs from an already activated immutable prefix",
                ));
            }
        }
        if fs::read(file(&source.input_root, &activation_name(n)))?
            != bytes(&activation::activation_value(source, preview, n)?)?
        {
            return Err(refused("previous activation proof changed"));
        }
    }
    Ok(())
}
fn destination(source: &Path, previous: Option<&Path>, output: &Path) -> Result<PathBuf> {
    if fs::symlink_metadata(output).is_ok() {
        return Err(refused("output must be new"));
    }
    let parent = safe::canonical_directory(
        output
            .parent()
            .ok_or_else(|| refused("output parent missing"))?,
        "output parent must be real",
    )?;
    let name = output
        .file_name()
        .ok_or_else(|| refused("output name missing"))?;
    if name == "." || name == ".." {
        return Err(refused("output name invalid"));
    }
    let output = parent.join(name);
    for root in std::iter::once(source).chain(previous) {
        if output.starts_with(root) || root.starts_with(&output) {
            return Err(refused("output must be external and disjoint"));
        }
    }
    Ok(output)
}
pub fn validate_source(source: &Path, n: i32) -> Result<Value> {
    let s = validate(source, n)?;
    Ok(
        json!({"status":"fixed_service_source_verified","plan":plan(n)?,"source_plan_sha256":digest(&plan(n)?)?,"sql_sha256":step(&s.catalog,n)?.change.sha256,"database_actions":false,"source_activation":false,"deferred":s.catalog.deferred}),
    )
}
/// Current definitions for a single atomic empty-development-database install.
/// This validates the real fixed inputs, not the historical upgrade receipts.
#[cfg(test)]
pub(crate) fn fresh_definitions(root: &Path) -> Result<Vec<(manifest::ChangeSpec, String)>> {
    let c = catalog()?;
    if fs::read(safe::source_file(root, CATALOG)?)?.as_slice() != CATALOG_BYTES {
        return Err(refused("fresh source catalog identity changed"));
    }
    c.steps
        .iter()
        .map(|s| {
            let sql = String::from_utf8(render(root, &c, s)?)
                .map_err(|_| refused("fresh definition encoding invalid"))?;
            Ok((s.change.clone(), sql))
        })
        .collect()
}
pub fn prepare(source: &Path, previous: Option<&Path>, output: &Path, n: i32) -> Result<Value> {
    let s = validate(source, n)?;
    let previous = previous
        .map(|p| safe::canonical_directory(p, "previous preview must be real"))
        .transpose()?;
    let from = if n == 30 {
        if previous.is_some() {
            return Err(refused("030 has no external predecessor"));
        }
        &s.root
    } else {
        let p = previous
            .as_deref()
            .ok_or_else(|| refused("next stage needs its real measured previous preview"))?;
        verify_preview(source, p, n - 1, true)?;
        p
    };
    if n <= s.input_epoch {
        return Err(refused("requested successor is already active"));
    }
    if let Some(previous) = previous.as_deref() {
        verify_existing(&s, previous)?;
    }
    let output = destination(&s.input_root, previous.as_deref(), output)?;
    fs::create_dir(&output)?;
    safe::copy_tree(&from.join("database"), &output.join("database"))?;
    fs::write(output.join(MANIFEST), bytes(&manifest_value(&s, n)?)?)?;
    safe::write_new_bytes(
        &file(&output, &step(&s.catalog, n)?.change.file),
        &render(&s.root, &s.catalog, step(&s.catalog, n)?)?,
    )?;
    safe::write_new_json(&output.join(MARKER), &marker(n)?)?;
    safe::write_new_json(
        &output.join("schema_admission_expected.json"),
        &admission_snapshot(&s, n)?,
    )?;
    safe::write_new_json(
        &output.join("service_admission_requirements.json"),
        &admission_requirements(&s, n),
    )?;
    // Retain a complete real029 source anchor so030 may activate without waiting
    // for optional031/032; subsequent preparation uses this pinned source view.
    let anchor = output.join("anchor_source_029");
    fs::create_dir(&anchor)?;
    safe::copy_tree(&s.root.join("database"), &anchor.join("database"))?;
    fs::create_dir(anchor.join("docs"))?;
    safe::copy_tree(&s.root.join("docs/design"), &anchor.join("docs/design"))?;
    copy_selected_inputs(&s.root, &anchor, n)?;
    verify_preview(source, &output, n, false)?;
    marker(n)
}
fn admission_requirements(source: &Source, n: i32) -> Value {
    let requirements = source
        .catalog
        .admission_requirements
        .iter()
        .filter(|(key, _)| {
            source
                .catalog
                .admission_minimum_epochs
                .get(*key)
                .is_some_and(|min| *min <= n)
        })
        .collect::<BTreeMap<_, _>>();
    json!({"schema":"ai-platform/service-schema-requirements/v1","source_epoch":n,"requirements":requirements,"minimum_epochs":source.catalog.admission_minimum_epochs.iter().filter(|(name,min)| **min<=n || (name.as_str()=="local_node" && n<=32)).collect::<BTreeMap<_,_>>()})
}
fn admission_snapshot(source: &Source, n: i32) -> Result<Value> {
    let baseline = read_json(
        &source.root,
        &format!("database/baselines/{BASELINE}/manifest.json"),
    )?;
    let base_changes=source.base_manifest["changes"].as_array().ok_or_else(||refused("anchor changes invalid"))?.iter().map(|c|json!({"baseline_id":BASELINE,"sequence":c["sequence"],"name":c["name"],"checksum_sha256":c["sha256"]})).collect::<Vec<_>>();
    let mut value = json!({"schema_version":"ai-platform/schema-admission-snapshot/v1","baselines":[{"baseline_id":BASELINE,"manifest_sha256":source.baseline_sha,"structural_sha256":baseline["structural_sha256"],"acl_sha256":baseline["acl_sha256"],"extensions_sha256":baseline["extensions_sha256"],"reference_data_sha256":baseline["reference_data_sha256"],"source_git_sha":baseline["source_git_sha"]}],"epoch":n,"changes":base_changes,"has_attempts":false,"has_incomplete_attempts":false});
    for s in source.catalog.steps.iter().filter(|s| s.sequence <= n) {
        value["changes"].as_array_mut().ok_or_else(||refused("admission prefix invalid"))?.push(json!({"baseline_id":BASELINE,"sequence":s.sequence,"name":s.change.name,"checksum_sha256":s.change.sha256}));
    }
    Ok(value)
}
pub fn verify_preview(source: &Path, preview: &Path, n: i32, measured: bool) -> Result<Value> {
    let s = validate(source, n)?;
    let root = safe::canonical_directory(preview, "preview must be a real directory")?;
    if root == s.input_root || root.starts_with(&s.input_root) {
        return Err(refused("preview must be external"));
    }
    verify_inputs(&s, &root, n, measured)?;
    validate(&root.join("anchor_source_029"), n)?;
    if read_json(&root, MARKER)? != marker(n)?
        || read_json(&root, "schema_admission_expected.json")? != admission_snapshot(&s, n)?
        || read_json(&root, "service_admission_requirements.json")? != admission_requirements(&s, n)
    {
        return Err(refused("preview metadata or admission export changed"));
    }
    let allowed = BTreeSet::from([
        "database",
        "anchor_source_029",
        MARKER,
        "schema_admission_expected.json",
        "service_admission_requirements.json",
    ]);
    for e in fs::read_dir(&root)? {
        let e = e?;
        let name = e.file_name();
        if !allowed.contains(name.to_str().unwrap_or("")) || e.file_type()?.is_symlink() {
            return Err(refused("unexpected preview entry"));
        }
    }
    Ok(
        json!({"status":if measured{"measured_service_preview_verified"}else{"inactive_service_preview_verified"},"candidate_epoch":n,"source_plan_sha256":digest(&plan(n)?)?,"database_actions":false}),
    )
}
#[cfg(test)]
#[path = "service_successors/tests.rs"]
mod tests;

/// Shared read-only admission contract; deployment preflight never invents app grants.
pub fn admission_checks(epoch: i32, principal: &str) -> Result<Vec<String>> {
    if epoch < 30 {
        return Ok(Vec::new());
    }
    let c = catalog()?;
    step(&c, epoch)?;
    let names: &[&str] = match principal {
        "gateway" => &[
            "gateway_sdk",
            "gateway_audit",
            "gateway_a2a",
            "local_node",
            "gateway_outbound",
            "partition_gateway",
        ],
        "runtime" => &["runtime", "partition_runtime"],
        "knowledge_worker" => &["knowledge_worker", "partition_knowledge_worker"],
        "knowledge_api" => &["knowledge_api", "partition_knowledge_api"],
        "capability_worker" => &["capability_worker", "partition_capability_worker"],
        _ => return Err(refused("unknown business principal")),
    };
    Ok(names
        .iter()
        .filter(|name| {
            c.admission_minimum_epochs
                .get(**name)
                .is_some_and(|minimum| *minimum <= epoch)
        })
        .map(|name| {
            c.admission_requirements
                .get(*name)
                .cloned()
                .ok_or_else(|| refused("admission requirement missing"))
        })
        .collect::<Result<Vec<_>>>()?)
}
