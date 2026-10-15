use super::*;
use crate::{
    protocol::{UnifiedRequest, UnifiedResponse},
    task_actor::{ActorFuture, TaskActor},
    task_delivery::{CancelResult, DeliveryError, DeliveryFuture, DeliveryObservation, RecoveryDisposition},
    task_manager::{CallbackFuture, CallbackPayload, MemoryTaskStorage, SafeCallbackTransport, TaskEnvelope, TaskStorage, SystemTaskRuntime, TaskRuntime},
    task_worker::{TaskDispatchFuture, TaskWorker},
};
use std::sync::{Mutex, atomic::{AtomicBool, AtomicUsize, Ordering}};
use tokio::sync::Notify;

fn actor() -> TaskActor {
    serde_json::from_value(serde_json::json!({"session_id":uuid::Uuid::new_v4().to_string(),"credential_revision":null,"expires_at":4_000_000_000u64})).unwrap()
}
fn request() -> UnifiedRequest {
    let mut request = UnifiedRequest::new("request","service",vec![]);
    request.user_id="user".into();request.tenant_id="tenant".into();
    request.callback_url=Some("https://callback.example/result".into());
    request
}
struct State { claim: Option<DeliveryClaim>, state: String }
struct Ledger {
    state: Mutex<State>, storage: Arc<MemoryTaskStorage>,
    publishes: AtomicUsize, unknown_finish: AtomicBool, cancel_on_claim: AtomicBool,
}
impl Ledger {
    fn new(storage: Arc<MemoryTaskStorage>) -> Self {
        Self {state:Mutex::new(State{claim:None,state:"absent".into()}),storage,publishes:AtomicUsize::new(0),unknown_finish:AtomicBool::new(false),cancel_on_claim:AtomicBool::new(false)}
    }
    fn status(&self)->String {self.state.lock().unwrap().state.clone()}
}
impl TaskDelivery for Ledger {
    fn publish(&self,envelope:TaskEnvelope,actor:TaskActor)->DeliveryFuture<'_,Task> {
        Box::pin(async move {
            self.publishes.fetch_add(1,Ordering::SeqCst);
            let task=Task::new(envelope.task_id.clone(),&envelope.request,SystemTaskRuntime.now());
            self.storage.save(&task).await.map_err(|_|DeliveryError::StorageUnavailable)?;
            let now=chrono::Utc::now();
            *self.state.lock().unwrap()=State{claim:Some(DeliveryClaim{envelope,actor,claim_id:uuid::Uuid::new_v4(),admitted_at:now,claimed_at:now}),state:"pending".into()};
            Ok(task)
        })
    }
    fn claim_next(&self)->DeliveryFuture<'_,DeliveryPoll> {
        Box::pin(async move {
            let claim={
                let mut state=self.state.lock().unwrap();
                if state.state!="pending" {return Ok(DeliveryPoll::Empty);}
                state.state=if self.cancel_on_claim.load(Ordering::SeqCst) {"cancelled"} else {"claimed_unknown"}.into();
                state.claim.clone().unwrap()
            };
            let mut task=self.storage.get(&claim.envelope.task_id).await.unwrap();
            task.status=TaskStatus::Processing;self.storage.save(&task).await.unwrap();
            Ok(DeliveryPoll::Claimed(claim))
        })
    }
    fn observe<'a>(&'a self, identity:&'a DeliveryIdentity)->DeliveryFuture<'a,Option<DeliveryObservation>> {
        Box::pin(async move {
            let state=self.state.lock().unwrap();let Some(claim)=&state.claim else{return Ok(None)};
            if DeliveryIdentity::of(&claim.envelope)!=*identity {return Ok(None);}
            Ok(Some(DeliveryObservation{state:state.state.clone(),recovery:RecoveryDisposition::ReconcileWithoutReplay,claim_id:Some(claim.claim_id),admitted_at:claim.admitted_at,claimed_at:Some(claim.claimed_at),finished_at:None,reconciliation_reason:None}))
        })
    }
    fn finish<'a>(&'a self,claim:&'a DeliveryClaim,outcome:DeliveryTerminal)->DeliveryFuture<'a,FinishResult> {
        Box::pin(async move {
            if self.unknown_finish.load(Ordering::SeqCst) {return Err(DeliveryError::StorageUnavailable);}
            let status={
                let mut state=self.state.lock().unwrap();
                if state.state!="claimed_unknown" || state.claim.as_ref().unwrap().claim_id!=claim.claim_id {return Ok(FinishResult::FenceRejected);}
                let status=match &outcome {
                    DeliveryTerminal::Completed(_)=>TaskStatus::Completed,
                    DeliveryTerminal::Failed(_)=>TaskStatus::Failed,
                    DeliveryTerminal::Cancelled=>TaskStatus::Cancelled,
                    DeliveryTerminal::Unknown=>return Ok(FinishResult::UnknownRecorded),
                };
                state.state=status.as_str().into();status
            };
            let mut task=self.storage.get(&claim.envelope.task_id).await.unwrap();task.status=status;
            match outcome {
                DeliveryTerminal::Completed(response)=>task.result=Some(response.into()),
                DeliveryTerminal::Failed(error)=>task.error=Some(error.into()),_=>{},
            }
            self.storage.save(&task).await.unwrap();Ok(FinishResult::Applied(status))
        })
    }
    fn cancel<'a>(&'a self,identity:&'a DeliveryIdentity)->DeliveryFuture<'a,CancelResult> {
        Box::pin(async move {
            {
                let mut state=self.state.lock().unwrap();
                let Some(claim)=&state.claim else{return Ok(CancelResult::NotManaged)};
                if DeliveryIdentity::of(&claim.envelope)!=*identity {return Ok(CancelResult::ProjectionConflict);}
                if state.state=="cancelled" {return Ok(CancelResult::AlreadyCancelled);}
                if matches!(state.state.as_str(),"completed"|"failed") {return Ok(CancelResult::TerminalConflict);}
                state.state="cancelled".into();
            }
            let mut task=self.storage.get(&identity.task_id).await.unwrap();task.status=TaskStatus::Cancelled;self.storage.save(&task).await.unwrap();
            Ok(CancelResult::Applied)
        })
    }
    fn verify_ready(&self)->DeliveryFuture<'_,()> {Box::pin(async {Ok(())})}
    fn drain_cancelled(&self)->DeliveryFuture<'_,()> {Box::pin(async {Ok(())})}
}
struct Actors(Result<Vec<String>,TaskActorError>);
impl TaskActorValidator for Actors {
    fn revalidate<'a>(&'a self,_:&'a TaskActor,_:&'a TaskEnvelope)->ActorFuture<'a> {Box::pin(async {self.0.clone()})}
}
struct Dispatcher {calls:AtomicUsize,roles:Mutex<Vec<String>>,unknown:bool,pending:bool,entered:Notify,signal:Mutex<Option<AdmissionCancellation>>}
impl Dispatcher {
    fn new(unknown:bool,pending:bool)->Self {Self{calls:AtomicUsize::new(0),roles:Mutex::new(vec![]),unknown,pending,entered:Notify::new(),signal:Mutex::new(None)}}
}
impl TaskDispatcher for Dispatcher {
    fn invoke(&self,envelope:TaskEnvelope,signal:AdmissionCancellation)->TaskDispatchFuture<'_> {
        Box::pin(async move {
            self.calls.fetch_add(1,Ordering::SeqCst);*self.roles.lock().unwrap()=envelope.roles;
            *self.signal.lock().unwrap()=Some(signal);self.entered.notify_one();
            if self.pending {return std::future::pending().await;}
            if self.unknown {Err(GatewayDispatchError::Dispatch(DispatchError::UpstreamOutcomeUnknown))}
            else {Ok(UnifiedResponse::new(envelope.request.request_id,"completed",vec![]))}
        })
    }
}
#[derive(Default)]
struct Callbacks(Mutex<Vec<String>>);
impl SafeCallbackTransport for Callbacks {
    fn post<'a>(&'a self,url:&'a str,_:&'a CallbackPayload)->CallbackFuture<'a> {Box::pin(async move {self.0.lock().unwrap().push(url.into());Ok(200)})}
}
struct Setup {ledger:Arc<Ledger>,manager:Arc<TaskManager>,worker:Arc<TaskWorker>,dispatcher:Arc<Dispatcher>,callbacks:Arc<Callbacks>,task:Task}
async fn setup(authority:Result<Vec<String>,TaskActorError>,unknown:bool,pending:bool)->Setup {
    let storage=Arc::new(MemoryTaskStorage::new());let ledger=Arc::new(Ledger::new(storage.clone()));
    let callbacks=Arc::new(Callbacks::default());
    let manager=Arc::new(TaskManager::durable(storage.clone(),ledger.clone()).with_safe_callback_transport(callbacks.clone()));
    let task=manager.create_task_with_actor(request(),vec!["stale-admin".into()],None,Some(actor())).await.unwrap();
    let dispatcher=Arc::new(Dispatcher::new(unknown,pending));
    let worker=Arc::new(TaskWorker::durable(storage,dispatcher.clone(),manager.clone(),Arc::new(Actors(authority))).unwrap());
    Setup{ledger,manager,worker,dispatcher,callbacks,task}
}
#[tokio::test]
async fn durable_worker_reloads_roles_and_callbacks_only_once() {
    let s=setup(Ok(vec!["current-user".into()]),false,false).await;
    s.worker.process_next().await.unwrap();s.worker.process_next().await.unwrap();
    assert_eq!(s.dispatcher.calls.load(Ordering::SeqCst),1);
    assert_eq!(*s.dispatcher.roles.lock().unwrap(),vec!["current-user"]);
    assert_eq!(s.ledger.status(),"completed");
    assert_eq!(*s.callbacks.0.lock().unwrap(),vec!["https://callback.example/result"]);
    assert_eq!(s.manager.cancel(&s.task.task_id).await,Err(TaskError::StateConflict));
}
#[tokio::test]
async fn durable_worker_revoked_and_unavailable_authority_never_dispatch() {
    for error in [TaskActorError::Revoked,TaskActorError::Unavailable] {
        let s=setup(Err(error),false,false).await;s.worker.process_next().await.unwrap();
        assert_eq!(s.dispatcher.calls.load(Ordering::SeqCst),0);assert_eq!(s.ledger.status(),"failed");
    }
}
#[tokio::test]
async fn durable_worker_unknown_outcome_is_never_replayed_or_called_back() {
    let s=setup(Ok(vec![]),true,false).await;s.worker.process_next().await.unwrap();s.worker.process_next().await.unwrap();
    assert_eq!(s.ledger.status(),"claimed_unknown");assert_eq!(s.dispatcher.calls.load(Ordering::SeqCst),1);
    assert!(s.callbacks.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn durable_worker_uncertain_finish_never_overwrites_or_replays() {
    let s=setup(Ok(vec![]),false,false).await;s.ledger.unknown_finish.store(true,Ordering::SeqCst);
    assert!(s.worker.process_next().await.is_err());s.worker.process_next().await.unwrap();
    assert_eq!(s.ledger.status(),"claimed_unknown");assert_eq!(s.dispatcher.calls.load(Ordering::SeqCst),1);
    assert!(s.callbacks.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn durable_worker_cancellation_after_claim_prevents_invoke() {
    let s=setup(Ok(vec![]),false,false).await;s.ledger.cancel_on_claim.store(true,Ordering::SeqCst);
    s.worker.process_next().await.unwrap();assert_eq!(s.dispatcher.calls.load(Ordering::SeqCst),0);
    assert!(s.callbacks.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn durable_worker_running_cancel_signals_owned_invoke() {
    let s=setup(Ok(vec![]),false,true).await;
    let run=tokio::spawn({let worker=s.worker.clone();async move {worker.process_next().await}});
    s.dispatcher.entered.notified().await;s.manager.cancel(&s.task.task_id).await.unwrap();
    tokio::time::timeout(Duration::from_secs(2),run).await.unwrap().unwrap().unwrap();
    assert!(s.dispatcher.signal.lock().unwrap().as_ref().unwrap().is_cancelled());
    assert_eq!(s.ledger.status(),"cancelled");assert!(s.callbacks.0.lock().unwrap().is_empty());
}
#[tokio::test]
async fn durable_worker_abort_preserves_unknown_claim() {
    let s=setup(Ok(vec![]),false,true).await;
    let run=tokio::spawn({let worker=s.worker.clone();async move {worker.process_next().await}});
    s.dispatcher.entered.notified().await;run.abort();let _=run.await;
    assert!(s.dispatcher.signal.lock().unwrap().as_ref().unwrap().is_cancelled());
    s.worker.process_next().await.unwrap();assert_eq!(s.dispatcher.calls.load(Ordering::SeqCst),1);
    assert_eq!(s.ledger.status(),"claimed_unknown");
}
#[tokio::test]
async fn durable_admission_without_session_proof_fails_before_publish() {
    let storage=Arc::new(MemoryTaskStorage::new());let ledger=Arc::new(Ledger::new(storage.clone()));
    let manager=TaskManager::durable(storage,ledger.clone());
    assert_eq!(manager.create_task(request(),vec!["admin".into()],None).await,Err(TaskError::DurableAuthorityRequired));
    assert_eq!(ledger.publishes.load(Ordering::SeqCst),0);
}
#[test]
fn durable_unknown_classification_preserves_post_dispatch_uncertainty() {
    assert!(matches!(classify_outcome(Err(GatewayDispatchError::Cleanup)),DeliveryTerminal::Unknown));
    assert!(matches!(classify_outcome(Err(GatewayDispatchError::Dispatch(DispatchError::CleanupFailed))),DeliveryTerminal::Unknown));
    assert!(matches!(classify_outcome(Err(GatewayDispatchError::Dispatch(DispatchError::Admission(AdmissionError::TaskFailed)))),DeliveryTerminal::Unknown));
    assert!(matches!(classify_outcome(Err(GatewayDispatchError::Dispatch(DispatchError::UpstreamOutcomeUnknown))),DeliveryTerminal::Unknown));
    assert!(matches!(classify_outcome(Err(GatewayDispatchError::Dispatch(DispatchError::Cancelled))),DeliveryTerminal::Unknown));
}

#[tokio::test]
async fn durable_worker_retries_polling_without_replaying_failed_claim() {
    let s=setup(Ok(vec![]),false,false).await;
    s.ledger.unknown_finish.store(true,Ordering::SeqCst);
    let mut worker=Arc::try_unwrap(s.worker).ok().unwrap();worker.start(1).await;
    tokio::time::timeout(Duration::from_secs(3),async {
        while worker.failed_iteration_count()==0 {tokio::time::sleep(Duration::from_millis(10)).await;}
    }).await.unwrap();
    assert_eq!(s.ledger.status(),"claimed_unknown");
    s.ledger.unknown_finish.store(false,Ordering::SeqCst);
    let mut second=request();second.request_id="different-request".into();
    let next=s.manager.create_task_with_actor(second,vec![],None,Some(actor())).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3),async {
        while s.ledger.status()!="completed" {tokio::time::sleep(Duration::from_millis(10)).await;}
    }).await.unwrap();
    worker.stop().await;
    assert_ne!(next.task_id,s.task.task_id);
    assert_eq!(s.manager.get_task(&s.task.task_id).await.unwrap().status,TaskStatus::Processing);
    assert_eq!(s.dispatcher.calls.load(Ordering::SeqCst),2);
    assert_eq!(s.callbacks.0.lock().unwrap().len(),1);
}
#[test]
fn durable_capacity_lease_loss_is_unknown() {
    let rejection=crate::load_shedder::CapacityRejected {budget_key:"test".into(),code:"GATEWAY_CAPACITY_LEASE_LOST".into(),status_code:503,queue_wait_ms:0.0,retry_after:1,message:"lease lost".into(),headers:Default::default()};
    assert!(matches!(classify_outcome(Err(GatewayDispatchError::Dispatch(DispatchError::Admission(AdmissionError::Rejected(Box::new(rejection)))))),DeliveryTerminal::Unknown));
}
