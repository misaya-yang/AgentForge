//! Gateway-owned generic task delivery ledger for explicit durable mode.
//!
//! Requires the reviewed canonical Gateway-only migration before startup. Existing shared tasks rows cannot authorize
//! new delivery. Pending rows may be claimed once; claimed work is never reset
//! or replayed after a crash, timeout or uncertain transaction outcome.
use crate::{
    postgres_operation::PgOperationStorage,
    task_actor::TaskActor,
    protocol::{ContentData, UnifiedResponse},
    task_manager::{SystemTaskRuntime, Task, TaskEnvelope, TaskResult, TaskRuntime, TaskStatus},
    task_storage::{result_format, saved_result},
};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

#[path="task_reconciliation.rs"]
mod reconciliation;
pub use reconciliation::{ReconcileRequest,ReconcileResult};

const FORMAT: &str = "gateway.generic-task-delivery/v1";
const NEXT: &str = "SELECT task_id,request_id,service_id,user_id,tenant_id,payload,admitted_at FROM gateway.task_deliveries WHERE state='pending' ORDER BY priority DESC,admitted_at,task_id FOR UPDATE SKIP LOCKED LIMIT 1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryError { InvalidEnvelope, StorageUnavailable, ProjectionConflict, FenceRejected }
impl std::fmt::Display for DeliveryError {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {
        f.write_str(match self {Self::InvalidEnvelope=>"task_delivery_invalid_envelope",Self::StorageUnavailable=>"task_delivery_unavailable",Self::ProjectionConflict=>"task_delivery_projection_conflict",Self::FenceRejected=>"task_delivery_fence_rejected"})
    }
}
impl std::error::Error for DeliveryError {}
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum RecoveryDisposition { RecoverPending, ReconcileWithoutReplay, Terminal }
pub fn recovery_disposition(state:&str)->Result<RecoveryDisposition,DeliveryError> {
    match state {
        "pending"=>Ok(RecoveryDisposition::RecoverPending),
        "claimed_unknown"|"reconciliation_required"=>Ok(RecoveryDisposition::ReconcileWithoutReplay),
        "completed"|"failed"|"cancelled"=>Ok(RecoveryDisposition::Terminal),
        _=>Err(DeliveryError::StorageUnavailable),
    }
}
#[derive(Clone,Debug)]
pub struct DeliveryObservation {
    pub state:String, pub recovery:RecoveryDisposition, pub claim_id:Option<Uuid>,
    pub admitted_at:DateTime<Utc>,pub claimed_at:Option<DateTime<Utc>>,
    pub finished_at:Option<DateTime<Utc>>,pub reconciliation_reason:Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeliveryIdentity {
    pub task_id: String, pub request_id: String, pub service_id: String,
    pub user_id: String, pub tenant_id: String,
}
impl DeliveryIdentity {
    pub fn of(envelope: &TaskEnvelope) -> Self {
        Self {task_id:envelope.task_id.clone(),request_id:envelope.request.request_id.clone(),
            service_id:envelope.request.service_id.clone(),user_id:envelope.request.user_id.clone(),tenant_id:envelope.request.tenant_id.clone()}
    }
    fn valid(&self) -> bool {
        [&self.task_id,&self.request_id,&self.service_id,&self.user_id,&self.tenant_id]
            .iter().all(|value| !value.is_empty() && value.len()<=255)
    }
}
#[derive(Clone)]
pub struct DeliveryClaim {
    pub envelope: TaskEnvelope,
    pub actor: TaskActor,
    pub claim_id: Uuid,
    pub admitted_at: DateTime<Utc>,
    pub claimed_at: DateTime<Utc>,
}
impl DeliveryClaim {
    /// Domain queue delay, not provider execution time. Wall-clock reversal is
    /// unknown rather than silently clamped or attributed to a client queue.
    pub fn queue_delay_ms(&self) -> Option<i64> {
        let value=(self.claimed_at-self.admitted_at).num_milliseconds();
        (value>=0).then_some(value)
    }
}
pub enum DeliveryPoll {
    Empty,
    Claimed(DeliveryClaim),
    ReconciliationRequired { task_id:String, reason:&'static str },
    CancelledBeforeDispatch { task_id:String },
}
#[derive(Clone)]
pub enum DeliveryTerminal {
    Completed(UnifiedResponse),
    /// A fixed sanitized code, never a provider/DB diagnostic or request body.
    Failed(&'static str),
    Cancelled,
    /// Execution may have produced an external effect. Preserve the claim forever.
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishResult { Applied(TaskStatus), UnknownRecorded, FenceRejected, ProjectionConflict }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CancelResult { NotManaged, Applied, AlreadyCancelled, TerminalConflict, ProjectionConflict }

pub type DeliveryFuture<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output=Result<T, DeliveryError>> + Send + 'a>>;
/// The same authority instance is shared by admission, cancellation and workers.
/// Test doubles implement this contract; production uses PgTaskDeliveryLedger.
pub trait TaskDelivery: Send + Sync {
    fn configure_provider_batches(&self,_coordinator:std::sync::Arc<crate::provider_batch_store::BatchCoordinator>)->bool{false}
    fn dispatch_provider_batch<'a>(&'a self,claim:&'a DeliveryClaim)->DeliveryFuture<'a,Option<DeliveryTerminal>>{Box::pin(async move{if crate::provider_batch::BatchPlan::from_envelope(&claim.envelope).map_err(|_|DeliveryError::InvalidEnvelope)?.is_some(){Err(DeliveryError::InvalidEnvelope)}else{Ok(None)}})}
    fn reconcile<'a>(&'a self,_identity:&'a DeliveryIdentity,_operator:&'a str,_request:ReconcileRequest)->DeliveryFuture<'a,ReconcileResult>{Box::pin(async{Err(DeliveryError::InvalidEnvelope)})}
    fn publish(&self, envelope: TaskEnvelope, actor: TaskActor) -> DeliveryFuture<'_, Task>;
    fn claim_next(&self) -> DeliveryFuture<'_, DeliveryPoll>;
    fn observe<'a>(&'a self, identity: &'a DeliveryIdentity) -> DeliveryFuture<'a, Option<DeliveryObservation>>;
    fn finish<'a>(&'a self, claim: &'a DeliveryClaim, outcome: DeliveryTerminal) -> DeliveryFuture<'a, FinishResult>;
    fn cancel<'a>(&'a self, identity: &'a DeliveryIdentity) -> DeliveryFuture<'a, CancelResult>;
    fn verify_ready(&self) -> DeliveryFuture<'_, ()>;
    fn drain_cancelled(&self) -> DeliveryFuture<'_, ()>;
}

/// The ledger is the execution admission/claim authority. `gateway.tasks` is
/// retained as the public projection; broader legacy task writers cannot create
/// ledger rows or reset a terminal/claimed delivery back to pending.
#[derive(Clone)]
pub struct PgTaskDeliveryLedger { pool:PgPool, operations:PgOperationStorage,provider_batches:std::sync::Arc<std::sync::OnceLock<std::sync::Arc<crate::provider_batch_store::BatchCoordinator>>> }
impl PgTaskDeliveryLedger {
    pub fn new(pool:PgPool,control_pool:PgPool)->Result<Self,DeliveryError> {
        Ok(Self { operations:PgOperationStorage::new(pool.clone(),control_pool).map_err(|_|DeliveryError::StorageUnavailable)?,pool,provider_batches:Default::default() })
    }
    /// Exact owner-scoped ledger observation. A claimed row surviving a process
    /// crash has an unknown effect; observation never grants replay permission.
    pub async fn observe(&self,identity:&DeliveryIdentity)->Result<Option<DeliveryObservation>,DeliveryError> {
        let row:Option<(String,Option<String>,DateTime<Utc>,Option<DateTime<Utc>>,Option<DateTime<Utc>>,Option<String>)>=sqlx::query_as("SELECT state,claim_id::text,admitted_at,claimed_at,finished_at,reconciliation_reason FROM gateway.task_deliveries WHERE task_id=$1 AND request_id=$2 AND service_id=$3 AND user_id=$4 AND tenant_id=$5")
            .bind(&identity.task_id).bind(&identity.request_id).bind(&identity.service_id).bind(&identity.user_id).bind(&identity.tenant_id)
            .fetch_optional(&self.pool).await.map_err(|_|DeliveryError::StorageUnavailable)?;
        row.map(|(state,claim_id,admitted_at,claimed_at,finished_at,reconciliation_reason)| {
            let claim_id = claim_id.map(|value| Uuid::parse_str(&value)
                .map_err(|_| DeliveryError::StorageUnavailable)).transpose()?;
            Ok(DeliveryObservation {recovery:recovery_disposition(&state)?,state,claim_id,admitted_at,claimed_at,finished_at,reconciliation_reason})
        }).transpose()
    }
    /// A local contract/least-authority guard, not canonical epoch acceptance.
    /// The host's separately reviewed schema admission must run before this gate.
    pub async fn verify_ready(&self)->Result<(),DeliveryError> {
        let ready: bool = sqlx::query_scalar(r#"
            SELECT current_user ~ '^[a-z][a-z0-9_]{0,20}_gateway$'
                AND pg_get_userbyid(c.relowner)=left(current_user,length(current_user)-7)||'owner'
                AND obj_description(c.oid,'pg_class')='gateway.generic-task-delivery/v1'
                AND has_table_privilege(current_user,c.oid,'SELECT')
                AND has_table_privilege(current_user,c.oid,'INSERT')
                AND has_table_privilege(current_user,c.oid,'UPDATE')
                AND NOT has_table_privilege(current_user,c.oid,'DELETE,TRUNCATE,REFERENCES,TRIGGER')
                AND NOT EXISTS(SELECT 1 FROM pg_roles r WHERE r.rolname=ANY(ARRAY[
                    left(current_user,length(current_user)-7)||'runtime',
                    left(current_user,length(current_user)-7)||'capability_worker',
                    left(current_user,length(current_user)-7)||'knowledge_api',
                    left(current_user,length(current_user)-7)||'knowledge_worker'])
                    AND has_table_privilege(r.oid,c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER'))
                AND NOT EXISTS(SELECT 1 FROM aclexplode(COALESCE(c.relacl,acldefault('r',c.relowner))) a WHERE a.grantee=0)
            FROM pg_class c WHERE c.oid='gateway.task_deliveries'::regclass AND c.relkind='r'
        "#).fetch_one(&self.pool).await.map_err(|_|DeliveryError::StorageUnavailable)?;
        if ready { Ok(()) } else { Err(DeliveryError::StorageUnavailable) }
    }
    async fn note_unknown(&self,claim:&DeliveryClaim)->Result<FinishResult,DeliveryError> {
        let identity=DeliveryIdentity::of(&claim.envelope);let claim_id=claim.claim_id;
        let mut operation=self.operations.acquire().await.map_err(|_|DeliveryError::StorageUnavailable)?;
        operation.execute(move |connection| Box::pin(async move {
            let Some(row)=locked_delivery(connection,&identity.task_id).await? else{return Ok(FinishResult::FenceRejected)};
            if !matches_claim(&row,&identity,claim_id) {return Ok(FinishResult::FenceRejected);}
            if task_status(connection,&identity).await?.as_deref()!=Some("processing") {return Ok(FinishResult::ProjectionConflict);}
            sqlx::query("UPDATE gateway.tasks SET error='task_dispatch_outcome_unknown',updated_at=clock_timestamp() WHERE task_id=$1")
                .bind(&identity.task_id).execute(connection).await?;
            // Deliberately leave state=claimed_unknown and retain the claim.
            Ok(FinishResult::UnknownRecorded)
        })).await.map_err(|_|DeliveryError::StorageUnavailable)
    }
    pub async fn drain_cancelled(&self)->Result<(),DeliveryError> {
        self.operations.drain_cancelled().await.map_err(|_|DeliveryError::StorageUnavailable)
    }
    /// Atomically persists the task and executable envelope in its protected
    /// ledger. A lost response is uncertain admission: inspect this exact ID;
    /// never manufacture another task ID or fall back to the in-process queue.
    pub async fn publish(&self,envelope:TaskEnvelope,actor:TaskActor)->Result<Task,DeliveryError> {
        let identity=DeliveryIdentity::of(&envelope);
        if !identity.valid() || !actor.valid() {return Err(DeliveryError::InvalidEnvelope);}
        if crate::provider_batch::BatchPlan::from_envelope(&envelope).map_err(|_|DeliveryError::InvalidEnvelope)?.is_some(){self.provider_batches.get().ok_or(DeliveryError::InvalidEnvelope)?.admit(&envelope).await?;}
        let mut payload=encode_envelope(&envelope)?;
        payload["actor"] = serde_json::to_value(actor).map_err(|_|DeliveryError::InvalidEnvelope)?;
        let mut task=Task::new(envelope.task_id.clone(),&envelope.request,SystemTaskRuntime.now());
        task.priority=envelope.request.priority;
        let priority=i32::try_from(task.priority).map_err(|_|DeliveryError::InvalidEnvelope)?;
        let saved=task.clone();
        let mut operation=self.operations.acquire().await.map_err(|_|DeliveryError::StorageUnavailable)?;
        operation.execute(move |connection| Box::pin(async move {
            // No upsert: an uncertain repeated admission cannot overwrite either
            // an old task projection or its original immutable delivery payload.
            sqlx::query("INSERT INTO gateway.tasks(task_id,request_id,service_id,user_id,tenant_id,status,priority,callback_url,metadata) VALUES($1,$2,$3,$4,$5,'pending',$6,$7,'{}'::jsonb)")
                .bind(&saved.task_id).bind(&saved.request_id).bind(&saved.service_id).bind(&saved.user_id).bind(&saved.tenant_id)
                .bind(priority).bind(&saved.callback_url).execute(&mut *connection).await?;
            sqlx::query("INSERT INTO gateway.task_deliveries(task_id,request_id,service_id,user_id,tenant_id,payload,priority,state) VALUES($1,$2,$3,$4,$5,$6,$7,'pending')")
                .bind(&saved.task_id).bind(&saved.request_id).bind(&saved.service_id).bind(&saved.user_id).bind(&saved.tenant_id)
                .bind(payload).bind(priority).execute(connection).await?;
            Ok(())
        })).await.map_err(|_|DeliveryError::StorageUnavailable)?;
        Ok(task)
    }
    /// Claims exactly one committed pending delivery. The row lock is skipped by
    /// other workers; claim and processing projection commit together. No lease
    /// expiry, reclaim, reset-to-pending or claimed-envelope replay API exists.
    pub async fn claim_next(&self)->Result<DeliveryPoll,DeliveryError> {
        let claim_id=Uuid::new_v4();
        let mut operation=self.operations.acquire().await.map_err(|_|DeliveryError::StorageUnavailable)?;
        operation.execute(move |connection| Box::pin(async move {
            let Some(row)=sqlx::query_as::<_,DeliveryRow>(NEXT).fetch_optional(&mut *connection).await? else {return Ok(DeliveryPoll::Empty)};
            let envelope=match decode_envelope(&row.payload) {
                Ok(envelope) if DeliveryIdentity::of(&envelope)==row.identity()=>envelope,
                _=>{quarantine(connection,&row.task_id,"invalid_envelope").await?;return Ok(DeliveryPoll::ReconciliationRequired{task_id:row.task_id,reason:"invalid_envelope"});}
            };
            let actor = match row.payload.get("actor").cloned().and_then(|value| serde_json::from_value::<TaskActor>(value).ok()).filter(TaskActor::valid) {
                Some(actor) => actor,
                None => {quarantine(connection,&row.task_id,"invalid_envelope").await?;return Ok(DeliveryPoll::ReconciliationRequired{task_id:row.task_id,reason:"invalid_envelope"});}
            };
            let current=task_status(connection,&row.identity()).await?;
            if current.as_deref()==Some("cancelled") {
                sqlx::query("UPDATE gateway.task_deliveries SET state='cancelled',finished_at=clock_timestamp() WHERE task_id=$1 AND state='pending'")
                    .bind(&row.task_id).execute(connection).await?;
                return Ok(DeliveryPoll::CancelledBeforeDispatch{task_id:row.task_id});
            }
            if current.as_deref()!=Some("pending") {
                quarantine(connection,&row.task_id,"task_projection_conflict").await?;
                return Ok(DeliveryPoll::ReconciliationRequired{task_id:row.task_id,reason:"task_projection_conflict"});
            }
            let claimed_at:DateTime<Utc>=sqlx::query_scalar("SELECT clock_timestamp()").fetch_one(&mut *connection).await?;
            sqlx::query("UPDATE gateway.tasks SET status='processing',started_at=$2,updated_at=$2 WHERE task_id=$1")
                .bind(&row.task_id).bind(claimed_at).execute(&mut *connection).await?;
            sqlx::query("UPDATE gateway.task_deliveries SET state='claimed_unknown',claim_id=$2::text::uuid,claimed_at=$3 WHERE task_id=$1 AND state='pending'")
                .bind(&row.task_id).bind(claim_id.to_string()).bind(claimed_at).execute(connection).await?;
            Ok(DeliveryPoll::Claimed(DeliveryClaim{envelope,actor,claim_id,admitted_at:row.admitted_at,claimed_at}))
        })).await.map_err(|_|DeliveryError::StorageUnavailable)
    }
    /// Call after the single owned invoke resolves. Only Applied permits a
    /// best-effort completion callback when Applied is completed/failed. Cancellation preserves the public
    /// cancellation fact; this never asserts remote side effects were undone.
    pub async fn finish(&self,claim:&DeliveryClaim,outcome:DeliveryTerminal)->Result<FinishResult,DeliveryError> {
        if matches!(outcome, DeliveryTerminal::Unknown) { return self.note_unknown(claim).await; }
        let identity=DeliveryIdentity::of(&claim.envelope);let claim_id=claim.claim_id;
        let (status,result,error)=match outcome {
            DeliveryTerminal::Completed(response)=>{
                if response.request_id!=identity.request_id {return Err(DeliveryError::InvalidEnvelope);}
                (TaskStatus::Completed,Some(TaskResult::from(response)),None)
            },
            DeliveryTerminal::Failed(code)=>(TaskStatus::Failed,None,Some(code.to_owned())),
            DeliveryTerminal::Cancelled=>(TaskStatus::Cancelled,None,None),
            DeliveryTerminal::Unknown=>unreachable!("handled before terminal projection"),
        };
        let value=saved_result(&result).map_err(|_|DeliveryError::InvalidEnvelope)?;
        let format=result_format(&result);
        let mut operation=self.operations.acquire().await.map_err(|_|DeliveryError::StorageUnavailable)?;
        operation.execute(move |connection| Box::pin(async move {
            let row=locked_delivery(connection,&identity.task_id).await?;
            let Some(row)=row else{return Ok(FinishResult::FenceRejected)};
            if !matches_claim(&row, &identity, claim_id) {return Ok(FinishResult::FenceRejected);}
            let current=task_status(connection,&identity).await?;
            let final_status=if current.as_deref()==Some("cancelled") {TaskStatus::Cancelled} else if current.as_deref()==Some("processing") {status} else {return Ok(FinishResult::ProjectionConflict)};
            if current.as_deref()==Some("processing") {
                sqlx::query(r#"UPDATE gateway.tasks SET status=$2,result=$3,error=$4,completed_at=clock_timestamp(),updated_at=clock_timestamp(),
                    metadata=CASE WHEN $5::jsonb IS NULL AND jsonb_typeof(metadata)='object' THEN metadata-'gateway.native_task_result.v1'
                    WHEN $5::jsonb IS NULL THEN metadata ELSE (CASE WHEN jsonb_typeof(metadata)='object' THEN metadata ELSE '{}'::jsonb END)||jsonb_build_object('gateway.native_task_result.v1',$5::jsonb) END
                    WHERE task_id=$1"#)
                    .bind(&identity.task_id).bind(final_status.as_str()).bind(value).bind(error).bind(format).execute(&mut *connection).await?;
            }
            sqlx::query("UPDATE gateway.task_deliveries SET state=$3,finished_at=clock_timestamp() WHERE task_id=$1 AND claim_id=$2::text::uuid AND state='claimed_unknown'")
                .bind(&identity.task_id).bind(claim_id.to_string()).bind(final_status.as_str()).execute(connection).await?;
            Ok(FinishResult::Applied(final_status))
        })).await.map_err(|_|DeliveryError::StorageUnavailable)
    }
    /// Public cancellation uses this path for ledger-backed tasks.
    /// It prevents a broader legacy writer resetting task.status from recreating
    /// delivery eligibility after an accepted cancellation.
    pub async fn cancel(&self,identity:&DeliveryIdentity)->Result<CancelResult,DeliveryError> {
        let identity=identity.clone();
        let mut operation=self.operations.acquire().await.map_err(|_|DeliveryError::StorageUnavailable)?;
        operation.execute(move |connection| Box::pin(async move {
            let Some(row)=locked_delivery(connection,&identity.task_id).await? else{return Ok(CancelResult::NotManaged)};
            if row.identity()!=identity {return Ok(CancelResult::ProjectionConflict);}
            match row.state.as_str() {
                "cancelled"=>return Ok(CancelResult::AlreadyCancelled),
                "completed"|"failed"=>return Ok(CancelResult::TerminalConflict),
                "pending"|"claimed_unknown"|"reconciliation_required"=>{},
                _=>return Ok(CancelResult::ProjectionConflict),
            }
            let current=task_status(connection,&identity).await?;
            if !matches!(current.as_deref(),Some("pending"|"processing"|"cancelled")) {return Ok(CancelResult::ProjectionConflict);}
            if current.as_deref()!=Some("cancelled") {
                sqlx::query("UPDATE gateway.tasks SET status='cancelled',completed_at=clock_timestamp(),updated_at=clock_timestamp() WHERE task_id=$1")
                    .bind(&identity.task_id).execute(&mut *connection).await?;
            }
            sqlx::query("UPDATE gateway.task_deliveries SET state='cancelled',reconciliation_reason=NULL,finished_at=clock_timestamp() WHERE task_id=$1")
                .bind(&identity.task_id).execute(connection).await?;
            Ok(CancelResult::Applied)
        })).await.map_err(|_|DeliveryError::StorageUnavailable)
    }
}
impl TaskDelivery for PgTaskDeliveryLedger {
    fn configure_provider_batches(&self,coordinator:std::sync::Arc<crate::provider_batch_store::BatchCoordinator>)->bool{self.provider_batches.set(coordinator).is_ok()}
    fn dispatch_provider_batch<'a>(
        &'a self,
        claim: &'a DeliveryClaim,
    ) -> DeliveryFuture<'a, Option<DeliveryTerminal>> {
        Box::pin(async move {
            if crate::provider_batch::BatchPlan::from_envelope(&claim.envelope)
                .map_err(|_| DeliveryError::InvalidEnvelope)?
                .is_none()
            {
                return Ok(None);
            }
            let outcome = self.provider_batches.get()
                .ok_or(DeliveryError::InvalidEnvelope)?
                .submit_claim_outcome(claim).await?;
            Ok(Some(outcome))
        })
    }
    fn reconcile<'a>(&'a self,identity:&'a DeliveryIdentity,operator:&'a str,request:ReconcileRequest)->DeliveryFuture<'a,ReconcileResult>{Box::pin(PgTaskDeliveryLedger::reconcile(self,identity,operator,request))}

    fn publish(&self,envelope:TaskEnvelope,actor:TaskActor)->DeliveryFuture<'_,Task> {Box::pin(self.publish(envelope,actor))}
    fn claim_next(&self)->DeliveryFuture<'_,DeliveryPoll> {Box::pin(self.claim_next())}
    fn observe<'a>(&'a self,identity:&'a DeliveryIdentity)->DeliveryFuture<'a,Option<DeliveryObservation>> {Box::pin(self.observe(identity))}
    fn finish<'a>(&'a self,claim:&'a DeliveryClaim,outcome:DeliveryTerminal)->DeliveryFuture<'a,FinishResult> {Box::pin(self.finish(claim,outcome))}
    fn cancel<'a>(&'a self,identity:&'a DeliveryIdentity)->DeliveryFuture<'a,CancelResult> {Box::pin(self.cancel(identity))}
    fn verify_ready(&self)->DeliveryFuture<'_,()> {Box::pin(self.verify_ready())}
    fn drain_cancelled(&self)->DeliveryFuture<'_,()> {Box::pin(self.drain_cancelled())}
}
#[derive(FromRow)]
struct DeliveryRow { task_id:String,request_id:String,service_id:String,user_id:String,tenant_id:String,payload:Value,admitted_at:DateTime<Utc> }
impl DeliveryRow {fn identity(&self)->DeliveryIdentity {DeliveryIdentity{task_id:self.task_id.clone(),request_id:self.request_id.clone(),service_id:self.service_id.clone(),user_id:self.user_id.clone(),tenant_id:self.tenant_id.clone()}}}
#[derive(FromRow)]
struct LockedDelivery { task_id:String,request_id:String,service_id:String,user_id:String,tenant_id:String,state:String,claim_id:Option<String> }
impl LockedDelivery {fn identity(&self)->DeliveryIdentity {DeliveryIdentity{task_id:self.task_id.clone(),request_id:self.request_id.clone(),service_id:self.service_id.clone(),user_id:self.user_id.clone(),tenant_id:self.tenant_id.clone()}}}
fn matches_claim(row: &LockedDelivery, identity: &DeliveryIdentity, claim_id: Uuid) -> bool {
    row.identity() == *identity
        && row.state == "claimed_unknown"
        && row.claim_id.as_deref() == Some(claim_id.to_string().as_str())
}
async fn locked_delivery(connection:&mut PgConnection,task_id:&str)->Result<Option<LockedDelivery>,sqlx::Error> {
    sqlx::query_as("SELECT task_id,request_id,service_id,user_id,tenant_id,state,claim_id::text AS claim_id FROM gateway.task_deliveries WHERE task_id=$1 FOR UPDATE")
        .bind(task_id).fetch_optional(connection).await
}
async fn task_status(connection:&mut PgConnection,identity:&DeliveryIdentity)->Result<Option<String>,sqlx::Error> {
    sqlx::query_scalar("SELECT status FROM gateway.tasks WHERE task_id=$1 AND request_id=$2 AND service_id=$3 AND user_id=$4 AND tenant_id=$5 FOR UPDATE")
        .bind(&identity.task_id).bind(&identity.request_id).bind(&identity.service_id).bind(&identity.user_id).bind(&identity.tenant_id).fetch_optional(connection).await
}
async fn quarantine(connection:&mut PgConnection,task_id:&str,reason:&str)->Result<(),sqlx::Error> {
    sqlx::query("UPDATE gateway.task_deliveries SET state='reconciliation_required',reconciliation_reason=$2 WHERE task_id=$1 AND state='pending'")
        .bind(task_id).bind(reason).execute(connection).await?;Ok(())
}
fn encode_envelope(envelope:&TaskEnvelope)->Result<Value,DeliveryError> {
    let value=serde_json::to_value(envelope).map_err(|_|DeliveryError::InvalidEnvelope)?;
    let kinds=envelope.request.inputs.iter().map(|item| match &item.data {
        None=>"none",Some(ContentData::Text(_))=>"text",Some(ContentData::Bytes(_))=>"bytes",Some(ContentData::Json(_))=>"json",
    }).collect::<Vec<_>>();
    Ok(json!({"schema":FORMAT,"envelope":value,"input_data_types":kinds}))
}
fn decode_envelope(value:&Value)->Result<TaskEnvelope,DeliveryError> {
    if value.get("schema").and_then(Value::as_str)!=Some(FORMAT) {return Err(DeliveryError::InvalidEnvelope);}
    let raw=value.get("envelope").ok_or(DeliveryError::InvalidEnvelope)?;
    let mut envelope:TaskEnvelope=serde_json::from_value(raw.clone()).map_err(|_|DeliveryError::InvalidEnvelope)?;
    let kinds=value.get("input_data_types").and_then(Value::as_array).ok_or(DeliveryError::InvalidEnvelope)?;
    if kinds.len()!=envelope.request.inputs.len() || !DeliveryIdentity::of(&envelope).valid() {return Err(DeliveryError::InvalidEnvelope);}
    for (index,(input,kind)) in envelope.request.inputs.iter_mut().zip(kinds).enumerate() {
        let data=&raw["request"]["inputs"][index]["data"];
        input.data=match kind.as_str() {
            Some("none") if data.is_null()=>None,
            Some("text")=>Some(ContentData::Text(data.as_str().ok_or(DeliveryError::InvalidEnvelope)?.to_owned())),
            Some("bytes")=>Some(ContentData::Bytes(serde_json::from_value(data.clone()).map_err(|_|DeliveryError::InvalidEnvelope)?)),
            Some("json")=>Some(ContentData::Json(data.clone())),_=>return Err(DeliveryError::InvalidEnvelope),
        };
    }
    Ok(envelope)
}
#[cfg(test)]
#[path="task_delivery_tests.rs"]
mod tests;
