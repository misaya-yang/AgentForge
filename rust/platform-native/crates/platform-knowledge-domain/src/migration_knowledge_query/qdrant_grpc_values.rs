//! Conversion of the source-used Qdrant REST-shaped values to official gRPC types.
use super::resilience::Failure;
use crate::migration_knowledge_query::types::QueryError;
use qdrant_client::{Payload, qdrant as q};
use serde_json::{Value, json};
use std::collections::HashMap;
pub type Result<T> = std::result::Result<T, Failure<QueryError>>;
pub fn invalid<T>() -> Result<T> {
    Err(Failure::Permanent(QueryError::VectorStore))
}
pub fn string(v: &Value) -> Result<String> {
    v.as_str()
        .map(str::to_owned)
        .ok_or(Failure::Permanent(QueryError::VectorStore))
}
pub fn payload(v: &Value) -> Result<HashMap<String, q::Value>> {
    if v.is_null() {
        return Ok(HashMap::new());
    }
    Payload::try_from(v.clone())
        .map(Into::into)
        .map_err(|_| Failure::Permanent(QueryError::VectorStore))
}
pub fn payload_json(p: HashMap<String, q::Value>) -> Value {
    Payload::from(p).into()
}
pub fn id(v: &Value) -> Result<q::PointId> {
    let option = if let Some(n) = v.as_u64() {
        q::point_id::PointIdOptions::Num(n)
    } else {
        q::point_id::PointIdOptions::Uuid(string(v)?)
    };
    Ok(q::PointId {
        point_id_options: Some(option),
    })
}
pub fn id_json(v: Option<q::PointId>) -> Value {
    match v.and_then(|v| v.point_id_options) {
        Some(q::point_id::PointIdOptions::Num(n)) => json!(n),
        Some(q::point_id::PointIdOptions::Uuid(s)) => json!(s),
        None => Value::Null,
    }
}
pub fn ids(v: &Value) -> Result<Vec<q::PointId>> {
    v.as_array()
        .ok_or(Failure::Permanent(QueryError::VectorStore))?
        .iter()
        .map(id)
        .collect()
}
fn floats(v: &Value) -> Result<Vec<f32>> {
    v.as_array()
        .ok_or(Failure::Permanent(QueryError::VectorStore))?
        .iter()
        .map(|n| {
            n.as_f64()
                .filter(|n| n.is_finite())
                .map(|n| n as f32)
                .filter(|n| n.is_finite())
                .ok_or(Failure::Permanent(QueryError::VectorStore))
        })
        .collect()
}
fn document(v: &Value) -> Result<q::Document> {
    Ok(q::Document {
        text: string(&v["text"])?,
        model: string(&v["model"])?,
        options: payload(&v["options"])?,
    })
}
fn sparse(v: &Value) -> Result<q::SparseVector> {
    let indices = v["indices"]
        .as_array()
        .ok_or(Failure::Permanent(QueryError::VectorStore))?
        .iter()
        .map(|v| {
            v.as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .ok_or(Failure::Permanent(QueryError::VectorStore))
        })
        .collect::<Result<Vec<_>>>()?;
    let values = floats(&v["values"])?;
    if indices.len() != values.len() {
        return invalid();
    }
    Ok(q::SparseVector { indices, values })
}
pub fn vector(v: &Value) -> Result<q::Vector> {
    let variant = if v.is_array() {
        q::vector::Vector::Dense(q::DenseVector { data: floats(v)? })
    } else if v.get("indices").is_some() {
        q::vector::Vector::Sparse(sparse(v)?)
    } else if v.get("text").is_some() {
        q::vector::Vector::Document(document(v)?)
    } else {
        return invalid();
    };
    Ok(q::Vector {
        vector: Some(variant),
        ..Default::default()
    })
}
pub fn vectors(v: &Value) -> Result<q::Vectors> {
    let variant = if v.is_array() || v.get("indices").is_some() || v.get("text").is_some() {
        q::vectors::VectorsOptions::Vector(vector(v)?)
    } else {
        let map = v
            .as_object()
            .ok_or(Failure::Permanent(QueryError::VectorStore))?
            .iter()
            .map(|(k, v)| Ok((k.clone(), vector(v)?)))
            .collect::<Result<HashMap<_, _>>>()?;
        q::vectors::VectorsOptions::Vectors(q::NamedVectors { vectors: map })
    };
    Ok(q::Vectors {
        vectors_options: Some(variant),
    })
}
pub fn vector_json(v: q::VectorOutput) -> Value {
    match v.vector {
        Some(q::vector_output::Vector::Dense(v)) => json!(v.data),
        Some(q::vector_output::Vector::Sparse(v)) => json!({"indices":v.indices,"values":v.values}),
        Some(q::vector_output::Vector::MultiDense(v)) => {
            json!(v.vectors.into_iter().map(|v| v.data).collect::<Vec<_>>())
        }
        None => {
            if let Some(indices) = v.indices {
                json!({"indices":indices.data,"values":v.data})
            } else if let Some(count) = v.vectors_count {
                // 1.18 legacy protobuf flattens multivectors. Preserve rows;
                // never divide/chunk by zero on a malformed remote response.
                let count = count as usize;
                if count == 0 || v.data.is_empty() || v.data.len() % count != 0 {
                    return Value::Null;
                }
                json!(
                    v.data
                        .chunks_exact(v.data.len() / count)
                        .collect::<Vec<_>>()
                )
            } else {
                json!(v.data)
            }
        }
    }
}
pub fn vectors_json(v: Option<q::VectorsOutput>) -> Value {
    match v.and_then(|v| v.vectors_options) {
        Some(q::vectors_output::VectorsOptions::Vector(v)) => vector_json(v),
        Some(q::vectors_output::VectorsOptions::Vectors(v)) => Value::Object(
            v.vectors
                .into_iter()
                .map(|(k, v)| (k, vector_json(v)))
                .collect(),
        ),
        None => Value::Null,
    }
}
pub fn filter(v: &Value) -> Result<Option<q::Filter>> {
    if v.is_null() {
        return Ok(None);
    }
    let clauses = |name: &str| -> Result<Vec<q::Condition>> {
        match v.get(name) {
            None => Ok(vec![]),
            Some(Value::Array(a)) => a.iter().map(condition).collect(),
            Some(v) => Ok(vec![condition(v)?]),
        }
    };
    Ok(Some(q::Filter {
        must: clauses("must")?,
        should: clauses("should")?,
        must_not: clauses("must_not")?,
        min_should: None,
    }))
}
fn condition(v: &Value) -> Result<q::Condition> {
    use q::condition::ConditionOneOf as C;
    let c = if v.get("must").is_some() || v.get("should").is_some() || v.get("must_not").is_some() {
        C::Filter(filter(v)?.ok_or(Failure::Permanent(QueryError::VectorStore))?)
    } else if let Some(ids_value) = v.get("has_id") {
        C::HasId(q::HasIdCondition {
            has_id: ids(ids_value)?,
        })
    } else if let Some(e) = v.get("is_empty") {
        C::IsEmpty(q::IsEmptyCondition {
            key: string(&e["key"])?,
        })
    } else if let Some(e) = v.get("is_null") {
        C::IsNull(q::IsNullCondition {
            key: string(&e["key"])?,
        })
    } else {
        let mut f = q::FieldCondition {
            key: string(&v["key"])?,
            ..Default::default()
        };
        if let Some(m) = v.get("match") {
            use q::r#match::MatchValue as M;
            let m = if let Some(x) = m.get("value") {
                if let Some(b) = x.as_bool() {
                    M::Boolean(b)
                } else if let Some(n) = x.as_i64() {
                    M::Integer(n)
                } else {
                    M::Keyword(string(x)?)
                }
            } else if let Some(prefix) = m.get("prefix") {
                M::Prefix(string(prefix)?)
            } else if let Some(a) = m.get("any").and_then(Value::as_array) {
                if a.iter().all(Value::is_string) {
                    M::Keywords(q::RepeatedStrings {
                        strings: a.iter().map(string).collect::<Result<_>>()?,
                    })
                } else {
                    M::Integers(q::RepeatedIntegers {
                        integers: a
                            .iter()
                            .map(|v| {
                                v.as_i64()
                                    .ok_or(Failure::Permanent(QueryError::VectorStore))
                            })
                            .collect::<Result<_>>()?,
                    })
                }
            } else {
                return invalid();
            };
            f.r#match = Some(q::Match {
                match_value: Some(m),
            });
        }
        if let Some(r) = v.get("range") {
            f.range = Some(q::Range {
                lt: r["lt"].as_f64(),
                gt: r["gt"].as_f64(),
                gte: r["gte"].as_f64(),
                lte: r["lte"].as_f64(),
            });
        }
        C::Field(f)
    };
    Ok(q::Condition {
        condition_one_of: Some(c),
    })
}
pub fn selector(v: &Value) -> Result<q::PointsSelector> {
    use q::points_selector::PointsSelectorOneOf as P;
    let p = if v.get("filter").is_some() {
        P::Filter(filter(&v["filter"])?.ok_or(Failure::Permanent(QueryError::VectorStore))?)
    } else {
        P::Points(q::PointsIdsList {
            ids: ids(&v["points"])?,
        })
    };
    Ok(q::PointsSelector {
        points_selector_one_of: Some(p),
    })
}
pub fn query(v: &Value) -> Result<Option<q::Query>> {
    if v.is_null() {
        return Ok(None);
    }
    use q::query::Variant as Q;
    let q = if let Some(r) = v.get("rrf") {
        Q::Rrf(q::Rrf {
            k: r["k"].as_u64().map(|v| v as u32),
            weights: if r["weights"].is_null() {
                vec![]
            } else {
                floats(&r["weights"])?
            },
        })
    } else if v["fusion"] == "rrf" {
        Q::Fusion(q::Fusion::Rrf as i32)
    } else {
        use q::vector_input::Variant as V;
        let input = if v.is_array() {
            V::Dense(q::DenseVector { data: floats(v)? })
        } else if v.get("indices").is_some() {
            V::Sparse(sparse(v)?)
        } else if v.get("text").is_some() {
            V::Document(document(v)?)
        } else {
            return invalid();
        };
        Q::Nearest(q::VectorInput {
            variant: Some(input),
        })
    };
    Ok(Some(q::Query { variant: Some(q) }))
}
pub fn shard_selector(v: &Value) -> Result<Option<q::ShardKeySelector>> {
    if v.is_null() {
        return Ok(None);
    }
    let key = |v: &Value| -> Result<q::ShardKey> {
        Ok(q::ShardKey {
            key: Some(q::shard_key::Key::Keyword(string(v)?)),
        })
    };
    if v.is_string() {
        return Ok(Some(q::ShardKeySelector {
            shard_keys: vec![key(v)?],
            fallback: None,
        }));
    }
    Ok(Some(q::ShardKeySelector {
        shard_keys: vec![key(&v["target"])?],
        fallback: Some(key(&v["fallback"])?),
    }))
}
pub fn search_params(v: &Value) -> Result<Option<q::SearchParams>> {
    if v.is_null() {
        return Ok(None);
    }
    let idf = if v["idf"].is_null() {
        None
    } else {
        Some(q::IdfParams {
            corpus: filter(&v["idf"]["corpus"])?,
        })
    };
    Ok(Some(q::SearchParams {
        hnsw_ef: v["hnsw_ef"].as_u64(),
        exact: v["exact"].as_bool(),
        indexed_only: v["indexed_only"].as_bool(),
        idf,
        quantization: (!v["quantization"].is_null()).then(|| q::QuantizationSearchParams {
            ignore: v["quantization"]["ignore"].as_bool(),
            rescore: v["quantization"]["rescore"].as_bool(),
            oversampling: v["quantization"]["oversampling"].as_f64(),
        }),
        acorn: (!v["acorn"].is_null()).then(|| q::AcornSearchParams {
            enable: v["acorn"]["enable"].as_bool(),
            max_selectivity: v["acorn"]["max_selectivity"].as_f64(),
        }),
    }))
}
pub fn prefetch(v: &Value) -> Result<Vec<q::PrefetchQuery>> {
    if v.is_null() {
        return Ok(vec![]);
    }
    let a = v
        .as_array()
        .ok_or(Failure::Permanent(QueryError::VectorStore))?;
    a.iter()
        .map(|v| {
            Ok(q::PrefetchQuery {
                query: query(&v["query"])?,
                params: search_params(&v["params"])?,
                prefetch: prefetch(&v["prefetch"])?,
                using: v["using"].as_str().map(str::to_owned),
                filter: filter(&v["filter"])?,
                limit: v["limit"].as_u64(),
                score_threshold: v["score_threshold"].as_f64().map(|v| v as f32),
                ..Default::default()
            })
        })
        .collect()
}
pub fn with_payload(v: &Value) -> Option<q::WithPayloadSelector> {
    v.as_bool().map(Into::into)
}
pub fn with_vectors(v: &Value) -> Option<q::WithVectorsSelector> {
    v.as_bool().map(Into::into)
}
pub fn update(v: q::PointsOperationResponse) -> Result<Value> {
    let v = v
        .result
        .ok_or(Failure::Permanent(QueryError::VectorStore))?;
    let status = q::UpdateStatus::try_from(v.status)
        .map_err(|_| Failure::Permanent(QueryError::VectorStore))?;
    Ok(json!({"operation_id":v.operation_id,"status":status.as_str_name().to_lowercase()}))
}
pub fn point(v: q::RetrievedPoint) -> Value {
    json!({"id":id_json(v.id),"payload":payload_json(v.payload),"vector":vectors_json(v.vectors)})
}
pub fn scored(v: q::ScoredPoint) -> Value {
    json!({"id":id_json(v.id),"payload":payload_json(v.payload),"vector":vectors_json(v.vectors),"score":v.score,"version":v.version})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_filters_preserve_tenant_dataset_and_point_ids() {
        let f=filter(&json!({"must":[{"key":"tenant_id","match":{"value":"t1"}},{"key":"dataset_id","match":{"value":"d1"}},{"has_id":[17,"deadbeef-dead-beef-dead-beefdeadbeef"]}]})).unwrap().unwrap();
        assert_eq!(f.must.len(), 3);
        assert!(matches!(
            f.must[2].condition_one_of,
            Some(q::condition::ConditionOneOf::HasId(_))
        ));
    }
    #[test]
    fn dense_sparse_and_server_inference_are_distinct_wire_variants() {
        assert!(matches!(
            vector(&json!([0.1, 0.2])).unwrap().vector,
            Some(q::vector::Vector::Dense(_))
        ));
        assert!(matches!(
            vector(&json!({"indices":[2,8],"values":[1.,3.]}))
                .unwrap()
                .vector,
            Some(q::vector::Vector::Sparse(_))
        ));
        assert!(matches!(vector(&json!({"text":"中文 retrieval","model":"Qdrant/bm25","options":{"language":"chinese"}})).unwrap().vector,Some(q::vector::Vector::Document(_))));
    }
    #[test]
    fn weighted_rrf_and_named_vectors_preserve_source_shapes() {
        let q = query(&json!({"rrf":{"k":60,"weights":[1.,2.]}}))
            .unwrap()
            .unwrap();
        assert!(
            matches!(q.variant,Some(q::query::Variant::Rrf(q::Rrf{k:Some(60),weights})) if weights==vec![1.,2.])
        );
        let v =
            vectors(&json!({"":[1.,2.],"bm25":{"text":"query","model":"Qdrant/bm25"}})).unwrap();
        assert!(
            matches!(v.vectors_options,Some(q::vectors::VectorsOptions::Vectors(v)) if v.vectors.len()==2)
        );
    }
}
#[cfg(test)]
mod optimized_search_tests {
    use super::*;
    #[test]
    fn typed_search_keeps_tenant_idf_acorn_prefix_and_fallback() {
        let p=search_params(&json!({"hnsw_ef":128,"quantization":{"rescore":true,"oversampling":2.0},"acorn":{"enable":true,"max_selectivity":0.4},"idf":{"corpus":{"must":[{"key":"tenant_id","match":{"value":"t"}},{"key":"dataset_id","match":{"value":"d"}}]}}})).unwrap().unwrap();
        assert_eq!(p.hnsw_ef, Some(128));
        assert_eq!(p.quantization.unwrap().rescore, Some(true));
        assert_eq!(p.idf.unwrap().corpus.unwrap().must.len(), 2);
        assert_eq!(p.acorn.unwrap().enable, Some(true));
        let selector = shard_selector(&json!({"target":"tenant","fallback":"shared"}))
            .unwrap()
            .unwrap();
        assert_eq!(selector.shard_keys.len(), 1);
        assert!(selector.fallback.is_some());
        let c =
            condition(&json!({"key":"metadata.source_path","match":{"prefix":"manual/"}})).unwrap();
        assert!(matches!(
            c.condition_one_of,
            Some(q::condition::ConditionOneOf::Field(q::FieldCondition {
                r#match: Some(q::Match {
                    match_value: Some(q::r#match::MatchValue::Prefix(_))
                }),
                ..
            }))
        ));
    }
}

#[cfg(test)]
mod output_protocol_upgrade_tests {
    use super::*;
    #[test]
    #[allow(deprecated)] // Exercise actual 1.18 peer fields during the 1.19 transition.
    fn legacy_and_119_multivectors_have_identical_rows() {
        let expected = json!([[1., 2., 3.], [4., 5., 6.]]);
        let old = q::VectorOutput {
            data: vec![1., 2., 3., 4., 5., 6.],
            vectors_count: Some(2),
            ..Default::default()
        };
        let new = q::VectorOutput {
            vector: Some(q::vector_output::Vector::MultiDense(q::MultiDenseVector {
                vectors: vec![
                    q::DenseVector {
                        data: vec![1., 2., 3.],
                    },
                    q::DenseVector {
                        data: vec![4., 5., 6.],
                    },
                ],
            })),
            ..Default::default()
        };
        assert_eq!(vector_json(old), expected);
        assert_eq!(vector_json(new), expected);
    }
    #[test]
    #[allow(deprecated)]
    fn malformed_legacy_multivector_never_panics_or_changes_shape() {
        for (data, count) in [
            (vec![1., 2.], 0),
            (vec![], 2),
            (vec![1., 2., 3.], 2),
            (vec![1.], 3),
        ] {
            assert!(
                vector_json(q::VectorOutput {
                    data,
                    vectors_count: Some(count),
                    ..Default::default()
                })
                .is_null()
            );
        }
        let conflicting = q::VectorOutput {
            data: vec![9.],
            vectors_count: Some(0),
            vector: Some(q::vector_output::Vector::Dense(q::DenseVector {
                data: vec![1., 2.],
            })),
            ..Default::default()
        };
        assert_eq!(vector_json(conflicting), json!([1., 2.]));
    }
    #[test]
    fn sparse_alignment_and_float32_range_are_validated_before_wire_dispatch() {
        assert!(vector(&json!({"indices":[1,2],"values":[1.]})).is_err());
        assert!(vector(&json!([1e100])).is_err());
        assert!(vector(&json!({"indices":[4294967296u64],"values":[1.]})).is_err());
        assert!(vector(&json!({"indices":[1,2],"values":[1.,2.]})).is_ok());
    }
}
