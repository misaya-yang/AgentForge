//! Observable memory-authority assertions from test_task_transitions.py.
use ai_platform_gateway::{protocol::UnifiedRequest, task_manager::*};
use serde_json::json;
use std::sync::Arc;
async fn fixture() -> (Arc<MemoryTaskStorage>, TaskManager, Task) {
    let storage = Arc::new(MemoryTaskStorage::new());
    let manager = TaskManager::new(storage.clone(), Arc::new(MemoryTaskQueue::new()));
    let mut request = UnifiedRequest::new("r", "s", vec![]);
    request.user_id = "u".into();
    request.tenant_id = "t".into();
    let task = manager.create_task(request, vec![], None).await.unwrap();
    (storage, manager, task)
}
#[tokio::test]
async fn all_twenty_five_status_edges_preserve_source_timestamps_and_rejected_rows() {
    let states = [
        TaskStatus::Pending,
        TaskStatus::Processing,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ];
    for before in states {
        for after in states {
            let (storage, manager, mut task) = fixture().await;
            task.status = before;
            storage.save(&task).await.unwrap();
            let allowed = matches!(
                (before, after),
                (
                    TaskStatus::Pending,
                    TaskStatus::Processing | TaskStatus::Cancelled
                ) | (
                    TaskStatus::Processing,
                    TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Cancelled
                )
            );
            assert_eq!(
                manager
                    .transition_status(&task.task_id, after, None, None)
                    .await
                    .unwrap(),
                allowed,
                "{before:?} -> {after:?}"
            );
            let row = storage.get(&task.task_id).await.unwrap();
            if allowed {
                assert_eq!(row.status, after);
                assert!(row.updated_at.is_some());
                assert_eq!(row.started_at.is_some(), after == TaskStatus::Processing);
                assert_eq!(row.completed_at.is_some(), after != TaskStatus::Processing);
            } else {
                assert_eq!(row, task);
                let mut proposed = task.clone();
                proposed.status = after;
                assert_eq!(
                    storage.compare_status(&proposed, before).await,
                    Err(TaskError::StateConflict)
                );
            }
        }
    }
}
#[tokio::test]
async fn every_immutable_identity_field_rejects_compare_and_preserves_row() {
    for field in [
        "task_id",
        "request_id",
        "service_id",
        "user_id",
        "tenant_id",
    ] {
        let (storage, _, task) = fixture().await;
        let mut proposed = task.clone();
        proposed.status = TaskStatus::Processing;
        match field {
            "task_id" => proposed.task_id = "replacement".into(),
            "request_id" => proposed.request_id = "replacement".into(),
            "service_id" => proposed.service_id = "replacement".into(),
            "user_id" => proposed.user_id = "replacement".into(),
            _ => proposed.tenant_id = "replacement".into(),
        }
        assert!(
            !storage
                .compare_status(&proposed, TaskStatus::Pending)
                .await
                .unwrap(),
            "{field}"
        );
        assert_eq!(storage.get(&task.task_id).await.unwrap(), task);
    }
}
#[tokio::test]
async fn detached_save_get_list_and_lifecycle_patch_keep_concurrent_metadata() {
    let (storage, _, mut task) = fixture().await;
    task.metadata = Some(serde_json::from_value(json!({"nested":[1]})).unwrap());
    task.result = Some(json!({"nested":[2]}).into());
    storage.save(&task).await.unwrap();
    let original = task.clone();
    task.metadata
        .as_mut()
        .unwrap()
        .insert("nested".into(), json!([1, 99]));
    task.result = Some(json!({"nested":[2,99]}).into());
    let mut fetched = storage.get(&task.task_id).await.unwrap();
    fetched
        .metadata
        .as_mut()
        .unwrap()
        .insert("nested".into(), json!([1, 88]));
    fetched.result = Some(json!({"nested":[2,88]}).into());
    let mut listed = storage.list(&TaskListFilter::default()).await.unwrap();
    listed[0]
        .metadata
        .as_mut()
        .unwrap()
        .insert("nested".into(), json!([1, 77]));
    listed[0].result = Some(json!({"nested":[2,77]}).into());
    assert_eq!(storage.get(&task.task_id).await.unwrap(), original);
    let mut current = original.clone();
    current
        .metadata
        .as_mut()
        .unwrap()
        .insert("concurrent".into(), json!(true));
    current.progress = 20.;
    storage.save(&current).await.unwrap();
    let mut proposed = original.clone();
    proposed.status = TaskStatus::Processing;
    proposed.result = Some(json!({"nested":[3]}).into());
    assert!(
        storage
            .compare_status(&proposed, TaskStatus::Pending)
            .await
            .unwrap()
    );
    proposed.result = Some(json!({"nested":[3,66]}).into());
    proposed
        .metadata
        .as_mut()
        .unwrap()
        .insert("nested".into(), json!([1, 55]));
    let row = storage.get(&task.task_id).await.unwrap();
    assert_eq!(row.result, Some(json!({"nested":[3]}).into()));
    assert_eq!(
        row.metadata,
        Some(serde_json::from_value(json!({"nested":[1],"concurrent":true})).unwrap())
    );
    assert_eq!(row.progress, 20.);
    assert_eq!(original.status, TaskStatus::Pending);
    assert!(
        !storage
            .compare_status(&proposed, TaskStatus::Pending)
            .await
            .unwrap()
    );
}

struct HeldStorage {
    inner: MemoryTaskStorage,
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    comparisons: std::sync::atomic::AtomicUsize,
    perpetual: bool,
}
impl HeldStorage {
    fn new(perpetual: bool) -> Self {
        Self {
            inner: MemoryTaskStorage::new(),
            entered: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            comparisons: std::sync::atomic::AtomicUsize::new(0),
            perpetual,
        }
    }
}
impl TaskStorage for HeldStorage {
    fn save<'a>(&'a self, task: &'a Task) -> TaskFuture<'a, ()> {
        self.inner.save(task)
    }
    fn get<'a>(&'a self, id: &'a str) -> TaskFuture<'a, Task> {
        self.inner.get(id)
    }
    fn list<'a>(&'a self, filter: &'a TaskListFilter) -> TaskFuture<'a, Vec<Task>> {
        self.inner.list(filter)
    }
    fn compare_status<'a>(&'a self, task: &'a Task, expected: TaskStatus) -> TaskFuture<'a, bool> {
        Box::pin(async move {
            if task.status == TaskStatus::Cancelled {
                let count = self
                    .comparisons
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                if self.perpetual {
                    if count >= 3 {
                        return Err(TaskError::StorageUnavailable);
                    }
                    return Ok(false);
                }
                if count == 0 {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
            }
            self.inner.compare_status(task, expected).await
        })
    }
}
#[tokio::test]
async fn cancellation_conflict_retry_is_bounded_to_three() {
    let storage = Arc::new(HeldStorage::new(true));
    let manager = TaskManager::new(storage.clone(), Arc::new(MemoryTaskQueue::new()));
    let task = manager
        .create_task(UnifiedRequest::new("r", "s", vec![]), vec![], None)
        .await
        .unwrap();
    // A separate yielding timer cannot interrupt a buggy synchronous-ready loop;
    // the adapter returns a terminal error after an excessive fourth comparison.
    // The regression's exact bound is enforced by the adapter below instead.
    let outcome = manager.cancel(&task.task_id).await;
    assert_eq!(outcome, Err(TaskError::StateConflict));
    assert_eq!(
        storage
            .comparisons
            .load(std::sync::atomic::Ordering::SeqCst),
        3
    );
    assert_eq!(storage.get(&task.task_id).await.unwrap(), task);
}
#[tokio::test]
async fn cancellation_reloads_claim_but_never_adopts_replacement_identity() {
    for replacement in [false, true] {
        let storage = Arc::new(HeldStorage::new(false));
        let manager = TaskManager::new(storage.clone(), Arc::new(MemoryTaskQueue::new()));
        let task = manager
            .create_task(UnifiedRequest::new("r", "s", vec![]), vec![], None)
            .await
            .unwrap();
        let cancelling = manager.cancel(&task.task_id);
        let mutate = async {
            storage.entered.notified().await;
            let mut current = task.clone();
            if replacement {
                current.tenant_id = "replacement".into();
                current.status = TaskStatus::Cancelled;
                storage.save(&current).await.unwrap();
            } else {
                assert!(
                    manager
                        .transition_status(&task.task_id, TaskStatus::Processing, None, None)
                        .await
                        .unwrap()
                );
                current = storage.get(&task.task_id).await.unwrap();
            }
            storage.release.notify_one();
            current
        };
        let (outcome, intermediate) = tokio::join!(cancelling, mutate);
        if replacement {
            assert_eq!(outcome, Err(TaskError::StateConflict));
            assert_eq!(storage.get(&task.task_id).await.unwrap(), intermediate);
        } else {
            outcome.unwrap();
            let cancelled = storage.get(&task.task_id).await.unwrap();
            assert_eq!(cancelled.status, TaskStatus::Cancelled);
            assert_eq!(cancelled.started_at, intermediate.started_at);
            assert!(cancelled.completed_at.is_some());
            manager.cancel(&task.task_id).await.unwrap();
            assert_eq!(storage.get(&task.task_id).await.unwrap(), cancelled);
        }
    }
}

#[derive(Default)]
struct PendingDispatch {
    entered: tokio::sync::Notify,
    calls: std::sync::atomic::AtomicUsize,
    cancellation:
        std::sync::Mutex<Option<ai_platform_gateway::capacity_admission::AdmissionCancellation>>,
}
impl ai_platform_gateway::task_worker::TaskDispatcher for PendingDispatch {
    fn invoke(
        &self,
        _: TaskEnvelope,
        cancellation: ai_platform_gateway::capacity_admission::AdmissionCancellation,
    ) -> ai_platform_gateway::task_worker::TaskDispatchFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            *self.cancellation.lock().unwrap() = Some(cancellation);
            self.entered.notify_one();
            std::future::pending().await
        })
    }
}
#[tokio::test]
async fn owned_invocation_stops_on_cancel_identity_or_terminal_loss_using_manager_authority() {
    use ai_platform_gateway::task_worker::TaskWorker;
    for loss in ["cancel", "identity", "completed"] {
        let storage = Arc::new(MemoryTaskStorage::new());
        let queue = Arc::new(MemoryTaskQueue::new());
        let manager = Arc::new(TaskManager::new(storage.clone(), queue.clone()));
        let task = manager
            .create_task(UnifiedRequest::new("r", "s", vec![]), vec![], None)
            .await
            .unwrap();
        let dispatch = Arc::new(PendingDispatch::default());
        // A deliberately different constructor store must not control work
        // claimed from manager authority (source regression explicitly covers it).
        let worker = TaskWorker::new(
            queue,
            Arc::new(MemoryTaskStorage::new()),
            dispatch.clone(),
            manager.clone(),
        );
        let processing = worker.process_next();
        let revoke = async {
            dispatch.entered.notified().await;
            if loss == "cancel" {
                manager.cancel(&task.task_id).await.unwrap();
            } else {
                let mut row = storage.get(&task.task_id).await.unwrap();
                if loss == "identity" {
                    row.tenant_id = "replacement".into();
                } else {
                    row.status = TaskStatus::Completed;
                    row.result = Some(json!({"winner":true}).into());
                }
                storage.save(&row).await.unwrap();
            }
            storage.get(&task.task_id).await.unwrap()
        };
        let (result, winner) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            tokio::join!(processing, revoke)
        })
        .await
        .expect("authority loss must promptly cancel owned dispatch");
        result.unwrap();
        assert!(
            dispatch
                .cancellation
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_cancelled()
        );
        assert_eq!(dispatch.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(storage.get(&task.task_id).await.unwrap(), winner);
    }
}

struct FaultStorage {
    inner: MemoryTaskStorage,
    failed: std::sync::atomic::AtomicBool,
}
impl TaskStorage for FaultStorage {
    fn save<'a>(&'a self, task: &'a Task) -> TaskFuture<'a, ()> {
        self.inner.save(task)
    }
    fn get<'a>(&'a self, id: &'a str) -> TaskFuture<'a, Task> {
        Box::pin(async move {
            if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
                Err(TaskError::StorageUnavailable)
            } else {
                self.inner.get(id).await
            }
        })
    }
    fn list<'a>(&'a self, filter: &'a TaskListFilter) -> TaskFuture<'a, Vec<Task>> {
        self.inner.list(filter)
    }
    fn compare_status<'a>(&'a self, task: &'a Task, expected: TaskStatus) -> TaskFuture<'a, bool> {
        self.inner.compare_status(task, expected)
    }
}
#[tokio::test]
async fn uncertain_authority_read_stops_dispatch_without_overwriting_processing() {
    let storage = Arc::new(FaultStorage {
        inner: MemoryTaskStorage::new(),
        failed: std::sync::atomic::AtomicBool::new(false),
    });
    let queue = Arc::new(MemoryTaskQueue::new());
    let manager = Arc::new(TaskManager::new(storage.clone(), queue.clone()));
    let task = manager
        .create_task(UnifiedRequest::new("r", "s", vec![]), vec![], None)
        .await
        .unwrap();
    let dispatch = Arc::new(PendingDispatch::default());
    let worker = ai_platform_gateway::task_worker::TaskWorker::new(
        queue,
        storage.clone(),
        dispatch.clone(),
        manager,
    );
    let fail = async {
        dispatch.entered.notified().await;
        storage
            .failed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(worker.process_next(), fail)
    })
    .await
    .unwrap();
    assert_eq!(
        result,
        Err(ai_platform_gateway::task_worker::TaskWorkerError::Task(
            TaskError::StorageUnavailable
        ))
    );
    assert!(
        dispatch
            .cancellation
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .is_cancelled()
    );
    assert_eq!(dispatch.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    let row = storage.inner.get(&task.task_id).await.unwrap();
    assert_eq!(row.status, TaskStatus::Processing);
    assert_eq!(row.error, None);
    assert_eq!(row.result, None);
}

#[derive(Default)]
struct CallbackRecorder(std::sync::Mutex<Vec<serde_json::Value>>);
impl SafeCallbackTransport for CallbackRecorder {
    fn post<'a>(&'a self, url: &'a str, payload: &'a CallbackPayload) -> CallbackFuture<'a> {
        Box::pin(async move {
            assert_eq!(url, "https://callback.invalid/fixture");
            self.0.lock().unwrap().push(payload.to_json().unwrap());
            Ok(200)
        })
    }
}
#[tokio::test]
async fn each_terminal_winner_preserves_timestamps_result_and_exact_callback_count() {
    for terminal in [TaskStatus::Completed, TaskStatus::Failed] {
        for cancel_first in [true, false] {
            let storage = Arc::new(MemoryTaskStorage::new());
            let callbacks = Arc::new(CallbackRecorder::default());
            let manager = TaskManager::new(storage.clone(), Arc::new(MemoryTaskQueue::new()))
                .with_safe_callback_transport(callbacks.clone());
            let mut request = UnifiedRequest::new("r", "s", vec![]);
            request.callback_url = Some("https://callback.invalid/fixture".into());
            let task = manager.create_task(request, vec![], None).await.unwrap();
            assert!(
                manager
                    .transition_status(&task.task_id, TaskStatus::Processing, None, None)
                    .await
                    .unwrap()
            );
            let started = storage.get(&task.task_id).await.unwrap().started_at;
            if cancel_first {
                manager.cancel(&task.task_id).await.unwrap();
                assert!(
                    !manager
                        .transition_status(
                            &task.task_id,
                            terminal,
                            Some(json!({"output":1}).into()),
                            Some("fixture".into())
                        )
                        .await
                        .unwrap()
                );
                assert!(callbacks.0.lock().unwrap().is_empty());
            } else {
                assert!(
                    manager
                        .transition_status(
                            &task.task_id,
                            terminal,
                            Some(json!({"output":1}).into()),
                            Some("fixture".into())
                        )
                        .await
                        .unwrap()
                );
                assert_eq!(
                    manager.cancel(&task.task_id).await,
                    Err(TaskError::StateConflict)
                );
                let calls = callbacks.0.lock().unwrap();
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0]["result"], json!({"output":1}));
                assert_eq!(calls[0]["error"], "fixture");
            }
            let row = storage.get(&task.task_id).await.unwrap();
            assert_eq!(
                row.status,
                if cancel_first {
                    TaskStatus::Cancelled
                } else {
                    terminal
                }
            );
            assert_eq!(row.started_at, started);
            assert!(row.completed_at.is_some());
            if cancel_first {
                assert_eq!(row.result, None);
                assert_eq!(row.error, None);
            } else {
                assert_eq!(row.result, Some(json!({"output":1}).into()));
                assert_eq!(row.error, Some("fixture".into()));
            }
        }
    }
}

#[derive(Default)]
struct HeldDispatch {
    entered: tokio::sync::Notify,
    release: tokio::sync::Notify,
    calls: std::sync::atomic::AtomicUsize,
    response: std::sync::Mutex<Option<ai_platform_gateway::protocol::UnifiedResponse>>,
}
impl ai_platform_gateway::task_worker::TaskDispatcher for HeldDispatch {
    fn invoke(
        &self,
        _: TaskEnvelope,
        _: ai_platform_gateway::capacity_admission::AdmissionCancellation,
    ) -> ai_platform_gateway::task_worker::TaskDispatchFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.entered.notify_one();
            self.release.notified().await;
            let response = ai_platform_gateway::protocol::UnifiedResponse::new(
                "r",
                "success",
                vec![],
            );
            *self.response.lock().unwrap() = Some(response.clone());
            Ok(response)
        })
    }
}
#[tokio::test]
async fn duplicate_claims_dispatch_once_and_retain_typed_result() {
    let storage = Arc::new(MemoryTaskStorage::new());
    let queue = Arc::new(MemoryTaskQueue::new());
    let manager = Arc::new(TaskManager::new(storage.clone(), queue.clone()));
    let task = manager
        .create_task(UnifiedRequest::new("r", "s", vec![]), vec![], None)
        .await
        .unwrap();
    let envelope = queue.dequeue().await.unwrap();
    queue.enqueue(envelope.clone()).await.unwrap();
    queue.enqueue(envelope.clone()).await.unwrap();
    let dispatch = Arc::new(HeldDispatch::default());
    let worker = ai_platform_gateway::task_worker::TaskWorker::new(
        queue.clone(),
        storage.clone(),
        dispatch.clone(),
        manager.clone(),
    );
    let second = async {
        dispatch.entered.notified().await;
        worker.process_next().await.unwrap();
        dispatch.release.notify_one();
    };
    let (first, ()) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(worker.process_next(), second)
    })
    .await
    .unwrap();
    first.unwrap();
    queue.enqueue(envelope).await.unwrap();
    worker.process_next().await.unwrap();
    assert_eq!(dispatch.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        storage.get(&task.task_id).await.unwrap().status,
        TaskStatus::Completed
    );
    assert_eq!(
        manager.get_result(&task.task_id).await.unwrap(),
        dispatch.response.lock().unwrap().clone().unwrap()
    );
}
#[tokio::test]
async fn terminal_queue_duplicates_never_dispatch_or_mutate() {
    for status in [
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
    ] {
        let storage = Arc::new(MemoryTaskStorage::new());
        let queue = Arc::new(MemoryTaskQueue::new());
        let manager = Arc::new(TaskManager::new(storage.clone(), queue.clone()));
        let mut task = manager
            .create_task(UnifiedRequest::new("r", "s", vec![]), vec![], None)
            .await
            .unwrap();
        task.status = status;
        storage.save(&task).await.unwrap();
        let dispatch = Arc::new(HeldDispatch::default());
        let worker = ai_platform_gateway::task_worker::TaskWorker::new(
            queue,
            storage.clone(),
            dispatch.clone(),
            manager,
        );
        tokio::time::timeout(std::time::Duration::from_secs(1), worker.process_next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(dispatch.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(storage.get(&task.task_id).await.unwrap(), task);
    }
}
