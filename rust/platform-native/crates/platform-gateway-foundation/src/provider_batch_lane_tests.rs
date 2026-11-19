use super::*;
use crate::{
    dispatcher::{DispatchError, DispatchFuture, DispatchLease},
    protocol::UnifiedRequest,
    provider_batch::BatchFuture,
    task_actor::TaskActor,
    task_manager::TaskEnvelope,
};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeaseFailure {
    None,
    Bind,
    Lost,
    Cancel,
    Mark,
    Completion,
    Release,
}
struct Lease {
    released: Arc<AtomicUsize>,
    failure: LeaseFailure,
}
impl DispatchLease for Lease {
    fn bind_owner(&self, cancellation: AdmissionCancellation) -> Result<(), DispatchError> {
        if self.failure == LeaseFailure::Bind {
            return Err(DispatchError::CleanupFailed);
        }
        if self.failure == LeaseFailure::Cancel {
            cancellation.cancel();
        }
        Ok(())
    }
    fn wait_lost(&self) -> DispatchFuture<'_, ()> {
        if self.failure == LeaseFailure::Lost {
            return Box::pin(async { Err(DispatchError::CleanupFailed) });
        }
        Box::pin(std::future::pending())
    }
    fn mark_dispatched(&self) -> DispatchFuture<'_, ()> {
        Box::pin(async move {
            if self.failure == LeaseFailure::Mark {
                return Err(DispatchError::CleanupFailed);
            }
            Ok(())
        })
    }
    fn mark_completion(&self, outcome: DispatchSettlement) -> DispatchFuture<'_, ()> {
        assert_eq!(outcome, DispatchSettlement::Unknown);
        Box::pin(async move {
            if self.failure == LeaseFailure::Completion {
                return Err(DispatchError::CleanupFailed);
            }
            Ok(())
        })
    }
    fn release(self: Box<Self>) -> DispatchFuture<'static, ()> {
        Box::pin(async move {
            self.released.fetch_add(1, Ordering::SeqCst);
            if self.failure == LeaseFailure::Release {
                return Err(DispatchError::CleanupFailed);
            }
            Ok(())
        })
    }
}
struct Admission {
    reject: bool,
    released: Arc<AtomicUsize>,
    failure: LeaseFailure,
}
impl DispatchAdmission for Admission {
    fn acquire<'a>(
        &'a self,
        _: &'a DispatchContext,
        _: &'a str,
        _: &'a str,
        _: &'a [crate::capacity::CapacityBudget],
    ) -> DispatchFuture<'a, Box<dyn DispatchLease>> {
        Box::pin(async { panic!("Batch must use the internal marker admission") })
    }
    fn acquire_provider_batch<'a>(
        &'a self,
        _: &'a BatchAdmission,
        context: &'a DispatchContext,
        _: &'a DispatchService,
        _: &'a [crate::capacity::CapacityBudget],
        request: &'a UnifiedRequest,
        _: &'a str,
    ) -> DispatchFuture<'a, Box<dyn DispatchLease>> {
        Box::pin(async move {
            assert_eq!(context.tenant_id, "tenant");
            assert_eq!(request.tenant_id, "tenant");
            if self.reject {
                Err(DispatchError::CleanupFailed)
            } else {
                Ok(Box::new(Lease {
                    released: self.released.clone(),
                    failure: self.failure,
                }) as Box<dyn DispatchLease>)
            }
        })
    }
}
struct Provider {
    calls: Arc<AtomicUsize>,
    error: Option<BatchError>,
}
impl BatchProvider for Provider {
    fn submit<'a>(&'a self, _: &'a BatchPlan, _: uuid::Uuid) -> BatchFuture<'a, Value> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match self.error {
                Some(error) => Err(error),
                None => Ok(json!({"id":"batch_accepted"})),
            }
        })
    }
    fn observe<'a>(&'a self, _: &'a str) -> BatchFuture<'a, Value> {
        Box::pin(async { panic!("submission lane cannot reconcile by retry") })
    }
}
fn claim() -> DeliveryClaim {
    let mut request = UnifiedRequest::new("request", "service", vec![]);
    request.tenant_id = "tenant".into();
    request.user_id = "user".into();
    DeliveryClaim{envelope:TaskEnvelope{task_id:"task".into(),roles:vec![],client_ip:None,request},actor:serde_json::from_value::<TaskActor>(json!({"session_id":uuid::Uuid::new_v4(),"credential_revision":null,"expires_at":4000000000u64})).unwrap(),claim_id:uuid::Uuid::new_v4(),admitted_at:chrono::Utc::now(),claimed_at:chrono::Utc::now()}
}
#[tokio::test]
async fn ambiguous_post_is_never_retried_and_capacity_is_released() {
    let calls = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicUsize::new(0));
    let lane = BatchLane::new(
        Arc::new(Admission {
            reject: false,
            released: released.clone(),
            failure: LeaseFailure::None,
        }),
        Arc::new(CapacityResolver::default()),
    );
    let plan = BatchPlan {
        provider_id: "openai".into(),
        model_id: "model".into(),
        input_file_id: "file-owned".into(),
        endpoint: "/v1/responses".into(),
    };
    assert_eq!(
        lane.submit(
            &claim(),
            &DispatchService::new("service"),
            Arc::new(Provider {
                calls: calls.clone(),
                error: Some(BatchError::UnknownSubmission),
            }),
            &plan,
            uuid::Uuid::new_v4()
        )
        .await,
        Err(BatchError::UnknownSubmission)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(released.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn refused_admission_never_polls_provider() {
    let calls = Arc::new(AtomicUsize::new(0));
    let released = Arc::new(AtomicUsize::new(0));
    let lane = BatchLane::new(
        Arc::new(Admission {
            reject: true,
            released,
            failure: LeaseFailure::None,
        }),
        Arc::new(CapacityResolver::default()),
    );
    let plan = BatchPlan {
        provider_id: "openai".into(),
        model_id: "model".into(),
        input_file_id: "file-owned".into(),
        endpoint: "/v1/responses".into(),
    };
    assert_eq!(
        lane.submit(
            &claim(),
            &DispatchService::new("service"),
            Arc::new(Provider {
                calls: calls.clone(),
                error: Some(BatchError::UnknownSubmission),
            }),
            &plan,
            uuid::Uuid::new_v4()
        )
        .await,
        Err(BatchError::NotDispatched),
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn provider_errors_cannot_claim_safe_release_after_being_polled() {
    for error in [BatchError::Unavailable, BatchError::NotDispatched] {
        let calls = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let lane = BatchLane::new(
            Arc::new(Admission {
                reject: false,
                released: released.clone(),
                failure: LeaseFailure::None,
            }),
            Arc::new(CapacityResolver::default()),
        );
        let plan = BatchPlan {
            provider_id: "openai".into(),
            model_id: "model".into(),
            input_file_id: "file-owned".into(),
            endpoint: "/v1/responses".into(),
        };
        assert_eq!(
            lane.submit(
                &claim(),
                &DispatchService::new("service"),
                Arc::new(Provider {
                    calls: calls.clone(),
                    error: Some(error)
                }),
                &plan,
                uuid::Uuid::new_v4(),
            )
            .await,
            Err(BatchError::UnknownSubmission),
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(released.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn lease_failures_and_cancellation_never_authorize_hold_release() {
    for (failure, expected_calls) in [
        (LeaseFailure::Bind, 0),
        (LeaseFailure::Lost, 0),
        (LeaseFailure::Cancel, 0),
        (LeaseFailure::Mark, 0),
        (LeaseFailure::Completion, 1),
        (LeaseFailure::Release, 1),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let released = Arc::new(AtomicUsize::new(0));
        let lane = BatchLane::new(
            Arc::new(Admission {
                reject: false,
                released: released.clone(),
                failure,
            }),
            Arc::new(CapacityResolver::default()),
        );
        let plan = BatchPlan {
            provider_id: "openai".into(),
            model_id: "model".into(),
            input_file_id: "file-owned".into(),
            endpoint: "/v1/responses".into(),
        };
        assert_eq!(
            lane.submit(
                &claim(),
                &DispatchService::new("service"),
                Arc::new(Provider {
                    calls: calls.clone(),
                    error: None
                }),
                &plan,
                uuid::Uuid::new_v4(),
            )
            .await,
            Err(BatchError::UnknownSubmission),
            "{failure:?}",
        );
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls, "{failure:?}");
        assert_eq!(released.load(Ordering::SeqCst), 1, "{failure:?}");
    }
}
