//! Versioned, server-promoted serving options. A dataset request is configuration,
//! never evidence that a quality, memory or isolation gate passed.
use super::types::{Dataset, QueryError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema: String,
    #[serde(default)]
    pub multimodal: bool,
    #[serde(default)]
    pub layout: bool,
    #[serde(default)]
    pub index: Option<Index>,
    #[serde(default)]
    pub shared: Option<Shared>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Shared {
    pub collection: String,
    pub fallback_shard: String,
    #[serde(default)]
    pub tenant_shard: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub quantization: Quantization,
    pub tier: Tier,
    #[serde(default = "yes")]
    pub tenant_idf: bool,
    #[serde(default = "yes")]
    pub acorn: bool,
    #[serde(default = "ef")]
    pub hnsw_ef: u64,
}
fn yes() -> bool {
    true
}
fn ef() -> u64 {
    128
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Quantization {
    Turbo,
    Turbofour,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Tier {
    Hot,
    Warm,
    Archive,
}
fn invalid(s: &str) -> QueryError {
    QueryError::Validation(s.into())
}
pub fn implementation_fingerprint() -> String {
    super::lexical_v2::fingerprint(
        &json!({"contract":"knowledge-serving/v1","source":concat!(include_str!("serving_profile.rs"),include_str!("qdrant.rs"),include_str!("qdrant_grpc_config.rs"),include_str!("qdrant_grpc_values.rs")),"qdrant":"1.19.0"}),
    )
}
impl Profile {
    pub fn from_dataset(d: &Dataset) -> Result<Option<Self>> {
        let Some(raw) = d.index_config.get("serving_v2").filter(|v| !v.is_null()) else {
            return Ok(None);
        };
        let p: Self = serde_json::from_value(raw.clone())
            .map_err(|_| invalid("invalid serving_v2 profile"))?;
        if p.schema != "knowledge-serving/v1" || (!p.multimodal && !p.layout && p.index.is_none()) {
            return Err(invalid("unsupported or empty serving_v2 profile"));
        }
        if let Some(i) = &p.index {
            if !(16..=1024).contains(&i.hnsw_ef)
                || (i.quantization == Quantization::Turbofour && i.tier != Tier::Archive)
            {
                return Err(invalid("invalid index tier or search budget"));
            }
        }
        if p.layout && !d.index_config["parsing_v2"]["layout"].is_object() {
            return Err(invalid(
                "layout promotion requires its exact parsing_v2 layout configuration",
            ));
        }
        if let Some(shared) = &p.shared {
            if p.index.as_ref().is_none_or(|i| !i.tenant_idf)
                || !shared.collection.starts_with("kbs_")
                || shared.collection.len() > 120
                || !shared
                    .collection
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || shared.fallback_shard.is_empty()
                || shared.fallback_shard.len() > 128
                || shared
                    .tenant_shard
                    .as_ref()
                    .is_some_and(|s| s.is_empty() || s.len() > 128)
                || !d.index_config["retrieval"]["lexical"].is_null()
            {
                return Err(invalid(
                    "shared collections require tenant IDF, a provisioned kbs_ group and the legacy lexical profile",
                ));
            }
        }
        if p.multimodal {
            let provider =
                super::embedding::canonical_provider(d.embedding_provider.as_deref().unwrap_or(""));
            if !provider.ends_with("multimodal")
                || d.embedding_model_version
                    .as_deref()
                    .unwrap_or("")
                    .trim()
                    .is_empty()
                || d.embedding_dimension.unwrap_or(0) == 0
            {
                return Err(invalid(
                    "multimodal profile requires a pinned provider vision model and dimension",
                ));
            }
        }
        Ok(Some(p))
    }
    pub fn options(&self) -> BTreeSet<String> {
        let mut s = BTreeSet::new();
        if self.layout {
            s.insert("layout_parser".into());
        }
        if self.multimodal {
            s.insert("multimodal".into());
        }
        if self.shared.is_some() {
            s.insert("shared_collection".into());
        }
        if self.index.is_some() {
            s.insert("index_1_19".into());
        }
        s
    }
    pub fn fingerprint(&self, d: &Dataset) -> String {
        super::lexical_v2::fingerprint(
            &json!({"implementation":implementation_fingerprint(),"tenant":d.tenant_id,"dataset":d.dataset_id,"embedding_provider":d.embedding_provider,"embedding_model":d.embedding_model,"embedding_model_version":d.embedding_model_version,"embedding_dimension":d.embedding_dimension,"embedding_config":d.embedding_config,"index_config":d.index_config}),
        )
    }
    pub fn shared_scope(&self, d: &Dataset) -> Option<Value> {
        self.shared.as_ref().map(|s|json!({"schema_version":2,"group":s.collection,"layout":super::lexical_v2::fingerprint(&json!({"index":self.index,"provider":d.embedding_provider,"model":d.embedding_model,"revision":d.embedding_model_version,"dimension":d.embedding_dimension}))}))
    }
    pub fn shard_key(&self) -> Option<Value> {
        self.shared.as_ref().map(|s| match &s.tenant_shard {
            Some(target) => json!({"target":target,"fallback":s.fallback_shard}),
            None => json!(s.fallback_shard),
        })
    }
    pub fn search_params(&self, d: &Dataset) -> Value {
        let Some(i) = &self.index else {
            return Value::Null;
        };
        let mut p = json!({"hnsw_ef":i.hnsw_ef,"quantization":{"rescore":i.quantization==Quantization::Turbo,"oversampling":2.0}});
        if i.acorn {
            p["acorn"] = json!({"enable":true,"max_selectivity":0.4});
        }
        if i.tenant_idf {
            p["idf"] = json!({"corpus":{"must":[{"key":"tenant_id","match":{"value":d.tenant_id}},{"key":"dataset_id","match":{"value":d.dataset_id}}]}});
        }
        p
    }
    pub fn verify_layout(&self, info: &Value) -> Result<()> {
        let Some(i) = &self.index else { return Ok(()) };
        let config = &info["config"];
        let vectors = &config["params"]["vectors"];
        let graph = if i.tier == Tier::Hot {
            "pinned"
        } else {
            "cached"
        };
        if vectors["memory"] != "cold"
            || config["hnsw_config"]["memory"] != graph
            || config["params"]["sparse_vectors"]["bm25"]["index"]["memory"] != "cached"
            || config["strict_mode_config"]["enabled"] != true
            || config["strict_mode_config"]["unindexed_filtering_retrieve"] != false
            || config["strict_mode_config"]["unindexed_filtering_update"] != false
        {
            return Err(invalid(
                "Qdrant physical memory/index policy differs from the promoted profile",
            ));
        }
        if i.quantization == Quantization::Turbofour {
            if vectors["datatype"] != "turbo4" {
                return Err(invalid("archive profile requires Turbo4 vector storage"));
            }
        } else {
            let quant = &config["quantization_config"]["turboquant"];
            if (quant["bits"] != "1.5" && quant["bits"].as_f64() != Some(1.5))
                || quant["memory"] != "cached"
            {
                return Err(invalid(
                    "TurboQuant physical policy differs from the promoted profile",
                ));
            }
        }
        Ok(())
    }
    pub fn collection_config(&self, body: &mut Value) {
        if let Some(i) = &self.index {
            body["vectors"]["memory"] = json!("cold");
            if i.quantization == Quantization::Turbo {
                body["quantization_config"] =
                    json!({"turboquant":{"bits":"1.5","memory":"cached"}});
            } else {
                body["vectors"]["datatype"] = json!("turbo4");
            }
            body["hnsw_config"]["memory"] = json!(if i.tier == Tier::Hot {
                "pinned"
            } else {
                "cached"
            });
            for name in ["bm25", "bm25_v2"] {
                if body["sparse_vectors"].get(name).is_some() {
                    body["sparse_vectors"][name]["index"] = json!({"memory":"cached"});
                }
            }
            body["strict_mode_config"] = json!({"enabled":true,"unindexed_filtering_retrieve":false,"unindexed_filtering_update":false,"max_query_limit":1000,"max_timeout":3,"search_max_hnsw_ef":1024,"search_allow_exact":false,"search_max_oversampling":4.0,"upsert_max_batchsize":256});
        }
    }
}
/// The native ops gate emits this map only after independently checked real
/// captures. Read at serving admission so removing a permit revokes new work.
pub fn require(d: &Dataset) -> Result<Option<Profile>> {
    let Some(profile) = Profile::from_dataset(d)? else {
        return Ok(None);
    };
    let path = std::env::var_os("KNOWLEDGE_SERVING_PROMOTIONS_FILE")
        .ok_or_else(|| invalid("serving_v2 has not passed the promotion gate"))?;
    use std::io::Read;
    let mut bytes = vec![];
    std::fs::File::open(path)
        .map_err(|_| invalid("serving promotion decision unavailable"))?
        .take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("serving promotion decision unavailable"))?;
    if bytes.len() > 1024 * 1024 {
        return Err(invalid("serving promotion decision exceeds byte budget"));
    }
    let v: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("invalid serving promotion decision"))?;
    authorize(&profile, d, &v)?;
    Ok(Some(profile))
}
pub fn authorize(profile: &Profile, d: &Dataset, v: &Value) -> Result<()> {
    if v["eligible"] != true
        || v["promotable"] != true
        || v["fixture_only"] != false
        || v["cases"].as_u64().unwrap_or(0) == 0
    {
        return Err(invalid(
            "real successful serving promotion decision required",
        ));
    }
    let permits: BTreeMap<String, BTreeSet<String>> =
        serde_json::from_value(v["serving_promotions"].clone())
            .map_err(|_| invalid("serving promotion bindings absent"))?;
    if permits.len() > 256
        || !permits
            .get(&profile.fingerprint(d))
            .is_some_and(|v| profile.options().is_subset(v))
    {
        return Err(invalid(
            "serving profile configuration or model has not passed its gates",
        ));
    }
    Ok(())
}
pub fn multimodal(d: &Dataset) -> Result<bool> {
    Ok(require(d)?.is_some_and(|p| p.multimodal))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn dataset() -> Dataset {
        Dataset {
            dataset_id: "d".into(),
            tenant_id: "t".into(),
            embedding_provider: Some("unified_multimodal".into()),
            embedding_model: Some("tongyi-embedding-vision-plus".into()),
            embedding_model_version: Some("revision".into()),
            embedding_dimension: Some(1024),
            index_config: json!({"serving_v2":{"schema":"knowledge-serving/v1","multimodal":true}}),
            ..Default::default()
        }
    }
    #[test]
    fn fixture_and_model_substitution_never_authorize() {
        let d = dataset();
        let p = Profile::from_dataset(&d).unwrap().unwrap();
        let mut v = json!({"eligible":true,"promotable":true,"fixture_only":false,"cases":2,"serving_promotions":{}});
        v["serving_promotions"][p.fingerprint(&d)] = json!(["multimodal"]);
        assert!(authorize(&p, &d, &v).is_ok());
        let mut changed = d;
        changed.embedding_model_version = Some("other".into());
        assert!(authorize(&p, &changed, &v).is_err());
        v["fixture_only"] = json!(true);
        assert!(authorize(&p, &changed, &v).is_err());
    }
    #[test]
    fn unrecognized_profile_options_fail_closed() {
        let mut d = dataset();
        d.index_config["serving_v2"]["provider_url"] = json!("https://bad.invalid");
        assert!(Profile::from_dataset(&d).is_err());
    }
}
#[cfg(test)]
mod layout_binding_tests {
    use super::*;
    #[test]
    fn archive_and_shared_options_are_explicit_and_model_bound() {
        let mut d = Dataset {
            dataset_id: "d".into(),
            tenant_id: "t".into(),
            embedding_provider: Some("local_semantic".into()),
            embedding_model: Some("m".into()),
            embedding_model_version: Some("r1".into()),
            embedding_dimension: Some(1024),
            index_config: json!({"serving_v2":{"schema":"knowledge-serving/v1","index":{"quantization":"turbofour","tier":"archive"},"shared":{"collection":"kbs_small","fallback_shard":"default"}}}),
            ..Default::default()
        };
        let p = Profile::from_dataset(&d).unwrap().unwrap();
        assert!(p.options().contains("shared_collection"));
        let old = p.shared_scope(&d);
        d.embedding_model_version = Some("r2".into());
        assert_ne!(old, p.shared_scope(&d));
        d.index_config["serving_v2"]["index"]["tier"] = json!("hot");
        assert!(Profile::from_dataset(&d).is_err());
    }
    #[test]
    fn physical_drift_never_counts_as_a_promoted_layout() {
        let d = Dataset {
            index_config: json!({"serving_v2":{"schema":"knowledge-serving/v1","index":{"quantization":"turbo","tier":"hot"}}}),
            ..Default::default()
        };
        let p = Profile::from_dataset(&d).unwrap().unwrap();
        let mut body =
            json!({"vectors":{},"hnsw_config":{},"sparse_vectors":{"bm25":{"modifier":"idf"}}});
        p.collection_config(&mut body);
        let mut info = json!({"config":{"params":{"vectors":body["vectors"],"sparse_vectors":body["sparse_vectors"]},"hnsw_config":body["hnsw_config"],"strict_mode_config":body["strict_mode_config"],"quantization_config":body["quantization_config"]}});
        assert!(p.verify_layout(&info).is_ok());
        info["config"]["params"]["vectors"]["memory"] = json!("cached");
        assert!(p.verify_layout(&info).is_err());
    }
}
