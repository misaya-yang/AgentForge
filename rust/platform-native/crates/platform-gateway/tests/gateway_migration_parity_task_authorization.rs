//! Source task HTTP authorization matrices. Opt-in Redis initializes unused AuthService only;
//! all actual task requests use explicit trusted identity and recording in-memory storage.
use ai_platform_auth::{
    PasswordEngine, TokenCodec, accounts::PgAccounts, service::AuthService, sessions::RedisSessions,
};
use ai_platform_gateway_foundation::{
    invocation_http::*,
    protocol::{ContentItem, UnifiedRequest, UnifiedResponse},
    task_http,
    task_manager::*,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Request, StatusCode},
};
use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;
struct Identity {
    roles: Vec<String>,
    tenant: String,
}
impl InvocationIdentityResolver for Identity {
    fn resolve<'a>(&'a self, _: &'a HeaderMap, _: &'a str) -> InvocationIdentityFuture<'a> {
        Box::pin(async move {
            Ok(InvocationIdentity {
                user_id: "actor".into(),
                tenant_id: self.tenant.clone(),
                is_authenticated: true,
                tier: "public".into(),
                roles: self.roles.clone(),
                permissions: vec![],
                allowed_models: None,
                session_principal: None,
                session_jwt_selected: false,
            })
        })
    }
}
#[derive(Default)]
struct Store {
    inner: MemoryTaskStorage,
    reads: AtomicUsize,
    writes: AtomicUsize,
    filters: Mutex<Vec<TaskListFilter>>,
    replacement: Mutex<Option<Task>>,
}
impl TaskStorage for Store {
    fn save<'a>(&'a self, task: &'a Task) -> TaskFuture<'a, ()> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.save(task)
    }
    fn compare_status<'a>(&'a self, task: &'a Task, status: TaskStatus) -> TaskFuture<'a, bool> {
        self.writes.fetch_add(1, Ordering::SeqCst);
        self.inner.compare_status(task, status)
    }
    fn get<'a>(&'a self, id: &'a str) -> TaskFuture<'a, Task> {
        Box::pin(async move {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let original = self.inner.get(id).await?;
            let replacement = self.replacement.lock().unwrap().take();
            if let Some(replacement) = replacement {
                self.inner.save(&replacement).await?;
            }
            Ok(original)
        })
    }
    fn list<'a>(&'a self, filter: &'a TaskListFilter) -> TaskFuture<'a, Vec<Task>> {
        self.filters.lock().unwrap().push(filter.clone());
        self.inner.list(filter)
    }
}
async fn auth() -> TestResult<Arc<AuthService>> {
    if std::env::var("AUTH_PARITY_DISPOSABLE").as_deref() != Ok("1") {
        return Err("AUTH_PARITY_DISPOSABLE=1 required".into());
    }
    let raw = std::env::var("AUTH_PARITY_REDIS_URL")?;
    let url = reqwest::Url::parse(&raw)?;
    if url.scheme() != "redis"
        || !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
    {
        return Err("Redis fixture must be loopback".into());
    }
    let redis = redis::Client::open(raw)?.get_connection_manager().await?;
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgresql://unused:unused@127.0.0.1:1/unused")?;
    Ok(Arc::new(AuthService::new(
        PgAccounts::new(pool),
        RedisSessions::new(redis),
        TokenCodec::new(b"task-auth-synthetic-fixture-key-only", None, None)?,
        PasswordEngine::new(1)?,
        "example.com".into(),
    )))
}
fn row(owner: &str, tenant: &str, status: TaskStatus) -> Task {
    let mut request = UnifiedRequest::new("request-1", "service-1", vec![]);
    request.user_id = owner.into();
    request.tenant_id = tenant.into();
    let mut task = Task::new("task-1".into(), &request, chrono::Utc::now().naive_utc());
    task.status = status;
    task.result = Some(
        UnifiedResponse::new(
            "request-1",
            "completed",
            vec![ContentItem::text("synthetic-private-result")],
        )
        .into(),
    );
    task
}
fn router(auth: Arc<AuthService>, store: Arc<Store>, roles: &[&str], tenant: &str) -> Router {
    task_http::router_with_identity(
        auth,
        Arc::new(TaskManager::new(store, Arc::new(MemoryTaskQueue::new()))),
        Some(Arc::new(Identity {
            roles: roles.iter().map(|s| s.to_string()).collect(),
            tenant: tenant.into(),
        })),
    )
}
async fn call(router: Router, method: &str, path: &str) -> TestResult<(StatusCode, Value)> {
    let response = router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(format!("/api/v1/tasks{path}"))
                .body(Body::empty())?,
        )
        .await?;
    Ok((
        response.status(),
        serde_json::from_slice(&to_bytes(response.into_body(), 16384).await?)?,
    ))
}
const ROUTES: [(&str, &str); 3] = [
    ("GET", "/task-1"),
    ("GET", "/task-1/result"),
    ("DELETE", "/task-1"),
];
#[tokio::test]
#[ignore = "requires explicit disposable loopback Redis; no infrastructure execution claimed"]
async fn all_denied_task_routes_conceal_resources_before_results_or_cancel() -> TestResult {
    let auth = auth().await?;
    let cases: Vec<(&[&str], &str, &str, &str)> = vec![
        (&["admin"], "tenant-a", "other", "tenant-b"),
        (&["admin"], "tenant-a", "actor", "tenant-b"),
        (
            &["admin", "platform_admin"],
            "tenant-a",
            "other",
            "tenant-b",
        ),
        (&["platform_admin"], "tenant-a", "other", "tenant-b"),
        (&["platform_admin"], "tenant-a", "other", "tenant-a"),
        (&["Admin"], "tenant-a", "other", "tenant-a"),
        (&["admin "], "tenant-a", "other", "tenant-a"),
        (&[], "tenant-a", "other", "tenant-a"),
        (&[], "tenant-a", "actor", "tenant-b"),
        (&["admin"], "tenant-a", "actor", ""),
        (&["admin"], "tenant-a", "actor", ""),
        (&["admin"], "", "actor", ""),
        (&["admin"], "", "actor", ""),
    ];
    for (roles, actor_tenant, owner, tenant) in cases {
        for (method, path) in ROUTES {
            let store = Arc::new(Store::default());
            let task = row(owner, tenant, TaskStatus::Completed);
            store.inner.save(&task).await?;
            let (status, body) = call(
                router(auth.clone(), store.clone(), roles, actor_tenant),
                method,
                path,
            )
            .await?;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(body, json!({"detail":"task not found"}));
            assert!(!body.to_string().contains("synthetic-private-result"));
            assert_eq!(store.reads.load(Ordering::SeqCst), 1);
            assert_eq!(store.writes.load(Ordering::SeqCst), 0);
            assert_eq!(store.inner.get("task-1").await?, task);
        }
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires explicit disposable loopback Redis; no infrastructure execution claimed"]
async fn task_owner_and_exact_same_tenant_admin_can_read_and_cancel() -> TestResult {
    let auth = auth().await?;
    let cases: Vec<(&[&str], &str)> = vec![
        (&[], "actor"),
        (&["admin"], "actor"),
        (&["admin"], "other"),
        (&["admin", "platform_admin"], "other"),
        (&["platform_admin"], "actor"),
    ];
    for (roles, owner) in cases {
        for (method, path) in ROUTES {
            let store = Arc::new(Store::default());
            store
                .inner
                .save(&row(
                    owner,
                    "tenant-a",
                    if method == "DELETE" {
                        TaskStatus::Pending
                    } else {
                        TaskStatus::Completed
                    },
                ))
                .await?;
            let (status, body) = call(
                router(auth.clone(), store.clone(), roles, "tenant-a"),
                method,
                path,
            )
            .await?;
            assert_eq!(status, StatusCode::OK);
            if method == "DELETE" {
                assert_eq!(body, json!({"task_id":"task-1","status":"cancelled"}));
                assert_eq!(store.writes.load(Ordering::SeqCst), 1);
            } else if path.ends_with("/result") {
                assert_eq!(body["outputs"][0]["data"], "synthetic-private-result");
                assert_eq!(store.writes.load(Ordering::SeqCst), 0);
            } else {
                assert_eq!(body["task_id"], "task-1");
                assert_eq!(store.writes.load(Ordering::SeqCst), 0);
            }
        }
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires explicit disposable loopback Redis; no infrastructure execution claimed"]
async fn task_list_is_owner_tenant_scoped_and_missing_resources_have_no_side_effects() -> TestResult
{
    let auth = auth().await?;
    for roles in [
        &[][..],
        &["admin"][..],
        &["platform_admin"][..],
        &["admin", "platform_admin"][..],
    ] {
        let store = Arc::new(Store::default());
        let (status, _) = call(
            router(auth.clone(), store.clone(), roles, "tenant-a"),
            "GET",
            "?limit=7&offset=3&status=completed",
        )
        .await?;
        assert_eq!(status, StatusCode::OK);
        let filters = store.filters.lock().unwrap();
        assert_eq!(filters.len(), 1);
        let f = &filters[0];
        assert_eq!(f.user_id.as_deref(), Some("actor"));
        assert_eq!(f.tenant_id.as_deref(), Some("tenant-a"));
        assert_eq!(f.status.as_deref(), Some("completed"));
        assert_eq!((f.limit, f.offset), (7, 3));
    }
    for (method, path) in ROUTES {
        let store = Arc::new(Store::default());
        let (status, _) = call(
            router(auth.clone(), store.clone(), &["admin"], "tenant-a"),
            method,
            path,
        )
        .await?;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(store.reads.load(Ordering::SeqCst), 1);
        assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    }
    Ok(())
}
#[tokio::test]
#[ignore = "requires explicit disposable loopback Redis; no infrastructure execution claimed"]
async fn cancellation_fences_the_exact_authorized_snapshot() -> TestResult {
    let auth = auth().await?;
    let store = Arc::new(Store::default());
    store
        .inner
        .save(&row("actor", "tenant-a", TaskStatus::Pending))
        .await?;
    let mut replacement = row("victim", "tenant-b", TaskStatus::Pending);
    replacement.request_id = "replacement-request".into();
    *store.replacement.lock().unwrap() = Some(replacement.clone());
    let (status, _) = call(
        router(auth, store.clone(), &["admin"], "tenant-a"),
        "DELETE",
        "/task-1",
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(store.writes.load(Ordering::SeqCst), 0);
    assert_eq!(store.inner.get("task-1").await?, replacement);
    Ok(())
}
