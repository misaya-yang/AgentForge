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
                index: None,
            },
        );
    }
    Ok(Some(q::SparseVectorConfig { map }))
}
pub fn strict(v: &Value) -> Option<q::StrictModeConfig> {
    (!v.is_null()).then(|| q::StrictModeConfig {
        enabled: v["enabled"].as_bool(),
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
                ..Default::default()
            })),
        }),
        sparse_vectors_config: sparse(&v["sparse_vectors"])?,
        metadata: payload(&v["metadata"])?,
        hnsw_config: Some(q::HnswConfigDiff {
            m: v["hnsw_config"]["m"].as_u64(),
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
            json!({"size":p.size,"distance":q::Distance::try_from(p.distance).map(|v|v.as_str_name()).unwrap_or("Unknown")})
        }
        _ => Value::Null,
    };
    let sparse:serde_json::Map<String,Value>=p.sparse_vectors_config.unwrap_or_default().map.into_iter().map(|(k,v)|(k,json!({"modifier":v.modifier.and_then(|v|q::Modifier::try_from(v).ok()).map(|v|v.as_str_name().to_lowercase())}))).collect();
    let schema:serde_json::Map<String,Value>=v.payload_schema.into_iter().map(|(k,v)|{let params=match v.params.and_then(|p|p.index_params){Some(q::payload_index_params::IndexParams::KeywordIndexParams(p))=>json!({"type":"keyword","is_tenant":p.is_tenant,"on_disk":p.on_disk}),_=>Value::Null};(k,json!({"data_type":q::PayloadSchemaType::try_from(v.data_type).map(|v|v.as_str_name().to_lowercase()).unwrap_or_default(),"params":params,"points":v.points}))}).collect();
    let strict=c.strict_mode_config.map(|v|json!({"enabled":v.enabled,"unindexed_filtering_retrieve":v.unindexed_filtering_retrieve,"unindexed_filtering_update":v.unindexed_filtering_update}));
    json!({"status":q::CollectionStatus::try_from(v.status).map(|v|v.as_str_name().to_lowercase()).unwrap_or_default(),"points_count":v.points_count,"indexed_vectors_count":v.indexed_vectors_count,"segments_count":v.segments_count,"payload_schema":schema,"config":{"metadata":payload_json(c.metadata),"params":{"vectors":vectors,"sparse_vectors":sparse,"shard_number":p.shard_number,"on_disk_payload":p.on_disk_payload},"strict_mode_config":strict}})
}
