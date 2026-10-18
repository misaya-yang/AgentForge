//! Durable phase claims. SQL owns admission, owner/fence and checkpoint pointers;
//! object storage owns bounded immutable stage bodies. Advisory publication locks
//! remain the final cross-replica mutation authority.
use super::{
    Chunk, Dataset, IngestError, IngestionService, Result, canonical_json, object, sha256, string,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    #[default]
    Worker,
    All,
    Parse,
    Embed,
    Index,
}
impl Stage {
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "worker" => Ok(Self::Worker),
            "all" => Ok(Self::All),
            "parse" => Ok(Self::Parse),
            "embed" => Ok(Self::Embed),
            "index" => Ok(Self::Index),
            _ => Err(IngestError::Validation(
                "invalid Knowledge pipeline stage".into(),
            )),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Worker => "worker",
            Self::Parse => "parse",
            Self::Embed => "embed",
            Self::Index => "index",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Configuration {
    pub enabled: bool,
    pub stage: Stage,
    pub lease: Duration,
    pub renew: Duration,
}
impl Default for Configuration {
    fn default() -> Self {
        Self {
            enabled: false,
            stage: Stage::Worker,
            lease: Duration::from_secs(60),
            renew: Duration::from_secs(20),
        }
    }
}
impl Configuration {
    pub fn validate(&self) -> Result<()> {
        if self.lease < Duration::from_secs(15)
            || self.lease > Duration::from_secs(300)
            || self.renew.is_zero()
            || self.renew * 2 >= self.lease
        {
            return Err(IngestError::Validation(
                "pipeline lease must be 15-300s and renew below half the lease".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone)]
pub struct Claim {
    pub job: Value,
    pub token: String,
    pub config: Configuration,
    pub yielded: Arc<AtomicBool>,
}
tokio::task_local! {static CURRENT:Claim;}
pub fn current() -> Option<Claim> {
    CURRENT.try_with(Clone::clone).ok()
}
pub fn yielded() -> bool {
    current().is_some_and(|c| c.yielded.load(Ordering::Acquire))
}
pub fn stage() -> Stage {
    current().map_or(Stage::Worker, |c| c.config.stage)
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Parsed {
    pub schema: String,
    pub identity: String,
    pub execution: String,
    pub source_sha256: String,
    pub content: String,
    pub patch: Value,
    pub pending: Option<Value>,
    pub old: Vec<Chunk>,
    pub chunks: Vec<Chunk>,
    pub reembed: bool,
    pub image_points: BTreeMap<String, Vec<Value>>,
    pub replace_images: bool,
    pub docir: Option<super::parsing_v2::integration::Prepared>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct SpecialParsed {
    pub schema: String,
    pub identity: String,
    pub execution: String,
    pub source_sha256: String,
    pub plan: super::hierarchy::CandidatePlan,
    pub expected_content: String,
    pub patch: Value,
    pub docir: Option<super::parsing_v2::integration::Prepared>,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Embedded {
    pub schema: String,
    pub chunks_sha256: String,
    pub points: BTreeMap<String, Vec<Value>>,
}
fn fail() -> IngestError {
    IngestError::GenerationChanged
}
/// Lock the exact durable owner through a source-publication transaction. The
/// existing document/dataset advisory leases still serialize cleanup and Qdrant.
pub async fn require_fence(conn: &mut sqlx::PgConnection) -> Result<()> {
    let Some(claim) = current() else {
        return Ok(());
    };
    let held:Option<i64>=sqlx::query_scalar("SELECT fence FROM knowledge.pipeline_work WHERE job_id=$1::uuid AND owner_token=$2 AND fence=$3 AND status='running' AND lease_expires_at>clock_timestamp() FOR SHARE")
      .bind(string(&claim.job,"job_id")).bind(&claim.token).bind(claim.job["fence"].as_i64().unwrap_or(-1)).fetch_optional(crate::database::connection(&mut *conn)).await?;
    if held.is_none() {
        return Err(fail());
    }
    Ok(())
}
pub async fn resume_claim(
    conn: &mut sqlx::PgConnection,
    dataset: &str,
    document: &str,
) -> Result<()> {
    let Some(claim) = current() else {
        return Ok(());
    };
    let exists:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM knowledge.pipeline_work WHERE job_id=$1::uuid AND owner_token=$2 AND fence=$3 AND dataset_id=$4 AND document_id=$5 AND status='running' AND lease_expires_at>clock_timestamp())").bind(string(&claim.job,"job_id")).bind(&claim.token).bind(claim.job["fence"].as_i64().unwrap_or(-1)).bind(dataset).bind(document).fetch_one(crate::database::connection(&mut *conn)).await?;
    if !exists {
        return Err(fail());
    }
    sqlx::query("UPDATE knowledge.documents SET status='waiting' WHERE document_id=$1 AND dataset_id=$2 AND status IN ('parsing','splitting','indexing') AND ($3::text IS NULL OR metadata->>'_document_pipeline_execution_id'=$3)").bind(document).bind(dataset).bind(claim.job["execution_id"].as_str()).execute(crate::database::connection(&mut *conn)).await?;
    Ok(())
}
impl IngestionService {
    pub async fn discover_pipeline_work(&self) -> Result<usize> {
        if crate::database::control_enabled(
            &self.store.pool,
            "knowledge.discover_pipeline_work_v1()",
        )
        .await?
        {
            let count: i64 = sqlx::query_scalar("SELECT knowledge.discover_pipeline_work_v1()")
                .fetch_one(crate::database::executor(&self.store.pool))
                .await?;
            return usize::try_from(count).map_err(|_| IngestError::GenerationChanged);
        }
        // Initial and replacement admissions are observed from the canonical waiting
        // rows. A completed job can be replaced only by a newly waiting admission.
        Ok(sqlx::query("INSERT INTO knowledge.pipeline_work(document_id,dataset_id,tenant_id,job_id,phase,status,execution_id,checkpoints) SELECT d.document_id,d.dataset_id,ds.tenant_id,gen_random_uuid(),'parse','ready',d.metadata->>'_document_pipeline_execution_id','{}'::jsonb FROM knowledge.documents d JOIN knowledge.datasets ds USING(dataset_id) WHERE d.status='waiting' AND NOT ds.is_deleted AND NOT ds.is_archived AND COALESCE(d.enabled,TRUE) AND NOT COALESCE(d.archived,FALSE) ORDER BY d.created_at LIMIT 1000 ON CONFLICT(document_id) DO UPDATE SET job_id=EXCLUDED.job_id,phase=EXCLUDED.phase,status='ready',execution_id=EXCLUDED.execution_id,checkpoints='{}'::jsonb,owner_token=NULL,lease_expires_at=NULL,fence=knowledge.pipeline_work.fence+1 WHERE knowledge.pipeline_work.status IN ('completed','error')").execute(crate::database::executor(&self.store.pool)).await?.rows_affected() as usize)
    }
    pub async fn claim_pipeline_work(&self, config: &Configuration) -> Result<Option<Claim>> {
        config.validate()?;
        let token = uuid::Uuid::new_v4().to_string();
        let claim_sql = if crate::database::control_enabled(
            &self.store.pool,
            "knowledge.claim_pipeline_work_v1(text,text,double precision)",
        )
        .await?
        {
            "SELECT * FROM knowledge.claim_pipeline_work_v1($1,$2,$3)"
        } else {
            "WITH candidate AS (SELECT w.document_id FROM knowledge.pipeline_work w JOIN knowledge.documents d ON d.document_id=w.document_id AND d.dataset_id=w.dataset_id JOIN knowledge.datasets ds ON ds.dataset_id=w.dataset_id AND ds.tenant_id=w.tenant_id WHERE (w.phase=$1 OR $1='all') AND (w.status='ready' OR (w.status='running' AND w.lease_expires_at<=clock_timestamp())) AND NOT ds.is_deleted AND NOT ds.is_archived AND d.status IN ('waiting','parsing','splitting','indexing') ORDER BY w.updated_at,w.document_id FOR UPDATE OF w SKIP LOCKED LIMIT 1) UPDATE knowledge.pipeline_work w SET status='running',owner_token=$2,fence=w.fence+1,lease_expires_at=clock_timestamp()+make_interval(secs=>$3),updated_at=clock_timestamp() FROM candidate c WHERE w.document_id=c.document_id RETURNING to_jsonb(w)"
        };
        let row: Option<Value> = sqlx::query_scalar(claim_sql)
            .bind(if config.stage == Stage::Worker {
                "all"
            } else {
                config.stage.name()
            })
            .bind(&token)
            .bind(config.lease.as_secs_f64())
            .fetch_optional(crate::database::executor(&self.store.pool))
            .await?;
        Ok(row.map(|job| {
            let mut config = config.clone();
            config.stage = Stage::parse(string(&job, "phase")).unwrap_or(Stage::Worker);
            Claim {
                job,
                token,
                config,
                yielded: Arc::new(AtomicBool::new(false)),
            }
        }))
    }
    pub async fn run_pipeline_claim(&self, claim: Claim) -> Result<()> {
        let tenant = string(&claim.job, "tenant_id").to_owned();
        crate::database::with_job_scope(&tenant, self.run_owned_pipeline_claim(claim)).await?
    }

    async fn run_owned_pipeline_claim(&self, claim: Claim) -> Result<()> {
        let dataset = string(&claim.job, "dataset_id").to_owned();
        let document = string(&claim.job, "document_id").to_owned();
        let (lost, mut loss) = tokio::sync::watch::channel(false);
        let pool = self.store.pool.clone();
        let owner = claim.clone();
        let heartbeat = tokio::spawn(async move {
            let mut clock = tokio::time::interval(owner.config.renew);
            clock.tick().await;
            loop {
                clock.tick().await;
                if owner.yielded.load(Ordering::Acquire) {
                    return;
                }
                let update=sqlx::query("UPDATE knowledge.pipeline_work SET lease_expires_at=clock_timestamp()+make_interval(secs=>$4),updated_at=clock_timestamp() WHERE job_id=$1::uuid AND owner_token=$2 AND fence=$3 AND status='running' AND lease_expires_at>clock_timestamp()")
                    .bind(string(&owner.job,"job_id")).bind(&owner.token).bind(owner.job["fence"].as_i64().unwrap_or(-1)).bind(owner.config.lease.as_secs_f64()).execute(crate::database::executor(&pool));
                let ok =
                    tokio::time::timeout(owner.config.renew.min(Duration::from_secs(5)), update)
                        .await;
                if !matches!(ok,Ok(Ok(r)) if r.rows_affected()==1) {
                    if !owner.yielded.load(Ordering::Acquire) {
                        lost.send_replace(true);
                    }
                    return;
                }
            }
        });
        // Dropping/aborting the ingestion future must also stop renewal. A
        // detached heartbeat would prevent another owner from ever recovering.
        struct StopHeartbeat(tokio::task::AbortHandle);
        impl Drop for StopHeartbeat {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _heartbeat = StopHeartbeat(heartbeat.abort_handle());
        let work = CURRENT.scope(
            claim.clone(),
            super::model_plane_worker::ingest(self, &dataset, &document),
        );
        tokio::pin!(work);
        let result = tokio::select! {result=&mut work=>result.map(|_|()),_=loss.changed()=>{if claim.yielded.load(Ordering::Acquire){Ok(())}else{Err(fail())}}};
        heartbeat.abort();
        if !claim.yielded.load(Ordering::Acquire) {
            // A refused CAS never changes a successor's job. LeaseBusy is only local
            // advisory-lock contention, so return the same claim to durable discovery.
            let state = if result == Err(IngestError::LeaseBusy) {
                "ready"
            } else if result.is_ok() {
                "completed"
            } else {
                "error"
            };
            sqlx::query("UPDATE knowledge.pipeline_work SET status=$4,owner_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp() WHERE job_id=$1::uuid AND owner_token=$2 AND fence=$3 AND status='running'")
    .bind(string(&claim.job,"job_id")).bind(&claim.token).bind(claim.job["fence"].as_i64().unwrap_or(-1)).bind(state).execute(crate::database::executor(&self.store.pool)).await?;
        }
        result
    }
    pub(crate) async fn pipeline_checkpoint<T: Serialize>(
        &self,
        name: &str,
        value: &T,
        next: Option<Stage>,
        execution: &str,
    ) -> Result<()> {
        let Some(claim) = current() else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(value)?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(IngestError::Validation(
                "pipeline checkpoint exceeds byte budget".into(),
            ));
        }
        let digest = sha256(&bytes);
        let key = format!(
            "knowledge-stage/{}/{}/{}.json",
            sha256(string(&claim.job, "tenant_id").as_bytes()),
            string(&claim.job, "job_id"),
            digest
        );
        self.assets
            .backend
            .upload(&key, bytes.clone(), "application/json", Default::default())
            .await
            .map_err(|_| IngestError::Storage)?;
        let receipt = json!({"key":key,"sha256":digest,"bytes":bytes.len(),"job_id":claim.job["job_id"],"dataset_id":claim.job["dataset_id"],"document_id":claim.job["document_id"]});
        let mut tx = self.store.pool.begin().await?;
        crate::database::bind_current(&mut tx).await?;
        let count=sqlx::query("UPDATE knowledge.pipeline_work SET checkpoints=jsonb_set(checkpoints,ARRAY[$4::text],$5::jsonb,TRUE),execution_id=$6,phase=COALESCE($7,phase),status=CASE WHEN $7::text IS NULL THEN status ELSE 'ready' END,owner_token=CASE WHEN $7::text IS NULL THEN owner_token ELSE NULL END,lease_expires_at=CASE WHEN $7::text IS NULL THEN lease_expires_at ELSE NULL END,updated_at=clock_timestamp() WHERE job_id=$1::uuid AND owner_token=$2 AND fence=$3 AND status='running' AND lease_expires_at>clock_timestamp()")
   .bind(string(&claim.job,"job_id")).bind(&claim.token).bind(claim.job["fence"].as_i64().unwrap_or(-1)).bind(name).bind(receipt).bind(execution).bind(next.map(Stage::name)).execute(&mut *tx).await?.rows_affected();
        if count != 1 {
            return Err(fail());
        }
        if next.is_some() {
            let count=sqlx::query("UPDATE knowledge.documents SET status='waiting',updated_at=NOW() WHERE document_id=$1 AND dataset_id=$2 AND metadata->>'_document_pipeline_execution_id'=$3 AND status IN ('parsing','splitting','indexing')").bind(string(&claim.job,"document_id")).bind(string(&claim.job,"dataset_id")).bind(execution).execute(&mut *tx).await?.rows_affected();
            if count != 1 {
                return Err(fail());
            }
            sqlx::query("SELECT pg_notify('knowledge_pipeline_ready',$1)")
                .bind(string(&claim.job, "dataset_id"))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if next.is_some() {
            claim.yielded.store(true, Ordering::Release);
            return Err(IngestError::PublicationContended);
        }
        Ok(())
    }
    pub(crate) async fn load_pipeline_checkpoint<T: serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Result<Option<T>> {
        let Some(claim) = current() else {
            return Ok(None);
        };
        let pointer = &claim.job["checkpoints"][name];
        if pointer.is_null() {
            return Ok(None);
        }
        let key = pointer["key"].as_str().ok_or_else(fail)?;
        let expected = format!(
            "knowledge-stage/{}/{}/",
            sha256(string(&claim.job, "tenant_id").as_bytes()),
            string(&claim.job, "job_id")
        );
        if !key.starts_with(&expected)
            || pointer["job_id"] != claim.job["job_id"]
            || pointer["dataset_id"] != claim.job["dataset_id"]
            || pointer["document_id"] != claim.job["document_id"]
        {
            return Err(fail());
        }
        let bytes = self.assets.download_bounded(key, 64 * 1024 * 1024).await?;
        if pointer["sha256"] != sha256(&bytes) || pointer["bytes"] != bytes.len() {
            return Err(fail());
        }
        Ok(Some(serde_json::from_slice(&bytes)?))
    }
}
pub fn source_identity(dataset: &Dataset, doc: &Value, index: &Value) -> String {
    sha256(canonical_json(&json!({"dataset":dataset.dataset_id,"tenant":dataset.tenant_id,"document":doc["document_id"],"title":doc["title"],"source_type":doc["source_type"],"mime_type":doc["mime_type"],"process_rule":doc["process_rule_id"],"content":doc["content"],"source_key":doc["metadata"]["original_file_key"],"index":index})).as_bytes())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phase_configuration_rejects_unsafe_leases() {
        let mut c = Configuration::default();
        assert!(c.validate().is_ok());
        c.renew = c.lease;
        assert!(c.validate().is_err());
        assert!(Stage::parse("publisher").is_err());
    }
    #[test]
    fn checkpoint_identity_pins_source_and_parser_configuration() {
        let d = Dataset {
            dataset_id: "d".into(),
            tenant_id: "t".into(),
            ..Default::default()
        };
        let doc = json!({"document_id":"x","content":"source"});
        let old = source_identity(&d, &doc, &json!({}));
        assert_ne!(
            old,
            source_identity(&d, &doc, &json!({"parsing_v2":{"profile_id":"other"}}))
        );
        let mut other = doc.clone();
        other["content"] = json!("replacement");
        assert_ne!(old, source_identity(&d, &other, &json!({})));
    }
}
#[cfg(test)]
mod phase_scope_tests {
    use super::*;
    #[tokio::test]
    async fn phase_context_is_restored_after_yield_and_keeps_owner_fence() {
        let claim = Claim {
            job: json!({"job_id":"j","fence":9}),
            token: "owner".into(),
            config: Configuration {
                stage: Stage::Parse,
                ..Default::default()
            },
            yielded: Arc::new(AtomicBool::new(false)),
        };
        assert_eq!(stage(), Stage::Worker);
        CURRENT
            .scope(claim.clone(), async {
                assert_eq!(stage(), Stage::Parse);
                tokio::task::yield_now().await;
                assert_eq!(current().unwrap().job["fence"], 9);
                current().unwrap().yielded.store(true, Ordering::Release);
                assert!(yielded());
            })
            .await;
        assert_eq!(stage(), Stage::Worker);
        assert!(!yielded());
        assert!(claim.yielded.load(Ordering::Acquire));
    }
}
