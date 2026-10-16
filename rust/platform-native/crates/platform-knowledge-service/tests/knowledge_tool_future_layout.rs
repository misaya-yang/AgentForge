//! Measures public future layouts without constructing or polling those futures.
//! The closed lazy pool fixture prevents accidental backend access.
use ai_platform_knowledge_service::{
    KnowledgeBackends, KnowledgeConnections, KnowledgeGraph,
    langgraph_tools::{
        DifyCompatibleKBAPI, KBToolConfig, KnowledgeBaseTool, KnowledgeRetriever,
        MultiKnowledgeBaseTool,
    },
    migration_knowledge_query::config::RetrieveRequest,
    process::ProcessSettings,
};
use serde_json::json;
use std::{collections::BTreeMap, future::Future};
fn future_size<F: Future>(_: impl FnOnce() -> F) -> usize {
    std::mem::size_of::<F>()
}
#[tokio::test]
async fn public_tool_future_layout() {
    let config = ProcessSettings::from_values(
        BTreeMap::from([
            ("ENVIRONMENT".into(), "test".into()),
            (
                "AI_PLATFORM_INTERNAL_TOKEN".into(),
                "offline-tools-signing-material".into(),
            ),
            (
                "AI_PLATFORM_CAPABILITY_PROOF_SECRET".into(),
                "offline-tools-proof-material".into(),
            ),
            ("INTERNAL_COMM_STATE_BACKEND".into(), "memory".into()),
            ("INTERNAL_IDEMPOTENCY_BACKEND".into(), "memory".into()),
            ("KNOWLEDGE_RUNTIME_ROLE".into(), "api".into()),
        ]),
        "/tmp/knowledge-future-layout".into(),
    )
    .unwrap();
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://127.0.0.1:1/unavailable")
        .unwrap();
    pool.close().await;
    let service = KnowledgeGraph::new(
        KnowledgeConnections {
            database: pool,
            replay_redis: None,
            idempotency_redis: None,
        },
        config.configuration,
        KnowledgeBackends::default(),
    )
    .unwrap()
    .retrieval;
    let user = KnowledgeRetriever::create_system_context();
    let retriever = KnowledgeRetriever::new(
        service.clone(),
        "dataset".into(),
        user.clone(),
        KBToolConfig::default(),
    )
    .unwrap();
    let single = KnowledgeBaseTool::new(
        service.clone(),
        "dataset".into(),
        user.clone(),
        None,
        None,
        KBToolConfig::default(),
    )
    .unwrap();
    let multi = MultiKnowledgeBaseTool::new(
        service.clone(),
        vec!["dataset".into()],
        user.clone(),
        "search".into(),
        None,
        KBToolConfig::default(),
    )
    .unwrap();
    let dify = DifyCompatibleKBAPI::new(service.clone(), user.clone()).unwrap();
    for (name, size) in [
        (
            "RetrievalService::retrieve",
            future_size(|| service.retrieve(&user, "dataset", RetrieveRequest::default())),
        ),
        (
            "KnowledgeRetriever::retrieve",
            future_size(|| retriever.retrieve(json!({"query":"q"}))),
        ),
        (
            "KnowledgeBaseTool::arun",
            future_size(|| single.arun(json!({"query":"q"}))),
        ),
        (
            "MultiKnowledgeBaseTool::arun",
            future_size(|| multi.arun(json!({"query":"q"}))),
        ),
        (
            "DifyCompatibleKBAPI::retrieve",
            future_size(|| dify.retrieve("dataset", json!("q"), json!({}))),
        ),
    ] {
        println!("{name}: {size} bytes");
        // A generous stable bound, not an ABI-size assertion. The old inline
        // pipeline was311320–311696bytes and exhausted default test stacks.
        assert!(size <= 16 * 1024, "{name} future has grown to {size} bytes");
    }
}
