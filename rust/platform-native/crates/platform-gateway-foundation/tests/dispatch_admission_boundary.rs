//! Provider I/O must follow an acknowledged admission transition, including
//! lazy stream open. These tests use the real executor and its owned cleanup.
use ai_platform_gateway_foundation::{
    capacity::{CapacityBudget, CapacityResolver},
    capacity_admission::AdmissionCancellation,
    dispatcher::{
        DispatchAdmission, DispatchContext, DispatchError, DispatchExecutor, DispatchFuture,
        DispatchLease, DispatchProducer, DispatchService,
    },
};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Default)]
struct Counts {
    marked: AtomicUsize,
    released: AtomicUsize,
    polled: AtomicUsize,
}
struct Admission {
    counts: Arc<Counts>,
    refuse_mark: bool,
}
struct Lease {
    counts: Arc<Counts>,
    refuse_mark: bool,
}
impl DispatchAdmission for Admission {
    fn acquire<'a>(
        &'a self,
        _: &'a DispatchContext,
        _: &'a str,
        _: &'a str,
        _: &'a [CapacityBudget],
    ) -> DispatchFuture<'a, Box<dyn DispatchLease>> {
        Box::pin(async move {
            Ok(Box::new(Lease {
                counts: self.counts.clone(),
                refuse_mark: self.refuse_mark,
            }) as Box<dyn DispatchLease>)
        })
    }
}
impl DispatchLease for Lease {
    fn bind_owner(&self, _: AdmissionCancellation) -> Result<(), DispatchError> {
        Ok(())
    }
    fn mark_dispatched(&self) -> DispatchFuture<'_, ()> {
        Box::pin(async move {
            self.counts.marked.fetch_add(1, Ordering::SeqCst);
            if self.refuse_mark {
                Err(DispatchError::CleanupFailed)
            } else {
                Ok(())
            }
        })
    }
    fn wait_lost(&self) -> DispatchFuture<'_, ()> {
        Box::pin(std::future::pending())
    }
    fn release(self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async move {
            self.counts.released.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
struct Producer {
    counts: Arc<Counts>,
}
impl DispatchProducer<()> for Producer {
    fn next(&mut self) -> DispatchFuture<'_, Option<()>> {
        Box::pin(async move {
            self.counts.polled.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        })
    }
    fn close(self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async { Ok(()) })
    }
}
fn executor(counts: Arc<Counts>, refuse_mark: bool) -> DispatchExecutor {
    DispatchExecutor::new(
        Arc::new(Admission {
            counts,
            refuse_mark,
        }),
        CapacityResolver::default(),
    )
}
#[tokio::test]
async fn failed_durable_transition_never_polls_sync_provider() {
    let counts = Arc::new(Counts::default());
    let executor = executor(counts.clone(), true);
    let result = executor
        .invoke(
            &DispatchContext::default(),
            &DispatchService::new("s"),
            || async {
                counts.polled.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await;
    assert!(result.is_err());
    assert_eq!(counts.marked.load(Ordering::SeqCst), 1);
    assert_eq!(counts.polled.load(Ordering::SeqCst), 0);
    assert_eq!(counts.released.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn unused_stream_closes_without_marking_dispatched() {
    let counts = Arc::new(Counts::default());
    let executor = executor(counts.clone(), false);
    let mut stream = executor
        .open_stream(
            &DispatchContext::default(),
            &DispatchService::new("s"),
            || {
                Box::new(Producer {
                    counts: counts.clone(),
                })
            },
        )
        .await
        .unwrap();
    assert_eq!(counts.marked.load(Ordering::SeqCst), 0);
    stream.close().await.unwrap();
    assert_eq!(counts.marked.load(Ordering::SeqCst), 0);
    assert_eq!(counts.polled.load(Ordering::SeqCst), 0);
    assert_eq!(counts.released.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn first_stream_poll_marks_once_and_mark_refusal_prevents_open() {
    for refuse in [false, true] {
        let counts = Arc::new(Counts::default());
        let executor = executor(counts.clone(), refuse);
        let mut stream = executor
            .open_stream(
                &DispatchContext::default(),
                &DispatchService::new("s"),
                || {
                    Box::new(Producer {
                        counts: counts.clone(),
                    })
                },
            )
            .await
            .unwrap();
        let first = stream.next().await;
        assert_eq!(first.is_err(), refuse);
        assert_eq!(counts.marked.load(Ordering::SeqCst), 1);
        assert_eq!(counts.polled.load(Ordering::SeqCst), usize::from(!refuse));
        stream.close().await.unwrap();
        assert_eq!(counts.released.load(Ordering::SeqCst), 1);
    }
}
