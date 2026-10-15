use ai_platform_capability_contract::canonical_json_hash;
use ai_platform_capability_worker::RuntimeConnectorBinding;
use ai_platform_capability_worker::postgres_store::PostgresExecutionStore;
use ai_platform_capability_worker::read_capabilities::{
    PostgresSessionMemoryReadAdapter, PublicHttpResponse, ReadCapabilityConfig,
    ReadCapabilityContext, ReadCapabilityError, ReadCapabilityExecutor, ReadHttpAdapter,
    SessionMemoryReadAdapter, working_memory_key,
};
use async_trait::async_trait;
use reqwest::Url;
use serde_json::{Value, json};
use sqlx::postgres::PgPoolOptions;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::{IpAddr, Ipv4Addr},
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

type InternalPost = (Url, Vec<(String, String)>, Value);
type RecordedPosts = Arc<Mutex<Vec<InternalPost>>>;

#[derive(Clone, Default)]
struct FakeSessionMemory {
    values: Arc<Mutex<BTreeMap<String, Value>>>,
    reads: Arc<Mutex<Vec<(String, String, String)>>>,
}

#[async_trait]
impl SessionMemoryReadAdapter for FakeSessionMemory {
    async fn get_session_memory(
        &self,
        tenant_id: &str,
        session_id: &str,
        key: &str,
    ) -> Result<Option<Value>, ReadCapabilityError> {
        self.reads.lock().unwrap().push((
            tenant_id.to_string(),
            session_id.to_string(),
            key.to_string(),
        ));
        Ok(self.values.lock().unwrap().get(key).cloned())
    }
}

#[derive(Clone, Default)]
struct FakeReadHttpAdapter {
    posts: RecordedPosts,
    public: Arc<Mutex<Vec<PublicHttpResponse>>>,
}
#[async_trait]
impl ReadHttpAdapter for FakeReadHttpAdapter {
    async fn post_internal_json(
        &self,
        url: Url,
        headers: &[(String, String)],
        body: Value,
    ) -> Result<Value, ReadCapabilityError> {
        self.posts
            .lock()
            .unwrap()
            .push((url, headers.to_vec(), body.clone()));
        Ok(json!({"results":[{"text":"ok"}],"metadata":{}}))
    }
    async fn get_public(&self, _url: Url) -> Result<PublicHttpResponse, ReadCapabilityError> {
        self.public
            .lock()
            .unwrap()
            .pop()
            .ok_or(ReadCapabilityError::DownstreamUnavailable)
    }
}

fn root() -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("cap-worker-read-{suffix}"));
    fs::create_dir_all(&p).unwrap();
    p
}
fn ctx(tenant: &str) -> ReadCapabilityContext {
    ReadCapabilityContext {
        tenant_id: tenant.into(),
        user_id: "user-a".into(),
        session_id: "session-a".into(),
        execution_id: "exec-a".into(),
        tool_call_id: "tool-call-a".into(),
        run_id: "run-a".into(),
        capability_revision: 1,
        bound_dataset_ids: BTreeSet::from(["docs".into()]),
        connector_binding: None,
    }
}
fn executor(adapter: FakeReadHttpAdapter, workspace: PathBuf) -> ReadCapabilityExecutor {
    ReadCapabilityExecutor::new(
        ReadCapabilityConfig {
            knowledge_base_url: "http://knowledge:8092".into(),
            gateway_url: "http://gateway:8000".into(),
            workspace_root: workspace,
            internal_token: "x".repeat(32),
            proof_secret: "p".repeat(32),
        },
        Arc::new(adapter),
    )
    .unwrap()
}

fn memory_envelope(context: &ReadCapabilityContext, tasks: Value) -> Value {
    json!({
        "schema_version": "assistant-working-memory/v2",
        "owner_scope": working_memory_key(&context.tenant_id, &context.user_id, &context.session_id)
            .strip_prefix("working_memory:").unwrap(),
        "working_memory": {
            "session_id": context.session_id,
            "tasks": tasks
        }
    })
}

fn memory_executor(http: FakeReadHttpAdapter, memory: FakeSessionMemory) -> ReadCapabilityExecutor {
    executor(http, root()).with_session_memory(Arc::new(memory))
}

#[tokio::test]
async fn web_search_uses_the_scope_bound_gateway_broker() {
    let http = FakeReadHttpAdapter::default();
    let posts = Arc::clone(&http.posts);
    let value = executor(http, root())
        .execute(
            "search_web",
            &ctx("tenant-a"),
            json!({"queries": ["rust agent runtime"], "max_results": 3}),
        )
        .await
        .unwrap();

    assert!(value["results"].is_array());
    let posts = posts.lock().unwrap();
    assert_eq!(posts.len(), 1);
    assert_eq!(
        posts[0].0.path(),
        "/internal/v2/agent-capabilities/web-search"
    );
    assert_eq!(
        posts[0].2,
        json!({"queries": ["rust agent runtime"], "max_results": 3})
    );
    assert!(
        posts[0]
            .1
            .iter()
            .any(|(name, value)| name == "x-ai-capability-proof" && !value.is_empty())
    );
}

fn store_memory(memory: &FakeSessionMemory, context: &ReadCapabilityContext, envelope: Value) {
    memory.values.lock().unwrap().insert(
        working_memory_key(&context.tenant_id, &context.user_id, &context.session_id),
        envelope,
    );
}

#[tokio::test]
#[allow(clippy::disallowed_methods)]
async fn postgres_memory_reader_is_constructed_from_the_execution_store_pool() {
    let pool = PgPoolOptions::new()
        .connect_lazy("postgres://user:password@localhost/test")
        .unwrap();
    let store = PostgresExecutionStore::from_pool(pool.clone());
    let reader = PostgresSessionMemoryReadAdapter::new(store.pool());
    let shared = reader.pool();
    pool.close().await;
    assert!(shared.is_closed());
}

#[tokio::test]
async fn todo_read_returns_stable_empty_object_and_v2_key() {
    let http = FakeReadHttpAdapter::default();
    let memory = FakeSessionMemory::default();
    let reads = memory.reads.clone();
    let context = ctx("tenant-a");
    let value = memory_executor(http, memory)
        .execute("todo_read", &context, json!({}))
        .await
        .unwrap();
    assert_eq!(
        value,
        json!({
            "markdown": "(no tasks)",
            "task_count": 0,
            "progress": {"total": 0, "completed": 0, "failed": 0, "percentage": 0}
        })
    );
    assert_eq!(
        reads.lock().unwrap().as_slice(),
        &[(
            "tenant-a".to_string(),
            "session-a".to_string(),
            working_memory_key("tenant-a", "user-a", "session-a"),
        )]
    );
}

#[tokio::test]
async fn todo_read_renders_all_statuses_and_progress() {
    let http = FakeReadHttpAdapter::default();
    let memory = FakeSessionMemory::default();
    let context = ctx("tenant-a");
    store_memory(
        &memory,
        &context,
        memory_envelope(
            &context,
            json!([
            {"id":"a","description":"one\ninjected","status":"pending"},
            {"id":"b","description":"two","status":"in_progress"},
            {"id":"c","description":"three","status":"completed"},
            {"id":"d","description":"four","status":"failed","error":"bad"},
            {"id":"e","description":"five","status":"blocked"}
            ]),
        ),
    );
    let value = memory_executor(http, memory)
        .execute("todo_read", &context, json!({}))
        .await
        .unwrap();
    assert_eq!(value["task_count"], 5);
    assert_eq!(
        value["progress"],
        json!({
            "total": 5, "completed": 1, "failed": 1, "percentage": 20.0
        })
    );
    let markdown = value["markdown"].as_str().unwrap();
    for expected in [
        "[ ] one injected",
        "[~] two <- current",
        "[x] three",
        "[!] four (error: bad)",
        "[B] five",
    ] {
        assert!(
            markdown.contains(expected),
            "missing {expected}: {markdown}"
        );
    }
}

#[tokio::test]
async fn todo_read_does_not_reuse_another_users_digest() {
    let http = FakeReadHttpAdapter::default();
    let memory = FakeSessionMemory::default();
    let original = ctx("tenant-a");
    store_memory(
        &memory,
        &original,
        memory_envelope(
            &original,
            json!([
            {"id":"a","description":"private","status":"pending"}
            ]),
        ),
    );
    let mut other_user = original.clone();
    other_user.user_id = "user-b".into();
    let reads = memory.reads.clone();
    let value = memory_executor(http, memory)
        .execute("todo_read", &other_user, json!({}))
        .await
        .unwrap();
    assert_eq!(value["markdown"], "(no tasks)");
    assert_eq!(
        reads.lock().unwrap()[0].2,
        working_memory_key("tenant-a", "user-b", "session-a")
    );
}

#[tokio::test]
async fn todo_read_rejects_malformed_envelopes_oversize_and_duplicate_tasks() {
    for envelope in [
        json!({"schema_version":"assistant-working-memory/legacy-compat","owner_scope":"x","working_memory":{"session_id":"session-a","tasks":[]}}),
        json!({"schema_version":"assistant-working-memory/v2","owner_scope":"wrong","working_memory":{"session_id":"session-a","tasks":[]}}),
        memory_envelope(
            &ctx("tenant-a"),
            json!([
                {"id":"duplicate","description":"one","status":"pending"},
                {"id":"duplicate","description":"two","status":"completed"}
            ]),
        ),
        memory_envelope(
            &ctx("tenant-a"),
            json!([{"id":"large","description":"x".repeat(1001),"status":"pending"}]),
        ),
        json!({
            "schema_version":"assistant-working-memory/v2",
            "owner_scope": working_memory_key("tenant-a", "user-a", "session-a")
                .strip_prefix("working_memory:").unwrap(),
            "working_memory": {
                "session_id":"session-a",
                "tasks":[],
                "archived":{"payload":"x".repeat(100_000)}
            }
        }),
    ] {
        let http = FakeReadHttpAdapter::default();
        let memory = FakeSessionMemory::default();
        let context = ctx("tenant-a");
        store_memory(&memory, &context, envelope);
        let err = memory_executor(http, memory)
            .execute("todo_read", &context, json!({}))
            .await
            .unwrap_err();
        assert_eq!(err, ReadCapabilityError::WorkingMemoryInvalid);
    }

    let http = FakeReadHttpAdapter::default();
    let memory = FakeSessionMemory::default();
    let tasks = (0..=100)
        .map(|index| json!({"id":index.to_string(),"description":"task","status":"pending"}))
        .collect::<Vec<_>>();
    let context = ctx("tenant-a");
    store_memory(&memory, &context, memory_envelope(&context, json!(tasks)));
    let err = memory_executor(http, memory)
        .execute("todo_read", &context, json!({}))
        .await
        .unwrap_err();
    assert_eq!(err, ReadCapabilityError::WorkingMemoryInvalid);
}

#[tokio::test]
async fn knowledge_uses_tenant_headers_and_rejects_dataset_outside_bound_subset() {
    let adapter = FakeReadHttpAdapter::default();
    let posts = adapter.posts.clone();
    let exec = executor(adapter, root());
    let ok = exec
        .execute(
            "search_knowledge_base",
            &ctx("tenant-a"),
            json!({"query":"refund","dataset_ids":["docs"]}),
        )
        .await
        .unwrap();
    assert_eq!(ok["query"], "refund");
    let request = posts.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        request.0.path(),
        "/internal/v2/capabilities/knowledge/docs/retrieve"
    );
    assert!(
        request
            .1
            .iter()
            .any(|(k, v)| k == "x-ai-tenant-id" && v == "tenant-a")
    );
    assert!(
        request
            .1
            .iter()
            .any(|(k, v)| k == "x-ai-user-id" && v == "user-a")
    );
    assert!(
        request
            .1
            .iter()
            .any(|(k, v)| k == "x-ai-session-id" && v == "session-a")
    );
    assert!(
        request
            .1
            .iter()
            .any(|(k, v)| k == "x-ai-execution-id" && v == "exec-a")
    );
    assert!(
        request
            .1
            .iter()
            .any(|(k, v)| k == "x-ai-run-id" && v == "run-a")
    );
    assert!(
        request
            .1
            .iter()
            .any(|(k, v)| k == "x-ai-capability-proof" && v.starts_with("v1."))
    );
    let err = exec
        .execute(
            "search_knowledge_base",
            &ctx("tenant-a"),
            json!({"query":"refund","dataset_ids":["other"]}),
        )
        .await
        .unwrap_err();
    assert_eq!(err, ReadCapabilityError::Scope);
}

#[tokio::test]
async fn omitted_dataset_ids_use_trusted_context_without_mutating_arguments() {
    let adapter = FakeReadHttpAdapter::default();
    let posts = adapter.posts.clone();
    let exec = executor(adapter, root());
    let arguments = json!({"query": "refund"});
    let original_hash = canonical_json_hash(&arguments).unwrap();
    exec.execute("search_knowledge_base", &ctx("tenant-a"), arguments.clone())
        .await
        .unwrap();
    assert_eq!(canonical_json_hash(&arguments).unwrap(), original_hash);
    let request = posts.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        request.0.path(),
        "/internal/v2/capabilities/knowledge/docs/retrieve"
    );
    assert_eq!(request.2["query"], "refund");
}

#[tokio::test]
async fn artifact_uses_internal_headers_and_never_public_headers() {
    let adapter = FakeReadHttpAdapter::default();
    let posts = adapter.posts.clone();
    let exec = executor(adapter, root());
    exec.execute(
        "read_tool_artifact",
        &ctx("tenant-a"),
        json!({"artifact_id":"art_12345678","offset":0,"limit":20}),
    )
    .await
    .unwrap();
    let (_, headers, _) = posts.lock().unwrap().last().unwrap().clone();
    assert!(
        headers
            .iter()
            .any(|(k, v)| k == "x-ai-platform-internal-token" && v == &"x".repeat(32))
    );
    assert!(
        headers
            .iter()
            .any(|(k, v)| k == "x-ai-tenant-id" && v == "tenant-a")
    );
}

#[tokio::test]
async fn confluence_read_uses_gateway_broker_and_never_receives_credentials() {
    let adapter = FakeReadHttpAdapter::default();
    let posts = adapter.posts.clone();
    let exec = executor(adapter, root());
    let mut context = ctx("tenant-a");
    context.connector_binding = Some(RuntimeConnectorBinding {
        binding_type: "grant".into(),
        provider: "confluence".into(),
        tool_name: "confluence_read".into(),
        principal_type: Some("service_account".into()),
        grant_id: Some("00000000-0000-0000-0000-000000000001".into()),
        connection_id: None,
        schema_hash: None,
        risk_level: None,
        channel: "api".into(),
    });
    exec.execute(
        "confluence_read",
        &context,
        json!({"action":"search","query":"roadmap"}),
    )
    .await
    .unwrap();
    let request = posts.lock().unwrap().last().unwrap().clone();
    assert_eq!(
        request.0.path(),
        "/internal/v2/agent-capabilities/confluence/read"
    );
    assert_eq!(request.2["arguments"]["action"], "search");
    assert_eq!(request.2["binding"]["provider"], "confluence");
    assert_eq!(request.2["binding"]["channel"], "api");
    assert!(request.2.get("api_token").is_none());
    assert!(request.1.iter().all(|(name, value)| {
        name != "authorization" && !value.to_ascii_lowercase().contains("token")
    }));
}

#[tokio::test]
async fn dynamic_mcp_read_uses_gateway_once_with_scope_bound_body() {
    let adapter = FakeReadHttpAdapter::default();
    let posts = adapter.posts.clone();
    let exec = executor(adapter, root());
    let mut context = ctx("tenant-a");
    context.connector_binding = Some(RuntimeConnectorBinding {
        binding_type: "grant".into(),
        provider: "mcp".into(),
        tool_name: "mcp_search".into(),
        principal_type: Some("service_account".into()),
        grant_id: None,
        connection_id: Some("00000000-0000-0000-0000-000000000111".into()),
        schema_hash: Some(format!("sha256:{}", "a".repeat(64))),
        risk_level: Some("low".into()),
        channel: "api".into(),
    });
    let value = exec
        .execute("mcp_search", &context, json!({"q": "hello"}))
        .await
        .unwrap();
    assert_eq!(value["results"][0]["text"], "ok");
    let posts = posts.lock().unwrap();
    assert_eq!(posts.len(), 1);
    let (url, headers, body) = &posts[0];
    assert_eq!(url.path(), "/internal/v2/agent-capabilities/mcp/read");
    assert_eq!(
        body["connection_id"],
        "00000000-0000-0000-0000-000000000111"
    );
    assert_eq!(body["runtime_name"], "mcp_search");
    assert_eq!(body["schema_hash"], format!("sha256:{}", "a".repeat(64)));
    assert!(
        headers
            .iter()
            .any(|(name, _)| name == "x-ai-capability-proof")
    );
    assert!(
        headers
            .iter()
            .any(|(name, value)| name == "x-ai-tool-call-id" && value == "tool-call-a")
    );
}

#[tokio::test]
async fn web_rejects_local_private_and_redirect_to_private_addresses() {
    let adapter = FakeReadHttpAdapter::default();
    let exec = executor(adapter.clone(), root());
    for url in [
        "http://localhost/a",
        "http://127.0.0.1/a",
        "http://10.0.0.1/a",
        "http://[::1]/a",
    ] {
        let err = exec
            .execute("web_fetch", &ctx("tenant-a"), json!({"url":url}))
            .await
            .unwrap_err();
        assert_eq!(err, ReadCapabilityError::SsrfBlocked);
    }
    adapter.public.lock().unwrap().push(PublicHttpResponse {
        status: 302,
        content_type: "text/plain".into(),
        location: Some("http://127.0.0.1/private".into()),
        remote_ip: Some(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))),
        bytes: Vec::new(),
    });
    let err = exec
        .execute(
            "web_fetch",
            &ctx("tenant-a"),
            json!({"url":"http://1.1.1.1/start"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err, ReadCapabilityError::SsrfBlocked);
}

#[tokio::test]
async fn web_rejects_dns_rebinding_remote_private_ip() {
    let adapter = FakeReadHttpAdapter::default();
    adapter.public.lock().unwrap().push(PublicHttpResponse {
        status: 200,
        content_type: "text/plain".into(),
        location: None,
        remote_ip: Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10))),
        bytes: b"secret".to_vec(),
    });
    let err = executor(adapter, root())
        .execute(
            "web_fetch",
            &ctx("tenant-a"),
            json!({"url":"http://1.1.1.1/start"}),
        )
        .await
        .unwrap_err();
    assert_eq!(err, ReadCapabilityError::SsrfBlocked);
}

#[tokio::test]
async fn workspace_scope_hash_isolated_and_symlink_escape_is_denied() {
    let workspace = root();
    let adapter = FakeReadHttpAdapter::default();
    let exec = executor(adapter, workspace.clone());
    let tenant_root = workspace.join(sha256_scope("tenant-a", "user-a", "session-a"));
    fs::create_dir_all(&tenant_root).unwrap();
    fs::write(tenant_root.join("note.txt"), "hello\nworld").unwrap();
    let value = exec
        .execute("fs_read", &ctx("tenant-a"), json!({"path":"note.txt"}))
        .await
        .unwrap();
    assert_eq!(value["content"], "hello\nworld");
    let other = ctx("tenant-b");
    let err = exec
        .execute("fs_read", &other, json!({"path":"note.txt"}))
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ReadCapabilityError::NotFound | ReadCapabilityError::Configuration
    ));
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc/hosts", tenant_root.join("escape")).unwrap();
        let err = exec
            .execute("fs_read", &ctx("tenant-a"), json!({"path":"escape"}))
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            ReadCapabilityError::PathEscape | ReadCapabilityError::NotFound
        ));
    }
}

#[tokio::test]
async fn glob_and_grep_are_bounded() {
    let workspace = root();
    let tenant_root = workspace.join(sha256_scope("tenant-a", "user-a", "session-a"));
    fs::create_dir_all(&tenant_root).unwrap();
    for i in 0..1100 {
        fs::write(
            tenant_root.join(format!("{i}.txt")),
            format!("needle {i}\n"),
        )
        .unwrap();
    }
    let exec = executor(FakeReadHttpAdapter::default(), workspace);
    let glob = exec
        .execute("fs_glob", &ctx("tenant-a"), json!({"pattern":"*.txt"}))
        .await
        .unwrap();
    assert!(glob["paths"].as_array().unwrap().len() <= 1000);
    let grep = exec
        .execute(
            "fs_grep",
            &ctx("tenant-a"),
            json!({"pattern":"needle","glob":"*.txt"}),
        )
        .await
        .unwrap();
    assert!(grep["matches"].as_array().unwrap().len() <= 500);
}

#[tokio::test]
async fn fake_large_response_documents_adapter_body_limit_boundary() {
    let adapter = FakeReadHttpAdapter::default();
    adapter.public.lock().unwrap().push(PublicHttpResponse {
        status: 200,
        content_type: "text/plain".into(),
        location: None,
        remote_ip: Some(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))),
        bytes: vec![b'x'; 2 * 1024 * 1024 + 1],
    });
    // ReqwestReadHttpAdapter enforces the byte limit while consuming chunks;
    // this fake deliberately exposes the boundary so integration can assert
    // that the executor also rejects oversized injected responses if desired.
    let value = executor(adapter, root())
        .execute(
            "web_fetch",
            &ctx("tenant-a"),
            json!({"url":"http://1.1.1.1/a","max_chars":1}),
        )
        .await
        .unwrap();
    assert_eq!(value["content"], "x");
}

fn sha256_scope(tenant: &str, user: &str, session: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(tenant.as_bytes());
    h.update(b"\0");
    h.update(user.as_bytes());
    h.update(b"\0");
    h.update(session.as_bytes());
    format!("{:x}", h.finalize())
}
