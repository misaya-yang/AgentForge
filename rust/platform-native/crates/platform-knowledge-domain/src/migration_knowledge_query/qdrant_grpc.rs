//! Optional official Qdrant gRPC transport; the shared resilience budget remains authoritative.
use super::{grpc_config as config, grpc_values::*, resilience::Failure};
use crate::migration_knowledge_query::types::QueryError;
use qdrant_client::{Qdrant, QdrantError, qdrant as q};
use reqwest::{Method, Url};
use serde_json::{Value, json};
use std::time::Duration;
pub struct GrpcTransport {
    client: Qdrant,
}
fn error(e: QdrantError) -> Failure<QueryError> {
    let retry = match e {
        QdrantError::ResponseError { status }
        | QdrantError::ResourceExhaustedError { status, .. } => {
            matches!(status.code() as i32, 2 | 4 | 8 | 13 | 14)
        }
        QdrantError::Io(_) => true,
        _ => false,
    };
    if retry {
        Failure::Transient(QueryError::VectorStore)
    } else {
        Failure::Permanent(QueryError::VectorStore)
    }
}
impl GrpcTransport {
    pub fn new(
        origin: &Url,
        key: Option<String>,
        timeout: Duration,
    ) -> std::result::Result<Self, QueryError> {
        let mut url = origin.clone();
        url.set_port(Some(6334))
            .map_err(|_| QueryError::VectorStore)?;
        url.set_path("");
        let client = Qdrant::from_url(url.as_str())
            .api_key(key)
            .timeout(timeout)
            .skip_compatibility_check()
            .build()
            .map_err(|_| QueryError::VectorStore)?;
        Ok(Self { client })
    }
    pub async fn exists(&self, name: &str) -> Result<bool> {
        self.client.collection_exists(name).await.map_err(error)
    }
    pub async fn list(&self) -> Result<Value> {
        let v = self.client.list_collections().await.map_err(error)?;
        Ok(
            json!({"collections":v.collections.into_iter().map(|v|json!({"name":v.name})).collect::<Vec<_>>()}),
        )
    }
    pub async fn call(
        &self,
        method: &Method,
        name: &str,
        suffix: &[&str],
        body: Option<&Value>,
    ) -> Result<Value> {
        let empty = Value::Null;
        let v = body.unwrap_or(&empty);
        let collection_name = name.to_owned();
        let wait = Some(true);
        match (method.as_str(), suffix) {
            ("GET", []) => {
                let r = self.client.collection_info(name).await.map_err(error)?;
                Ok(config::info(
                    r.result
                        .ok_or(Failure::Permanent(QueryError::VectorStore))?,
                ))
            }
            ("PUT", []) => Ok(json!(
                self.client
                    .create_collection(config::create(name, v)?)
                    .await
                    .map_err(error)?
                    .result
            )),
            ("PATCH", []) => Ok(json!(
                self.client
                    .update_collection(config::update(name, v)?)
                    .await
                    .map_err(error)?
                    .result
            )),
            ("DELETE", []) => Ok(json!(
                self.client
                    .delete_collection(name)
                    .await
                    .map_err(error)?
                    .result
            )),
            ("PUT", ["index"]) => update(
                self.client
                    .create_field_index(config::index(name, v)?)
                    .await
                    .map_err(error)?,
            ),
            ("POST", ["points", "query", "batch"]) => {
                let query_points = v["searches"]
                    .as_array()
                    .ok_or(Failure::Permanent(QueryError::VectorStore))?
                    .iter()
                    .map(|v| query_points(name, v))
                    .collect::<Result<Vec<_>>>()?;
                let r = self
                    .client
                    .query_batch(q::QueryBatchPoints {
                        collection_name,
                        query_points,
                        ..Default::default()
                    })
                    .await
                    .map_err(error)?;
                Ok(Value::Array(r.result.into_iter().map(|r|json!({"points":r.result.into_iter().map(scored).collect::<Vec<_>>()})).collect()))
            }
            ("POST", ["points", "query"]) => {
                let r = self
                    .client
                    .query(query_points(name, v)?)
                    .await
                    .map_err(error)?;
                Ok(json!({"points":r.result.into_iter().map(scored).collect::<Vec<_>>()}))
            }
            ("POST", ["points"]) => {
                let r = self
                    .client
                    .get_points(q::GetPoints {
                        collection_name,
                        ids: ids(&v["ids"])?,
                        with_payload: with_payload(&v["with_payload"]),
                        with_vectors: with_vectors(&v["with_vector"]),
                        ..Default::default()
                    })
                    .await
                    .map_err(error)?;
                Ok(Value::Array(r.result.into_iter().map(point).collect()))
            }
            ("PUT", ["points"]) => {
                let points = v["points"]
                    .as_array()
                    .ok_or(Failure::Permanent(QueryError::VectorStore))?
                    .iter()
                    .map(|p| {
                        Ok(q::PointStruct {
                            id: Some(id(&p["id"])?),
                            payload: payload(&p["payload"])?,
                            vectors: Some(vectors(&p["vector"])?),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                update(
                    self.client
                        .upsert_points(q::UpsertPoints {
                            collection_name,
                            wait,
                            points,
                            shard_key_selector: shard_selector(&v["shard_key"])?,
                            ..Default::default()
                        })
                        .await
                        .map_err(error)?,
                )
            }
            ("PUT", ["points", "vectors"]) => {
                let points = v["points"]
                    .as_array()
                    .ok_or(Failure::Permanent(QueryError::VectorStore))?
                    .iter()
                    .map(|p| {
                        Ok(q::PointVectors {
                            id: Some(id(&p["id"])?),
                            vectors: Some(vectors(&p["vector"])?),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                update(
                    self.client
                        .update_vectors(q::UpdatePointVectors {
                            collection_name,
                            wait,
                            points,
                            ..Default::default()
                        })
                        .await
                        .map_err(error)?,
                )
            }
            ("POST", ["points", "payload"]) => update(
                self.client
                    .set_payload(q::SetPayloadPoints {
                        collection_name,
                        wait,
                        payload: payload(&v["payload"])?,
                        points_selector: Some(selector(v)?),
                        ..Default::default()
                    })
                    .await
                    .map_err(error)?,
            ),
            ("POST", ["points", "delete"]) => update(
                self.client
                    .delete_points(q::DeletePoints {
                        collection_name,
                        wait,
                        points: Some(selector(v)?),
                        ..Default::default()
                    })
                    .await
                    .map_err(error)?,
            ),
            ("POST", ["points", "scroll"]) => {
                let r = self
                    .client
                    .scroll(q::ScrollPoints {
                        collection_name,
                        filter: filter(&v["filter"])?,
                        offset: if v["offset"].is_null() {
                            None
                        } else {
                            Some(id(&v["offset"])?)
                        },
                        limit: v["limit"].as_u64().map(|v| v as u32),
                        with_payload: with_payload(&v["with_payload"]),
                        with_vectors: with_vectors(&v["with_vector"]),
                        ..Default::default()
                    })
                    .await
                    .map_err(error)?;
                Ok(
                    json!({"points":r.result.into_iter().map(point).collect::<Vec<_>>(),"next_page_offset":id_json(r.next_page_offset)}),
                )
            }
            ("POST", ["points", "count"]) => {
                let r = self
                    .client
                    .count(q::CountPoints {
                        collection_name,
                        filter: filter(&v["filter"])?,
                        exact: v["exact"].as_bool(),
                        ..Default::default()
                    })
                    .await
                    .map_err(error)?;
                Ok(
                    json!({"count":r.result.ok_or(Failure::Permanent(QueryError::VectorStore))?.count}),
                )
            }
            _ => invalid(),
        }
    }
}

fn query_points(name: &str, v: &Value) -> Result<q::QueryPoints> {
    Ok(q::QueryPoints {
        collection_name: name.to_owned(),
        params: search_params(&v["params"])?,
        shard_key_selector: shard_selector(&v["shard_key"])?,
        query: query(&v["query"])?,
        prefetch: prefetch(&v["prefetch"])?,
        using: v["using"].as_str().map(str::to_owned),
        filter: filter(&v["filter"])?,
        limit: v["limit"].as_u64(),
        offset: v["offset"].as_u64(),
        score_threshold: v["score_threshold"].as_f64().map(|v| v as f32),
        with_payload: with_payload(&v["with_payload"]),
        with_vectors: with_vectors(&v["with_vector"]),
        ..Default::default()
    })
}
