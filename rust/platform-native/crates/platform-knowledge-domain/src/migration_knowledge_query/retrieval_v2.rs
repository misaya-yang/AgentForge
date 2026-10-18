//! Opt-in retrieval quality profile. Dataset input never grants promotion authority.
use super::{
    config::RetrieveRequest,
    lexical_v2,
    types::{Candidate, Dataset, QueryError, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, OnceLock},
    time::{Duration, Instant},
};
use unicode_casefold::UnicodeCaseFold;
use unicode_normalization::UnicodeNormalization;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct QueryUnderstanding {
    pub normalize: bool,
    pub detect_language: bool,
    /// Optional evaluated fusion weights by detected script; never an ACL/language filter.
    pub language_weights: BTreeMap<String, [f64; 2]>,
    pub conversational_rewrite: bool,
    pub multi_query: bool,
    /// Reviewed corpus-specific alternatives, never an unbounded model-generated list.
    pub synonyms: BTreeMap<String, Vec<String>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RerankStage {
    pub provider: String,
    pub model: String,
    pub revision: String,
    pub candidates: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Cascade {
    pub budget_ms: u64,
    pub stages: Vec<RerankStage>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u32,
    pub profile_id: String,
    #[serde(default)]
    pub query: QueryUnderstanding,
    pub cascade: Option<Cascade>,
    #[serde(default)]
    pub contextual_headers: bool,
    #[serde(default)]
    pub deduplicate: bool,
}
fn implementation_fingerprint() -> &'static str {
    use sha2::{Digest, Sha256};
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        format!(
            "{:x}",
            Sha256::digest(
                concat!(
                    include_str!("retrieval_v2.rs"),
                    include_str!("index.rs"),
                    include_str!("rerank.rs"),
                    include_str!("config.rs"),
                    include_str!("service.rs"),
                    include_str!("../migration_knowledge_ingest/parsing_v2/integration.rs")
                )
                .as_bytes()
            )
        )
    })
}
fn invalid(message: &str) -> QueryError {
    QueryError::Validation(message.into())
}
impl Profile {
    pub fn from_dataset(dataset: &Dataset) -> Result<Option<Self>> {
        Self::from_index_config(&dataset.index_config)
    }
    pub fn from_index_config(index: &Value) -> Result<Option<Self>> {
        let profile = Self::parse(&index["retrieval"]["v2"])?;
        if profile.as_ref().is_some_and(|p| p.contextual_headers)
            && index["chunking"]["mode"]
                .as_str()
                .is_some_and(|mode| mode.eq_ignore_ascii_case("hierarchical"))
        {
            return Err(invalid(
                "retrieval.v2 contextual headers require standard chunk indexing",
            ));
        }
        Ok(profile)
    }
    pub fn parse(raw: &Value) -> Result<Option<Self>> {
        if raw.is_null() || raw == &Value::Bool(false) {
            return Ok(None);
        }
        // Typed deny_unknown_fields rejects endpoint/credential options at every
        // configuration layer. Synonym dictionary keys are query data ("token"
        // and "endpoint" are legitimate terms), not provider configuration.
        let p: Self = serde_json::from_value(raw.clone())
            .map_err(|_| invalid("invalid retrieval.v2 profile"))?;
        p.validate()?;
        Ok(Some(p))
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 2 || self.profile_id.trim().is_empty() || self.profile_id.len() > 128 {
            return Err(invalid(
                "retrieval.v2 requires version=2 and a bounded profile_id",
            ));
        }
        if self.query.language_weights.len() > 7
            || self.query.language_weights.iter().any(|(lang, weights)| {
                !["en", "zh", "ja", "ko", "ar", "ru", "und"].contains(&lang.as_str())
                    || weights
                        .iter()
                        .any(|w| !w.is_finite() || !(0.0..=1.0).contains(w))
                    || weights[0] + weights[1] <= 0.
            })
        {
            return Err(invalid("invalid retrieval.v2 language fusion weights"));
        }
        if self.query.synonyms.len() > 64
            || self.query.synonyms.iter().any(|(k, vs)| {
                k.trim().is_empty()
                    || k.chars().count() > 128
                    || vs.len() > 2
                    || vs
                        .iter()
                        .any(|v| v.trim().is_empty() || v.chars().count() > 256)
            })
        {
            return Err(invalid("retrieval.v2 synonym budget exceeded"));
        }
        if let Some(c) = &self.cascade {
            if !(50..=2500).contains(&c.budget_ms) || !(1..=2).contains(&c.stages.len()) {
                return Err(invalid(
                    "rerank cascade requires 1-2 stages and 50-2500ms budget",
                ));
            }
            for (i, s) in c.stages.iter().enumerate() {
                if s.provider.trim().is_empty()
                    || s.model.trim().is_empty()
                    || s.revision.trim().is_empty()
                    || s.provider.len() > 128
                    || s.model.len() > 256
                    || s.revision.len() > 256
                    || s.candidates == 0
                    || s.candidates > if i == 0 { 50 } else { 10 }
                    || (i == 0 && !(s.provider == "bge" || s.provider == "local"))
                    || (i == 1
                        && (s.candidates > c.stages[0].candidates
                            || s.provider == "bge"
                            || s.provider == "local"))
                {
                    return Err(invalid("invalid pinned CPU-first rerank cascade stage"));
                }
            }
        }
        Ok(())
    }
    pub fn fingerprint(&self) -> String {
        lexical_v2::fingerprint(&serde_json::to_value(self).expect("serializable profile"))
    }
    /// Bind executable configuration and semantic model identity, while allowing
    /// ordinary document publication to advance the dataset generation.
    pub fn activation_fingerprint(&self, dataset: &Dataset) -> String {
        lexical_v2::fingerprint(
            &json!({"implementation":implementation_fingerprint(),"profile":self,"tenant_id":dataset.tenant_id,"dataset_id":dataset.dataset_id,
            "embedding_provider":dataset.embedding_provider,"embedding_model":dataset.embedding_model,
            "embedding_model_version":dataset.embedding_model_version,"embedding_dimension":dataset.embedding_dimension,
            "embedding_config":dataset.embedding_config,"index_config":dataset.index_config}),
        )
    }
    pub fn options(&self) -> BTreeSet<String> {
        let mut options = BTreeSet::new();
        for (name, enabled) in [
            ("normalize", self.query.normalize),
            ("detect_language", self.query.detect_language),
            ("conversational_rewrite", self.query.conversational_rewrite),
            ("multi_query", self.query.multi_query),
            ("rerank_cascade", self.cascade.is_some()),
            (
                "rerank_stage_2",
                self.cascade.as_ref().is_some_and(|c| c.stages.len() == 2),
            ),
            ("contextual_headers", self.contextual_headers),
            ("deduplicate", self.deduplicate),
        ] {
            if enabled {
                options.insert(name.into());
            }
        }
        options
    }
}

/// Server-owned output of the native evaluation gate, never accepted from a dataset.
/// The gate binds exact canonical profile bytes and independent option gains.
#[derive(Clone, Default)]
pub struct Promotions {
    allowed: Arc<BTreeMap<String, BTreeMap<String, f64>>>,
    error: Option<String>,
}
impl Promotions {
    pub fn from_gate_decision(raw: &Value) -> Result<Self> {
        if raw["eligible"] != true
            || raw["promotable"] != true
            || raw["fixture_only"] != false
            || raw["cases"].as_u64().unwrap_or(0) == 0
        {
            return Err(invalid(
                "retrieval.v2 requires a real eligible promotion decision",
            ));
        }
        let allowed: BTreeMap<String, BTreeMap<String, f64>> =
            serde_json::from_value(raw["retrieval_v2_promotions"].clone()).map_err(|_| {
                invalid("promotion decision lacks retrieval.v2 configuration bindings")
            })?;
        if allowed.len() > 256
            || allowed.iter().any(|(key, options)| {
                !key.strip_prefix("sha256:")
                    .is_some_and(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
                    || options.is_empty()
                    || options
                        .values()
                        .any(|gain| !gain.is_finite() || *gain <= 0. || *gain > 1.)
            })
        {
            return Err(invalid("invalid retrieval.v2 promotion bindings/gains"));
        }
        Ok(Self {
            allowed: Arc::new(allowed),
            error: None,
        })
    }
    pub fn from_server_environment() -> Self {
        let Some(path) = std::env::var_os("KNOWLEDGE_RETRIEVAL_V2_PROMOTIONS_FILE") else {
            return Self::default();
        };
        let loaded = (|| {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .map_err(|_| invalid("cannot open retrieval.v2 promotion decision"))?
                .take(1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| invalid("cannot read retrieval.v2 promotion decision"))?;
            if bytes.len() > 1024 * 1024 {
                return Err(invalid(
                    "retrieval.v2 promotion decision exceeds byte budget",
                ));
            }
            let raw = serde_json::from_slice(&bytes)
                .map_err(|_| invalid("invalid retrieval.v2 promotion decision JSON"))?;
            Self::from_gate_decision(&raw)
        })();
        loaded.unwrap_or_else(|e| Self {
            error: Some(e.to_string()),
            ..Self::default()
        })
    }
    pub fn require(&self, profile: &Profile, dataset: &Dataset) -> Result<()> {
        profile.validate()?;
        if let Some(error) = &self.error {
            return Err(invalid(error));
        }
        if profile.options().is_empty() {
            return Ok(());
        }
        let options = self
            .allowed
            .get(&profile.activation_fingerprint(dataset))
            .ok_or_else(|| {
                invalid("retrieval.v2 profile has not passed the evaluation/promotion gate")
            })?;
        if profile
            .options()
            .iter()
            .any(|option| !options.contains_key(option))
        {
            return Err(invalid(
                "each enabled retrieval.v2 option requires a measured nDCG@10 gain",
            ));
        }
        Ok(())
    }
}

pub fn normalize(text: &str) -> String {
    text.nfkc()
        .case_fold()
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
/// Script detection is deliberately conservative: it never adds a recall filter.
pub fn language(text: &str) -> &'static str {
    let mut scripts = BTreeSet::new();
    for c in text.chars() {
        match c as u32 {
            0x3040..=0x30ff => {
                return "ja";
            }
            0xac00..=0xd7af => {
                return "ko";
            }
            0x4e00..=0x9fff => {
                scripts.insert("zh");
            }
            0x0600..=0x06ff => {
                scripts.insert("ar");
            }
            0x0400..=0x04ff => {
                scripts.insert("ru");
            }
            _ if c.is_ascii_alphabetic() => {
                scripts.insert("en");
            }
            _ => {}
        }
    }
    if scripts.len() == 1 {
        scripts.into_iter().next().unwrap_or("und")
    } else {
        "und"
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Turn {
    role: String,
    content: String,
}
/// A maximum of three queries per input; all filters and request authority survive cloning.
pub fn understand(request: &RetrieveRequest, p: &Profile) -> Result<(Vec<RetrieveRequest>, Value)> {
    let mut primary = request.clone();
    if p.query.normalize {
        primary.query = normalize(&primary.query);
    }
    let detected = if p.query.detect_language {
        Some(language(&primary.query))
    } else {
        None
    };
    let mut rewritten = false;
    if p.query.conversational_rewrite {
        if let Some(raw) = request.extra.get("conversation") {
            let turns: Vec<Turn> = serde_json::from_value(raw.clone())
                .map_err(|_| invalid("conversation must contain role/content turns"))?;
            if turns.len() > 8
                || turns.iter().any(|t| {
                    !["user", "assistant"].contains(&t.role.as_str())
                        || t.content.chars().count() > 1024
                })
                || turns
                    .iter()
                    .map(|t| t.content.chars().count())
                    .sum::<usize>()
                    > 4096
            {
                return Err(invalid("conversation budget exceeded"));
            }
            // Only user questions become retrieval terms. Assistant output cannot rewrite authority.
            if let Some(context) = turns
                .iter()
                .rev()
                .find(|t| t.role == "user" && !t.content.trim().is_empty())
            {
                let context = if p.query.normalize {
                    normalize(&context.content)
                } else {
                    context.content.trim().into()
                };
                primary.query = format!("{context}\n{}", primary.query);
                rewritten = true;
            }
        }
    }
    if !(1..=4096).contains(&primary.query.chars().count()) {
        return Err(invalid("rewritten query exceeds 4096 characters"));
    }
    let mut queries = vec![primary.clone()];
    if p.query.multi_query
        && request.query.split_whitespace().count() <= 8
        && request.query.chars().count() <= 128
    {
        let key = normalize(&request.query);
        if let Some(alternatives) = p.query.synonyms.get(&key) {
            for alternative in alternatives {
                let mut r = primary.clone();
                r.query = if rewritten {
                    format!("{}\n{alternative}", primary.query)
                } else {
                    alternative.clone()
                };
                if r.query.chars().count() <= 4096
                    && !queries
                        .iter()
                        .any(|q| normalize(&q.query) == normalize(&r.query))
                {
                    queries.push(r);
                }
            }
        }
        // Corpus-neutral identifier expansion complements reviewed synonym entries.
        let expanded: String = primary
            .query
            .chars()
            .map(|c| if matches!(c, '_' | '-' | '/') { ' ' } else { c })
            .collect();
        let expanded = expanded.split_whitespace().collect::<Vec<_>>().join(" ");
        if queries.len() < 3
            && !queries
                .iter()
                .any(|q| normalize(&q.query) == normalize(&expanded))
        {
            let mut r = primary.clone();
            r.query = expanded;
            queries.push(r);
        }
    }
    let meta = json!({"profile_fingerprint":p.fingerprint(),"normalize":p.query.normalize,"detected_language":detected,"conversational_rewrite":rewritten,"query_count":queries.len()});
    Ok((queries, meta))
}

/// Bounded MinHash over character shingles supports whitespace-free scripts too.
fn signature(text: &str) -> (usize, [u64; 32]) {
    let chars: Vec<_> = normalize(text).chars().take(16_384).collect();
    let mut signature = [u64::MAX; 32];
    for shingle in chars.windows(5).step_by((chars.len() / 512).max(1)) {
        let mut hash = 0xcbf29ce484222325u64;
        for c in shingle {
            hash = (hash ^ (*c as u64)).wrapping_mul(0x100000001b3);
        }
        for (i, slot) in signature.iter_mut().enumerate() {
            let mut h = hash ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15);
            h = (h ^ (h >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            h = (h ^ (h >> 27)).wrapping_mul(0x94d049bb133111eb);
            *slot = (*slot).min(h ^ (h >> 31));
        }
    }
    (chars.len(), signature)
}
pub fn deduplicate(ranked: &mut Vec<Candidate>) -> usize {
    let mut kept: Vec<(String, usize, [u64; 32])> = Vec::new();
    let before = ranked.len();
    ranked.retain(|candidate| {
        let text = normalize(&candidate.text);
        let (len, sig) = signature(&text);
        let duplicate = kept.iter().any(|(other, size, fingerprint)| {
            other == &text
                || (len >= 32
                    && len.max(*size) as f64 / len.min(*size).max(1) as f64 <= 1.15
                    && sig.iter().zip(fingerprint).filter(|(a, b)| a == b).count() >= 29)
        });
        if !duplicate {
            kept.push((text, len, sig));
        }
        !duplicate
    });
    before - ranked.len()
}

pub struct MixedContext<'a> {
    pub store: &'a super::store::QueryStore,
    pub assets: &'a crate::migration_knowledge_ingest::assets::AssetStorage,
    pub dataset: &'a Dataset,
}
pub async fn rerank_cascade(
    ranked: &mut [Candidate],
    query: &str,
    cascade: &Cascade,
    deadline: Instant,
) -> (bool, Value) {
    rerank_cascade_mixed(ranked, query, cascade, deadline, None).await
}
pub async fn rerank_cascade_mixed(
    ranked: &mut [Candidate],
    query: &str,
    cascade: &Cascade,
    deadline: Instant,
    mixed: Option<MixedContext<'_>>,
) -> (bool, Value) {
    let end = deadline.min(Instant::now() + Duration::from_millis(cascade.budget_ms));
    let mut stages = Vec::new();
    let mut applied = false;
    for (i, stage) in cascade.stages.iter().enumerate() {
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining < Duration::from_millis(25) {
            stages.push(json!({"stage":i+1,"status":"budget_exhausted"}));
            break;
        }
        let count = ranked.len().min(stage.candidates);
        let docs: Vec<_> = ranked[..count].iter().map(|c| c.text.clone()).collect();
        let started = Instant::now();
        let result = tokio::time::timeout(remaining, async {
            if i > 0
                && stage.model == "qwen3-vl-rerank"
                && ranked[..count]
                    .iter()
                    .any(|c| super::hierarchy::contains_multimodal(&c.metadata, 0))
            {
                let context = mixed
                    .as_ref()
                    .ok_or_else(|| invalid("VL rerank requires an object-source adapter"))?;
                let documents = mixed_documents(&ranked[..count], context).await?;
                super::rerank::governed_vision(stage, query, &documents, remaining).await
            } else {
                super::rerank::governed(stage, query, &docs, i == 0, remaining).await
            }
        })
        .await;
        match result {
            Ok(Ok(scores)) => {
                apply_stage_scores(&mut ranked[..count], scores, i + 1);
                stages.push(json!({"stage":i+1,"status":"applied","provider":stage.provider,"model":stage.model,"revision":stage.revision,"candidates":count,"elapsed_ms":started.elapsed().as_millis()}));
                applied = true;
            }
            Ok(Err(_)) => {
                stages.push(json!({"stage":i+1,"status":"error"}));
                break;
            }
            Err(_) => {
                stages.push(json!({"stage":i+1,"status":"timeout"}));
                break;
            }
        }
    }
    (
        applied,
        json!({"budget_ms":cascade.budget_ms,"stages":stages}),
    )
}

async fn mixed_documents(
    candidates: &[Candidate],
    context: &MixedContext<'_>,
) -> Result<Vec<Value>> {
    use base64::Engine;
    if !super::serving_profile::multimodal(context.dataset)? {
        return Err(invalid("VL rerank profile is not promoted"));
    }
    let mut documents = vec![];
    let mut total = 0usize;
    for candidate in candidates {
        if !super::hierarchy::contains_multimodal(&candidate.metadata, 0) {
            documents.push(json!({"text":candidate.text}));
            continue;
        }
        let chunk = context
            .store
            .segment_scoped(context.dataset, &candidate.segment_id)
            .await?
            .ok_or_else(|| invalid("VL rerank image source is inactive"))?;
        if chunk.content_type != "image" || chunk.document_id != candidate.document_id {
            return Err(invalid("VL rerank image source identity differs"));
        }
        let mime = chunk
            .image_media_type
            .as_deref()
            .filter(|m| ["image/png", "image/jpeg", "image/webp"].contains(m))
            .ok_or_else(|| invalid("VL rerank image media type unsupported"))?;
        let key = crate::migration_knowledge_ingest::assets::AssetStorage::image_key(
            &context.dataset.tenant_id,
            &chunk.document_id,
            chunk
                .image_attachment_id
                .as_deref()
                .ok_or_else(|| invalid("image attachment missing"))?,
            chunk
                .image_filename
                .as_deref()
                .ok_or_else(|| invalid("image filename missing"))?,
        )
        .map_err(|_| invalid("image source scope invalid"))?;
        let bytes = context
            .assets
            .download_bounded(&key, 3 * 1024 * 1024)
            .await
            .map_err(|_| invalid("VL rerank source unavailable"))?;
        total += bytes.len();
        if total > 8 * 1024 * 1024 {
            return Err(invalid("VL rerank image byte budget exceeded"));
        }
        documents.push(json!({"text":candidate.text,"image":format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes))}));
    }
    Ok(documents)
}
fn apply_stage_scores(
    ranked: &mut [Candidate],
    scores: Vec<super::rerank::RerankResult>,
    stage: usize,
) {
    // governed() has validated a complete, unique, finite score for every input.
    for score in scores {
        let c = &mut ranked[score.index];
        c.final_score = score.relevance_score;
        c.rerank_score = Some(score.relevance_score);
        c.metadata["_rerank_cascade_stage"] = json!(stage);
        c.metadata[format!("_rerank_stage_{stage}_score")] = json!(score.relevance_score);
    }
    ranked.sort_by(|a, b| b.final_score.total_cmp(&a.final_score));
}

#[cfg(test)]
#[path = "retrieval_v2_tests.rs"]
mod tests;
