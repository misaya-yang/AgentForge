//! Public tool entrypoints must reject unsafe inputs before database/service work.
use ai_platform_knowledge_service::{
    KnowledgeBackends, KnowledgeConnections, KnowledgeGraph,
    langgraph_tools::*,
    migration_knowledge_query::{service::RetrievalService, types::QueryError},
    process::ProcessSettings,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
async fn service() -> Arc<RetrievalService> {
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
        "/tmp/knowledge-tool-parity".into(),
    )
    .unwrap();
    // Lazy construction only; validation errors must precede any database access.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://127.0.0.1:1/unavailable")
        .unwrap();
    pool.close().await;
    KnowledgeGraph::new(
        KnowledgeConnections {
            database: pool,
            replay_redis: None,
            idempotency_redis: None,
        },
        config.configuration,
        KnowledgeBackends::default(),
    )
    .unwrap()
    .retrieval
}
fn validation<T>(result: Result<T, QueryError>) {
    assert!(
        matches!(result, Err(QueryError::Validation(_))),
        "must fail at validation, before database/service errors"
    );
}
#[tokio::test]
async fn tool_constructors_and_schemas_are_bounded_and_text_only() {
    let s = service().await;
    let user = KnowledgeRetriever::create_system_context();
    for ids in [vec![], (0..9).map(|i| format!("dataset-{i}")).collect()] {
        assert!(
            MultiDatasetRetriever::new(s.clone(), ids.clone(), user.clone(), 5, "hybrid".into())
                .is_err()
        );
        assert!(
            MultiKnowledgeBaseTool::new(
                s.clone(),
                ids,
                user.clone(),
                "search".into(),
                None,
                KBToolConfig::default()
            )
            .is_err()
        );
    }
    for top_k in [0, 21] {
        assert!(
            KnowledgeRetriever::new(
                s.clone(),
                "dataset-a".into(),
                user.clone(),
                KBToolConfig {
                    top_k,
                    ..Default::default()
                }
            )
            .is_err()
        );
    }
    let retriever = KnowledgeRetriever::new(
        s.clone(),
        "dataset-a".into(),
        user.clone(),
        KBToolConfig::default(),
    )
    .unwrap();
    let single = KnowledgeBaseTool::new(
        s.clone(),
        "dataset-a".into(),
        user.clone(),
        None,
        None,
        KBToolConfig::default(),
    )
    .unwrap();
    let multi = MultiKnowledgeBaseTool::new(
        s.clone(),
        vec!["dataset-a".into()],
        user.clone(),
        "search".into(),
        None,
        KBToolConfig::default(),
    )
    .unwrap();
    for schema in [
        retriever.as_openai_function()["function"]["parameters"].clone(),
        single.args_schema,
        multi.args_schema,
    ] {
        assert_eq!(schema["properties"]["query"]["maxLength"], 4096);
        assert_eq!(schema["properties"]["top_k"]["minimum"], 1);
        assert_eq!(schema["properties"]["top_k"]["maximum"], 20);
        assert_eq!(
            schema["properties"]["intent"]["enum"],
            json!(["general", "find_document"])
        );
        assert!(!schema.to_string().contains("find_image"));
    }
}
#[tokio::test]
async fn every_tool_entrypoint_rejects_invalid_inputs_before_retrieval() {
    let s = service().await;
    let user = KnowledgeRetriever::create_system_context();
    let retriever = KnowledgeRetriever::new(
        s.clone(),
        "dataset-a".into(),
        user.clone(),
        KBToolConfig::default(),
    )
    .unwrap();
    let single = KnowledgeBaseTool::new(
        s.clone(),
        "dataset-a".into(),
        user.clone(),
        None,
        None,
        KBToolConfig::default(),
    )
    .unwrap();
    let multi = MultiKnowledgeBaseTool::new(
        s.clone(),
        vec!["dataset-a".into()],
        user.clone(),
        "search".into(),
        None,
        KBToolConfig::default(),
    )
    .unwrap();
    for input in [
        json!({"query":"q".repeat(4097)}),
        json!({"query":"query","top_k":21}),
        json!({"query":"query","top_k":true}),
        json!({"query":"query","top_k":"5"}),
        json!({"query":"query","intent":"find_image"}),
        json!({"query":"query","score_threshold":1.1}),
    ] {
        validation(retriever.retrieve(input.clone()).await);
        validation(single.arun(input.clone()).await);
        validation(multi.arun(input).await);
    }
    let dify = DifyCompatibleKBAPI::new(s, user).unwrap();
    for model in [
        json!({"top_k":21}),
        json!({"top_k":true}),
        json!({"top_k":"5"}),
        json!({"score_threshold":-0.1}),
    ] {
        validation(
            dify.retrieve("dataset-a", Value::String("query".into()), model)
                .await,
        );
    }
}
