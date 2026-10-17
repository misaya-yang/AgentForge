//! Actual reqwest adapter against an isolated loopback provider fixture.
#[path="common/model_plane_fixture.rs"]
mod model_plane_fixture;
use ai_platform_knowledge_service::migration_knowledge_query::rerank::{
    RerankProviderConfig, Reranker,
};
use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
struct Fixture {
    reranker: ScopedReranker,
    _plane:model_plane_fixture::Plane,
    calls: Arc<Mutex<Vec<Value>>>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
async fn fixture(schema: &str, rows: Value) -> Fixture {
    let calls = Arc::new(Mutex::new(vec![]));
    let state = (calls.clone(), rows);
    let router = Router::new()
        .route(
            "/reranks",
            post(
                |State((calls, rows)): State<(Arc<Mutex<Vec<Value>>>, Value)>,
                 Json(body): Json<Value>| async move {
                    calls.lock().unwrap().push(body);
                    Json(json!({"results":rows}))
                },
            ),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/reranks", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let reranker = Reranker::new(
        reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .build()
            .unwrap(),
        BTreeMap::from([(
            "dashscope".into(),
            RerankProviderConfig {
                api_key: "synthetic-key".into(),
                endpoint:endpoint.clone(),
                request_schema: schema.into(),
                instruct: None,
            },
        )]),
    );
    let plane=model_plane_fixture::Plane::new(endpoint,"synthetic-key".into()).await;
    Fixture {
        reranker:ScopedReranker{reranker,client:plane.client.clone()},
        _plane:plane,
        calls,
        server,
    }
}
struct ScopedReranker {reranker:Reranker,client:Arc<ai_platform_knowledge_service::migration_knowledge_query::model_plane::ModelPlaneClient>}
impl ScopedReranker {async fn rerank(&self,provider:&str,model:&str,query:&str,documents:&[String],top_n:usize)->ai_platform_knowledge_service::migration_knowledge_query::types::Result<Vec<ai_platform_knowledge_service::migration_knowledge_query::rerank::RerankResult>> {model_plane_fixture::run(self.client.clone(),self.reranker.rerank(provider,model,query,documents,top_n)).await}}
#[tokio::test]
async fn legacy_unsorted_provider_output_is_resorted() {
    let f=fixture("legacy",json!([{"index":0,"relevance_score":0.11},{"index":2,"relevance_score":0.97},{"index":1,"relevance_score":0.55}])).await;
    let result = f
        .reranker
        .rerank(
            "dashscope",
            "gte-rerank-v2",
            "query",
            &["a".into(), "b".into(), "c".into()],
            3,
        )
        .await
        .unwrap();
    assert_eq!(
        result.iter().map(|r| r.index).collect::<Vec<_>>(),
        vec![2, 1, 0]
    );
    assert!(
        result
            .windows(2)
            .all(|w| w[0].relevance_score >= w[1].relevance_score)
    );
    assert_eq!(f.calls.lock().unwrap()[0]["input"]["query"], "query");
}
#[tokio::test]
async fn flat_unsorted_provider_output_is_resorted() {
    let f=fixture("flat",json!([{"index":3,"relevance_score":0.02},{"index":1,"relevance_score":0.44},{"index":0,"relevance_score":0.88},{"index":2,"relevance_score":0.66}])).await;
    let result = f
        .reranker
        .rerank(
            "dashscope",
            "qwen3-rerank",
            "query",
            &["a".into(), "b".into(), "c".into(), "d".into()],
            4,
        )
        .await
        .unwrap();
    assert_eq!(
        result.iter().map(|r| r.index).collect::<Vec<_>>(),
        vec![0, 2, 1, 3]
    );
    assert!(
        result
            .windows(2)
            .all(|w| w[0].relevance_score >= w[1].relevance_score)
    );
    assert_eq!(f.calls.lock().unwrap()[0]["query"], "query");
}
#[tokio::test]
async fn cache_preserves_resorted_order_top_n_and_document_boundaries() {
    let f = fixture(
        "legacy",
        json!([{"index":0,"relevance_score":0.30},{"index":1,"relevance_score":0.75}]),
    )
    .await;
    let docs = vec!["a".into(), "b".into()];
    let live = f
        .reranker
        .rerank("dashscope", "gte-rerank-v2", "query", &docs, 2)
        .await
        .unwrap();
    let cached = f
        .reranker
        .rerank("dashscope", "gte-rerank-v2", "query", &docs, 2)
        .await
        .unwrap();
    assert_eq!(live.iter().map(|r| r.index).collect::<Vec<_>>(), vec![1, 0]);
    assert_eq!(
        live.iter()
            .map(|r| (r.index, r.relevance_score))
            .collect::<Vec<_>>(),
        cached
            .iter()
            .map(|r| (r.index, r.relevance_score))
            .collect::<Vec<_>>()
    );
    assert_eq!(f.calls.lock().unwrap().len(), 1);
    f.reranker
        .rerank("dashscope", "gte-rerank-v2", "query", &docs, 1)
        .await
        .unwrap();
    assert_eq!(f.calls.lock().unwrap().len(), 2);
    for docs in [
        vec!["a|||b".into(), "c".into()],
        vec!["a".into(), "b|||c".into()],
    ] {
        f.reranker
            .rerank("dashscope", "gte-rerank-v2", "query", &docs, 2)
            .await
            .unwrap();
    }
    assert_eq!(f.calls.lock().unwrap().len(), 4);
}
