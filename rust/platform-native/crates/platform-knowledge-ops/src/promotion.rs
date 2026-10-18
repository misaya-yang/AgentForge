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
    /// Only independently measured options from real nonfixture captures may activate.
    pub retrieval_v2_promotions: BTreeMap<String, BTreeMap<String, f64>>,
    pub serving_promotions: BTreeMap<String, BTreeSet<String>>,
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
            include_str!(
                "../../platform-knowledge-domain/src/migration_knowledge_query/retrieval_v2.rs"
            ),
            include_str!(
                "../../platform-knowledge-domain/src/migration_knowledge_query/serving_profile.rs"
            ),
            include_str!(
                "../../platform-knowledge-domain/src/migration_knowledge_ingest/table_teds.rs"
            ),
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
                let mut measured = BTreeMap::from([
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
                ]);
                if let Some(teds) = full_table_teds(&expected, &actual)? {
                    measured.insert("table_teds_full".into(), teds);
                }
                measured
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
    let mut v2_captures = Vec::new();
    let mut serving_captures = Vec::new();
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
                "text" | "scanned" | "table" | "multilingual" | "multimodal"
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
                let configuration = artifact(root, &p.configuration)?;
                {
                    let configuration: Value = decode(&configuration)?;
                    if configuration
                        .get("index_config")
                        .and_then(|v| v.get("serving_v2"))
                        .is_some()
                    {
                        use ai_platform_knowledge_service::migration_knowledge_query::{
                            serving_profile, types::Dataset,
                        };
                        let dataset: Dataset = decode(
                            &serde_json::to_vec(&configuration).map_err(|e| e.to_string())?,
                        )?;
                        if let Some(profile) = serving_profile::Profile::from_dataset(&dataset)
                            .map_err(|e| e.to_string())?
                        {
                            serving_captures.push(ServingCapture {
                                candidate: std::ptr::eq(run, &candidate),
                                suite: suite.id.clone(),
                                class: suite.corpus_class.clone(),
                                configuration: configuration.clone(),
                                dataset,
                                profile: Some(profile),
                                observations: rows(&artifact(root, &p.observations)?)?,
                                scores: scores(
                                    suite,
                                    &golden,
                                    &rows(&artifact(root, &p.observations)?)?,
                                )?,
                            });
                        }
                    } else if let Ok(dataset) = serde_json::from_value(configuration.clone()) {
                        serving_captures.push(ServingCapture {
                            candidate: std::ptr::eq(run, &candidate),
                            suite: suite.id.clone(),
                            class: suite.corpus_class.clone(),
                            configuration: configuration.clone(),
                            dataset,
                            profile: None,
                            observations: rows(&artifact(root, &p.observations)?)?,
                            scores: scores(
                                suite,
                                &golden,
                                &rows(&artifact(root, &p.observations)?)?,
                            )?,
                        });
                    }
                    if suite.kind == "retrieval" {
                        if let Some(profile) = retrieval_v2_profile(&configuration)? {
                            v2_captures.push(V2Capture {
                                candidate: std::ptr::eq(run, &candidate),
                                suite: suite.id.clone(),
                                configuration,
                                profile,
                                scores: scores(
                                    suite,
                                    &golden,
                                    &rows(&artifact(root, &p.observations)?)?,
                                )?,
                            });
                        }
                    }
                }
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
        serving_promotions: if fixture {
            BTreeMap::new()
        } else {
            serving_promotions(&serving_captures)?
        },
        retrieval_v2_promotions: if fixture {
            BTreeMap::new()
        } else {
            v2_promotions(&v2_captures)?
        },
    })
}

use ai_platform_knowledge_service::migration_knowledge_query::retrieval_v2::Profile as RetrievalV2Profile;
struct V2Capture {
    candidate: bool,
    suite: String,
    configuration: Value,
    profile: RetrievalV2Profile,
    scores: BTreeMap<String, BTreeMap<String, f64>>,
}
fn retrieval_v2_profile(configuration: &Value) -> Result<Option<RetrievalV2Profile>> {
    // Canonical full dataset/index configuration. Never trust request-supplied gains.
    let index = configuration.get("index_config").unwrap_or(configuration);
    RetrievalV2Profile::from_index_config(index).map_err(|e| e.to_string())
}
fn without_option(capture: &V2Capture, option: &str) -> Value {
    let mut config = capture.configuration.clone();
    let mut profile = serde_json::to_value(&capture.profile).expect("serializable typed profile");
    match option {
        "normalize" | "detect_language" | "conversational_rewrite" | "multi_query" => {
            profile["query"][option] = json!(false);
            if option == "multi_query" {
                profile["query"]["synonyms"] = json!({});
            }
            if option == "detect_language" {
                profile["query"]["language_weights"] = json!({});
            }
        }
        "rerank_cascade" => {
            profile["cascade"] = Value::Null;
        }
        "rerank_stage_2" => {
            profile["cascade"]["stages"]
                .as_array_mut()
                .expect("validated cascade")
                .truncate(1);
        }
        _ => {
            profile[option] = json!(false);
        }
    }
    // Profile names identify captures; equality covers all executable options instead.
    profile["profile_id"] = json!("");
    if let Some(index) = config.get_mut("index_config") {
        index["retrieval"]["v2"] = profile;
    } else {
        config["retrieval"]["v2"] = profile;
    }
    config
}
fn comparable_configuration(capture: &V2Capture) -> Value {
    let mut config = capture.configuration.clone();
    let mut profile = serde_json::to_value(&capture.profile).expect("serializable typed profile");
    profile["profile_id"] = json!("");
    if !capture.profile.query.multi_query {
        profile["query"]["synonyms"] = json!({});
    }
    if !capture.profile.query.detect_language {
        profile["query"]["language_weights"] = json!({});
    }
    if let Some(index) = config.get_mut("index_config") {
        index["retrieval"]["v2"] = profile;
    } else {
        config["retrieval"]["v2"] = profile;
    }
    config
}
fn activation_fingerprint(capture: &V2Capture) -> Option<String> {
    use ai_platform_knowledge_service::migration_knowledge_query::types::Dataset;
    let dataset: Dataset = serde_json::from_value(capture.configuration.clone()).ok()?;
    if dataset.dataset_id.is_empty()
        || dataset.tenant_id.is_empty()
        || dataset.index_config["retrieval"]["v2"].is_null()
    {
        return None;
    }
    Some(capture.profile.activation_fingerprint(&dataset))
}
fn v2_promotions(captures: &[V2Capture]) -> Result<BTreeMap<String, BTreeMap<String, f64>>> {
    let mut result: BTreeMap<String, BTreeMap<String, f64>> = BTreeMap::new();
    // A profile only appears when every enabled option has a leave-one-option-out
    // control in the same corpus. Existing per-case regression/calibration gates
    // have already validated all captures. A missing ablation never grants a permit.
    let mut by_profile: BTreeMap<String, Vec<&V2Capture>> = BTreeMap::new();
    for capture in captures.iter().filter(|capture| capture.candidate) {
        // A root-only index profile can be measured, but cannot activate a serving
        // dataset without its exact tenant/dataset/model/configuration binding.
        if let Some(fingerprint) = activation_fingerprint(capture) {
            by_profile.entry(fingerprint).or_default().push(capture);
        }
    }
    for (fingerprint, group) in by_profile {
        let mut gains: BTreeMap<String, f64> = BTreeMap::new();
        let mut complete = true;
        for capture in group {
            for option in capture.profile.options() {
                let expected = without_option(capture, &option);
                let controls: Vec<_> = captures
                    .iter()
                    .filter(|control| {
                        control.suite == capture.suite
                            && comparable_configuration(control) == expected
                    })
                    .collect();
                if controls.is_empty() {
                    complete = false;
                    continue;
                }
                for control in controls {
                    if !capture.scores.keys().eq(control.scores.keys()) {
                        return Err("retrieval.v2 ablation case mismatch".into());
                    }
                    let mut gain = 0.;
                    let mut valid = true;
                    for (id, before) in &control.scores {
                        for (metric, value) in before {
                            if capture.scores[id]
                                .get(metric)
                                .is_none_or(|after| after + 1e-12 < *value)
                            {
                                valid = false;
                            }
                        }
                        let Some(before) = before.get("ndcg_at_10") else {
                            valid = false;
                            continue;
                        };
                        let Some(after) = capture.scores[id].get("ndcg_at_10") else {
                            valid = false;
                            continue;
                        };
                        gain += after - before;
                    }
                    gain /= capture.scores.len().max(1) as f64;
                    if !valid || gain <= 1e-12 {
                        complete = false;
                    }
                    gains
                        .entry(option.clone())
                        .and_modify(|g| *g = g.min(gain))
                        .or_insert(gain);
                }
            }
        }
        if complete && !gains.is_empty() {
            result.insert(fingerprint, gains);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod retrieval_v2_promotion_tests {
    use super::*;
    fn capture(candidate: bool, normalize: bool, headers: bool, ndcg: f64) -> V2Capture {
        let config = json!({"tenant_id":"t","dataset_id":"d","embedding_model":"same-pinned-model","index_config":{"retrieval":{"v2":{"version":2,"profile_id":"controlled-candidate","query":{"normalize":normalize},"cascade":null,"contextual_headers":headers,"deduplicate":false}}}});
        V2Capture {
            candidate,
            suite: "reviewed-corpus".into(),
            profile: retrieval_v2_profile(&config).unwrap().unwrap(),
            configuration: config,
            scores: BTreeMap::from([(
                "case".into(),
                BTreeMap::from([
                    ("ndcg_at_10".into(), ndcg),
                    ("recall_at_20".into(), 1.),
                    ("mrr".into(), 1.),
                ]),
            )]),
        }
    }
    #[test]
    fn permit_requires_independent_option_controls_and_actual_gain() {
        let full = capture(true, true, true, 1.);
        let fingerprint = activation_fingerprint(&full).unwrap();
        assert!(v2_promotions(&[full]).unwrap().is_empty());
        let captures = [
            capture(true, true, true, 1.),
            capture(true, false, true, 0.8),
            capture(true, true, false, 0.9),
        ];
        let allowed = v2_promotions(&captures).unwrap();
        assert_eq!(allowed[&fingerprint].len(), 2);
        assert!((allowed[&fingerprint]["normalize"] - 0.2).abs() < 1e-12);
        // Labels and computed observations, not a caller's claimed nDCG gain.
        let mut no_gain = [
            capture(true, true, true, 1.),
            capture(true, false, true, 1.),
            capture(true, true, false, 0.9),
        ];
        assert!(!v2_promotions(&no_gain).unwrap().contains_key(&fingerprint));
        no_gain[1]
            .scores
            .get_mut("case")
            .unwrap()
            .insert("ndcg_at_10".into(), 0.8);
        no_gain[1].configuration["embedding_model"] = json!("different-model");
        assert!(!v2_promotions(&no_gain).unwrap().contains_key(&fingerprint));
    }
    #[test]
    fn baseline_controls_are_valid_but_baseline_profiles_cannot_authorize() {
        let candidate = capture(true, true, false, 1.);
        let fingerprint = activation_fingerprint(&candidate).unwrap();
        let allowed = v2_promotions(&[candidate, capture(false, false, false, 0.7)]).unwrap();
        assert_eq!(allowed[&fingerprint].len(), 1);
        assert!(
            v2_promotions(&[
                capture(false, true, false, 1.),
                capture(false, false, false, 0.7)
            ])
            .unwrap()
            .is_empty()
        );
    }
    #[test]
    fn same_profile_must_gain_on_every_captured_corpus_and_never_regress_a_case() {
        let full = capture(true, true, false, 1.);
        let fingerprint = activation_fingerprint(&full).unwrap();
        let mut other = capture(true, true, false, 0.5);
        other.suite = "another-corpus".into();
        let mut other_control = capture(true, false, false, 0.5);
        other_control.suite = "another-corpus".into();
        assert!(
            !v2_promotions(&[
                full,
                capture(false, false, false, 0.7),
                other,
                other_control
            ])
            .unwrap()
            .contains_key(&fingerprint)
        );
        let full = capture(true, true, false, 1.);
        let mut control = capture(false, false, false, 0.7);
        control
            .scores
            .get_mut("case")
            .unwrap()
            .insert("mrr".into(), 2.);
        assert!(v2_promotions(&[full, control]).unwrap().is_empty());
    }
}

struct ServingCapture {
    candidate: bool,
    suite: String,
    class: String,
    configuration: Value,
    dataset: ai_platform_knowledge_service::migration_knowledge_query::types::Dataset,
    profile:
        Option<ai_platform_knowledge_service::migration_knowledge_query::serving_profile::Profile>,
    observations: BTreeMap<String, Value>,
    scores: BTreeMap<String, BTreeMap<String, f64>>,
}
fn serving_promotions(captures: &[ServingCapture]) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let mut result = BTreeMap::new();
    for c in captures.iter().filter(|c| c.candidate) {
        let Some(profile) = &c.profile else { continue };
        if c.dataset.dataset_id.is_empty() || c.dataset.tenant_id.is_empty() {
            continue;
        }
        let mut permitted = BTreeSet::new();
        for option in profile.options() {
            let evaluated = if option == "index_1_19" && profile.shared.is_some() {
                let mut configuration = c.configuration.clone();
                configuration["index_config"]["serving_v2"]["shared"] = Value::Null;
                let Some(control) = captures.iter().find(|b| {
                    b.candidate && b.suite == c.suite && b.configuration == configuration
                }) else {
                    continue;
                };
                control
            } else {
                c
            };
            let mut expected = evaluated.configuration.clone();
            if option == "layout_parser" {
                if !["scanned", "table"].contains(&c.class.as_str()) {
                    continue;
                }
                expected["index_config"]["serving_v2"]["layout"] = json!(false);
                expected["index_config"]["parsing_v2"]["layout"] = Value::Null;
            } else if option == "multimodal" {
                if c.class != "multimodal" {
                    continue;
                }
                expected["index_config"]["serving_v2"]["multimodal"] = json!(false);
            } else if option == "shared_collection" {
                expected["index_config"]["serving_v2"]["shared"] = Value::Null;
            } else {
                expected["index_config"]["serving_v2"]["index"] = Value::Null;
            }
            // Baseline serving_v2 may be entirely absent once its final option is removed.
            if expected["index_config"]["serving_v2"]["multimodal"] != true
                && expected["index_config"]["serving_v2"]["index"].is_null()
                && expected["index_config"]["serving_v2"]["shared"].is_null()
                && expected["index_config"]["serving_v2"]["layout"] != true
            {
                expected["index_config"]
                    .as_object_mut()
                    .ok_or("configuration index missing")?
                    .remove("serving_v2");
            }
            let controls: Vec<_> = captures
                .iter()
                .filter(|b| b.suite == c.suite && b.configuration == expected)
                .collect();
            let mut passed = false;
            for b in controls {
                if b.scores.keys().ne(evaluated.scores.keys()) {
                    continue;
                }
                let no_regression = b.scores.iter().all(|(id, m)| {
                    m.iter().all(|(k, v)| {
                        evaluated.scores[id].get(k).is_some_and(|a| {
                            if k == "cer" {
                                *a <= *v + 1e-12
                            } else {
                                *a + 1e-12 >= *v
                            }
                        })
                    })
                });
                if !no_regression {
                    continue;
                }
                if option == "layout_parser" {
                    if c.class == "scanned" {
                        passed |= b
                            .scores
                            .iter()
                            .map(|(id, m)| {
                                m.get("cer").copied().unwrap_or(0.)
                                    - evaluated.scores[id].get("cer").copied().unwrap_or(0.)
                            })
                            .sum::<f64>()
                            > 1e-12;
                    } else {
                        passed |= b.scores.values().all(|m| m.contains_key("table_teds_full"))
                            && b.scores
                                .iter()
                                .map(|(id, m)| {
                                    evaluated.scores[id]
                                        .get("table_teds_full")
                                        .copied()
                                        .unwrap_or(0.)
                                        - m.get("table_teds_full").copied().unwrap_or(0.)
                                })
                                .sum::<f64>()
                                > 1e-12;
                    }
                } else if option == "multimodal" {
                    let gain: f64 = evaluated
                        .scores
                        .iter()
                        .map(|(id, m)| {
                            m.get("ndcg_at_10").copied().unwrap_or(0.)
                                - b.scores[id].get("ndcg_at_10").copied().unwrap_or(0.)
                        })
                        .sum();
                    passed |= gain > 1e-12;
                } else if option == "shared_collection" {
                    passed|=b.observations.iter().all(|(id,b)|{let a=&evaluated.observations[id];let Some(before)=b["bm25_scores"].as_object()else{return false};let Some(after)=a["bm25_scores"].as_object()else{return false};!before.is_empty() && before.len()==after.len() && before.iter().all(|(id,score)|matches!((score.as_f64(),after.get(id).and_then(Value::as_f64)),(Some(x),Some(y)) if x.is_finite() && y.is_finite() && (x-y).abs()<=1e-6)) && a["cross_acl_hits"]==0 && a["resources"]["qdrant_version"]=="1.19.2"});
                } else {
                    passed |= b.observations.iter().all(|(id, b)| {
                        let a = &evaluated.observations[id];
                        let before = b["resources"]["index_resident_bytes"].as_u64();
                        let after = a["resources"]["index_resident_bytes"].as_u64();
                        matches!((before,after),(Some(x),Some(y)) if y>0 && x/y>=4)
                            && b["resources"]["points"] == a["resources"]["points"]
                            && a["resources"]["points"].as_u64().unwrap_or(0) > 0
                            && b["resources"]["qdrant_version"] == "1.19.2"
                            && a["resources"]["qdrant_version"] == "1.19.2"
                    });
                }
            }
            if passed {
                permitted.insert(if option == "layout_parser" {
                    format!("layout_{}", c.class)
                } else {
                    option
                });
            }
        }
        result
            .entry(profile.fingerprint(&c.dataset))
            .or_insert_with(BTreeSet::new)
            .extend(permitted);
    }
    for options in result.values_mut() {
        if options.contains("layout_scanned") && options.contains("layout_table") {
            options.insert("layout_parser".into());
        }
        options.remove("layout_scanned");
        options.remove("layout_table");
    }
    result.retain(|fingerprint, options| {
        captures
            .iter()
            .filter_map(|c| c.profile.as_ref().map(|p| (p, &c.dataset)))
            .find(|(p, d)| p.fingerprint(d) == *fingerprint)
            .is_some_and(|(p, _)| p.options().is_subset(options))
    });
    Ok(result)
}

fn full_table_teds(
    expected: &ai_platform_knowledge_service::migration_knowledge_ingest::parser::DocIR,
    actual: &ai_platform_knowledge_service::migration_knowledge_ingest::parser::DocIR,
) -> Result<Option<f64>> {
    let expected: Vec<_> = expected
        .iter_blocks()
        .into_iter()
        .filter(|b| b.kind == "table")
        .collect();
    if expected.is_empty()
        || expected
            .iter()
            .any(|b| b.table.as_ref().and_then(|v| v["html"].as_str()).is_none())
    {
        return Ok(None);
    }
    let actual: Vec<_> = actual
        .iter_blocks()
        .into_iter()
        .filter(|b| b.kind == "table")
        .collect();
    let denominator = expected.len().max(actual.len());
    let mut sum = 0.;
    for (a, b) in expected.iter().zip(&actual) {
        if let Some(html) = b.table.as_ref().and_then(|v| v["html"].as_str()) {
            sum +=
                ai_platform_knowledge_service::migration_knowledge_ingest::table_teds::score_html(
                    a.table
                        .as_ref()
                        .and_then(|v| v["html"].as_str())
                        .ok_or("golden HTML table missing")?,
                    html,
                )
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(Some(sum / denominator as f64))
}
#[cfg(test)]
mod serving_gate_tests {
    use super::*;
    use ai_platform_knowledge_service::migration_knowledge_query::{
        serving_profile, types::Dataset,
    };
    fn captured(multimodal: bool, score: f64) -> ServingCapture {
        let mut configuration = json!({"dataset_id":"d","tenant_id":"t","embedding_provider":"unified_multimodal","embedding_model":"tongyi-embedding-vision-plus","embedding_model_version":"revision","embedding_dimension":1024,"index_config":{}});
        if multimodal {
            configuration["index_config"]["serving_v2"] =
                json!({"schema":"knowledge-serving/v1","multimodal":true});
        }
        let dataset: Dataset = serde_json::from_value(configuration.clone()).unwrap();
        let profile = serving_profile::Profile::from_dataset(&dataset).unwrap();
        ServingCapture {
            candidate: multimodal,
            suite: "mm".into(),
            class: "multimodal".into(),
            configuration,
            dataset,
            profile,
            observations: BTreeMap::from([("case".into(), json!({"status":"succeeded"}))]),
            scores: BTreeMap::from([(
                "case".into(),
                BTreeMap::from([
                    ("ndcg_at_10".into(), score),
                    ("recall_at_20".into(), 1.),
                    ("mrr".into(), 1.),
                ]),
            )]),
        }
    }
    #[test]
    fn multimodal_permit_requires_an_independent_gain_and_bound_control() {
        assert!(
            serving_promotions(&[captured(true, 0.8)])
                .unwrap()
                .is_empty()
        );
        assert!(
            serving_promotions(&[captured(true, 0.8), captured(false, 0.8)])
                .unwrap()
                .is_empty()
        );
        let captures = [captured(true, 0.9), captured(false, 0.8)];
        let bindings = serving_promotions(&captures).unwrap();
        assert_eq!(bindings.len(), 1);
        assert!(bindings.values().next().unwrap().contains("multimodal"));
        let mut other = captured(false, 0.8);
        other.configuration["tenant_id"] = json!("other");
        assert!(
            serving_promotions(&[captured(true, 0.9), other])
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn layout_and_quantization_cannot_promote_without_their_measurements() {
        let mut c = captured(false, 1.);
        c.candidate = true;
        c.configuration["index_config"]["serving_v2"] =
            json!({"schema":"knowledge-serving/v1","index":{"quantization":"turbo","tier":"hot"}});
        c.dataset = serde_json::from_value(c.configuration.clone()).unwrap();
        c.profile = serving_profile::Profile::from_dataset(&c.dataset).unwrap();
        assert!(
            serving_promotions(&[c, captured(false, 1.)])
                .unwrap()
                .is_empty()
        );
    }
}
