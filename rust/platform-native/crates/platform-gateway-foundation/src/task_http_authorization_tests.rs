//! Production post-authentication handler paths; no JWT/Redis/PostgreSQL claim.
use super::*;
use crate::{
    protocol::{ContentItem, UnifiedRequest},
    task_manager::{MemoryTaskQueue, MemoryTaskStorage, TaskFuture, TaskStorage},
};
use ai_platform_auth::accounts::{Account, Authority};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Default)]
struct RecordingStorage {
    inner: MemoryTaskStorage,
    reads: AtomicUsize,
    transitions: AtomicUsize,
}
impl TaskStorage for RecordingStorage {
    fn save<'a>(&'a self, task: &'a Task) -> TaskFuture<'a, ()> {
        self.inner.save(task)
    }
    fn compare_status<'a>(&'a self, task: &'a Task, expected: TaskStatus) -> TaskFuture<'a, bool> {
        self.transitions.fetch_add(1, Ordering::SeqCst);
        self.inner.compare_status(task, expected)
    }
    fn get<'a>(&'a self, task_id: &'a str) -> TaskFuture<'a, Task> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.get(task_id)
    }
    fn list<'a>(&'a self, filter: &'a TaskListFilter) -> TaskFuture<'a, Vec<Task>> {
        self.inner.list(filter)
    }
}
fn principal(user: &str, tenant: &str, roles: &[&str]) -> Principal {
    Principal {
        account: Account {
            user_id: user.into(),
            tenant_id: tenant.into(),
            email: None,
            display_name: None,
            department: None,
            tier: "admin".into(),
            // An account-array role is not a substitute for the resolved authority.
            roles: vec!["admin".into()],
            permissions: vec!["admin:*".into()],
            status: "active".into(),
            password_hash: None,
            force_password_change: false,
            login_attempts: 0,
            locked_until: None,
            password_changed_at: None,
        },
        authority: Authority {
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            tier: "admin".into(),
            permissions: vec!["admin:*".into()],
        },
        jti: "synthetic-resolved-principal".into(),
        expires_at: u64::MAX,
    }
}
async fn setup(
    user: &str,
    tenant: &str,
    status: TaskStatus,
) -> (Arc<RecordingStorage>, TaskManager) {
    let storage = Arc::new(RecordingStorage::default());
    let manager = TaskManager::new(storage.clone(), Arc::new(MemoryTaskQueue::new()));
    let mut request = UnifiedRequest::new("request-1", "service-1", vec![]);
    request.user_id = user.into();
    request.tenant_id = tenant.into();
    let mut task = Task::new(
        "task-1".into(),
        &request,
        ProtocolTimestamp::now().naive_utc(),
    );
    task.status = status;
    if status == TaskStatus::Completed {
        task.result = Some(
            UnifiedResponse::new(
                "request-1",
                "completed",
                vec![ContentItem::text("synthetic-private-result")],
            )
            .into(),
        );
    }
    storage.inner.save(&task).await.unwrap();
    (storage, manager)
}
async fn call(action: &str, manager: &TaskManager, principal: &Principal) -> Response {
    let headers = HeaderMap::new();
    match action {
        "get" => get_task_for_principal(manager, principal, "task-1", &headers).await,
        "result" => get_task_result_for_principal(manager, principal, "task-1", &headers).await,
        "cancel" => cancel_task_for_principal(manager, principal, "task-1", &headers).await,
        _ => unreachable!(),
    }
}
async fn body(response: Response) -> Value {
    serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn task_resource_handlers_deny_cross_tenant_admin_before_result_or_cancel() {
    for roles in [&["admin"][..], &["admin", "platform_admin"][..]] {
        for task_user in ["actor", "other"] {
            for task_tenant in ["tenant-b", ""] {
                for action in ["get", "result", "cancel"] {
                    let (storage, manager) =
                        setup(task_user, task_tenant, TaskStatus::Completed).await;
                    let response =
                        call(action, &manager, &principal("actor", "tenant-a", roles)).await;
                    assert_eq!(
                        response.status(),
                        StatusCode::NOT_FOUND,
                        "{action} {roles:?} {task_tenant}"
                    );
                    assert_eq!(body(response).await, json!({"detail": "task not found"}));
                    // The ownership read is allowed; get_result must not perform its second read.
                    assert_eq!(storage.reads.load(Ordering::SeqCst), 1);
                    assert_eq!(storage.transitions.load(Ordering::SeqCst), 0);
                    assert_eq!(
                        storage.inner.get("task-1").await.unwrap().status,
                        TaskStatus::Completed
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn task_resource_handlers_preserve_owner_and_same_tenant_admin() {
    for (roles, task_user) in [
        (&[][..], "actor"),
        (&["admin"][..], "actor"),
        (&["admin"][..], "other"),
        (&["admin", "platform_admin"][..], "other"),
        (&["platform_admin"][..], "actor"),
    ] {
        for action in ["get", "result", "cancel"] {
            let initial = if action == "cancel" {
                TaskStatus::Pending
            } else {
                TaskStatus::Completed
            };
            let (storage, manager) = setup(task_user, "tenant-a", initial).await;
            let response = call(action, &manager, &principal("actor", "tenant-a", roles)).await;
            assert_eq!(
                response.status(),
                StatusCode::OK,
                "{action} {roles:?} {task_user}"
            );
            let value = body(response).await;
            if action == "result" {
                assert_eq!(value["outputs"][0]["data"], "synthetic-private-result");
            } else {
                assert_eq!(value["task_id"], "task-1");
            }
            let after = storage.inner.get("task-1").await.unwrap();
            assert_eq!(
                after.status,
                if action == "cancel" {
                    TaskStatus::Cancelled
                } else {
                    initial
                }
            );
            assert_eq!(
                storage.transitions.load(Ordering::SeqCst),
                usize::from(action == "cancel")
            );
        }
    }
}

#[tokio::test]
async fn task_resource_handlers_do_not_invent_role_or_permission_aliases() {
    for roles in [
        &[][..],
        &["platform_admin"][..],
        &["Admin"][..],
        &["admin "][..],
    ] {
        for task_tenant in ["tenant-a", "tenant-b"] {
            for action in ["get", "result", "cancel"] {
                let (storage, manager) = setup("other", task_tenant, TaskStatus::Completed).await;
                let response = call(action, &manager, &principal("actor", "tenant-a", roles)).await;
                assert_eq!(response.status(), StatusCode::NOT_FOUND);
                assert_eq!(body(response).await, json!({"detail": "task not found"}));
                assert_eq!(storage.reads.load(Ordering::SeqCst), 1);
                assert_eq!(storage.transitions.load(Ordering::SeqCst), 0);
            }
        }
    }
}

#[tokio::test]
async fn task_resource_handlers_cannot_cancel_cross_tenant_pending_task() {
    let (storage, manager) = setup("other", "tenant-b", TaskStatus::Pending).await;
    let response = call(
        "cancel",
        &manager,
        &principal("actor", "tenant-a", &["admin"]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        storage.inner.get("task-1").await.unwrap().status,
        TaskStatus::Pending
    );
    assert_eq!(storage.reads.load(Ordering::SeqCst), 1);
    assert_eq!(storage.transitions.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn task_resource_handlers_reject_empty_tenant_and_conceal_missing_task() {
    for action in ["get", "result", "cancel"] {
        let (storage, manager) = setup("actor", "", TaskStatus::Completed).await;
        let response = call(action, &manager, &principal("actor", "", &["admin"])).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(storage.reads.load(Ordering::SeqCst), 1);
        assert_eq!(storage.transitions.load(Ordering::SeqCst), 0);

        let empty = Arc::new(RecordingStorage::default());
        let manager = TaskManager::new(empty.clone(), Arc::new(MemoryTaskQueue::new()));
        let response = call(
            action,
            &manager,
            &principal("actor", "tenant-a", &["admin"]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(empty.reads.load(Ordering::SeqCst), 1);
        assert_eq!(empty.transitions.load(Ordering::SeqCst), 0);
    }
}
