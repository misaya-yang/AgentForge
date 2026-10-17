//! Offline promotion of captured measurements. Never invokes a provider or invents scores.
use ai_platform_knowledge_service::{migration_knowledge_query::retrieval_eval, parsing_eval};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
type Result<T> = std::result::Result<T, String>;
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    pub path: String,
    pub sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub id: String,
    pub kind: String,
    pub dataset_version: String,
    pub golden: Artifact,
    pub label_provenance: String,
    pub corpus_class: String,
    pub profiles: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema: String,
    pub baseline: Artifact,
    pub evaluator_sha256: String,
    pub suites: Vec<Suite>,
    pub human_labels: Artifact,
    pub judge_model: String,
    pub judge_prompt_sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    schema: String,
    provenance: String,
    collector: String,
    source_sha: String,
    evaluator_sha256: String,
    profiles: Vec<Profile>,
    calibration: Calibration,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Profile {
    suite: String,
    profile: String,
    version: String,
    configuration: Artifact,
    dataset_version: String,
    golden_sha256: String,
    observations: Artifact,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Calibration {
    judge_model: String,
    prompt_sha256: String,
    scores: Artifact,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HumanLabels {
    provenance: String,
    reviewer: String,
    reviewed_at: String,
    labels: BTreeMap<String, f64>,
}
#[derive(Debug, Serialize)]
pub struct Decision {
    pub eligible: bool,
    pub promotable: bool,
    pub fixture_only: bool,
    pub cases: usize,
    pub calibration: Value,
    pub metrics: BTreeMap<String, BTreeMap<String, f64>>,
}
fn require(ok: bool, message: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(message.into()) }
}
fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn artifact(root: &Path, a: &Artifact) -> Result<Vec<u8>> {
    require(hex(&a.sha256, 64), "artifact SHA-256 invalid")?;
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let p = root
        .join(&a.path)
        .canonicalize()
        .map_err(|e| format!("artifact {}: {e}", a.path))?;
    require(
        p.starts_with(&root),
        "artifact must be inside evidence root",
    )?;
    let bytes = std::fs::read(p).map_err(|e| e.to_string())?;
    require(sha256(&bytes) == a.sha256, "artifact hash mismatch")?;
    Ok(bytes)
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}
fn rows(bytes: &[u8]) -> Result<BTreeMap<String, Value>> {
    let mut out = BTreeMap::new();
    for line in std::str::from_utf8(bytes)
        .map_err(|e| e.to_string())?
        .lines()
        .filter(|s| !s.trim().is_empty())
    {
        let row: Value = decode(line.as_bytes())?;
        let id = row["case_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("case_id required")?
            .to_owned();
        require(out.insert(id, row).is_none(), "duplicate case_id")?;
    }
    require(!out.is_empty(), "empty measurements/corpus")?;
    Ok(out)
}
/// Pins the actual metric implementations compiled into this executable.
pub fn evaluator_sha256() -> String {
    sha256(
        concat!(
            include_str!(
                "../../platform-knowledge-domain/src/migration_knowledge_query/retrieval_eval.rs"
            ),
            include_str!("../../platform-knowledge-service/src/parsing_eval.rs"),
            include_str!("promotion.rs")
        )
        .as_bytes(),
    )
}
fn scores(
    suite: &Suite,
    golden: &BTreeMap<String, Value>,
    observations: &BTreeMap<String, Value>,
) -> Result<BTreeMap<String, BTreeMap<String, f64>>> {
    require(
        golden.keys().eq(observations.keys()),
        "missing or extra observation cases",
    )?;
    let mut result = BTreeMap::new();
    for (id, g) in golden {
        let o = &observations[id];
        require(o["status"] == "succeeded", "measurement failed or skipped")?;
        let m = match suite.kind.as_str() {
            "retrieval" => {
                let relevance: BTreeMap<String, f64> =
                    decode(&serde_json::to_vec(&g["relevance"]).map_err(|e| e.to_string())?)?;
                require(
                    relevance.values().any(|v| *v > 0.0),
                    "golden has no relevant targets",
                )?;
                let retrieved: Vec<String> = decode(
                    &serde_json::to_vec(&o["ranked_segment_ids"]).map_err(|e| e.to_string())?,
                )?;
                let v = retrieval_eval::score_single_query(&retrieved, &relevance, &[10, 20])
                    .map_err(|e| e.to_string())?;
                BTreeMap::from([
                    (
                        "recall_at_20".into(),
                        v["by_k"]["20"]["recall_at_k"]
                            .as_f64()
                            .ok_or("missing recall")?,
                    ),
                    (
                        "ndcg_at_10".into(),
                        v["by_k"]["10"]["ndcg_at_k"]
                            .as_f64()
                            .ok_or("missing ndcg")?,
                    ),
                    (
                        "mrr".into(),
                        v["reciprocal_rank"].as_f64().ok_or("missing mrr")?,
                    ),
                ])
            }
            "parsing" => {
                let expected =
                    serde_json::from_value(g["document"].clone()).map_err(|e| e.to_string())?;
                let actual =
                    serde_json::from_value(o["document"].clone()).map_err(|e| e.to_string())?;
                let report = parsing_eval::score_document(&expected, &actual);
                let expected_text = expected
                    .pages
                    .iter()
                    .map(|p| p.page_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                let actual_text = actual
                    .pages
                    .iter()
                    .map(|p| p.page_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                let cer = parsing_eval::levenshtein(&expected_text, &actual_text) as f64
                    / expected_text.chars().count().max(1) as f64;
                // Existing positional TEDS-lite can omit unmatched pages/blocks. Explicit coverage
                // prevents a truncated document from winning by retaining only easy blocks.
                let expected_blocks = expected.iter_blocks().len();
                let actual_blocks = actual.iter_blocks().len();
                require(
                    expected_blocks > 0 && !expected.pages.is_empty(),
                    "empty parsing golden",
                )?;
                let mut unique = BTreeSet::new();
                require(
                    actual.pages.iter().all(|p| unique.insert(p.page_number)),
                    "duplicate parsed page",
                )?;
                let coverage = expected
                    .pages
                    .iter()
                    .filter(|p| actual.pages.iter().any(|a| a.page_number == p.page_number))
                    .count() as f64
                    / expected.pages.len() as f64;
                BTreeMap::from([
                    ("cer".into(), cer),
                    ("text_similarity".into(), report.text_similarity),
                    ("table_similarity_teds_lite".into(), report.table_similarity),
                    ("formula_similarity".into(), report.formula_similarity),
                    ("reading_order".into(), report.reading_order),
                    ("page_coverage".into(), coverage),
                    (
                        "block_coverage".into(),
                        actual_blocks.min(expected_blocks) as f64 / expected_blocks as f64,
                    ),
                ])
            }
            _ => return Err("unknown suite kind".into()),
        };
        require(
            m.iter().all(|(name, v)| {
                v.is_finite()
                    && if name == "cer" {
                        *v >= 0.0
                    } else {
                        (0.0..=1.0).contains(v)
                    }
            }),
            "invalid metric",
        )?;
        result.insert(id.clone(), m);
    }
    Ok(result)
}
fn calibration(
    root: &Path,
    policy: &Policy,
    run: &Run,
    fixture: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Value> {
    require(
        run.calibration.judge_model == policy.judge_model
            && run.calibration.prompt_sha256 == policy.judge_prompt_sha256,
        "judge identity mismatch",
    )?;
    require(
        hex(&policy.judge_prompt_sha256, 64) && !policy.judge_model.trim().is_empty(),
        "judge model/prompt pin missing",
    )?;
    if !fixture {
        require(policy.judge_prompt_sha256 == ai_platform_knowledge_service::migration_knowledge_query::ragas::judge_prompts_sha256(), "judge prompt does not match the deployed RAGAS evaluator")?;
    }
    let labels: HumanLabels = decode(&artifact(root, &policy.human_labels)?)?;
    require(
        labels.provenance == if fixture { "fixture" } else { "human_reviewed" },
        "human labels not reviewed",
    )?;
    require(!labels.reviewer.trim().is_empty(), "human reviewer missing")?;
    let reviewed = chrono::DateTime::parse_from_rfc3339(&labels.reviewed_at)
        .map_err(|e| e.to_string())?
        .with_timezone(&chrono::Utc);
    require(
        reviewed <= now && now.signed_duration_since(reviewed).num_days() <= 92,
        "judge calibration overdue or future dated",
    )?;
    let scores: BTreeMap<String, f64> = decode(&artifact(root, &run.calibration.scores)?)?;
    require(
        labels.labels.len() >= if fixture { 2 } else { 30 },
        "insufficient human calibration sample",
    )?;
    require(
        labels.labels.keys().eq(scores.keys()),
        "calibration labels/scores case mismatch",
    )?;
    require(
        labels
            .labels
            .values()
            .chain(scores.values())
            .all(|v| v.is_finite() && (0.0..=1.0).contains(v)),
        "calibration score out of range",
    )?;
    let n = scores.len() as f64;
    let mut mae = 0.0;
    let mut agree = 0.0;
    let mut hp = 0.0;
    let mut jp = 0.0;
    for (id, h) in &labels.labels {
        let j = scores[id];
        mae += (h - j).abs();
        let a = *h >= 0.5;
        let b = j >= 0.5;
        agree += u8::from(a == b) as f64;
        hp += u8::from(a) as f64;
        jp += u8::from(b) as f64;
    }
    let agreement = agree / n;
    let chance = (hp / n) * (jp / n) + (1.0 - hp / n) * (1.0 - jp / n);
    require(
        hp > 0.0 && hp < n,
        "calibration requires both human outcome classes",
    )?;
    let kappa = if chance < 1.0 {
        (agreement - chance) / (1.0 - chance)
    } else {
        0.0
    };
    require(
        mae / n <= 0.15 && agreement >= 0.8 && kappa >= 0.6,
        "judge failed human calibration",
    )?;
    Ok(
        json!({"samples":scores.len(),"mae":mae/n,"agreement":agreement,"kappa":kappa,"reviewed_at":labels.reviewed_at}),
    )
}
pub fn evaluate(
    root: &Path,
    policy_path: &Path,
    candidate_path: &Path,
    expected_source: &str,
    fixture: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Decision> {
    let policy: Policy = decode(&std::fs::read(policy_path).map_err(|e| e.to_string())?)?;
    let baseline: Run = decode(&artifact(root, &policy.baseline)?)?;
    let candidate: Run = decode(&std::fs::read(candidate_path).map_err(|e| e.to_string())?)?;
    require(
        policy.schema == "knowledge-promotion/v1"
            && baseline.schema == "knowledge-measurements/v1"
            && candidate.schema == baseline.schema,
        "schema mismatch",
    )?;
    require(
        hex(expected_source, 40)
            && candidate.source_sha == expected_source
            && hex(&baseline.source_sha, 40),
        "measurement source SHA mismatch",
    )?;
    let evaluator = evaluator_sha256();
    require(
        policy.evaluator_sha256 == evaluator
            && baseline.evaluator_sha256 == evaluator
            && candidate.evaluator_sha256 == evaluator,
        "evaluator changed: establish a new measured baseline",
    )?;
    for run in [&baseline, &candidate] {
        require(
            run.provenance == if fixture { "fixture" } else { "measured" },
            "fixtures/replays cannot authorize promotion",
        )?;
        require(
            matches!(
                run.collector.as_str(),
                "knowledge-native-capture/v1" | "fixture"
            ),
            "unknown measurement collector",
        )?;
        require(
            fixture || run.collector != "fixture",
            "fixture collector cannot promote",
        )?;
    }
    require(!policy.suites.is_empty(), "no required suites")?;
    if !fixture {
        for kind in ["retrieval", "parsing"] {
            for class in ["text", "scanned", "table", "multilingual"] {
                require(
                    policy
                        .suites
                        .iter()
                        .any(|s| s.kind == kind && s.corpus_class == class),
                    "production promotion requires every retrieval/parsing corpus class",
                )?;
            }
        }
    }
    let mut keys = BTreeSet::new();
    let mut metrics = BTreeMap::new();
    let mut count = 0;
    for suite in &policy.suites {
        require(
            !suite.dataset_version.is_empty() && !suite.id.is_empty() && !suite.profiles.is_empty(),
            "suite identity missing",
        )?;
        require(
            suite.label_provenance == if fixture { "fixture" } else { "human_reviewed" },
            "golden corpus labels have not been human reviewed",
        )?;
        require(
            matches!(
                suite.corpus_class.as_str(),
                "text" | "scanned" | "table" | "multilingual"
            ),
            "unknown corpus class",
        )?;
        let mut golden = rows(&artifact(root, &suite.golden)?)?;
        if suite.kind == "retrieval" {
            golden.retain(|_, g| g["track"] == "retrieval_only");
        }
        require(!golden.is_empty(), "no applicable golden cases")?;
        for profile in &suite.profiles {
            let key = format!("{}/{}", suite.id, profile);
            require(keys.insert(key.clone()), "duplicate profile")?;
            let mut paired = Vec::new();
            let mut identities = Vec::new();
            for run in [&baseline, &candidate] {
                let profiles: Vec<_> = run
                    .profiles
                    .iter()
                    .filter(|p| p.suite == suite.id && p.profile == *profile)
                    .collect();
                require(
                    profiles.len() == 1,
                    "required profile missing or duplicated",
                )?;
                let p = profiles[0];
                require(
                    !p.version.trim().is_empty()
                        && p.dataset_version == suite.dataset_version
                        && p.golden_sha256 == suite.golden.sha256,
                    "profile/dataset identity mismatch",
                )?;
                artifact(root, &p.configuration)?;
                identities.push((&p.version, &p.configuration.sha256));
                paired.push(scores(
                    suite,
                    &golden,
                    &rows(&artifact(root, &p.observations)?)?,
                )?);
            }
            require(
                identities[0].1 == identities[1].1 || identities[0].0 != identities[1].0,
                "changed profile configuration must have a new version",
            )?;
            for (id, b) in &paired[0] {
                for (metric, before) in b {
                    let after = paired[1][id][metric];
                    require(
                        if metric == "cer" {
                            after <= *before + 1e-12
                        } else {
                            after + 1e-12 >= *before
                        },
                        &format!("regression {key}/{id}/{metric}: {before} -> {after}"),
                    )?;
                }
            }
            let mut means = BTreeMap::new();
            for m in paired[1].values() {
                for (name, value) in m {
                    *means.entry(name.clone()).or_insert(0.0) += value;
                }
            }
            count += golden.len();
            for mean in means.values_mut() {
                *mean /= paired[1].len() as f64;
            }
            metrics.insert(key, means);
        }
    }
    for run in [&baseline, &candidate] {
        require(
            run.profiles.len() == keys.len(),
            "unconfigured extra profiles",
        )?;
    }
    let calibrated = calibration(root, &policy, &candidate, fixture, now)?;
    Ok(Decision {
        eligible: true,
        promotable: !fixture,
        fixture_only: fixture,
        cases: count,
        calibration: calibrated,
        metrics,
    })
}
