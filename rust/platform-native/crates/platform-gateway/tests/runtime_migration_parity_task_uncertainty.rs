//! Source task uncertainty receipts using actual manager/worker and owned memory CAS.
use ai_platform_gateway::{
    capacity_admission::AdmissionCancellation,
    protocol::{UnifiedRequest, UnifiedResponse},
    task_manager::*,
    task_worker::{TaskDispatchFuture, TaskDispatcher, TaskWorker, TaskWorkerError},
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Clone, Copy)]
enum Fault {
    Claim(bool),
    Completion(bool),
    CancelDuringCompletion,
}
struct FaultStore {
    inner: MemoryTaskStorage,
    fault: Fault,
}
impl TaskStorage for FaultStore {
    fn save<'a>(&'a self, t: &'a Task) -> TaskFuture<'a, ()> {
        self.inner.save(t)
    }
    fn get<'a>(&'a self, id: &'a str) -> TaskFuture<'a, Task> {
        self.inner.get(id)
    }
    fn list<'a>(&'a self, f: &'a TaskListFilter) -> TaskFuture<'a, Vec<Task>> {
        self.inner.list(f)
    }
    fn compare_status<'a>(&'a self, t: &'a Task, expected: TaskStatus) -> TaskFuture<'a, bool> {
        Box::pin(async move {
            match (self.fault, t.status) {
                (Fault::Claim(committed), TaskStatus::Processing)
                | (Fault::Completion(committed), TaskStatus::Completed) => {
                    if committed {
                        self.inner.compare_status(t, expected).await?;
                    }
                    Err(TaskError::StorageUnavailable)
                }
                (Fault::CancelDuringCompletion, TaskStatus::Completed) => {
                    let mut cancelled = t.clone();
                    cancelled.status = TaskStatus::Cancelled;
                    cancelled.result = None;
                    cancelled.error = None;
                    self.inner.compare_status(&cancelled, expected).await?;
                    Err(TaskError::StorageUnavailable)
                }
                _ => self.inner.compare_status(t, expected).await,
            }
        })
    }
}
#[derive(Default)]
struct Dispatch {
    calls: AtomicUsize,
}
impl TaskDispatcher for Dispatch {
    fn invoke(&self, _: TaskEnvelope, _: AdmissionCancellation) -> TaskDispatchFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(UnifiedResponse::new("r", "success", vec![]))
        })
    }
}
#[derive(Default)]
struct Callbacks(Mutex<Vec<CallbackPayload>>);
impl SafeCallbackTransport for Callbacks {
    fn post<'a>(&'a self, _: &'a str, p: &'a CallbackPayload) -> CallbackFuture<'a> {
        Box::pin(async move {
            self.0.lock().unwrap().push(p.clone());
            Ok(200)
        })
    }
}
async fn fixture(
    fault: Fault,
) -> (
    Arc<FaultStore>,
    Arc<Dispatch>,
    Arc<Callbacks>,
    TaskWorker,
    Task,
) {
    let storage = Arc::new(FaultStore {
        inner: MemoryTaskStorage::new(),
        fault,
    });
    let queue = Arc::new(MemoryTaskQueue::new());
    let callbacks = Arc::new(Callbacks::default());
    let manager = Arc::new(
        TaskManager::new(storage.clone(), queue.clone())
            .with_safe_callback_transport(callbacks.clone()),
    );
    let mut request = UnifiedRequest::new("r", "s", vec![]);
    request.callback_url = Some("https://callback.invalid/fixture".into());
    let task = manager.create_task(request, vec![], None).await.unwrap();
    let dispatch = Arc::new(Dispatch::default());
    let worker = TaskWorker::new(queue, storage.clone(), dispatch.clone(), manager);
    (storage, dispatch, callbacks, worker, task)
}
#[tokio::test]
async fn uncertain_claim_never_dispatches_whether_commit_won_or_not() {
    for committed in [true, false] {
        let (s, d, c, w, t) = fixture(Fault::Claim(committed)).await;
        assert_eq!(
            w.process_next().await,
            Err(TaskWorkerError::Task(TaskError::StorageUnavailable))
        );
        assert_eq!(d.calls.load(Ordering::SeqCst), 0);
        assert!(c.0.lock().unwrap().is_empty());
        assert_eq!(
            s.get(&t.task_id).await.unwrap().status,
            if committed {
                TaskStatus::Processing
            } else {
                TaskStatus::Pending
            }
        );
    }
}
#[tokio::test]
async fn uncertain_completion_projects_failure_only_if_original_claim_still_owns_row() {
    for committed in [true, false] {
        let (s, d, c, w, t) = fixture(Fault::Completion(committed)).await;
        w.process_next().await.unwrap();
        assert_eq!(d.calls.load(Ordering::SeqCst), 1);
        let row = s.get(&t.task_id).await.unwrap();
        assert_eq!(
            row.status,
            if committed {
                TaskStatus::Completed
            } else {
                TaskStatus::Failed
            }
        );
        let callbacks = c.0.lock().unwrap();
        assert_eq!(callbacks.len(), usize::from(!committed));
        if committed {
            assert!(matches!(row.result, Some(TaskResult::Response(_))));
            assert_eq!(row.error, None);
        } else {
            assert_eq!(row.result, None);
            assert!(row.error.is_some());
            assert_eq!(callbacks[0].status, TaskStatus::Failed);
        }
    }
}
#[tokio::test]
async fn completion_error_after_cancellation_cannot_project_failure_or_callback() {
    let (s, d, c, w, t) = fixture(Fault::CancelDuringCompletion).await;
    w.process_next().await.unwrap();
    assert_eq!(d.calls.load(Ordering::SeqCst), 1);
    assert!(c.0.lock().unwrap().is_empty());
    let row = s.get(&t.task_id).await.unwrap();
    assert_eq!(row.status, TaskStatus::Cancelled);
    assert_eq!(row.result, None);
    assert_eq!(row.error, None);
}
struct RefusingCallback {
    storage: Arc<MemoryTaskStorage>,
    calls: AtomicUsize,
}
impl SafeCallbackTransport for RefusingCallback {
    fn post<'a>(&'a self, _: &'a str, p: &'a CallbackPayload) -> CallbackFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                self.storage.get(&p.task_id).await.unwrap().status,
                TaskStatus::Completed
            );
            Err(CallbackFailure::Other)
        })
    }
}
#[tokio::test]
async fn failed_callback_runs_after_unlocked_completion_and_never_projects_failure() {
    let storage = Arc::new(MemoryTaskStorage::new());
    let queue = Arc::new(MemoryTaskQueue::new());
    let callback = Arc::new(RefusingCallback {
        storage: storage.clone(),
        calls: AtomicUsize::new(0),
    });
    let mut manager = TaskManager::new(storage.clone(), queue.clone())
        .with_safe_callback_transport(callback.clone());
    manager.callback_options.retries = 1;
    let manager = Arc::new(manager);
    let mut request = UnifiedRequest::new("r", "s", vec![]);
    request.callback_url = Some("https://callback.invalid/fixture".into());
    let task = manager.create_task(request, vec![], None).await.unwrap();
    let dispatch = Arc::new(Dispatch::default());
    let worker = TaskWorker::new(queue, storage.clone(), dispatch.clone(), manager);
    tokio::time::timeout(std::time::Duration::from_secs(1), worker.process_next())
        .await
        .expect("callback storage read must not deadlock under lifecycle lock")
        .unwrap();
    assert_eq!(
        storage.get(&task.task_id).await.unwrap().status,
        TaskStatus::Completed
    );
    assert_eq!(callback.calls.load(Ordering::SeqCst), 1);
    assert_eq!(dispatch.calls.load(Ordering::SeqCst), 1);
}
