//! Source task observer lifetime/read-budget contracts without network or DB I/O.
use ai_platform_gateway::{
    capacity_admission::AdmissionCancellation,
    protocol::{UnifiedRequest, UnifiedResponse},
    task_manager::*,
    task_worker::{TaskDispatchFuture, TaskDispatcher, TaskWorker},
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;
struct ObserverStore {
    inner: MemoryTaskStorage,
    gets: AtomicUsize,
    stall: bool,
    reading: Notify,
    read_stopped: AtomicBool,
    cancel_after_claim: bool,
}
struct StopRead<'a>(&'a AtomicBool);
impl Drop for StopRead<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl TaskStorage for ObserverStore {
    fn save<'a>(&'a self, t: &'a Task) -> TaskFuture<'a, ()> {
        self.inner.save(t)
    }
    fn get<'a>(&'a self, id: &'a str) -> TaskFuture<'a, Task> {
        Box::pin(async move {
            let call = self.gets.fetch_add(1, Ordering::SeqCst);
            if self.stall && call >= 2 {
                let _stop = StopRead(&self.read_stopped);
                self.reading.notify_one();
                std::future::pending::<()>().await;
            }
            self.inner.get(id).await
        })
    }
    fn list<'a>(&'a self, f: &'a TaskListFilter) -> TaskFuture<'a, Vec<Task>> {
        self.inner.list(f)
    }
    fn compare_status<'a>(&'a self, t: &'a Task, expected: TaskStatus) -> TaskFuture<'a, bool> {
        Box::pin(async move {
            let result = self.inner.compare_status(t, expected).await?;
            if result && self.cancel_after_claim && t.status == TaskStatus::Processing {
                let mut cancelled = t.clone();
                cancelled.status = TaskStatus::Cancelled;
                self.inner
                    .compare_status(&cancelled, TaskStatus::Processing)
                    .await?;
            }
            Ok(result)
        })
    }
}
#[derive(Default)]
struct Dispatch {
    entered: Notify,
    release: Notify,
    calls: AtomicUsize,
}
impl TaskDispatcher for Dispatch {
    fn invoke(&self, _: TaskEnvelope, _: AdmissionCancellation) -> TaskDispatchFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.entered.notify_one();
            self.release.notified().await;
            Ok(UnifiedResponse::new("r", "success", vec![]))
        })
    }
}
struct Callback {
    store: Arc<ObserverStore>,
    entered: Notify,
    release: Notify,
    hold: bool,
    calls: AtomicUsize,
}
impl SafeCallbackTransport for Callback {
    fn post<'a>(&'a self, _: &'a str, p: &'a CallbackPayload) -> CallbackFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                self.store.inner.get(&p.task_id).await.unwrap().status,
                TaskStatus::Completed
            );
            if self.store.stall {
                assert!(self.store.read_stopped.load(Ordering::SeqCst));
            }
            self.entered.notify_one();
            if self.hold {
                self.release.notified().await;
            }
            Ok(200)
        })
    }
}
async fn fixture(
    stall: bool,
    hold_callback: bool,
    cancel_after_claim: bool,
) -> (
    Arc<ObserverStore>,
    Arc<Dispatch>,
    Arc<Callback>,
    TaskWorker,
    Task,
) {
    let storage = Arc::new(ObserverStore {
        inner: MemoryTaskStorage::new(),
        gets: AtomicUsize::new(0),
        stall,
        reading: Notify::new(),
        read_stopped: AtomicBool::new(false),
        cancel_after_claim,
    });
    let queue = Arc::new(MemoryTaskQueue::new());
    let callback = Arc::new(Callback {
        store: storage.clone(),
        entered: Notify::new(),
        release: Notify::new(),
        hold: hold_callback,
        calls: AtomicUsize::new(0),
    });
    let manager = Arc::new(
        TaskManager::new(storage.clone(), queue.clone())
            .with_safe_callback_transport(callback.clone()),
    );
    let mut request = UnifiedRequest::new("r", "s", vec![]);
    request.callback_url = Some("https://callback.invalid".into());
    let task = manager.create_task(request, vec![], None).await.unwrap();
    let dispatch = Arc::new(Dispatch::default());
    let worker = TaskWorker::new(queue, storage.clone(), dispatch.clone(), manager);
    (storage, dispatch, callback, worker, task)
}
#[tokio::test]
async fn finished_invocation_drops_stalled_authority_read_before_commit_and_callback() {
    let (s, d, c, w, t) = fixture(true, false, false).await;
    let drive = async {
        tokio::time::timeout(Duration::from_millis(1600), s.reading.notified())
            .await
            .expect("first periodic observer must begin");
        d.release.notify_one();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(w.process_next(), drive)
    })
    .await
    .expect("completed invoke must not await stalled read");
    result.unwrap();
    assert!(s.read_stopped.load(Ordering::SeqCst));
    assert_eq!(c.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        s.inner.get(&t.task_id).await.unwrap().status,
        TaskStatus::Completed
    );
}
#[tokio::test]
async fn source_poll_budget_and_no_observer_survives_into_callback() {
    let (s, d, c, w, _) = fixture(false, true, false).await;
    let drive = async {
        d.entered.notified().await;
        tokio::time::sleep(Duration::from_millis(2100)).await;
        let observations = s.gets.load(Ordering::SeqCst) - 1;
        assert!(
            (2..=3).contains(&observations),
            "initial ownership verification plus at most2 polls, excluding the claim's own storage read: {observations}"
        );
        d.release.notify_one();
        c.entered.notified().await;
        let reads = s.gets.load(Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(s.gets.load(Ordering::SeqCst), reads);
        c.release.notify_one();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(w.process_next(), drive)
    })
    .await
    .unwrap();
    result.unwrap();
    assert_eq!(c.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cancellation_after_claim_before_invoke_never_starts_adapter() {
    let (s, d, c, w, t) = fixture(false, false, true).await;
    tokio::time::timeout(Duration::from_millis(500), w.process_next())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d.calls.load(Ordering::SeqCst), 0);
    assert_eq!(c.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        s.inner.get(&t.task_id).await.unwrap().status,
        TaskStatus::Cancelled
    );
}
