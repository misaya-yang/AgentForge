//! Real native embedding adapters against an isolated loopback provider.
//! No provider SDK monkeypatch, database, external request, or model weights.
use ai_platform_knowledge_service::migration_knowledge_query::{
    embedding::{EmbeddingManager, ProviderConfig},
    types::Dataset,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};
struct Fixture {
    endpoint: String,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture(wrong_dimension: bool) -> Fixture {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let state = (calls.clone(), wrong_dimension);
    let app=Router::new().fallback(|State((calls,wrong)):State<(Arc<Mutex<Vec<(String,Value)>>>,bool)>,headers:HeaderMap,Json(body):Json<Value>|async move {
        let key=headers.get("authorization").unwrap().to_str().unwrap().to_owned();
        calls.lock().unwrap().push((key,body.clone()));
        if body["input"]["contents"][0]["image"].as_str().is_some_and(|s|s.ends_with("YmFk")) {
            return (StatusCode::BAD_REQUEST,Json(json!({"error":"fixture failure"}))).into_response();
        }
        let dimension=if wrong {2}else{body["parameters"]["dimension"].as_u64().unwrap() as usize};
        let count=body["input"]["texts"].as_array().map_or(1,Vec::len);
        Json(json!({"output":{"embeddings":(0..count).map(|index|json!({"text_index":index,"embedding":vec![0.25;dimension]})).collect::<Vec<_>>()}})).into_response()
    }).with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Fixture {
        endpoint,
        calls,
        server,
    }
}
fn manager(f: &Fixture, provider: &str, key: &str) -> EmbeddingManager {
    EmbeddingManager::new(
        reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .build()
            .unwrap(),
        None,
        String::new(),
        BTreeMap::from([(
            provider.into(),
            ProviderConfig {
                api_key: key.into(),
                base_url: Some(f.endpoint.clone()),
                timeout: Duration::from_secs(5),
            },
        )]),
    )
}
fn dataset(provider: &str, dimension: usize) -> Dataset {
    Dataset {
        dataset_id: "fixture".into(),
        tenant_id: "tenant-fixture".into(),
        embedding_provider: Some(provider.into()),
        embedding_model: Some(
            if provider == "dashscope" {
                "text-embedding-v4"
            } else {
                "multimodal-embedding-v1"
            }
            .into(),
        ),
        embedding_dimension: Some(dimension),
        ..Default::default()
    }
}
#[tokio::test]
async fn query_cache_is_dimension_scoped_and_returns_owned_copies() {
    let f = fixture(false).await;
    let m = manager(&f, "dashscope", "fixture-key");
    let d512 = dataset("dashscope", 512);
    let d1024 = dataset("dashscope", 1024);
    let mut first = m.embed_query(&d512, "same query").await.unwrap();
    let second = m.embed_query(&d1024, "same query").await.unwrap();
    assert_eq!(first.len(), 512);
    assert_eq!(second.len(), 1024);
    assert_eq!(f.calls.lock().unwrap().len(), 2);
    first[0] = 99.0;
    assert_eq!(m.embed_query(&d512, "same query").await.unwrap()[0], 0.25);
    assert_eq!(f.calls.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn query_and_document_text_types_do_not_share_cached_vectors() {
    let f = fixture(false).await;
    let m = manager(&f, "dashscope", "fixture-key");
    let d = dataset("dashscope", 512);
    m.embed_query(&d, "same query").await.unwrap();
    m.embed_documents(&d, &["same query".into()]).await.unwrap();
    m.embed_query(&d, "same query").await.unwrap();
    let calls = f.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1["parameters"]["text_type"], "query");
    assert_eq!(calls[1].1["parameters"]["text_type"], "document");
}
#[tokio::test]
async fn endpoint_and_credential_instances_keep_each_request_identity() {
    let a = fixture(false).await;
    let b = fixture(false).await;
    let first = manager(&a, "dashscope", "credential-a");
    let second = manager(&b, "dashscope", "credential-b");
    assert_eq!(
        first
            .embed_query(&dataset("dashscope", 512), "same")
            .await
            .unwrap()
            .len(),
        512
    );
    assert_eq!(
        second
            .embed_query(&dataset("dashscope", 1024), "same")
            .await
            .unwrap()
            .len(),
        1024
    );
    let calls_a = a.calls.lock().unwrap();
    let calls_b = b.calls.lock().unwrap();
    assert_eq!(calls_a.len(), 1);
    assert_eq!(calls_b.len(), 1);
    assert_eq!(calls_a[0].0, "Bearer credential-a");
    assert_eq!(calls_b[0].0, "Bearer credential-b");
    assert_eq!(calls_a[0].1["parameters"]["dimension"], 512);
    assert_eq!(calls_b[0].1["parameters"]["dimension"], 1024);
}
#[tokio::test]
async fn text_and_image_dimensions_are_forwarded_and_mismatches_refused() {
    let bad = fixture(true).await;
    let text = manager(&bad, "dashscope", "fixture-key");
    assert!(
        text.embed_query(&dataset("dashscope", 512), "same")
            .await
            .is_err()
    );
    let image = manager(&bad, "dashscope_multimodal", "fixture-key");
    assert!(
        image
            .embed_images(&dataset("dashscope_multimodal", 512), &[b"good".to_vec()])
            .await
            .is_err()
    );
    let calls = bad.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert!(
        calls
            .iter()
            .all(|(_, body)| body["parameters"]["dimension"] == 512)
    );
}
#[tokio::test]
async fn both_multimodal_image_batches_fail_instead_of_returning_partial_vectors() {
    for provider in ["dashscope_multimodal", "unified_multimodal"] {
        let f = fixture(false).await;
        let m = manager(&f, provider, "fixture-key");
        assert!(
            m.embed_images(
                &dataset(provider, 512),
                &[b"good".to_vec(), b"bad".to_vec()]
            )
            .await
            .is_err()
        );
        assert_eq!(f.calls.lock().unwrap().len(), 2);
    }
}
