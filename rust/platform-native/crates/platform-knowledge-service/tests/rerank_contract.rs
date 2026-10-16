//! Source test_text_reranker_factory contracts, exercised without model weights
//! or a network provider. The test backend counts calls; it is never production.
use ai_platform_knowledge_service::migration_knowledge_query::{
    rerank::{LocalRerankBackend, Reranker, normalize_model, normalize_provider},
    types::Result,
};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
struct CountingBackend(Arc<AtomicUsize>);
impl LocalRerankBackend for CountingBackend {
    fn identity(&self) -> String {
        "test-only-counting".into()
    }
    fn scores<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        documents: &'a [String],
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<f64>>> + Send + 'a>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok((0..documents.len()).map(|index| index as f64).collect()) })
    }
}
fn service() -> (Reranker, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let reranker = Reranker::new(reqwest::Client::new(), BTreeMap::new())
        .with_local_backend(Arc::new(CountingBackend(calls.clone())));
    (reranker, calls)
}
#[test]
fn factory_aliases_preserve_source_defaults() {
    assert_eq!(normalize_provider("", "bge-reranker-v2-m3"), "bge");
    assert_eq!(normalize_provider("", "BAAI/bge-reranker-v2-m3"), "bge");
    assert_eq!(normalize_model("dashscope", "gte-rerank"), "gte-rerank-v2");
    assert_eq!(normalize_model("dashscope", ""), "qwen3-rerank");
    assert_eq!(
        normalize_model("local", ""),
        "cross-encoder/ms-marco-MiniLM-L-12-v2"
    );
}
#[tokio::test]
async fn cache_identity_preserves_document_boundaries() {
    let (service, calls) = service();
    service
        .rerank("local", "", "query", &["a|||b".into(), "c".into()], 2)
        .await
        .unwrap();
    service
        .rerank("local", "", "query", &["a".into(), "b|||c".into()], 2)
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn smaller_top_n_never_truncates_later_full_response() {
    let (service, calls) = service();
    let docs = vec!["one".into(), "two".into()];
    let short = service
        .rerank("local", "", "query", &docs, 1)
        .await
        .unwrap();
    let full = service
        .rerank("local", "", "query", &docs, 2)
        .await
        .unwrap();
    let cached = service
        .rerank("local", "", "query", &docs, 2)
        .await
        .unwrap();
    assert_eq!(short.len(), 1);
    assert_eq!(full.len(), 2);
    assert_eq!(full[0].index, 1);
    assert_eq!(cached.len(), 2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn empty_documents_do_not_initialize_inference() {
    let (service, calls) = service();
    assert!(
        service
            .rerank("bge", "", "query", &[], 2)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}
