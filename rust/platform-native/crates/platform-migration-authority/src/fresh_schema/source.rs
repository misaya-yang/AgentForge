use super::*;
use crate::manifest::{BaselineManifest, ChangeSpec, TransactionMode};
use std::{
    fs,
    path::{Component, Path, PathBuf},
};
pub(super) struct Definitions {
    pub baseline: BaselineManifest,
    pub baseline_sql: Vec<String>,
    pub owner_setup: String,
    pub extensions: String,
    pub changes: Vec<(ChangeSpec, String)>,
    pub verify_sql: String,
}
/// Self-contained native image entry: every compiled byte is checked against the
/// complete source-owned definition inventory, without a checkout or old proofs.
pub(super) fn compiled_definitions() -> Result<Definitions> {
    let inputs: BTreeMap<String, String> = serde_json::from_slice(include_bytes!("inputs.json"))
        .map_err(|_| refused("compiled definition bytes malformed"))?;
    let contract = source_contract()?;
    let pins: BTreeMap<String, String> = serde_json::from_value(contract["inputs_sha256"].clone())
        .map_err(|_| refused("compiled definition pins malformed"))?;
    if inputs.keys().ne(pins.keys())
        || inputs
            .iter()
            .any(|(p, b)| pins.get(p) != Some(&sha(b.as_bytes())))
    {
        return Err(refused("compiled complete definition inventory changed"));
    }
    let get = |p: &str| -> Result<String> {
        inputs
            .get(p)
            .cloned()
            .ok_or_else(|| refused("compiled definition missing"))
    };
    let base = format!("database/baselines/{}/", crate::DEFAULT_BASELINE_ID);
    let baseline: BaselineManifest =
        serde_json::from_str(&get(&format!("{base}manifest.json"))?)
            .map_err(|_| refused("compiled baseline definition malformed"))?;
    let epoch: Value = serde_json::from_str(&get(&format!(
        "database/migrations/{}/manifest.yml",
        crate::DEFAULT_BASELINE_ID
    ))?)
    .map_err(|_| refused("compiled base definitions malformed"))?;
    let specs: Vec<ChangeSpec> = serde_json::from_value(epoch["changes"].clone())
        .map_err(|_| refused("compiled base specs malformed"))?;
    if epoch["epoch"] != 29 || specs.len() != 29 {
        return Err(refused("compiled base allocation changed"));
    }
    let mut changes = specs
        .into_iter()
        .map(|s| {
            let sql = get(&format!(
                "database/migrations/{}/{}",
                crate::DEFAULT_BASELINE_ID,
                s.file
            ))?;
            Ok((s, sql))
        })
        .collect::<Result<Vec<_>>>()?;
    let c: Value = serde_json::from_str(&get(
        "docs/design/proposals/service-successors/catalog.json",
    )?)
    .map_err(|_| refused("compiled current definitions malformed"))?;
    for s in c["steps"]
        .as_array()
        .ok_or_else(|| refused("current definitions absent"))?
    {
        let spec: ChangeSpec = serde_json::from_value(s["change"].clone())
            .map_err(|_| refused("current spec malformed"))?;
        changes.push((
            spec,
            get(s["sql_source"]
                .as_str()
                .ok_or_else(|| refused("current SQL source absent"))?)?,
        ));
    }
    if changes.iter().map(|(s, _)| s.sequence).ne(1..=EPOCH) {
        return Err(refused("current definitions incomplete"));
    }
    for (s, sql) in &changes {
        if s.owner != "owner"
            || s.transaction_mode != TransactionMode::Transactional
            || s.sha256 != sha(sql.as_bytes())
            || s.postconditions.is_empty()
        {
            return Err(refused("compiled current definition failed exact contract"));
        }
        crate::sql::reject_embedded_transactions(sql, &s.name)?;
    }
    let roles = get("database/bootstrap/roles.sql")?;
    let owner_setup = roles
        .split_once("-- 5. Stable schema ownership")
        .and_then(|(_, s)| s.split_once('\n'))
        .map(|(_, s)| s.to_owned())
        .ok_or_else(|| refused("compiled owner-local setup unavailable"))?;
    Ok(Definitions {
        baseline,
        baseline_sql: ["init.sql", "reference_data.sql", "grants.sql"]
            .iter()
            .map(|n| get(&format!("{base}{n}")))
            .collect::<Result<_>>()?,
        owner_setup,
        extensions: get("database/bootstrap/extensions.sql")?,
        changes,
        verify_sql: get(&format!("{base}verify.sql"))?,
    })
}
pub fn source_contract() -> Result<Value> {
    let value: Value =
        serde_json::from_slice(SOURCE).map_err(|_| refused("invalid compiled source"))?;
    if value["schema"] != "agentforge/fresh-development-source/v1"
        || value["database"] != DATABASE
        || value["schema_epoch"] != EPOCH
        || value["definition_count"] != EPOCH
        || value["families"].as_array().is_none_or(|a| a.len() != 22)
    {
        return Err(refused("unreviewed fresh source allocation"));
    }
    Ok(value)
}
fn file(root: &Path, name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    if path.is_absolute()
        || path
            .components()
            .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(refused("unsafe source input path"));
    }
    let mut result = root.to_path_buf();
    for part in path.components() {
        result.push(part);
        if fs::symlink_metadata(&result)?.file_type().is_symlink() {
            return Err(refused("symlink source input"));
        }
    }
    if !result.is_file() {
        return Err(refused("source input unavailable"));
    }
    Ok(result)
}
pub fn source_inventory(root: &Path) -> Result<Value> {
    let root = root.canonicalize()?;
    if fs::read(file(
        &root,
        "rust/platform-native/crates/platform-migration-authority/src/fresh_schema/source.json",
    )?)?
    .as_slice()
        != SOURCE
    {
        return Err(refused(
            "fresh source contract differs from compiled identity",
        ));
    }
    let contract = source_contract()?;
    let inputs: BTreeMap<String, String> =
        serde_json::from_value(contract["inputs_sha256"].clone())
            .map_err(|_| refused("source inputs malformed"))?;
    for (name, expected) in &inputs {
        if sha(&fs::read(file(&root, name)?)?) != *expected {
            return Err(refused(
                "current definition input differs from compiled source",
            ));
        }
    }
    Ok(
        json!({"source_contract_sha256":source_sha256(),"inputs_verified":inputs.len(),
        "schema_epoch":EPOCH,"families":contract["families"],"database_actions":false,
        "historical_upgrade_acceptance":false}),
    )
}
#[cfg(test)]
pub(super) fn definitions(root: &Path) -> Result<Definitions> {
    source_inventory(root)?;
    let base = root
        .join("database/baselines")
        .join(crate::DEFAULT_BASELINE_ID);
    // Identity/hash validation only; deliberately never invokes Git, archive,
    // producer-retirement, activated-upgrade or external-preview verification.
    let baseline = crate::manifest::load_baseline_manifest(&base.join("manifest.json"))?;
    let epoch_dir = root
        .join("database/migrations")
        .join(crate::DEFAULT_BASELINE_ID);
    let epoch = crate::manifest::load_epoch_manifest(&epoch_dir.join("manifest.yml"))?;
    if epoch.epoch != 29 || epoch.changes.len() != 29 {
        return Err(refused("compiled definition anchor changed"));
    }
    let mut changes = epoch
        .changes
        .into_iter()
        .map(|s| {
            let sql = fs::read_to_string(epoch_dir.join(&s.file))?;
            Ok((s, sql))
        })
        .collect::<Result<Vec<_>>>()?;
    changes.extend(crate::source_tools::service_successors::fresh_definitions(
        root,
    )?);
    if changes.iter().map(|(s, _)| s.sequence).ne(1..=EPOCH) {
        return Err(refused("incomplete current definition set"));
    }
    for (spec, sql) in &changes {
        if spec.owner != "owner"
            || spec.transaction_mode != TransactionMode::Transactional
            || sha(sql.as_bytes()) != spec.sha256
            || spec.postconditions.is_empty()
        {
            return Err(refused(
                "fresh definition must be exact, owner-owned and transactional",
            ));
        }
        crate::sql::reject_embedded_transactions(sql, &spec.name)?;
    }
    let role_sql = fs::read_to_string(root.join("database/bootstrap/roles.sql"))?;
    let owner_setup = role_sql
        .split_once("-- 5. Stable schema ownership")
        .map(|(_, s)| s)
        .ok_or_else(|| refused("owner setup section unavailable"))?;
    // Drop the marker's trailing heading text; retain only the exact SQL body.
    let owner_setup = owner_setup
        .split_once('\n')
        .ok_or_else(|| refused("owner setup marker malformed"))?
        .1
        .to_owned();
    Ok(Definitions {
        baseline,
        baseline_sql: ["init.sql", "reference_data.sql", "grants.sql"]
            .iter()
            .map(|p| fs::read_to_string(base.join(p)).map_err(AuthorityError::from))
            .collect::<Result<_>>()?,
        owner_setup,
        extensions: fs::read_to_string(root.join("database/bootstrap/extensions.sql"))?,
        changes,
        verify_sql: fs::read_to_string(base.join("verify.sql"))?,
    })
}
