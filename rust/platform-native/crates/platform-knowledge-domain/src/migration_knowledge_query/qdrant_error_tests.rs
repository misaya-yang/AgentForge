//! Qdrant protocol/security regressions.
use super::{QdrantStore, resilience::Failure};
use reqwest::Method;
use serde_json::json;

#[test]
fn qdrant_failure_diagnostics_are_bounded_and_never_echo_response_text() {
    let dimension = json!({"status":{"error":"Wrong input: Vector dimension error: expected dim: 1024, got 1536"}});
    assert_eq!(
        QdrantStore::qdrant_error_class(&dimension),
        "vector_dimension_mismatch"
    );
    let missing = json!({"status":{"error":"Collection `kb-private` not found"}});
    assert_eq!(
        QdrantStore::qdrant_error_class(&missing),
        "collection_not_found"
    );
    let unknown = json!({"status":{"error":"detail containing an opaque token value"}});
    assert_eq!(
        QdrantStore::qdrant_error_class(&unknown),
        "upstream_response_error"
    );
    assert_eq!(
        QdrantStore::operation_name(&Method::POST, &["points", "query"]),
        "points_query"
    );
    assert_eq!(
        QdrantStore::operation_name(&Method::GET, &["collections"]),
        "collections_list"
    );
    assert_eq!(
        QdrantStore::dense_query_dimension(&json!({"query":[0.1,0.2,0.3]})),
        Some(3)
    );
    assert_eq!(
        QdrantStore::dense_query_dimension(
            &json!({"prefetch":[{"query":[0.1,0.2]}],"query":{"rrf":{"k":60}}})
        ),
        Some(2)
    );
    assert_eq!(
        QdrantStore::dense_query_dimension(&json!({"query":{"indices":[1],"values":[0.5]}})),
        None
    );
    let info = json!({"config":{"params":{"vectors":{"size":1024,"distance":"Cosine"}}}});
    assert_eq!(QdrantStore::collection_vector_dimension(&info), Some(1024));
    assert_eq!(
        QdrantStore::resilience_failure_class(&Failure::<()>::Deadline),
        "interactive_deadline"
    );
    assert_eq!(
        QdrantStore::resilience_failure_class(&Failure::<()>::CircuitOpen),
        "circuit_open"
    );
}
