//! Eval transport contracts against isolated loopback responders, never providers.
use ai_platform_gateway::migration_compat::eval_clients::{
    EvalGatewayLlmClient, EvalJwtSigner, KbRagasClient, LlmCompleteContext,
};
use axum::{
    Router,
    body::Bytes,
    http::{HeaderMap, StatusCode, Uri},
    response::IntoResponse,
    routing::post,
};
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
type Captures = Arc<Mutex<Vec<(String, HeaderMap, Value)>>>;
struct Server {
    base: String,
    captured: Captures,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn server(status: StatusCode, response: Value) -> Server {
    let captured = Captures::default();
    let sink = captured.clone();
    let app = Router::new().fallback(post(move |uri: Uri, headers: HeaderMap, body: Bytes| {
        let sink = sink.clone();
        let response = response.clone();
        async move {
            sink.lock().unwrap().push((
                uri.path().to_owned(),
                headers,
                serde_json::from_slice(&body).unwrap(),
            ));
            (status, axum::Json(response)).into_response()
        }
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server {
        base,
        captured,
        task,
    }
}
fn kb(base: &str, secret: Option<Vec<u8>>) -> KbRagasClient {
    KbRagasClient::new(base, secret, "fixture-key".into(), Duration::from_secs(3)).unwrap()
}
fn good() -> Value {
    json!({"metric":"context_relevancy","score":0.8,"explanation":"Relevant.","label":"pass"})
}
#[tokio::test]
async fn ragas_payload_and_infrastructure_results_preserve_contract() {
    let s=server(StatusCode::OK,json!({"judge_model":"qwen-test","results":[{"metric":"context_relevancy","score":0.0,"explanation":"Judge unavailable.","label":"review","failure_kind":"infrastructure"}]})).await;
    let r = kb(&s.base, None)
        .evaluate_retrieval(
            "refund policy",
            &["chunk one".into()],
            Some("Refunds are available for 30 days."),
            Some(&["context_relevancy".into()]),
            None,
            None,
        )
        .await
        .unwrap();
    let results = r["results"].as_array().unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["metric"], "context_relevancy");
    assert_eq!(results[0]["score"], 0.0);
    assert_eq!(results[0]["judge_model"], "qwen-test");
    assert_eq!(results[0]["failure_kind"], "infrastructure");
    let calls = s.captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "/api/v1/internal/eval/ragas");
    let p = &calls[0].2;
    assert_eq!(p["query"], "refund policy");
    assert_eq!(p["contexts"], json!(["chunk one"]));
    assert_eq!(p["answer"], "Refunds are available for 30 days.");
    assert!(p["llm_config"].is_null());
}
#[tokio::test]
async fn ragas_safe_selector_is_forwarded_and_sensitive_override_never_dispatches() {
    let s = server(
        StatusCode::OK,
        json!({"judge_model":"qwen-test","results":[good()]}),
    )
    .await;
    let client = kb(&s.base, None);
    let selector = json!({"provider":"dashscope","model":"qwen-test"});
    client
        .evaluate_retrieval("q", &["c".into()], None, None, None, Some(&selector))
        .await
        .unwrap();
    assert_eq!(s.captured.lock().unwrap()[0].2["llm_config"], selector);
    let e = client
        .evaluate_retrieval(
            "q",
            &["c".into()],
            None,
            None,
            None,
            Some(&json!({"api_key":"caller-controlled"})),
        )
        .await
        .unwrap_err();
    assert!(
        e.detail
            .as_str()
            .unwrap()
            .contains("only accepts provider and model")
    );
    assert_eq!(s.captured.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn ragas_http_error_and_five_malformed_collections_fail_closed() {
    let s = server(StatusCode::INTERNAL_SERVER_ERROR, json!({"error":"boom"})).await;
    let e = kb(&s.base, None)
        .evaluate_retrieval("q", &["c".into()], None, None, None, None)
        .await
        .unwrap_err();
    assert!(e.detail.as_str().unwrap().contains("HTTP 500"));
    for results in [
        json!([good(), "bad"]),
        json!([{"metric":" ","score":0.8,"explanation":"Relevant.","label":"pass"}]),
        json!([good(), good()]),
        json!("not-a-list"),
        json!([]),
    ] {
        let s = server(
            StatusCode::OK,
            json!({"judge_model":"qwen-test","results":results}),
        )
        .await;
        let e = kb(&s.base, None)
            .evaluate_retrieval("q", &["c".into()], None, None, None, None)
            .await
            .unwrap_err();
        assert_eq!(
            e.detail,
            "knowledge-service RAGAS eval returned invalid results"
        );
    }
}
#[tokio::test]
async fn ragas_injected_internal_secret_produces_bound_gateway_auth() {
    let s = server(
        StatusCode::OK,
        json!({"judge_model":"qwen-test","results":[good()]}),
    )
    .await;
    kb(&s.base, Some(b"knowledge-secret".to_vec()))
        .evaluate_retrieval("q", &["c".into()], None, None, None, None)
        .await
        .unwrap();
    let c = s.captured.lock().unwrap();
    let h = &c[0].1;
    let auth = h["x-gateway-secret"].to_str().unwrap();
    let parts = auth.split(':').collect::<Vec<_>>();
    assert_eq!(parts.len(), 6);
    assert_eq!(parts[0], "v2");
    assert_eq!(parts[1], "fixture-key");
    assert_eq!(parts[2], h["x-request-id"].to_str().unwrap());
    assert_eq!(parts[4].len(), 64);
    assert_eq!(parts[5].len(), 64);
}
const SECRET: &[u8] = b"fixture-signing-key-fixture-signing-key-fixture-signing-key";
#[tokio::test]
async fn ragas_scoped_actor_body_and_identity_headers_share_the_workload_signature() {
    use hmac::{Hmac, Mac};
    use sha2::{Digest, Sha256};
    let s = server(
        StatusCode::OK,
        json!({"judge_model":"qwen-test","results":[good()]}),
    )
    .await;
    let scope = serde_json::from_value(json!({"actor":{"tenant_id":"tenant-a","user_id":"member-a","user_tier":"normal","user_type":"user","roles":["user"],"is_authenticated":true},"dataset_id":null,"document_id":null,"execution_id":null})).unwrap();
    kb(&s.base, Some(SECRET.to_vec()))
        .evaluate_retrieval_scoped(&scope, "q", &["c".into()], None, None, None, None)
        .await
        .unwrap();
    let calls = s.captured.lock().unwrap();
    let (_, headers, body) = &calls[0];
    assert_eq!(headers["x-user-id"], "member-a");
    assert_eq!(headers["x-tenant-id"], "tenant-a");
    assert_eq!(body["actor_scope"]["actor"]["user_id"], "member-a");
    let parts = headers["x-gateway-secret"]
        .to_str()
        .unwrap()
        .split(':')
        .collect::<Vec<_>>();
    let hash = format!("{:x}", Sha256::digest(serde_json::to_vec(body).unwrap()));
    assert_eq!(parts[4], hash);
    let identities = [
        "x-tenant-id",
        "x-user-id",
        "x-user-roles",
        "x-user-tier",
        "x-user-type",
    ]
    .iter()
    .map(|name| format!("{name}:{}", headers[*name].to_str().unwrap()))
    .collect::<Vec<_>>()
    .join("\n");
    let canonical = [
        "gateway",
        "knowledge-service",
        "POST",
        "/api/v1/internal/eval/ragas",
        "",
        &hash,
        &identities,
        parts[2],
        parts[3],
        parts[1],
    ]
    .join("\n");
    let mut mac = Hmac::<Sha256>::new_from_slice(SECRET).unwrap();
    mac.update(canonical.as_bytes());
    let actual = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    assert_eq!(parts[5], actual);
}
#[tokio::test]
async fn ragas_scoped_call_rejects_missing_signer_and_anonymous_before_dispatch() {
    let s = server(
        StatusCode::OK,
        json!({"judge_model":"qwen-test","results":[good()]}),
    )
    .await;
    let mut scope: ai_platform_service_primitives::knowledge_model::ModelScope = serde_json::from_value(json!({"actor":{"tenant_id":"tenant-a","user_id":"member-a","is_authenticated":true},"dataset_id":null,"document_id":null,"execution_id":null})).unwrap();
    assert!(
        kb(&s.base, None)
            .evaluate_retrieval_scoped(&scope, "q", &[], None, None, None, None)
            .await
            .is_err()
    );
    scope.actor.is_authenticated = false;
    assert!(
        kb(&s.base, Some(SECRET.to_vec()))
            .evaluate_retrieval_scoped(&scope, "q", &[], None, None, None, None)
            .await
            .is_err()
    );
    assert!(s.captured.lock().unwrap().is_empty());
}
fn signer() -> Arc<EvalJwtSigner> {
    Arc::new(EvalJwtSigner::new(SECRET, Algorithm::HS256, None, None).unwrap())
}
fn claims(token: &str) -> Value {
    let mut validation = Validation::new(Algorithm::HS256);
    validation.validate_aud = false;
    jsonwebtoken::decode::<Value>(token, &DecodingKey::from_secret(SECRET), &validation)
        .unwrap()
        .claims
}
#[tokio::test]
async fn llm_judge_posts_bounded_tool_free_request_and_context() {
    let s = server(
        StatusCode::OK,
        json!({"output_text":"{\"numeric_value\":0.8,\"confidence\":0.7,\"label\":\"pass\"}"}),
    )
    .await;
    let client = EvalGatewayLlmClient::new(
        &s.base,
        signer(),
        Duration::from_secs(3),
        true,
        "judge-model".into(),
        "system".into(),
    )
    .unwrap();
    let text = client
        .complete(
            "judge-model",
            "Score this answer",
            &LlmCompleteContext {
                tenant_id: "tenant-a".into(),
                trace_family: "assistant".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(text.contains("0.8"));
    let calls = s.captured.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "/v1/responses");
    let headers = &calls[0].1;
    let auth = headers["authorization"].to_str().unwrap();
    assert!(auth.starts_with("Bearer "));
    assert!(!headers.contains_key("x-tenant-id"));
    assert!(!headers.contains_key("x-user-roles"));
    assert_eq!(
        claims(auth.strip_prefix("Bearer ").unwrap())["tenant_id"],
        "tenant-a"
    );
    let p = &calls[0].2;
    assert_eq!(p["input"], "Score this answer");
    assert_eq!(p["model"], "judge-model");
    assert_eq!(p["tools"], json!([]));
    assert_eq!(p["tool_choice"], "none");
    assert_eq!(p["parallel_tool_calls"], false);
    assert_eq!(p["max_output_tokens"], 512);
    assert_eq!(p["stream"], false);
}
#[tokio::test]
async fn disabled_judge_never_dispatches() {
    let s = server(StatusCode::OK, json!({"output_text":"unused"})).await;
    let client = EvalGatewayLlmClient::new(
        &s.base,
        signer(),
        Duration::from_secs(3),
        false,
        "judge-model".into(),
        "system".into(),
    )
    .unwrap();
    assert!(
        client
            .complete("judge-model", "hello", &LlmCompleteContext::default())
            .await
            .is_err()
    );
    assert!(s.captured.lock().unwrap().is_empty());
}
#[test]
fn delegated_eval_claims_bind_verified_subject_and_tenant() {
    let delegation = json!({"actor":"eval-worker","subject":"verified-user","tenant_id":"tenant-a","run_id":"run","job_id":"job"});
    let signer = signer();
    let c = claims(&signer.token("tenant-a", Some(&delegation)).unwrap());
    assert_eq!(c["tenant_id"], "tenant-a");
    assert_eq!(c["sub"], "verified-user");
    assert_eq!(c["user_id"], c["sub"]);
    assert_eq!(c["act"]["sub"], "eval-worker");
    assert_eq!(c["act"]["tenant_id"], "tenant-a");
    assert_eq!(c["roles"], json!(["user"]));
    assert_eq!(c["permissions"], json!([]));
    assert_eq!(c["exp"].as_i64().unwrap() - c["iat"].as_i64().unwrap(), 300);
    assert!(signer.token("tenant-b", Some(&delegation)).is_err());
}
async fn identity_server(status: StatusCode, responses: Vec<Value>) -> Server {
    let captured = Captures::default();
    let sink = captured.clone();
    let responses = Arc::new(Mutex::new(responses.into_iter()));
    let app = Router::new().fallback(axum::routing::any(
        move |method: axum::http::Method, uri: Uri, headers: HeaderMap| {
            let sink = sink.clone();
            let responses = responses.clone();
            async move {
                sink.lock().unwrap().push((
                    format!("{method} {}", uri.path()),
                    headers,
                    Value::Null,
                ));
                let response = responses
                    .lock()
                    .unwrap()
                    .next()
                    .unwrap_or(json!({"unexpected":"extra request"}));
                (
                    status,
                    [("location", "http://127.0.0.1:1/must-not-follow")],
                    axum::Json(response),
                )
            }
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Server {
        base,
        captured,
        task,
    }
}
fn candidate(
    base: &str,
    api_key: bool,
) -> ai_platform_gateway::migration_compat::eval_candidate::EvalCandidateClient {
    use ai_platform_gateway::migration_compat::{
        eval_candidate::EvalCandidateClient, eval_queue::EvalOutbox,
    };
    // Inert repository handle. Every case must fail at identity before any repository access.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(Duration::from_millis(50))
        .connect_lazy("postgresql://unused@127.0.0.1:1/unused")
        .unwrap();
    EvalCandidateClient::new(
        base,
        (!api_key).then(|| "test-token".into()),
        api_key.then(|| "test-key".into()),
        signer(),
        false,
        EvalOutbox {
            pool,
            enabled: false,
        },
    )
    .unwrap()
}
async fn candidate_error(
    client: &ai_platform_gateway::migration_compat::eval_candidate::EvalCandidateClient,
    tenant: &str,
) -> Value {
    client
        .run(
            &json!({"tenant_id":tenant}),
            &json!({"run_case_id":"case","input":{"message":"hello"}}),
            &json!({}),
        )
        .await
        .unwrap_err()
        .detail
}
#[tokio::test]
async fn mismatched_credentials_cannot_create_candidate_for_either_auth_mode() {
    for api_key in [false, true] {
        for (job, credential) in [("tenant-b", "tenant-a"), ("tenant-a", "tenant-b")] {
            let s = identity_server(
                StatusCode::OK,
                vec![json!({"user_id":"eval-user","tenant_id":credential})],
            )
            .await;
            let client = candidate(&s.base, api_key);
            assert_eq!(
                candidate_error(&client, job).await,
                "AGENT_EVAL_TENANT_MISMATCH"
            );
            let c = s.captured.lock().unwrap();
            assert_eq!(c.len(), 1);
            assert_eq!(c[0].0, "GET /api/v1/auth/me");
            assert!(!c[0].1.contains_key("x-tenant-id"));
            assert!(!c[0].1.contains_key("x-user-id"));
            if api_key {
                assert_eq!(c[0].1["x-api-key"], "test-key");
            } else {
                assert_eq!(c[0].1["authorization"], "Bearer test-token");
            }
        }
    }
}
#[tokio::test]
async fn nine_unscoped_or_malformed_identities_fail_before_writes() {
    for payload in [
        json!({}),
        json!([]),
        json!({"tenant_id":"tenant-a"}),
        json!({"user_id":"eval-user"}),
        json!({"user_id":"","tenant_id":"tenant-a"}),
        json!({"user_id":"eval-user","tenant_id":""}),
        json!({"user_id":"eval-user","tenant_id":"public"}),
        json!({"user_id":"eval-user","tenant_id":12}),
        json!({"user_id":"eval-user","tenant_id":" tenant-a "}),
    ] {
        let s = identity_server(StatusCode::OK, vec![payload]).await;
        assert_eq!(
            candidate_error(&candidate(&s.base, false), "tenant-a").await,
            "AGENT_EVAL_IDENTITY_INVALID"
        );
        let calls = s.captured.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "GET /api/v1/auth/me");
    }
}
#[tokio::test]
async fn identity_http_failure_never_redirects_writes_or_leaks_body() {
    for status in [
        StatusCode::FOUND,
        StatusCode::UNAUTHORIZED,
        StatusCode::FORBIDDEN,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        let s = identity_server(status, vec![json!("private response")]).await;
        let e = candidate_error(&candidate(&s.base, false), "tenant-a").await;
        assert_eq!(e, "AGENT_EVAL_IDENTITY_UNAVAILABLE");
        assert!(!e.to_string().contains("private"));
        let calls = s.captured.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "GET /api/v1/auth/me");
    }
}
#[tokio::test]
async fn reused_candidate_introspects_every_run() {
    let s = identity_server(
        StatusCode::OK,
        vec![
            json!({"user_id":"eval-user","tenant_id":"tenant-a"}),
            json!({"user_id":"eval-user","tenant_id":"tenant-b"}),
        ],
    )
    .await;
    let client = candidate(&s.base, false);
    for tenant in ["tenant-b", "tenant-a"] {
        assert_eq!(
            candidate_error(&client, tenant).await,
            "AGENT_EVAL_TENANT_MISMATCH"
        );
    }
    let c = s.captured.lock().unwrap();
    assert_eq!(
        c.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        vec!["GET /api/v1/auth/me", "GET /api/v1/auth/me"]
    );
}
#[tokio::test]
async fn identity_json_and_transport_failure_hide_private_context() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for malformed_json in [true, false] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = vec![0; 8192];
            let count = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(request.starts_with("GET /api/v1/auth/me HTTP/1.1\r\n"));
            if malformed_json {
                let body = "private non-json response";
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            } /* Otherwise close before headers: actual transport failure, without waiting 300 seconds. */
        });
        let e = candidate_error(&candidate(&base, false), "tenant-a").await;
        assert_eq!(
            e,
            if malformed_json {
                "AGENT_EVAL_IDENTITY_INVALID"
            } else {
                "AGENT_EVAL_IDENTITY_UNAVAILABLE"
            }
        );
        assert!(!e.to_string().contains("private"));
        task.await.unwrap();
    }
}
