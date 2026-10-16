//! Generic Gateway task consumer, separate from scheduled Agent/SDK execution.
//!
//! Atomic pending -> processing admission prevents duplicate queue entries from
//! invoking concurrently. Only the winning processing -> terminal transition
//! persists a result/callback. Persisted cancellation is polled and signals local
//! dispatch cancellation; this does not prove remote side effects have ceased.
//!
//! The explicit legacy/test constructor retains a destructive queue. A crash or
//! uncertain persistence can leave processing work whose dispatch is unknown.
//! Never automatically requeue or replay it. Cancellation/status comparison is
//! not an owner lease, a generation fence, or end-to-end exactly-once execution.
//!
//! Production uses the durable constructor and current actor revalidation.
//! Legacy/test queues retain their original trusted admission contract.
//! Storage errors stop that lane, fail closed and signal owned dispatch cleanup.
//! Stop/drop abort lanes without joining; keep the runtime alive for cleanup.
use crate::{
    capacity_admission::{AdmissionCancellation, AdmissionError},
    dispatch_preflight::PreflightAuthority,
    dispatcher::DispatchError,
    gateway_dispatcher::{GatewayDispatchError, GatewayDispatcher},
    protocol::UnifiedResponse,
    task_manager::{
        Task, TaskEnvelope, TaskError, TaskManager, TaskQueue, TaskStatus, TaskStorage,
    },
};
use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::task::JoinHandle;

pub type TaskDispatchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<UnifiedResponse, GatewayDispatchError>> + Send + 'a>>;

/// One canonical gateway invoke. Implementations must drop/cancel pending work
/// when this future is dropped, or use the supplied signal for owned cleanup.
/// Errors are persisted via Display, as Python uses str(exc); use the gateway's
/// sanitized error types, never credentials, raw provider bodies, or DSNs.
pub trait TaskDispatcher: Send + Sync {
    fn invoke(
        &self,
        envelope: TaskEnvelope,
        cancellation: AdmissionCancellation,
    ) -> TaskDispatchFuture<'_>;
}
impl TaskDispatcher for GatewayDispatcher {
    fn invoke(
        &self,
        envelope: TaskEnvelope,
        cancellation: AdmissionCancellation,
    ) -> TaskDispatchFuture<'_> {
        Box::pin(async move {
            // These values were resolved at the trusted enqueue boundary. Do
            // not substitute task-row owner fields or reload current roles.
            let TaskEnvelope {
                request,
                roles,
                client_ip,
                ..
            } = envelope;
            let user_id = request.user_id.clone();
            let tenant_id = request.tenant_id.clone();
            let authority = PreflightAuthority::from_resolved_context(&user_id, &tenant_id, &roles);
            GatewayDispatcher::invoke(
                self,
                request,
                &authority,
                client_ip.as_deref(),
                cancellation,
            )
            .await
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskWorkerError {
    Task(TaskError),
    Cancelled,
}
impl fmt::Display for TaskWorkerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Task(error) => error.fmt(f),
            Self::Cancelled => f.write_str("task_worker_cancelled"),
        }
    }
}
impl std::error::Error for TaskWorkerError {}
impl From<TaskError> for TaskWorkerError {
    fn from(error: TaskError) -> Self {
        Self::Task(error)
    }
}

struct Collaborators {
    queue: Option<Arc<dyn TaskQueue>>,
    durable: Option<Arc<crate::task_durable_worker::DurableCollaborators>>,
    storage: Arc<dyn TaskStorage>,
    dispatcher: Arc<dyn TaskDispatcher>,
    task_manager: Arc<TaskManager>,
}
impl Collaborators {
    async fn process_next(&self) -> Result<(), TaskWorkerError> {
        if let Some(durable) = &self.durable {
            return durable.process_next().await;
        }
        let envelope = self
            .queue
            .as_ref()
            .ok_or(TaskError::QueueUnavailable)?
            .dequeue()
            .await?;
        let task_id = envelope.task_id.clone();
        if !self.task_manager.claim_task(&envelope).await? {
            return Ok(());
        }

        let claimed = self.task_manager.get_task(&task_id).await?;
        if claimed.status != TaskStatus::Processing
            || claimed.task_id != envelope.task_id
            || claimed.request_id != envelope.request.request_id
            || claimed.service_id != envelope.request.service_id
            || claimed.user_id != envelope.request.user_id
            || claimed.tenant_id != envelope.request.tenant_id
        {
            return Ok(());
        }

        let cancellation = AdmissionCancellation::default();
        let mut guard = CancelInvokeOnDrop(Some(cancellation.clone()));
        let invoke = self.dispatcher.invoke(envelope, cancellation);
        tokio::pin!(invoke);
        let result = tokio::select! {
            result = &mut invoke => result,
            cancelled = self.wait_for_cancellation(&claimed) => {
                cancelled?;
                // Dropping invoke and the guard signals local cleanup. It never
                // means that the remote request/effect did not happen.
                return Ok(());
            }
        };
        guard.0 = None;
        let (status, result, error) = match result {
            Err(GatewayDispatchError::Dispatch(
                DispatchError::Cancelled | DispatchError::Admission(AdmissionError::Cancelled),
            )) => (TaskStatus::Cancelled, None, None),
            Err(error) => (TaskStatus::Failed, None, Some(error.to_string())),
            Ok(response) => (TaskStatus::Completed, Some(response.into()), None),
        };
        match self
            .task_manager
            .transition_task(claimed.clone(), status, result, error)
            .await
        {
            Ok(_) => {}
            Err(error) if status == TaskStatus::Completed => {
                // Preserve the existing result-persistence failure outcome, but
                // only if the row is still processing. An uncertain COMMIT that
                // actually completed, or a winning cancel, cannot be overwritten.
                // This never retries dispatch or requeues the envelope.
                self.task_manager
                    .transition_task(
                        claimed.clone(),
                        TaskStatus::Failed,
                        None,
                        Some(error.to_string()),
                    )
                    .await?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    async fn wait_for_cancellation(&self, claimed: &Task) -> Result<(), TaskWorkerError> {
        loop {
            // Initial ownership was checked before dispatch. This legacy adapter
            // reads the full row/result, so bound subsequent reads to one per
            // second instead of amplifying payload reads fourfold at 250 ms.
            tokio::time::sleep(Duration::from_secs(1)).await;
            let current = self.task_manager.get_task(&claimed.task_id).await?;
            if current.status != TaskStatus::Processing
                || current.task_id != claimed.task_id
                || current.request_id != claimed.request_id
                || current.service_id != claimed.service_id
                || current.user_id != claimed.user_id
                || current.tenant_id != claimed.tenant_id
            {
                return Ok(());
            }
        }
    }
}
struct CancelInvokeOnDrop(Option<AdmissionCancellation>);
impl Drop for CancelInvokeOnDrop {
    fn drop(&mut self) {
        if let Some(cancellation) = self.0.take() {
            cancellation.cancel();
        }
    }
}

pub struct TaskWorker {
    collaborators: Arc<Collaborators>,
    running: Arc<AtomicBool>,
    failed_iterations: Arc<AtomicUsize>,
    workers: Vec<JoinHandle<Result<(), TaskWorkerError>>>,
}
impl TaskWorker {
    /// Storage and manager should refer to the same backing task store, just as
    /// in the Python composition. The manager owns the injected clock/callbacks.
    pub fn new(
        queue: Arc<dyn TaskQueue>,
        storage: Arc<dyn TaskStorage>,
        dispatcher: Arc<dyn TaskDispatcher>,
        task_manager: Arc<TaskManager>,
    ) -> Self {
        Self {
            collaborators: Arc::new(Collaborators {
                queue: Some(queue),
                durable: None,
                storage,
                dispatcher,
                task_manager,
            }),
            running: Arc::new(AtomicBool::new(false)),
            failed_iterations: Arc::new(AtomicUsize::new(0)),
            workers: Vec::new(),
        }
    }

    pub fn durable(
        storage: Arc<dyn TaskStorage>,
        dispatcher: Arc<dyn TaskDispatcher>,
        manager: Arc<TaskManager>,
        actors: Arc<dyn crate::task_actor::TaskActorValidator>,
    ) -> Result<Self, TaskError> {
        let delivery = manager.delivery().ok_or(TaskError::QueueUnavailable)?;
        let durable = Arc::new(crate::task_durable_worker::DurableCollaborators {
            delivery,
            actors,
            dispatcher: dispatcher.clone(),
            manager: manager.clone(),
        });
        Ok(Self {
            collaborators: Arc::new(Collaborators {
                queue: None,
                durable: Some(durable),
                storage,
                dispatcher,
                task_manager: manager,
            }),
            running: Arc::new(AtomicBool::new(false)),
            failed_iterations: Arc::new(AtomicUsize::new(0)),
            workers: Vec::new(),
        })
    }
    pub async fn verify_ready(&self) -> Result<(), TaskError> {
        if let Some(durable) = &self.collaborators.durable {
            durable
                .delivery
                .verify_ready()
                .await
                .map_err(|_| TaskError::StorageUnavailable)?;
        }
        Ok(())
    }
    /// Python range(concurrency) starts no lanes for zero or negative values,
    /// but still marks the worker running. Repeated start is a no-op, including
    /// after all lanes exited with errors; stop is required before a restart.
    pub async fn start(&mut self, concurrency: i64) {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        for _ in 0..concurrency {
            let collaborators = self.collaborators.clone();
            let running = self.running.clone();
            let failed_iterations = self.failed_iterations.clone();
            self.workers.push(tokio::spawn(async move {
                while running.load(Ordering::Acquire) {
                    if let Err(error) = collaborators.process_next().await {
                        if collaborators.durable.is_none() { return Err(error); }
                        let count = failed_iterations.fetch_add(1, Ordering::AcqRel) + 1;
                        if count == 1 || count % 60 == 0 {
                            eprintln!("durable_task_worker_iteration_failed; retained claim; polling pending work after backoff");
                        }
                        // Poll a fresh committed pending row; never retry the
                        // previous claim, invocation or terminal persistence.
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
                Ok(())
            }));
        }
    }

    pub async fn stop(&mut self) {
        self.running.store(false, Ordering::Release);
        let workers = std::mem::take(&mut self.workers);
        for worker in &workers {
            worker.abort();
        }
        // Establish cancellation before the host drains supervised DB operations.
        for worker in workers {
            let _ = worker.await;
        }
    }

    /// Cumulative failed durable iterations; liveness is not a health verdict.
    pub fn failed_iteration_count(&self) -> usize {
        self.failed_iterations.load(Ordering::Acquire)
    }

    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Includes finished handles, as the source retains them until stop().
    pub fn worker_count(&self) -> usize {
        self.workers.len()
    }

    pub fn finished_worker_count(&self) -> usize {
        self.workers
            .iter()
            .filter(|worker| worker.is_finished())
            .count()
    }

    /// Execute precisely one source loop iteration without spawning a task.
    /// Useful for hosts that own scheduling and for deterministic fault tests.
    /// Dropping this future has the same destructive-pop limitation as stop().
    pub async fn process_next(&self) -> Result<(), TaskWorkerError> {
        self.collaborators.process_next().await
    }

    fn abort_workers(&mut self) {
        self.running.store(false, Ordering::Release);
        for worker in self.workers.drain(..) {
            worker.abort();
        }
    }
}
impl Drop for TaskWorker {
    fn drop(&mut self) {
        self.abort_workers();
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use crate::protocol::UnifiedRequest;
    use crate::task_manager::{MemoryTaskQueue, MemoryTaskStorage};
    use std::sync::{Mutex, atomic::AtomicUsize};
    use tokio::sync::Notify;

    #[derive(Default)]
    struct PendingDispatcher {
        entered: Notify,
        calls: AtomicUsize,
        signal: Mutex<Option<AdmissionCancellation>>,
    }
    impl TaskDispatcher for PendingDispatcher {
        fn invoke(
            &self,
            _: TaskEnvelope,
            cancellation: AdmissionCancellation,
        ) -> TaskDispatchFuture<'_> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                *self.signal.lock().unwrap() = Some(cancellation);
                self.entered.notify_one();
                std::future::pending().await
            })
        }
    }
    #[tokio::test]
    async fn running_cancel_signals_dispatch_and_does_not_resurrect() {
        let storage = Arc::new(MemoryTaskStorage::new());
        let queue = Arc::new(MemoryTaskQueue::new());
        let manager = Arc::new(TaskManager::new(storage.clone(), queue.clone()));
        let task = manager
            .create_task(UnifiedRequest::new("r", "s", vec![]), vec![], None)
            .await
            .unwrap();
        let dispatcher = Arc::new(PendingDispatcher::default());
        let worker = Arc::new(TaskWorker::new(
            queue,
            storage.clone(),
            dispatcher.clone(),
            manager.clone(),
        ));
        let running = tokio::spawn({
            let worker = worker.clone();
            async move { worker.process_next().await }
        });
        dispatcher.entered.notified().await;
        manager.cancel(&task.task_id).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(
            dispatcher
                .signal
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .is_cancelled()
        );
        assert_eq!(
            storage.get(&task.task_id).await.unwrap().status,
            TaskStatus::Cancelled
        );
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn pending_cancel_never_dispatches() {
        let storage = Arc::new(MemoryTaskStorage::new());
        let queue = Arc::new(MemoryTaskQueue::new());
        let manager = Arc::new(TaskManager::new(storage.clone(), queue.clone()));
        let task = manager
            .create_task(UnifiedRequest::new("r", "s", vec![]), vec![], None)
            .await
            .unwrap();
        manager.cancel(&task.task_id).await.unwrap();
        let dispatcher = Arc::new(PendingDispatcher::default());
        let worker = TaskWorker::new(queue, storage, dispatcher.clone(), manager);
        worker.process_next().await.unwrap();
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
    }
}
