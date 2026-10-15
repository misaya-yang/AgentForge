//! Versioned lexical schema and receipt validation, preserving the Python wire fingerprints.
use super::{
    qdrant::{QdrantStore, id_string},
    types::{Dataset, QueryError, Result},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
const CLIENT_VERSION: &str = "1.18.0";
#[derive(Clone, Debug)]
pub struct LexicalProfile {
    pub active: bool,
    pub shadow: bool,
    pub options: Value,
    pub filtering: Value,
    pub runtime_revision: i64,
}
fn invalid(message: &str) -> QueryError {
    QueryError::CollectionAuthority(message.into())
}
pub fn canonical_json(v: &Value) -> Value {
    match v {
        Value::Object(m) => {
            let sorted: std::collections::BTreeMap<_, _> = m
                .iter()
                .map(|(k, v)| (k.clone(), canonical_json(v)))
                .collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(a) => Value::Array(a.iter().map(canonical_json).collect()),
        _ => v.clone(),
    }
}
pub fn fingerprint(v: &Value) -> String {
    format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&canonical_json(v)).unwrap_or_default())
    )
}
impl LexicalProfile {
    pub fn from_dataset(d: &Dataset) -> Result<Self> {
        let raw = &d.index_config["retrieval"]["lexical"];
        if !raw.is_null() && !raw.is_object() {
            return Err(invalid("retrieval.lexical must be an object"));
        }
        if let Some(m) = raw.as_object() {
            if m.keys()
                .any(|k| !["active_version", "bm25_v2"].contains(&k.as_str()))
            {
                return Err(invalid("unsupported retrieval.lexical option"));
            }
        }
        Self::parse(
            raw["active_version"].as_str().unwrap_or("lexical_v1"),
            &raw["bm25_v2"],
            &raw["bm25_v2"]["filtering"],
            0,
        )
    }
    pub fn from_collection(raw: &Value) -> Result<Self> {
        if raw["schema_version"] != 2 {
            return Err(invalid("invalid lexical metadata schema"));
        }
        Self::parse(
            raw["active_version"].as_str().unwrap_or("lexical_v1"),
            &raw["bm25_v2"],
            &raw["filtering"],
            raw["runtime_revision"].as_i64().unwrap_or(0),
        )
    }
    fn parse(active: &str, bm25: &Value, filter: &Value, revision: i64) -> Result<Self> {
        if !["lexical_v1", "bm25_v2"].contains(&active) {
            return Err(invalid(
                "retrieval.lexical.active_version must be lexical_v1|bm25_v2",
            ));
        }
        if !bm25.is_null() && !bm25.is_object() {
            return Err(invalid("bm25_v2 must be an object"));
        }
        let shadow = match bm25.get("shadow_write_enabled") {
            None => false,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| invalid("shadow_write_enabled must be boolean"))?,
        };
        if active == "bm25_v2" && !shadow {
            return Err(invalid("bm25_v2 reads require shadow_write_enabled=true"));
        }
        let mut options = json!({"field":"bm25_v2","model":"qdrant/bm25","encoder_implementation":"qdrant-cluster-inference","encoder_contract_version":"qdrant-bm25-document-v1","qdrant_client_version":CLIENT_VERSION,"k":1.2,"b":0.75,"avg_len":256.0,"tokenizer":"multilingual","language":"none","lowercase":true,"ascii_folding":false,"stopwords":null,"stemmer":null,"min_token_len":null,"max_token_len":null});
        if let Some(m) = bm25.as_object() {
            for (k, v) in m {
                if [
                    "shadow_write_enabled",
                    "schema_fingerprint",
                    "semantics",
                    "filtering",
                ]
                .contains(&k.as_str())
                {
                    continue;
                }
                if !options.get(k).is_some() {
                    return Err(invalid("unsupported bm25_v2 option"));
                }
                if [
                    "field",
                    "model",
                    "encoder_implementation",
                    "encoder_contract_version",
                    "qdrant_client_version",
                ]
                .contains(&k.as_str())
                    && v != &options[k]
                {
                    return Err(invalid("bm25_v2 encoder contract mismatch"));
                }
                options[k] = v.clone();
            }
        }
        for key in ["k", "avg_len"] {
            if !options[key]
                .as_f64()
                .is_some_and(|v| v.is_finite() && v > 0.0)
            {
                return Err(invalid("bm25_v2 k/avg_len must be finite and positive"));
            }
        }
        if !options["b"]
            .as_f64()
            .is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v))
        {
            return Err(invalid("bm25_v2.b must be between 0 and 1"));
        }
        for key in ["lowercase", "ascii_folding"] {
            if !options[key].is_boolean() {
                return Err(invalid("bm25_v2 preprocessing flags must be boolean"));
            }
        }
        if !options["stopwords"].is_null() || !options["stemmer"].is_null() {
            return Err(invalid("bm25_v2 stopwords/stemmer are fixed by language"));
        }
        if !["prefix", "whitespace", "word", "multilingual"]
            .contains(&options["tokenizer"].as_str().unwrap_or(""))
        {
            return Err(invalid("unsupported bm25_v2 tokenizer"));
        }
        if ![
            "none",
            "arabic",
            "azerbaijani",
            "basque",
            "bengali",
            "catalan",
            "chinese",
            "danish",
            "dutch",
            "english",
            "finnish",
            "french",
            "german",
            "greek",
            "hebrew",
            "hinglish",
            "hungarian",
            "indonesian",
            "italian",
            "japanese",
            "kazakh",
            "nepali",
            "norwegian",
            "portuguese",
            "romanian",
            "russian",
            "slovene",
            "spanish",
            "swedish",
            "tajik",
            "turkish",
        ]
        .contains(&options["language"].as_str().unwrap_or(""))
        {
            return Err(invalid("unsupported bm25_v2 language"));
        }
        for key in ["min_token_len", "max_token_len"] {
            if !options[key].is_null() && !options[key].as_u64().is_some_and(|v| v > 0) {
                return Err(invalid("bm25_v2 token bounds must be positive integers"));
            }
        }
        if let (Some(min), Some(max)) = (
            options["min_token_len"].as_u64(),
            options["max_token_len"].as_u64(),
        ) {
            if min > max {
                return Err(invalid("min_token_len exceeds max_token_len"));
            }
        }
        if !filter.is_null() && !filter.is_object() {
            return Err(invalid("filtering must be an object"));
        }
        if let Some(m) = filter.as_object() {
            if m.keys().any(|k| {
                ![
                    "required_payload_indexes",
                    "strict_unindexed_filtering",
                    "profile_fingerprint",
                ]
                .contains(&k.as_str())
            }) {
                return Err(invalid("unsupported filtering option"));
            }
        }
        if !filter["required_payload_indexes"].is_null()
            && filter["required_payload_indexes"] != json!(["tenant_id", "dataset_id"])
        {
            return Err(invalid("required filter indexes cannot be weakened"));
        }
        let strict = match filter.get("strict_unindexed_filtering") {
            None => false,
            Some(v) => v
                .as_bool()
                .ok_or_else(|| invalid("strict_unindexed_filtering must be boolean"))?,
        };
        // Python coerces the three floating options before computing its JSON hash.
        for k in ["k", "b", "avg_len"] {
            options[k] = json!(options[k].as_f64().unwrap_or(0.0));
        }
        Ok(Self {
            active: active == "bm25_v2",
            shadow,
            options,
            filtering: json!({"required_payload_indexes":["tenant_id","dataset_id"],"strict_unindexed_filtering":strict}),
            runtime_revision: revision,
        })
    }
    pub fn schema_fingerprint(&self) -> String {
        fingerprint(&self.options)
    }
    pub fn filtering_fingerprint(&self) -> String {
        fingerprint(&self.filtering)
    }
    pub fn collection_profile(&self) -> Value {
        let mut bm25 = self.options.clone();
        bm25["shadow_write_enabled"] = json!(self.shadow);
        bm25["schema_fingerprint"] = json!(self.schema_fingerprint());
        bm25["semantics"] = json!("qdrant_bm25_tf_idf_document_length_normalized");
        let mut filtering = self.filtering.clone();
        filtering["profile_fingerprint"] = json!(self.filtering_fingerprint());
        json!({"schema_version":2,"runtime_revision":self.runtime_revision,"active_version":if self.active{"bm25_v2"}else{"lexical_v1"},"legacy":{"version":"lexical_v1","field":"bm25","semantics":"hashed_term_presence_plus_qdrant_idf"},"bm25_v2":bm25,"filtering":filtering})
    }
    pub fn document(&self, text: &str) -> Value {
        let mut opts = self.options.clone();
        if let Some(m) = opts.as_object_mut() {
            for k in [
                "field",
                "model",
                "encoder_implementation",
                "encoder_contract_version",
                "qdrant_client_version",
            ] {
                m.remove(k);
            }
        }
        json!({"text":text,"model":"qdrant/bm25","options":opts})
    }
    pub fn marker(&self, text: &str) -> Value {
        json!({"versions":["lexical_v1","bm25_v2"],"bm25_v2_schema_fingerprint":self.schema_fingerprint(),"filtering_profile_fingerprint":self.filtering_fingerprint(),"source_text_sha256":format!("{:x}",Sha256::digest(text.as_bytes()))})
    }
}
pub fn validate_collection_profile(raw: &Value) -> Result<()> {
    let profile = LexicalProfile::from_collection(raw)?;
    if raw["bm25_v2"]["schema_fingerprint"] != profile.schema_fingerprint()
        || raw["filtering"]["profile_fingerprint"] != profile.filtering_fingerprint()
    {
        return Err(invalid("collection lexical profile fingerprint mismatch"));
    }
    Ok(())
}
pub fn eligible(payload: &Value) -> bool {
    payload["content_type"].as_str().unwrap_or("text") == "text"
        && (payload["level"].is_null() || payload["level"].as_i64() == Some(3))
        && (payload["enabled"].is_null() || payload["enabled"].as_bool() == Some(true))
}
pub fn lexical_filter(dataset: &Dataset) -> Value {
    let mut filter = QdrantStore::filter(dataset, None, false);
    for (key, value) in [
        ("content_type", json!("text")),
        ("level", json!(3)),
        ("enabled", json!(true)),
    ] {
        if let Some(m) = filter["must"].as_array_mut() {
            m.push(
                json!({"should":[{"key":key,"match":{"value":value}},{"is_empty":{"key":key}}]}),
            );
        }
    }
    filter
}
pub async fn verify_active(
    store: &QdrantStore,
    d: &Dataset,
    profile: &LexicalProfile,
    info: &Value,
) -> Result<Value> {
    let raw = &info["config"]["metadata"]["knowledge_lexical"];
    validate_collection_profile(raw)?;
    let stored = LexicalProfile::from_collection(raw)?;
    store.ensure_bm25_v2_capability(&stored).await?;
    store
        .ensure_filtering_profile(d.collection()?, &stored, false, None, true)
        .await?;
    if !stored.active
        || profile.schema_fingerprint() != stored.schema_fingerprint()
        || profile.filtering_fingerprint() != stored.filtering_fingerprint()
    {
        return Err(invalid("Qdrant and PostgreSQL lexical selections disagree"));
    }
    for field in ["bm25", "bm25_v2"] {
        if info["config"]["params"]["sparse_vectors"][field]["modifier"] != "idf" {
            return Err(invalid(
                "collection must retain both sparse fields with IDF",
            ));
        }
    }
    for field in ["tenant_id", "dataset_id"] {
        if info["payload_schema"][field].is_null() {
            return Err(invalid("required filtering payload index is unavailable"));
        }
    }
    if stored.filtering["strict_unindexed_filtering"] == true
        && (info["config"]["strict_mode_config"]["enabled"] != true
            || info["config"]["strict_mode_config"]["unindexed_filtering_retrieve"] != false)
    {
        return Err(invalid("strict filtering profile is not active"));
    }
    let receipt = &info["config"]["metadata"]["knowledge_bm25_v2_backfill"];
    if receipt["status"] != "complete"
        || receipt["collection_name"].as_str() != Some(d.collection()?)
        || receipt["tenant_id"] != d.tenant_id
        || receipt["dataset_id"] != d.dataset_id
        || receipt["bm25_v2_schema_fingerprint"] != stored.schema_fingerprint()
        || receipt["filtering_profile_fingerprint"] != stored.filtering_fingerprint()
        || receipt["authority_kind"] != "postgres-segments-enabled-l3-text-base-v2"
        || receipt["authority_content_revision"].as_i64() != d.content_revision
    {
        return Err(invalid(
            "bm25_v2 receipt is missing, stale or does not match the collection profile",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut entries = Vec::new();
    for point in store.scroll(d, &lexical_filter(d), true).await? {
        let id = id_string(&point["id"]).ok_or_else(|| invalid("lexical point has no id"))?;
        if !ids.insert(id.clone()) {
            return Err(invalid("duplicate lexical point id"));
        }
        let p = &point["payload"];
        if p["tenant_id"] != d.tenant_id || p["dataset_id"] != d.dataset_id || !eligible(p) {
            return Err(invalid("point escaped requested lexical scope"));
        }
        let text = p["text"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(|| invalid("lexical point has empty text"))?;
        let expected = stored.marker(text);
        let marker = &p["_lexical"];
        for k in [
            "bm25_v2_schema_fingerprint",
            "filtering_profile_fingerprint",
            "source_text_sha256",
        ] {
            if marker[k] != expected[k] {
                return Err(invalid("lexical point coverage marker mismatch"));
            }
        }
        let versions = marker["versions"]
            .as_array()
            .ok_or_else(|| invalid("lexical versions marker missing"))?;
        if !versions.contains(&json!("lexical_v1"))
            || !versions.contains(&json!("bm25_v2"))
            || point["vector"]["bm25_v2"].is_null()
        {
            return Err(invalid("lexical point is missing versioned vector"));
        }
        entries.push((id, format!("{:x}", Sha256::digest(text.as_bytes()))));
    }
    let ids_hash = format!(
        "{:x}",
        Sha256::digest(
            ids.iter()
                .map(|id| format!("{id}\n"))
                .collect::<String>()
                .as_bytes()
        )
    );
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let text_hash = format!(
        "{:x}",
        Sha256::digest(
            entries
                .iter()
                .map(|(id, hash)| format!("{id}\0{hash}\n"))
                .collect::<String>()
                .as_bytes()
        )
    );
    // Python's `point_count or -1` rejects an empty receipt as well.
    if ids.is_empty()
        || receipt["point_count"].as_u64() != Some(ids.len() as u64)
        || receipt["point_ids_sha256"] != ids_hash
        || receipt["source_text_sha256"] != text_hash
    {
        return Err(invalid("bm25_v2 lexical scope drifted from receipt"));
    }
    Ok(receipt.clone())
}
