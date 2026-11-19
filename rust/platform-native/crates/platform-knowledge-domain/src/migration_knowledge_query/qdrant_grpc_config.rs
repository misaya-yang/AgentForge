//! Official protocol collection configuration used by the Knowledge vector store.
use super::grpc_values::*;
use qdrant_client::qdrant as q;
use serde_json::{Value, json};
use std::collections::HashMap;
pub fn sparse(v: &Value) -> Result<Option<q::SparseVectorConfig>> {
    if v.is_null() {
        return Ok(None);
    }
    let mut map = HashMap::new();
    for (k, v) in v.as_object().ok_or(super::resilience::Failure::Permanent(
        crate::migration_knowledge_query::types::QueryError::VectorStore,
    ))? {
        let modifier = match v["modifier"].as_str() {
            Some("idf") => Some(q::Modifier::Idf as i32),
            Some("none") => Some(q::Modifier::None as i32),
            None => None,
            _ => return invalid(),
        };
        map.insert(
            k.clone(),
            q::SparseVectorParams {
                modifier,
                index: if v["index"].is_null() {
                    None
                } else {
                    Some(q::SparseIndexConfig {
                        memory: memory(&v["index"]["memory"])?,
                        full_scan_threshold: v["index"]["full_scan_threshold"].as_u64(),
                        ..Default::default()
                    })
                },
            },
        );
    }
    Ok(Some(q::SparseVectorConfig { map }))
}
pub fn memory(v: &Value) -> Result<Option<i32>> {
    Ok(match v.as_str() {
        None if v.is_null() => None,
        Some("cold") => Some(q::Memory::Cold as i32),
        Some("cached") => Some(q::Memory::Cached as i32),
        Some("pinned") => Some(q::Memory::Pinned as i32),
        _ => return invalid(),
    })
}
pub fn quantization(v: &Value) -> Result<Option<q::QuantizationConfig>> {
    if v.is_null() {
        return Ok(None);
    }
    if !v["turboquant"].is_object() {
        return invalid();
    }
    let bits = match v["turboquant"]["bits"].as_str() {
        Some("1") => q::TurboQuantBitSize::Bits1,
        Some("1.5") => q::TurboQuantBitSize::Bits15,
        Some("2") => q::TurboQuantBitSize::Bits2,
        Some("4") => q::TurboQuantBitSize::Bits4,
        _ => return invalid(),
    };
    Ok(Some(q::QuantizationConfig {
        quantization: Some(q::quantization_config::Quantization::Turboquant(
            q::TurboQuantization {
                bits: Some(bits as i32),
                memory: memory(&v["turboquant"]["memory"])?,
                ..Default::default()
            },
        )),
    }))
}
pub fn strict(v: &Value) -> Option<q::StrictModeConfig> {
    (!v.is_null()).then(|| q::StrictModeConfig {
        enabled: v["enabled"].as_bool(),
        max_query_limit: v["max_query_limit"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok()),
        max_timeout: v["max_timeout"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok()),
        search_max_hnsw_ef: v["search_max_hnsw_ef"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok()),
        search_allow_exact: v["search_allow_exact"].as_bool(),
        search_max_oversampling: v["search_max_oversampling"].as_f64().map(|n| n as f32),
        upsert_max_batchsize: v["upsert_max_batchsize"].as_u64(),
        search_max_batchsize: v["search_max_batchsize"].as_u64(),
        filter_max_conditions: v["filter_max_conditions"].as_u64(),
        condition_max_size: v["condition_max_size"].as_u64(),
        max_points_count: v["max_points_count"].as_u64(),
        max_payload_index_count: v["max_payload_index_count"].as_u64(),
        unindexed_filtering_retrieve: v["unindexed_filtering_retrieve"].as_bool(),
        unindexed_filtering_update: v["unindexed_filtering_update"].as_bool(),
        ..Default::default()
    })
}
pub fn create(name: &str, v: &Value) -> Result<q::CreateCollection> {
    let distance = match v["vectors"]["distance"].as_str() {
        Some("Cosine") => q::Distance::Cosine,
        Some("Dot") => q::Distance::Dot,
        Some("Euclid") => q::Distance::Euclid,
        Some("Manhattan") => q::Distance::Manhattan,
        _ => return invalid(),
    };
    Ok(q::CreateCollection {
        collection_name: name.into(),
        vectors_config: Some(q::VectorsConfig {
            config: Some(q::vectors_config::Config::Params(q::VectorParams {
                size: v["vectors"]["size"].as_u64().ok_or(
                    super::resilience::Failure::Permanent(
                        crate::migration_knowledge_query::types::QueryError::VectorStore,
                    ),
                )?,
                distance: distance as i32,
                memory: memory(&v["vectors"]["memory"])?,
                datatype: match v["vectors"]["datatype"].as_str() {
                    None => None,
                    Some("turbo4") => Some(q::Datatype::Turbo4 as i32),
                    Some("float32") => Some(q::Datatype::Float32 as i32),
                    Some("float16") => Some(q::Datatype::Float16 as i32),
                    _ => return invalid(),
                },
                ..Default::default()
            })),
        }),
        sparse_vectors_config: sparse(&v["sparse_vectors"])?,
        metadata: payload(&v["metadata"])?,
        quantization_config: quantization(&v["quantization_config"])?,
        hnsw_config: Some(q::HnswConfigDiff {
            m: v["hnsw_config"]["m"].as_u64(),
            memory: memory(&v["hnsw_config"]["memory"])?,
            max_indexing_threads: v["hnsw_config"]["max_indexing_threads"].as_u64(),
            ef_construct: v["hnsw_config"]["ef_construct"].as_u64(),
            full_scan_threshold: v["hnsw_config"]["full_scan_threshold"].as_u64(),
            ..Default::default()
        }),
        optimizers_config: Some(q::OptimizersConfigDiff {
            indexing_threshold: v["optimizers_config"]["indexing_threshold"].as_u64(),
            ..Default::default()
        }),
        strict_mode_config: strict(&v["strict_mode_config"]),
        ..Default::default()
    })
}
pub fn update(name: &str, v: &Value) -> Result<q::UpdateCollection> {
    Ok(q::UpdateCollection {
        collection_name: name.into(),
        sparse_vectors_config: sparse(&v["sparse_vectors"])?,
        metadata: payload(&v["metadata"])?,
        strict_mode_config: strict(&v["strict_mode_config"]),
        ..Default::default()
    })
}
pub fn index(name: &str, v: &Value) -> Result<q::CreateFieldIndexCollection> {
    let schema = &v["field_schema"];
    let kind = schema.as_str().or_else(|| schema["type"].as_str());
    let field_type = match kind {
        Some("keyword") => q::PayloadSchemaType::Keyword,
        Some("integer") => q::PayloadSchemaType::Integer,
        Some("bool") => q::PayloadSchemaType::Bool,
        _ => return invalid(),
    };
    let field_index_params = if kind == Some("keyword") && schema.is_object() {
        Some(q::PayloadIndexParams {
            index_params: Some(q::payload_index_params::IndexParams::KeywordIndexParams(
                q::KeywordIndexParams {
                    is_tenant: schema["is_tenant"].as_bool(),
                    prefix: (!schema["prefix"].is_null()).then_some(q::KeywordPrefixParams {}),
                    memory: memory(&schema["memory"])?,
                    ..Default::default()
                },
            )),
        })
    } else {
        None
    };
    Ok(q::CreateFieldIndexCollection {
        collection_name: name.into(),
        wait: Some(true),
        field_name: string(&v["field_name"])?,
        field_type: Some(field_type as i32),
        field_index_params,
        ..Default::default()
    })
}
pub fn info(v: q::CollectionInfo) -> Value {
    let c = v.config.unwrap_or_default();
    let p = c.params.unwrap_or_default();
    let vectors = match p.vectors_config.and_then(|v| v.config) {
        Some(q::vectors_config::Config::Params(p)) => {
            json!({"size":p.size,"distance":q::Distance::try_from(p.distance).map(|v|v.as_str_name()).unwrap_or("Unknown"),"memory":p.memory.and_then(|v|q::Memory::try_from(v).ok()).map(|v|v.as_str_name().to_lowercase()),"datatype":p.datatype.and_then(|v|q::Datatype::try_from(v).ok()).map(|v|v.as_str_name().to_lowercase())})
        }
        _ => Value::Null,
    };
    let sparse:serde_json::Map<String,Value>=p.sparse_vectors_config.unwrap_or_default().map.into_iter().map(|(k,v)|(k,json!({"modifier":v.modifier.and_then(|v|q::Modifier::try_from(v).ok()).map(|v|v.as_str_name().to_lowercase()),"index":v.index.map(|i|json!({"memory":i.memory.and_then(|m|q::Memory::try_from(m).ok()).map(|m|m.as_str_name().to_lowercase()),"full_scan_threshold":i.full_scan_threshold}))}))).collect();
    let schema:serde_json::Map<String,Value>=v.payload_schema.into_iter().map(|(k,v)|{let params=match v.params.and_then(|p|p.index_params){Some(q::payload_index_params::IndexParams::KeywordIndexParams(p))=>json!({"type":"keyword","is_tenant":p.is_tenant,"on_disk":p.on_disk}),_=>Value::Null};(k,json!({"data_type":q::PayloadSchemaType::try_from(v.data_type).map(|v|v.as_str_name().to_lowercase()).unwrap_or_default(),"params":params,"points":v.points}))}).collect();
    let strict=c.strict_mode_config.map(|v|json!({"enabled":v.enabled,"unindexed_filtering_retrieve":v.unindexed_filtering_retrieve,"unindexed_filtering_update":v.unindexed_filtering_update}));
    let hnsw=c.hnsw_config.map(|h|json!({"m":h.m,"ef_construct":h.ef_construct,"memory":h.memory.and_then(|m|q::Memory::try_from(m).ok()).map(|m|m.as_str_name().to_lowercase())}));
    let quantization=c.quantization_config.and_then(|v|v.quantization).and_then(|v|match v {q::quantization_config::Quantization::Turboquant(t)=>Some(json!({"turboquant":{"bits":t.bits.and_then(|v|q::TurboQuantBitSize::try_from(v).ok()).map(|v|match v {q::TurboQuantBitSize::Bits1=>"1",q::TurboQuantBitSize::Bits15=>"1.5",q::TurboQuantBitSize::Bits2=>"2",q::TurboQuantBitSize::Bits4=>"4"}),"memory":t.memory.and_then(|m|q::Memory::try_from(m).ok()).map(|m|m.as_str_name().to_lowercase())}})),_=>None});
    json!({"status":q::CollectionStatus::try_from(v.status).map(|v|v.as_str_name().to_lowercase()).unwrap_or_default(),"points_count":v.points_count,"indexed_vectors_count":v.indexed_vectors_count,"segments_count":v.segments_count,"payload_schema":schema,"config":{"metadata":payload_json(c.metadata),"params":{"vectors":vectors,"sparse_vectors":sparse,"shard_number":p.shard_number,"on_disk_payload":p.on_disk_payload},"strict_mode_config":strict,"hnsw_config":hnsw,"quantization_config":quantization}})
}
#[cfg(test)]
mod optimized_profile_tests {
    use super::*;
    #[test]
    fn typed_protocol_preserves_memory_quantization_and_budget_fields() {
        let request=create("test",&json!({"vectors":{"size":1024,"distance":"Cosine","memory":"cold"},"sparse_vectors":{"bm25":{"modifier":"idf","index":{"memory":"cached"}}},"hnsw_config":{"m":16,"ef_construct":200,"memory":"pinned"},"quantization_config":{"turboquant":{"bits":"1.5","memory":"cached"}},"strict_mode_config":{"enabled":true,"max_query_limit":1000,"max_timeout":3,"search_max_hnsw_ef":1024,"search_allow_exact":false,"search_max_oversampling":4.0,"upsert_max_batchsize":256}})).unwrap();
        let Some(q::vectors_config::Config::Params(vectors)) =
            request.vectors_config.unwrap().config
        else {
            panic!("dense params")
        };
        assert_eq!(vectors.memory, Some(q::Memory::Cold as i32));
        assert_eq!(
            request.hnsw_config.unwrap().memory,
            Some(q::Memory::Pinned as i32)
        );
        assert_eq!(
            request.sparse_vectors_config.unwrap().map["bm25"]
                .index
                .unwrap()
                .memory,
            Some(q::Memory::Cached as i32)
        );
        assert!(
            matches!(request.quantization_config.unwrap().quantization,Some(q::quantization_config::Quantization::Turboquant(q::TurboQuantization{bits:Some(v),..})) if v==q::TurboQuantBitSize::Bits15 as i32)
        );
        let strict = request.strict_mode_config.unwrap();
        assert_eq!(strict.max_timeout, Some(3));
        assert_eq!(strict.upsert_max_batchsize, Some(256));
        assert_eq!(strict.search_max_oversampling, Some(4.));
        assert!(quantization(&json!({"turboquant":{"bits":"unknown"}})).is_err());
    }
}
