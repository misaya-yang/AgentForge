//! Opt-in Qdrant one-minor upgrade rehearsal. Never changes the default pin,
//! downgrades an upgraded volume, runs models, pulls images or switches traffic.
mod deployment;
mod metadata;
#[cfg(test)]
mod tests;
mod transport;
use crate::{Args, Result, error, fail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use transport::Node;
pub const OLD_VERSION: &str = "1.18.2";
pub const NEW_VERSION: &str = "1.19.2";
pub const OLD_IMAGE: &str =
    "qdrant/qdrant@sha256:75eab8c4ba42096724fdcfde8b4de0b5713d529dde32f285a1f86fdcb2c9e50c";
pub const NEW_IMAGE: &str =
    "qdrant/qdrant@sha256:b7b0444c4c351c970b98e90a6f89c2ee4287c65b44e52b4cb503fa5b2aa927ad";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub url: String,
    pub api_key_file: PathBuf,
    pub ca_file: PathBuf,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub schema: String,
    pub run_id: String,
    pub repository: PathBuf,
    pub artifacts: PathBuf,
    pub source: Endpoint,
    pub canary: Endpoint,
    pub rollback: Endpoint,
    pub deployment: deployment::Deployment,
    pub golden_file: PathBuf,
    /// Explicit corpus receipt, never inferred from a fixture passing assertions.
    pub corpus_kind: String,
    pub max_snapshot_bytes: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Golden {
    pub schema: String,
    pub corpus_id: String,
    pub corpus_kind: String,
    pub model_revision: String,
    pub index_generation: String,
    pub cases: Vec<Case>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub collection: String,
    pub tenant_id: String,
    pub dataset_id: String,
    pub request: Value,
    pub expected_ids: Vec<Value>,
    pub relevance: BTreeMap<String, u8>,
    pub negative: bool,
}
#[derive(Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Hit {
    pub id: Value,
    pub score: f64,
    pub payload_sha256: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub collection: String,
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
    pub count: u64,
    pub config: Value,
    pub payload_schema: Value,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub schema: String,
    pub stage: String,
    pub run_id: String,
    pub spec_sha256: String,
    pub golden_sha256: String,
    pub previous_sha256: Option<String>,
    pub metadata_sha256: String,
    pub snapshots: Vec<Snapshot>,
    pub aliases: Value,
    pub results: BTreeMap<String, Vec<Hit>>,
    pub product_acceptance: String,
}
pub(super) fn sha(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub(super) fn component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b))
        && s != "."
        && s != ".."
}
pub(super) fn private_read(path: &Path, max: u64) -> Result<Vec<u8>> {
    read_file(path, max, true)
}
pub(super) fn read_file(path: &Path, max: u64, private: bool) -> Result<Vec<u8>> {
    if !path.is_absolute() {
        return fail("upgrade input must use an absolute path");
    }
    let m = std::fs::symlink_metadata(path).map_err(|_| error("upgrade input unavailable"))?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > max {
        return fail("upgrade input file invalid");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if (private && m.permissions().mode() & 0o077 != 0)
            || (!private && m.permissions().mode() & 0o022 != 0)
        {
            return fail("upgrade input must be private (0600/0400)");
        }
    }
    let mut data = Vec::new();
    std::fs::File::open(path)?
        .take(max + 1)
        .read_to_end(&mut data)?;
    if data.len() as u64 > max {
        return fail("upgrade input exceeds limit");
    }
    Ok(data)
}
fn load<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    Ok(serde_json::from_slice(&private_read(
        path,
        16 * 1024 * 1024,
    )?)?)
}
pub(super) fn write_new<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| error("upgrade evidence already exists or cannot be created"))?;
    file.write_all(&serde_json::to_vec_pretty(value)?)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(())
}
fn digest_file(path: &Path) -> Result<(String, u64)> {
    let m = std::fs::symlink_metadata(path)?;
    if !m.is_file() || m.file_type().is_symlink() {
        return fail("snapshot file invalid");
    }
    let mut file = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut n = 0;
    let mut buf = [0; 65536];
    loop {
        let size = file.read(&mut buf)?;
        if size == 0 {
            break;
        }
        n += size as u64;
        h.update(&buf[..size]);
    }
    Ok((format!("{:x}", h.finalize()), n))
}
impl Spec {
    fn validate(&self) -> Result<()> {
        if self.schema != "qdrant-upgrade/v1"
            || !component(&self.run_id)
            || !self.repository.is_absolute()
            || !self.artifacts.is_absolute()
            || !self.golden_file.is_absolute()
            || !(1..=1024 * 1024 * 1024 * 1024u64).contains(&self.max_snapshot_bytes)
            || !["production_export", "fixture"].contains(&self.corpus_kind.as_str())
        {
            return fail("upgrade spec invalid");
        }
        let mut existing = self.artifacts.clone();
        let mut missing = Vec::new();
        while !existing.exists() {
            missing.push(
                existing
                    .file_name()
                    .ok_or_else(|| error("artifact path invalid"))?
                    .to_os_string(),
            );
            existing = existing
                .parent()
                .ok_or_else(|| error("artifact parent unavailable"))?
                .to_path_buf();
        }
        let mut resolved = std::fs::canonicalize(existing)?;
        for name in missing.into_iter().rev() {
            resolved.push(name);
        }
        if resolved.starts_with(&self.repository)
            || self.artifacts.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::CurDir
                )
            })
        {
            return fail(
                "upgrade artifacts must stay outside repository (including symlinked parents)",
            );
        }
        let urls = [&self.source.url, &self.canary.url, &self.rollback.url]
            .into_iter()
            .map(|s| transport::endpoint(s))
            .collect::<Result<Vec<_>>>()?;
        if urls
            .iter()
            .map(|url| url.origin().ascii_serialization())
            .collect::<BTreeSet<_>>()
            .len()
            != 3
        {
            return fail("source, canary and fresh rollback node must have different origins");
        }
        self.deployment.validate(&self.repository, &self.run_id)?;
        Ok(())
    }
    fn receipt(&self, stage: &str) -> PathBuf {
        self.artifacts.join(format!("{stage}.json"))
    }
    fn fingerprint(&self) -> Result<String> {
        Ok(sha(&serde_json::to_vec(
            &json!({"spec":self,"deployment_files":self.deployment.fingerprints()?}),
        )?))
    }
}
impl Golden {
    fn validate(&self, spec: &Spec) -> Result<()> {
        if self.schema != "qdrant-retrieval-golden/v1"
            || self.corpus_id.is_empty()
            || self.corpus_kind != spec.corpus_kind
            || self.model_revision.is_empty()
            || self.index_generation.is_empty()
            || self.cases.is_empty()
            || self.cases.len() > 1000
        {
            return fail("frozen golden corpus identity invalid");
        }
        let mut ids = BTreeSet::new();
        let mut coverage = BTreeMap::<&str, (bool, bool)>::new();
        for c in &self.cases {
            if !component(&c.id)
                || !component(&c.collection)
                || !ids.insert(&c.id)
                || c.tenant_id.is_empty()
                || c.dataset_id.is_empty()
                || !c.request.is_object()
                || c.expected_ids.len() > 20
                || c.request["limit"].as_u64() != Some(20)
                || c.request["with_payload"] != true
                || c.request.get("lookup_from").is_some()
                || c.request.get("prefetch").is_some()
            {
                return fail("golden case invalid or unsupported query shape");
            }
            // Upgrade parity uses frozen vectors/sparse vectors, never a server-side
            // provider inference request (or a newly enabled advanced feature).
            let q = &c.request["query"];
            let dense = q.as_array().is_some_and(|v| {
                !v.is_empty()
                    && v.len() <= 65536
                    && v.iter().all(|n| n.as_f64().is_some_and(f64::is_finite))
            });
            let sparse = match (q["indices"].as_array(), q["values"].as_array()) {
                (Some(indices), Some(values)) => {
                    !indices.is_empty()
                        && indices.len() == values.len()
                        && indices.len() <= 65536
                        && indices
                            .iter()
                            .all(|i| i.as_u64().is_some_and(|i| i <= u32::MAX as u64))
                        && values
                            .iter()
                            .all(|v| v.as_f64().is_some_and(f64::is_finite))
                        && indices.windows(2).all(|v| v[0].as_u64() < v[1].as_u64())
                }
                _ => false,
            };
            if !dense && !sparse {
                return fail("golden query must contain frozen numeric dense/sparse vectors");
            }
            let must = c.request["filter"]["must"]
                .as_array()
                .ok_or_else(|| error("golden must carry exact tenant/dataset filters"))?;
            for (key, value) in [("tenant_id", &c.tenant_id), ("dataset_id", &c.dataset_id)] {
                if !must
                    .iter()
                    .any(|f| f["key"] == key && f["match"]["value"] == value.as_str())
                {
                    return fail("golden tenant/dataset filter missing");
                }
            }
            if c.negative != c.expected_ids.is_empty()
                || (!c.negative && c.relevance.is_empty())
                || c.relevance.values().any(|g| *g > 4)
            {
                return fail("golden expected results/relevance invalid");
            }
            let entry = coverage.entry(&c.collection).or_default();
            if c.negative {
                entry.1 = true
            } else {
                entry.0 = true
            }
        }
        if coverage.values().any(|v| !v.1) {
            return fail(
                "each collection requires positive and cross-tenant/empty negative golden cases",
            );
        }
        Ok(())
    }
}
fn receipt(
    spec: &Spec,
    stage: &str,
    previous: Option<&str>,
    snapshots: Vec<Snapshot>,
    aliases: Value,
    results: BTreeMap<String, Vec<Hit>>,
) -> Result<Receipt> {
    Ok(Receipt {
        schema: "qdrant-upgrade-receipt/v1".into(),
        stage: stage.into(),
        run_id: spec.run_id.clone(),
        spec_sha256: spec.fingerprint()?,
        golden_sha256: sha(&private_read(&spec.golden_file, 16 * 1024 * 1024)?),
        previous_sha256: previous
            .map(|s| private_read(&spec.receipt(s), 16 * 1024 * 1024).map(|b| sha(&b)))
            .transpose()?,
        metadata_sha256: sha(&private_read(&spec.receipt("metadata"), 2 * 1024 * 1024)?),
        snapshots,
        aliases,
        results,
        product_acceptance: if spec.corpus_kind == "fixture" {
            "not_run_fixture_only"
        } else {
            "not_run"
        }
        .into(),
    })
}
fn read_receipt(spec: &Spec, stage: &str, golden: &Golden) -> Result<Receipt> {
    let r: Receipt = load(&spec.receipt(stage))?;
    if r.schema != "qdrant-upgrade-receipt/v1"
        || r.stage != stage
        || r.run_id != spec.run_id
        || r.spec_sha256 != spec.fingerprint()?
        || r.golden_sha256 != sha(&private_read(&spec.golden_file, 16 * 1024 * 1024)?)
        || r.metadata_sha256 != sha(&private_read(&spec.receipt("metadata"), 2 * 1024 * 1024)?)
    {
        return fail("upgrade receipt binding mismatch");
    }
    let expected = match stage {
        "snapshot" => None,
        "restore" => Some("snapshot"),
        "upgrade" => Some("restore"),
        "golden" => Some("upgrade"),
        "rollback" => Some("golden"),
        _ => return fail("unknown receipt stage"),
    };
    if let Some(previous) = expected {
        let _ = read_receipt(spec, previous, golden)?;
        if r.previous_sha256.as_ref()
            != Some(&sha(&private_read(
                &spec.receipt(previous),
                16 * 1024 * 1024,
            )?))
        {
            return fail("upgrade receipt chain mismatch");
        }
    }
    if r.snapshots.is_empty()
        || r.snapshots
            .iter()
            .map(|s| &s.collection)
            .collect::<BTreeSet<_>>()
            .len()
            != r.snapshots.len()
    {
        return fail("snapshot inventory invalid");
    }
    for s in &r.snapshots {
        if s.count > 0
            && !golden
                .cases
                .iter()
                .any(|c| c.collection == s.collection && !c.negative)
        {
            return fail("nonempty collection requires a positive golden case");
        }
        if !component(&s.file)
            || !component(&s.collection)
            || s.bytes == 0
            || digest_file(&spec.artifacts.join(&s.file))? != (s.sha256.clone(), s.bytes)
        {
            return fail("snapshot checksum/size mismatch");
        }
    }
    let corpus = golden
        .cases
        .iter()
        .map(|c| c.collection.as_str())
        .collect::<BTreeSet<_>>();
    if corpus != r.snapshots.iter().map(|s| s.collection.as_str()).collect() {
        return fail("golden must cover every snapshotted collection");
    }
    Ok(r)
}
pub(super) fn compare(
    golden: &Golden,
    baseline: &BTreeMap<String, Vec<Hit>>,
    actual: &BTreeMap<String, Vec<Hit>>,
) -> Result<Value> {
    if baseline.len() != golden.cases.len() || actual.len() != baseline.len() {
        return fail("golden case inventory mismatch");
    }
    let mut rows = Vec::new();
    for c in &golden.cases {
        let a = actual
            .get(&c.id)
            .ok_or_else(|| error("golden case missing"))?;
        let b = baseline
            .get(&c.id)
            .ok_or_else(|| error("golden baseline case missing"))?;
        if a.iter().map(|h| &h.id).collect::<Vec<_>>() != c.expected_ids.iter().collect::<Vec<_>>()
            || a.len() != b.len()
        {
            return fail("retrieval golden IDs/order changed");
        }
        for (x, y) in a.iter().zip(b) {
            if x.id != y.id
                || !x.score.is_finite()
                || !y.score.is_finite()
                || (x.score - y.score).abs() > 1e-6_f64.max(y.score.abs() * 1e-6)
                || x.payload_sha256 != y.payload_sha256
            {
                return fail("retrieval golden score/payload changed");
            }
        }
        let new_ndcg = ndcg(c, a)?;
        let old = ndcg(c, b)?;
        if (new_ndcg - old).abs() > 1e-12 {
            return fail("retrieval nDCG changed");
        }
        rows.push(json!({"case_id":c.id,"ndcg_at_20":if c.negative {Value::Null}else{json!(new_ndcg)},"negative":c.negative}));
    }
    Ok(
        json!({"case_count":rows.len(),"cases":rows,"parity":"passed","product_acceptance":"not_run"}),
    )
}
fn ndcg(c: &Case, hits: &[Hit]) -> Result<f64> {
    let grade = |id: &Value| -> u8 {
        c.relevance
            .get(
                &id.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| id.to_string()),
            )
            .copied()
            .unwrap_or(0)
    };
    let dcg = hits
        .iter()
        .take(20)
        .enumerate()
        .map(|(i, h)| ((1u64 << grade(&h.id)) - 1) as f64 / ((i + 2) as f64).log2())
        .sum::<f64>();
    let mut ideal = c.relevance.values().copied().collect::<Vec<_>>();
    ideal.sort_by(|a, b| b.cmp(a));
    let idcg = ideal
        .into_iter()
        .take(20)
        .enumerate()
        .map(|(i, g)| ((1u64 << g) - 1) as f64 / ((i + 2) as f64).log2())
        .sum::<f64>();
    if !c.negative && idcg == 0.0 {
        return fail("positive golden has no relevance grades");
    }
    Ok(if idcg == 0.0 { 0.0 } else { dcg / idcg })
}
fn apply(args: &Args) -> Result<()> {
    if !args.has("apply") {
        return fail("mutation requires --apply; inspect plan first");
    }
    Ok(())
}
pub async fn run(args: &[String]) -> Result<()> {
    let command = args.first().map(String::as_str).unwrap_or("help");
    let a = Args::parse(&args[1.min(args.len())..], &["apply", "writes-quiesced"])?;
    if a.options.keys().any(|k| !["spec"].contains(&k.as_str())) || !a.positional.is_empty() {
        return fail("unknown qdrant-upgrade argument");
    }
    if command == "help" {
        println!(
            "qdrant-upgrade <plan|metadata|prepare|snapshot|restore|upgrade|golden|rollback|render> --spec /absolute/private.json [--apply] [--writes-quiesced]"
        );
        return Ok(());
    }
    let spec: Spec = load(Path::new(a.get("spec", "")))?;
    spec.validate()?;
    let golden: Golden = load(&spec.golden_file)?;
    golden.validate(&spec)?;
    if command == "plan" {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &json!({"run_id":spec.run_id,"from":OLD_IMAGE,"to":NEW_IMAGE,"stages":["metadata","prepare --apply","snapshot --apply --writes-quiesced","restore --apply","upgrade --apply --writes-quiesced","golden","rollback --apply","render --writes-quiesced"],"snapshot":"not_run","restore":"not_run","golden":"not_run","rollback":"not_run","production_acceptance":"not_run","automatic_traffic_switch":false})
            )?
        );
        return Ok(());
    }
    if !spec.artifacts.exists() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&spec.artifacts)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let m = std::fs::symlink_metadata(&spec.artifacts)?;
        if !m.is_dir() || m.file_type().is_symlink() || m.permissions().mode() & 0o077 != 0 {
            return fail("artifact directory must be private (0700)");
        }
    }
    if command == "metadata" {
        return write_new(&spec.receipt("metadata"), &metadata::verify().await?);
    }
    metadata::validate_saved(&load::<metadata::Metadata>(&spec.receipt("metadata"))?)?;
    if command == "prepare" {
        apply(&a)?;
        return deployment::prepare(&spec).await;
    }
    if command == "snapshot" {
        apply(&a)?;
        if !a.has("writes-quiesced") {
            return fail("snapshot requires externally quiesced writers (--writes-quiesced)");
        }
        let node = Node::new(&spec.source)?;
        node.version(OLD_VERSION).await?;
        node.single_node().await?;
        let inventory = node.collections().await?;
        if inventory
            .iter()
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
            != golden.cases.iter().map(|c| c.collection.as_str()).collect()
        {
            return fail("golden must cover the full source collection inventory");
        }
        let before = node.inventory(&inventory).await?;
        if before.iter().any(|s| {
            s.count > 0
                && !golden
                    .cases
                    .iter()
                    .any(|c| c.collection == s.collection && !c.negative)
        }) {
            return fail("nonempty collection requires a positive golden case");
        }
        let baseline = node.golden(&golden).await?;
        compare(&golden, &baseline, &baseline)?;
        let aliases = node.aliases().await?;
        let mut snapshots = Vec::new();
        for mut snapshot in before {
            let result = node
                .json(
                    reqwest::Method::POST,
                    &format!("/collections/{}/snapshots?wait=true", snapshot.collection),
                    None,
                )
                .await?;
            let remote = result["result"]["name"]
                .as_str()
                .filter(|s| component(s))
                .ok_or_else(|| error("snapshot response name invalid"))?;
            snapshot.file = format!("{}.snapshot", snapshot.collection);
            let path = spec.artifacts.join(&snapshot.file);
            node.download(
                &format!("/collections/{}/snapshots/{remote}", snapshot.collection),
                &path,
                spec.max_snapshot_bytes,
            )
            .await?;
            let (hash, size) = digest_file(&path)?;
            if size == 0 {
                return fail("empty snapshot");
            }
            snapshot.sha256 = hash;
            snapshot.bytes = size;
            snapshots.push(snapshot);
        }
        node.unchanged(&snapshots).await?;
        node.verify_aliases(&aliases).await?;
        if node.golden(&golden).await? != baseline {
            return fail("source changed while snapshotting");
        }
        return write_new(
            &spec.receipt("snapshot"),
            &receipt(&spec, "snapshot", None, snapshots, aliases, baseline)?,
        );
    }
    if command == "restore" {
        apply(&a)?;
        let old = read_receipt(&spec, "snapshot", &golden)?;
        deployment::assert_isolated(&spec, false).await?;
        let node = Node::new(&spec.canary)?;
        restore(&node, &spec, &old, &golden).await?;
        return write_new(
            &spec.receipt("restore"),
            &receipt(
                &spec,
                "restore",
                Some("snapshot"),
                old.snapshots,
                old.aliases,
                old.results,
            )?,
        );
    }
    if command == "upgrade" {
        apply(&a)?;
        if !a.has("writes-quiesced") {
            return fail("upgrade rehearsal requires externally quiesced writers");
        }
        let old = read_receipt(&spec, "restore", &golden)?;
        let source = Node::new(&spec.source)?;
        source.version(OLD_VERSION).await?;
        source.unchanged(&old.snapshots).await?;
        source.verify_aliases(&old.aliases).await?;
        let canary = Node::new(&spec.canary)?;
        canary.version(OLD_VERSION).await?;
        canary.unchanged(&old.snapshots).await?;
        canary.verify_aliases(&old.aliases).await?;
        deployment::upgrade(&spec).await?;
        canary.wait_version(NEW_VERSION).await?;
        return write_new(
            &spec.receipt("upgrade"),
            &receipt(
                &spec,
                "upgrade",
                Some("restore"),
                old.snapshots,
                old.aliases,
                old.results,
            )?,
        );
    }
    if command == "golden" {
        let old = read_receipt(&spec, "upgrade", &golden)?;
        let node = Node::new(&spec.canary)?;
        node.version(NEW_VERSION).await?;
        node.wait_unchanged(&old.snapshots).await?;
        node.verify_aliases(&old.aliases).await?;
        let actual = node.golden(&golden).await?;
        let comparison = compare(&golden, &old.results, &actual)?;
        write_new(&spec.artifacts.join("comparison.json"), &comparison)?;
        return write_new(
            &spec.receipt("golden"),
            &receipt(
                &spec,
                "golden",
                Some("upgrade"),
                old.snapshots,
                old.aliases,
                actual,
            )?,
        );
    }
    if command == "rollback" {
        apply(&a)?;
        let old = read_receipt(&spec, "golden", &golden)?;
        deployment::assert_isolated(&spec, true).await?;
        let node = Node::new(&spec.rollback)?;
        restore(&node, &spec, &old, &golden).await?;
        return write_new(
            &spec.receipt("rollback"),
            &receipt(
                &spec,
                "rollback",
                Some("golden"),
                old.snapshots,
                old.aliases,
                old.results,
            )?,
        );
    }
    if command == "render" {
        if !a.has("writes-quiesced") {
            return fail("render requires writers to remain externally quiesced until cutover");
        }
        let verified = read_receipt(&spec, "rollback", &golden)?;
        if spec.corpus_kind != "production_export" {
            return fail("fixture cannot authorize deployment pin");
        }
        let source = Node::new(&spec.source)?;
        source.version(OLD_VERSION).await?;
        source.unchanged(&verified.snapshots).await?;
        source.verify_aliases(&verified.aliases).await?;
        compare(&golden, &verified.results, &source.golden(&golden).await?)?;
        let canary = Node::new(&spec.canary)?;
        canary.version(NEW_VERSION).await?;
        canary.unchanged(&verified.snapshots).await?;
        canary.verify_aliases(&verified.aliases).await?;
        compare(&golden, &verified.results, &canary.golden(&golden).await?)?;
        return deployment::render(
            &spec,
            &sha(&private_read(&spec.receipt("rollback"), 16 * 1024 * 1024)?),
        );
    }
    fail("unknown qdrant-upgrade stage")
}
async fn restore(node: &Node, spec: &Spec, old: &Receipt, golden: &Golden) -> Result<()> {
    node.version(OLD_VERSION).await?;
    node.single_node().await?;
    if !node.collections().await?.is_empty() {
        return fail("restore requires a fresh empty isolated node; never overwrite a collection");
    }
    for snapshot in &old.snapshots {
        node.upload(
            &snapshot.collection,
            &spec.artifacts.join(&snapshot.file),
            snapshot.bytes,
            &snapshot.sha256,
        )
        .await?;
    }
    node.wait_unchanged(&old.snapshots).await?;
    let aliases = old
        .aliases
        .as_array()
        .ok_or_else(|| error("snapshot alias inventory missing"))?;
    if !aliases.is_empty() {
        let actions=aliases.iter().map(|a|json!({"create_alias":{"collection_name":a["collection_name"],"alias_name":a["alias_name"]}})).collect::<Vec<_>>();
        node.json(
            reqwest::Method::POST,
            "/collections/aliases",
            Some(json!({"actions":actions})),
        )
        .await?;
    }
    let restored = node.aliases().await?;
    if restored != old.aliases {
        return fail("restored aliases differ");
    }
    compare(golden, &old.results, &node.golden(golden).await?)?;
    Ok(())
}
