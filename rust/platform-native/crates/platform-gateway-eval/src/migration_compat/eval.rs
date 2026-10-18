//! Tenant-scoped eval dataset/evaluator/experiment persistence and HTTP boundaries.
use super::common::{CompatError, CompatUser, object, text};
use crate::ports::{DocumentSourceReference, KnowledgeActor, KnowledgeAuthority};
use ai_platform_auth::{
    api_keys::{SelectedCredential, select_credential},
    service::AuthService,
};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderName, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::{
    collections::{BTreeSet, HashMap},
    sync::Arc,
};

pub struct EvalService {
    pub pool: PgPool,
    pub knowledge: Option<Arc<dyn KnowledgeAuthority>>,
}
fn uuid(value: &str) -> Result<(), CompatError> {
    uuid::Uuid::parse_str(value)
        .map(|_| ())
        .map_err(|_| CompatError::new(422, "Invalid resource ID"))
}
fn metadata_patch(payload: &Value) -> Value {
    let mut metadata = object(&payload["metadata"]);
    for key in [
        "case_id",
        "expected_trajectory",
        "assertions",
        "tags",
        "difficulty",
        "owner",
        "review_status",
    ] {
        if let Some(value) = payload.get(key) {
            metadata.insert(key.into(), value.clone());
        }
    }
    json!(metadata)
}
pub fn reject_reserved_kb_metadata(metadata: &Value) -> Result<(), CompatError> {
    if [
        "source_kind",
        "kb_source_versions",
        "kb_source_versions_verified",
        "case_revision",
        "supersedes_example_id",
    ]
    .iter()
    .any(|key| metadata.get(*key).is_some())
    {
        return Err(CompatError::new(
            422,
            "KB failure provenance must use the dedicated revision endpoint",
        ));
    }
    Ok(())
}
impl EvalService {
    pub fn new(pool: PgPool, knowledge: Option<Arc<dyn KnowledgeAuthority>>) -> Self {
        Self { pool, knowledge }
    }
    pub async fn get(
        &self,
        tenant: &str,
        resource: &str,
        id: &str,
    ) -> Result<Option<Value>, CompatError> {
        uuid(id)?;
        let (table, key) = resource_table(resource)?;
        let sql =
            format!("SELECT to_jsonb(t) FROM {table} t WHERE tenant_id=$1 AND {key}=$2::uuid");
        let mut result = sqlx::query_scalar::<_, Value>(&sql)
            .bind(tenant)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        if resource == "experiments" {
            if let Some(value) = &mut result {
                let runs=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(r) FROM eval_experiment_runs r WHERE tenant_id=$1 AND experiment_id=$2::uuid ORDER BY created_at DESC").bind(tenant).bind(id).fetch_all(&self.pool).await?;
                value["runs"] = json!(runs);
            }
        }
        if resource == "experiment-runs" {
            if let Some(value) = &mut result {
                let progress=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('total_trials',COUNT(*)::int,'completed_trials',COUNT(*) FILTER (WHERE status IN ('succeeded','failed','skipped'))::int,'failed_trials',COUNT(*) FILTER (WHERE status='failed')::int) FROM eval_experiment_run_cases WHERE tenant_id=$1 AND run_id=$2::uuid").bind(tenant).bind(id).fetch_one(&self.pool).await?;
                if value["run_mode"] == "live_candidate" {
                    value["progress"] = progress;
                    value["runtime_fingerprint"] = value["metrics"]["actual_fingerprint"].clone();
                    value["gate_status"] = value["metrics"]["gate"]["status"].clone();
                }
            }
        }
        Ok(result)
    }
    pub async fn list(
        &self,
        tenant: &str,
        resource: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Value, CompatError> {
        let (table, _) = resource_table(resource)?;
        let total = sqlx::query_scalar::<_, i64>(&format!(
            "SELECT COUNT(*) FROM {table} WHERE tenant_id=$1"
        ))
        .bind(tenant)
        .fetch_one(&self.pool)
        .await?;
        let rows=sqlx::query_scalar::<_,Value>(&format!("SELECT to_jsonb(t) FROM {table} t WHERE tenant_id=$1 ORDER BY created_at DESC LIMIT $2 OFFSET $3")).bind(tenant).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        let mut result = json!({"total":total,"limit":limit,"offset":offset});
        result[resource] = json!(rows);
        Ok(result)
    }
    pub async fn dataset_manifest(&self, tenant: &str) -> Result<Vec<Value>, CompatError> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(d) FROM eval_datasets d WHERE tenant_id=$1 ORDER BY created_at DESC,dataset_id DESC").bind(tenant).fetch_all(&self.pool).await?)
    }
    pub async fn examples(
        &self,
        tenant: &str,
        dataset: &str,
        split: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Value, CompatError> {
        uuid(dataset)?;
        let split = split.filter(|v| !v.is_empty());
        let total=sqlx::query_scalar::<_,i64>("SELECT COUNT(*) FROM eval_examples WHERE tenant_id=$1 AND dataset_id=$2::uuid AND ($3::text IS NULL OR split=$3)").bind(tenant).bind(dataset).bind(split).fetch_one(&self.pool).await?;
        let examples=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(e) FROM eval_examples e WHERE tenant_id=$1 AND dataset_id=$2::uuid AND ($3::text IS NULL OR split=$3) ORDER BY created_at DESC LIMIT $4 OFFSET $5").bind(tenant).bind(dataset).bind(split).bind(limit).bind(offset).fetch_all(&self.pool).await?;
        Ok(json!({"examples":examples,"total":total,"limit":limit,"offset":offset}))
    }
    pub async fn example(
        &self,
        tenant: &str,
        dataset: &str,
        example: &str,
    ) -> Result<Option<Value>, CompatError> {
        uuid(dataset)?;
        uuid(example)?;
        Ok(sqlx::query_scalar("SELECT to_jsonb(e) FROM eval_examples e WHERE tenant_id=$1 AND dataset_id=$2::uuid AND example_id=$3::uuid").bind(tenant).bind(dataset).bind(example).fetch_optional(&self.pool).await?)
    }
    pub async fn create(
        &self,
        user: &CompatUser,
        resource: &str,
        payload: &Value,
    ) -> Result<Value, CompatError> {
        if text(payload, "name").is_empty() {
            return Err(CompatError::new(422, "name is required"));
        }
        let version = if text(payload, "version").is_empty() {
            "v1"
        } else {
            text(payload, "version")
        };
        match resource {
            "datasets"=>Ok(sqlx::query_scalar("WITH created AS (INSERT INTO eval_datasets(tenant_id,name,description,version,schema,metadata,created_by) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING *) SELECT to_jsonb(created) FROM created").bind(&user.tenant_id).bind(text(payload,"name")).bind(text(payload,"description")).bind(version).bind(payload.get("schema").or_else(||payload.get("json_schema")).cloned().unwrap_or(json!({}))).bind(json!(object(&payload["metadata"]))).bind(&user.user_id).fetch_one(&self.pool).await?),
            "evaluators"=>Ok(sqlx::query_scalar("WITH created AS (INSERT INTO eval_evaluators(tenant_id,name,evaluator_type,rubric,version,sampling_config,filter_config,metadata,created_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *) SELECT to_jsonb(created) FROM created").bind(&user.tenant_id).bind(text(payload,"name")).bind(if text(payload,"evaluator_type").is_empty(){"human"}else{text(payload,"evaluator_type")}).bind(text(payload,"rubric")).bind(version).bind(json!(object(&payload["sampling_config"]))).bind(json!(object(&payload["filter_config"]))).bind(json!(object(&payload["metadata"]))).bind(&user.user_id).fetch_one(&self.pool).await?),
            "experiments"=>{let dataset=payload.get("dataset_id").and_then(Value::as_str);if let Some(dataset)=dataset{uuid(dataset)?;}let mut result:Value=sqlx::query_scalar("WITH created AS (INSERT INTO eval_experiments(tenant_id,dataset_id,name,description,target_config,metadata,created_by) VALUES($1,$2::uuid,$3,$4,$5,$6,$7) RETURNING *) SELECT to_jsonb(created) FROM created").bind(&user.tenant_id).bind(dataset).bind(text(payload,"name")).bind(text(payload,"description")).bind(json!(object(&payload["target_config"]))).bind(json!(object(&payload["metadata"]))).bind(&user.user_id).fetch_one(&self.pool).await?;result["runs"]=json!([]);Ok(result)},
            _=>Err(CompatError::new(404,"Unknown eval resource")),
        }
    }
    pub async fn create_example(
        &self,
        user: &CompatUser,
        dataset: &str,
        payload: &Value,
    ) -> Result<Value, CompatError> {
        uuid(dataset)?;
        let trace = payload.get("source_trace_id").and_then(Value::as_str);
        let span = payload.get("source_span_id").and_then(Value::as_str);
        if let Some(trace) = trace {
            uuid(trace)?;
        }
        if let Some(span) = span {
            uuid(span)?;
        }
        Ok(sqlx::query_scalar("WITH created AS (INSERT INTO eval_examples(dataset_id,tenant_id,split,input,expected_output,metadata,source_trace_id,source_span_id,created_by) VALUES($1::uuid,$2,$3,$4,$5,$6,$7::uuid,$8::uuid,$9) RETURNING *) SELECT to_jsonb(created) FROM created").bind(dataset).bind(&user.tenant_id).bind(if text(payload,"split").is_empty(){"regression"}else{text(payload,"split")}).bind(json!(object(&payload["input"]))).bind(json!(object(&payload["expected_output"]))).bind(metadata_patch(payload)).bind(trace).bind(span).bind(&user.user_id).fetch_one(&self.pool).await?)
    }
    pub async fn update_example(
        &self,
        user: &CompatUser,
        dataset: &str,
        example: &str,
        payload: &Value,
    ) -> Result<Value, CompatError> {
        reject_reserved_kb_metadata(&payload["metadata"])?;
        let existing = self
            .example(&user.tenant_id, dataset, example)
            .await?
            .ok_or_else(|| CompatError::new(404, "Example not found"))?;
        if text(&existing["metadata"], "source_kind") == "kb_failure" {
            return self
                .review_kb_failure(user, dataset, example, payload)
                .await;
        }
        let mut metadata = object(&existing["metadata"]);
        metadata.extend(object(&metadata_patch(payload)));
        let merged = json!({"case_id":metadata.get("case_id").cloned().unwrap_or(json!(example)),"input":payload.get("input").filter(|v|!v.is_null()).unwrap_or(&existing["input"]),"expected_output":payload.get("expected_output").filter(|v|!v.is_null()).unwrap_or(&existing["expected_output"]),"expected_trajectory":metadata.get("expected_trajectory").cloned().unwrap_or(json!({})),"assertions":metadata.get("assertions").cloned().unwrap_or(json!([])),"metadata":metadata});
        let errors = validate_case(&merged);
        if !errors.is_empty() {
            return Err(CompatError::value(
                422,
                json!({"case_id":example,"errors":errors}),
            ));
        }
        let value=sqlx::query_scalar("WITH changed AS (UPDATE eval_examples SET split=COALESCE($4,split),input=COALESCE($5::jsonb,input),expected_output=COALESCE($6::jsonb,expected_output),metadata=COALESCE(metadata,'{}'::jsonb)||$7::jsonb WHERE tenant_id=$1 AND dataset_id=$2::uuid AND example_id=$3::uuid RETURNING *) SELECT to_jsonb(changed) FROM changed").bind(&user.tenant_id).bind(dataset).bind(example).bind(payload.get("split").and_then(Value::as_str)).bind(payload.get("input").filter(|v|!v.is_null()).cloned()).bind(payload.get("expected_output").filter(|v|!v.is_null()).cloned()).bind(metadata_patch(payload)).fetch_optional(&self.pool).await?;
        value.ok_or_else(|| CompatError::new(404, "Example not found"))
    }
    pub async fn delete_example(
        &self,
        tenant: &str,
        dataset: &str,
        example: &str,
    ) -> Result<(), CompatError> {
        let existing = self
            .example(tenant, dataset, example)
            .await?
            .ok_or_else(|| CompatError::new(404, "Example not found"))?;
        if text(&existing["metadata"], "source_kind") == "kb_failure" {
            return Err(CompatError::new(
                409,
                "KB failure revisions are immutable and cannot be deleted",
            ));
        }
        let count=sqlx::query("DELETE FROM eval_examples WHERE tenant_id=$1 AND dataset_id=$2::uuid AND example_id=$3::uuid").bind(tenant).bind(dataset).bind(example).execute(&self.pool).await?.rows_affected();
        if count != 1 {
            Err(CompatError::new(404, "Example not found"))
        } else {
            Ok(())
        }
    }
    pub async fn import_examples(
        &self,
        user: &CompatUser,
        dataset: &str,
        examples: &[Value],
        mode: &str,
    ) -> Result<Value, CompatError> {
        uuid(dataset)?;
        if !matches!(mode, "append" | "skip_duplicates") {
            return Err(CompatError::new(422, "Invalid import mode"));
        }
        let mut errors = vec![];
        for example in examples {
            reject_reserved_kb_metadata(&example["metadata"])?;
            let findings = validate_case(example);
            if !findings.is_empty() {
                errors.push(json!({"case_id":example["case_id"],"errors":findings}));
            }
        }
        if !errors.is_empty() {
            return Err(CompatError::value(422, json!({"cases":errors})));
        }
        let mut ids: BTreeSet<String> = if mode == "skip_duplicates" {
            sqlx::query_scalar::<_,String>("SELECT DISTINCT metadata->>'case_id' FROM eval_examples WHERE tenant_id=$1 AND dataset_id=$2::uuid AND COALESCE(metadata->>'case_id','')<>''").bind(&user.tenant_id).bind(dataset).fetch_all(&self.pool).await?.into_iter().collect()
        } else {
            BTreeSet::new()
        };
        let mut imported = vec![];
        let mut skipped = 0;
        for example in examples {
            let id = text(example, "case_id").trim();
            if mode == "skip_duplicates" && !id.is_empty() && ids.contains(id) {
                skipped += 1;
                continue;
            }
            imported.push(self.create_example(user, dataset, example).await?);
            if !id.is_empty() {
                ids.insert(id.into());
            }
        }
        Ok(json!({"imported":imported.len(),"skipped":skipped,"examples":imported}))
    }
    pub async fn require_kb_access(
        &self,
        actor: Option<&KnowledgeActor>,
        dataset: &Value,
        examples: &[Value],
    ) -> Result<(), CompatError> {
        let mut datasets = BTreeSet::new();
        if let Some(id) = dataset["metadata"]["kb_dataset_id"]
            .as_str()
            .filter(|v| !v.is_empty())
        {
            datasets.insert(id.to_owned());
        }
        let mut references: HashMap<String, BTreeSet<DocumentSourceReference>> = HashMap::new();
        for example in examples {
            let metadata = &example["metadata"];
            if text(metadata, "source_kind") != "kb_failure" {
                continue;
            }
            let id = text(metadata, "kb_dataset_id");
            if id.is_empty() {
                return Err(CompatError::new(403, "KB case source is unavailable"));
            }
            datasets.insert(id.into());
            let empty = vec![];
            let sources = if metadata["kb_source_versions"].is_null() {
                &empty
            } else {
                metadata["kb_source_versions"]
                    .as_array()
                    .ok_or_else(|| CompatError::new(403, "KB case source is unavailable"))?
            };
            let observed = if metadata["kb_observed_segment_ids"].is_null() {
                &empty
            } else {
                metadata["kb_observed_segment_ids"]
                    .as_array()
                    .ok_or_else(|| CompatError::new(403, "KB case source is incomplete"))?
            };
            let mut expected_ids = sources
                .iter()
                .map(|s| text(s, "segment_id").to_owned())
                .collect::<Vec<_>>();
            let mut observed_ids = observed
                .iter()
                .map(|s| {
                    s.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| CompatError::new(403, "KB case source is incomplete"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            expected_ids.sort();
            observed_ids.sort();
            if expected_ids != observed_ids {
                return Err(CompatError::new(403, "KB case source is incomplete"));
            }
            for source in sources {
                let hash = text(source, "source_hash").to_ascii_lowercase();
                let version = source["source_version"]
                    .as_i64()
                    .filter(|v| *v > 0)
                    .ok_or_else(|| CompatError::new(403, "KB case source is unavailable"))?;
                if text(source, "kb_dataset_id") != id
                    || text(source, "document_id").is_empty()
                    || hash.len() != 64
                    || !hash.bytes().all(|v| v.is_ascii_hexdigit())
                {
                    return Err(CompatError::new(403, "KB case source is unavailable"));
                }
                references
                    .entry(id.into())
                    .or_default()
                    .insert(DocumentSourceReference {
                        document_id: text(source, "document_id").into(),
                        source_version: version,
                        source_hash: hash,
                    });
            }
        }
        if datasets.is_empty() {
            return Ok(());
        }
        let actor =
            actor.ok_or_else(|| CompatError::new(403, "Knowledge dataset is not accessible"))?;
        let knowledge = &actor.0;
        let visible = knowledge
            .visible_dataset_ids()
            .await
            .map_err(|_| CompatError::new(403, "Knowledge dataset is not accessible"))?;
        if !datasets.is_subset(&visible) {
            return Err(CompatError::new(403, "Knowledge dataset is not accessible"));
        }
        for (dataset, references) in references {
            let refs = references.iter().cloned().collect::<Vec<_>>();
            let allowed = knowledge
                .authorize_document_sources(&dataset, &refs)
                .await
                .map_err(|_| CompatError::new(403, "KB case source is unavailable"))?;
            if allowed != references {
                return Err(CompatError::new(403, "KB case source is unavailable"));
            }
        }
        Ok(())
    }
    /// Persistence cancellation in the same outbox-before-domain fencing order as Python.
    /// Returned active handles must be interrupted by the canonical runtime adapter.
    pub async fn cancel_experiment_run(
        &self,
        user: &CompatUser,
        id: &str,
    ) -> Result<Value, CompatError> {
        uuid(id)?;
        let mut tx = self.pool.begin().await?;
        let run = sqlx::query_scalar::<_, Value>(
            "SELECT to_jsonb(r) FROM eval_experiment_runs r WHERE tenant_id=$1 AND run_id=$2::uuid",
        )
        .bind(&user.tenant_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(|| CompatError::new(404, "Experiment run not found"))?;
        if text(&run, "status") == "succeeded" {
            tx.commit().await?;
            return Ok(json!({"run_id":id,"status":"succeeded","cases":[]}));
        }
        sqlx::query("UPDATE agent_trace_outbox SET status='cancelled',lease_until=NULL,updated_at=NOW() WHERE tenant_id=$1 AND payload->>'run_id'=$2 AND status IN ('queued','running')").bind(&user.tenant_id).bind(id).execute(&mut *tx).await?;
        sqlx::query("UPDATE eval_experiment_runs SET status='cancelled',finished_at=NOW(),metrics=metrics||jsonb_build_object('cancelled_by',$3::text),updated_at=NOW() WHERE tenant_id=$1 AND run_id=$2::uuid AND status IN ('queued','running','failed','cancelled')").bind(&user.tenant_id).bind(id).bind(&user.user_id).execute(&mut *tx).await?;
        let status = sqlx::query_scalar::<_, String>(
            "SELECT status FROM eval_experiment_runs WHERE tenant_id=$1 AND run_id=$2::uuid",
        )
        .bind(&user.tenant_id)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if status == "succeeded" {
            tx.commit().await?;
            return Ok(json!({"run_id":id,"status":"succeeded","cases":[]}));
        }
        sqlx::query("UPDATE eval_experiment_run_cases SET status='skipped',observed_metrics=observed_metrics||'{\"execution_outcome\":\"cancelled\"}'::jsonb,error_message='eval_run_cancelled',updated_at=NOW() WHERE tenant_id=$1 AND run_id=$2::uuid AND status IN ('queued','running')").bind(&user.tenant_id).bind(id).execute(&mut *tx).await?;
        let cases=sqlx::query_scalar::<_,Value>("SELECT jsonb_build_object('run_case_id',run_case_id,'runtime_handle',runtime_handle,'dispatch_state',dispatch_state) FROM eval_experiment_run_cases WHERE tenant_id=$1 AND run_id=$2::uuid AND dispatch_state<>'not_started'").bind(&user.tenant_id).bind(id).fetch_all(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"run_id":id,"status":"cancelled","cases":cases}))
    }
}
fn resource_table(resource: &str) -> Result<(&'static str, &'static str), CompatError> {
    match resource {
        "datasets" => Ok(("eval_datasets", "dataset_id")),
        "evaluators" => Ok(("eval_evaluators", "evaluator_id")),
        "experiments" => Ok(("eval_experiments", "experiment_id")),
        "experiment-runs" => Ok(("eval_experiment_runs", "run_id")),
        _ => Err(CompatError::new(404, "Unknown eval resource")),
    }
}

pub fn validate_case(case: &Value) -> Vec<String> {
    super::eval_behavior::validate_case(case)
}
pub(crate) fn validate_case_base(case: &Value) -> Vec<String> {
    let mut errors = vec![];
    for key in [
        "case_id",
        "input",
        "expected_output",
        "expected_trajectory",
        "assertions",
        "metadata",
    ] {
        if case.get(key).is_none() {
            errors.push(format!("missing {key}"));
        }
    }
    if text(case, "case_id").is_empty() {
        errors.push("case_id must be a non-empty string".into());
    }
    for key in [
        "input",
        "expected_output",
        "expected_trajectory",
        "metadata",
    ] {
        if case.get(key).is_some_and(|v| !v.is_object()) {
            errors.push(format!("{key} must be an object"));
        }
    }
    let trajectory = &case["expected_trajectory"];
    if trajectory.get("runtime").is_some_and(|v| !v.is_object()) {
        errors.push("expected_trajectory.runtime must be an object".into());
    }
    let strings = |value: &Value| {
        value.as_array().is_some_and(|v| {
            v.iter()
                .all(|v| v.as_str().is_some_and(|v| !v.trim().is_empty()))
        })
    };
    if trajectory
        .get("required_span_kinds")
        .is_some_and(|v| !strings(v))
    {
        errors.push("expected_trajectory.required_span_kinds must be a string list".into());
    }
    for key in ["reference", "rubric"] {
        if case["expected_output"]
            .get(key)
            .is_some_and(|v| v.as_str().is_none_or(|v| v.trim().is_empty()))
        {
            errors.push(format!("expected_output.{key} must be a non-empty string"));
        }
    }
    for key in ["contains", "not_contains"] {
        if let Some(value) = case["expected_output"].get(key) {
            if !(value.as_str().is_some_and(|v| !v.trim().is_empty())
                || strings(value) && value.as_array().is_some_and(|v| !v.is_empty()))
            {
                errors.push(format!(
                    "expected_output.{key} must be a non-empty string or string list"
                ));
            }
        }
    }
    if let Some(tools) = trajectory.get("tools") {
        if let Some(tools) = tools.as_array() {
            for (index, tool) in tools.iter().enumerate() {
                let prefix = format!("expected_trajectory.tools[{}]", index + 1);
                let Some(map) = tool.as_object() else {
                    errors.push(format!("{prefix} must be an object"));
                    continue;
                };
                let mut unknown = map
                    .keys()
                    .filter(|key| {
                        !matches!(
                            key.as_str(),
                            "name"
                                | "required"
                                | "forbidden"
                                | "arguments_subset"
                                | "order"
                                | "max_calls"
                                | "status"
                        )
                    })
                    .cloned()
                    .collect::<Vec<_>>();
                unknown.sort();
                if !unknown.is_empty() {
                    errors.push(format!(
                        "{prefix} has unsupported fields {}",
                        unknown.join(", ")
                    ));
                }
                if text(tool, "name").trim().is_empty() {
                    errors.push(format!("{prefix}.name must be a non-empty string"));
                }
                for key in ["required", "forbidden"] {
                    if map.get(key).is_some_and(|v| !v.is_boolean()) {
                        errors.push(format!("{prefix}.{key} must be boolean"));
                    }
                }
                if tool["required"] == json!(true) && tool["forbidden"] == json!(true) {
                    errors.push(format!("{prefix} cannot be both required and forbidden"));
                }
                if map.get("arguments_subset").is_some_and(|v| !v.is_object()) {
                    errors.push(format!("{prefix}.arguments_subset must be an object"));
                }
                for (key, min) in [("order", 1), ("max_calls", 0)] {
                    if map
                        .get(key)
                        .is_some_and(|v| v.as_u64().is_none_or(|n| n < min))
                    {
                        errors.push(format!(
                            "{prefix}.{key} must be a {} integer",
                            if min == 1 { "positive" } else { "non-negative" }
                        ));
                    }
                }
            }
        } else {
            errors.push("expected_trajectory.tools must be a list".into());
        }
    }
    if let Some(assertions) = case.get("assertions") {
        if let Some(assertions) = assertions.as_array() {
            for (index, assertion) in assertions.iter().enumerate() {
                let prefix = format!("assertions[{}]", index + 1);
                if !assertion.is_object() {
                    errors.push(format!("{prefix} must be an object"));
                    continue;
                }
                let kind = text(assertion, "type");
                if !matches!(
                    kind,
                    "output_equals"
                        | "output_contains"
                        | "output_not_contains"
                        | "required_span_kind"
                        | "no_sensitive_output"
                        | "latency_ms_lt"
                        | "total_tokens_lt"
                        | "cost_cents_lt"
                        | "tool_called"
                        | "tool_not_called"
                        | "failure_mode_absent"
                ) {
                    errors.push(format!("{prefix} has unsupported type {kind:?}"));
                } else if matches!(kind, "latency_ms_lt" | "total_tokens_lt" | "cost_cents_lt") {
                    if assertion["value"]
                        .as_f64()
                        .is_none_or(|v| !v.is_finite() || v <= 0.0)
                    {
                        errors.push(format!("{prefix}.value must be a positive finite number"));
                    }
                } else if kind != "no_sensitive_output"
                    && text(assertion, "value").trim().is_empty()
                {
                    errors.push(format!("{prefix}.value must be a non-empty string"));
                }
            }
        } else {
            errors.push("assertions must be a list".into());
        }
    }
    if trajectory.get("stateful").is_some_and(|v| !v.is_object()) {
        errors.push("expected_trajectory.stateful must be an object".into());
    }
    errors
}

#[derive(Clone)]
struct EvalHttpState {
    auth: Arc<AuthService>,
    service: Arc<EvalService>,
    options: super::eval_http::EvalHttpOptions,
}
pub fn router(auth: Arc<AuthService>, service: Arc<EvalService>) -> Router {
    router_with_options(auth, service, super::eval_http::EvalHttpOptions::default())
}
pub fn router_with_options(
    auth: Arc<AuthService>,
    service: Arc<EvalService>,
    options: super::eval_http::EvalHttpOptions,
) -> Router {
    Router::new()
        .route("/api/v1/eval/{*path}", any(http))
        .with_state(EvalHttpState {
            auth,
            service,
            options,
        })
}
async fn http(State(state): State<EvalHttpState>, request: Request) -> Response {
    let result=async{
        let service=&state.service;
        let(mut user,actor)=if let Some(admission)=&state.options.admission{let auth=admission.request(request.headers(),request.uri().path());let principal=auth.resolve().await.map_err(|e|CompatError::new(e.status(),"Authentication required"))?;let actor=if let Some(jwt)=principal.jwt_principal(){service.knowledge.as_ref().and_then(|k|k.from_principal(jwt).ok())}else{principal.api_key().and_then(|key|service.knowledge.as_ref().and_then(|k|k.from_api_key(key).ok()))};(CompatUser::from_admitted(principal,request.headers()),actor)}else{let token=match select_credential(request.headers(),&HeaderName::from_static("x-api-key")){Ok(SelectedCredential::Jwt(token))=>token,_=>return Err(CompatError::new(401,"Authentication required"))};let principal=state.auth.authenticate(token).await.map_err(|e|CompatError::new(e.status(),"Authentication failed"))?;let actor=service.knowledge.as_ref().and_then(|k|k.from_principal(&principal).ok());(CompatUser{user_id:principal.account.user_id,tenant_id:principal.account.tenant_id,tier:principal.authority.tier,authenticated:true,roles:principal.authority.roles,permissions:principal.authority.permissions,..Default::default()},actor)};user.apply_request_context(request.extensions());
        let method=request.method().clone();let write=method!=Method::GET;let can=|permission:&str|if let Some(rbac)=&state.options.rbac{let capability=if permission=="console:eval:run"{ai_platform_auth::rbac::Capability::GatewayEvalRun}else{ai_platform_auth::rbac::Capability::GatewayEvalTraceRead};ai_platform_auth::rbac::require_capability(rbac,&user.roles,&user.permissions,capability).is_ok()}else{user.roles.iter().any(|role|role=="admin")||user.permissions.iter().any(|p|p=="*"||p==permission)};
        if user.tenant_id.is_empty()||(!user.operator()&&user.user_id.is_empty())||!can("console:eval:view")||(write&&!can("console:eval:run")){return Err(CompatError::new(403,"Permission denied"));}
        let path=request.uri().path().trim_start_matches("/api/v1/eval/").to_owned();let parts=path.split('/').collect::<Vec<_>>();let url=reqwest::Url::parse(&format!("http://gateway{}",request.uri())).map_err(|_|CompatError::new(400,"Invalid URL"))?;let params=url.query_pairs().into_owned().collect::<HashMap<_,_>>();let limit=params.get("limit").map(String::as_str).unwrap_or(if parts.len()>2{"200"}else{"50"}).parse::<i64>().ok().filter(|n|*n>=1&&*n<=if parts.len()>2{500}else{200}).ok_or_else(||CompatError::new(422,"Invalid limit"))?;let offset=params.get("offset").map(String::as_str).unwrap_or("0").parse::<i64>().ok().filter(|n|*n>=0).ok_or_else(||CompatError::new(422,"Invalid offset"))?;
        let idempotency_key=super::common::header(request.headers(),"idempotency-key").to_owned();let bytes=to_bytes(request.into_body(),16*1024*1024).await.map_err(|_|CompatError::new(413,"Request too large"))?;let body=if bytes.is_empty(){json!({})}else{serde_json::from_slice::<Value>(&bytes).map_err(|_|CompatError::new(422,"Invalid JSON"))?};
        if let Some((status,value))=super::eval_trace_http::handle(&state.options,&user,method.as_str(),&parts,&params,&body).await?{return Ok((StatusCode::from_u16(status).unwrap_or(StatusCode::OK),Json(value)).into_response());}
        let service=&state.service;let value=match(method.as_str(),parts.as_slice()){
            ("POST",["trace-feedback:preview"])=>service.preview_trace_feedback(&user,&body,&state.options).await?,
            ("GET",["knowledge","summary"])=>{let days=params.get("days").map(String::as_str).unwrap_or("7").parse::<i32>().ok().filter(|n|(1..=90).contains(n)).ok_or_else(||CompatError::new(422,"Invalid days"))?;service.kb_ragas_summary(&user.tenant_id,days,params.get("dataset_id").map(String::as_str)).await?},
            ("POST",["knowledge",dataset,"batch-score"])=>return Ok((StatusCode::ACCEPTED,Json(service.kb_ragas_batch(&user,dataset,&body,&state.options).await?)).into_response()),
            ("POST",["knowledge","score-retrieval"])=>super::eval_knowledge::score_retrieval(&user,&body,&state.options).await?,
            ("GET",["experiment-runs:compare"])=>service.compare_experiment_runs(&user.tenant_id,params.get("baseline_run_id").ok_or_else(||CompatError::new(422,"baseline_run_id is required"))?,params.get("candidate_run_id").ok_or_else(||CompatError::new(422,"candidate_run_id is required"))?).await?.ok_or_else(||CompatError::new(404,"Experiment run not found"))?,
            ("POST",["experiments",command]) if command.ends_with(":promote-baseline")=>service.promote_baseline(&user,command.trim_end_matches(":promote-baseline"),text(&body,"run_id")).await?,
            ("POST",["gates:dry-run"])=>service.dry_run_gate(&user,&body).await?,
            ("GET",["experiment-runs",id,"results"])=>service.run_results(&user.tenant_id,id,limit,offset).await?,
            ("POST",["experiments",command]) if command.ends_with(":run")=>{let id=command.trim_end_matches(":run");return Ok((StatusCode::ACCEPTED,Json(service.run_experiment(&user,id,&body,&state.options).await?)).into_response());},
            ("POST",["evaluators",command]) if command.ends_with(":run-async")=>{let mut payload=body.clone();payload["evaluator_id"]=json!(command.trim_end_matches(":run-async"));return Ok((StatusCode::ACCEPTED,Json(service.enqueue_evaluator_run(&user,&payload).await?)).into_response());},
            ("POST",["experiment-runs",command]) if command.ends_with(":cancel")=>service.cancel_and_interrupt(&user,command.trim_end_matches(":cancel"),&state.options).await?,
            ("POST",["experiment-runs",command]) if command.ends_with(":retry-failed")=>{if body["acknowledge_replay"]!=true{return Err(CompatError::new(422,"Confirm that retry runs a new model attempt and may repeat tool effects and costs"));}let ids=body["case_ids"].as_array().filter(|v|(1..=100).contains(&v.len())).ok_or_else(||CompatError::new(422,"Invalid case_ids"))?.iter().map(|v|v.as_str().map(str::to_owned).ok_or_else(||CompatError::new(422,"Invalid case_ids"))).collect::<Result<Vec<_>,_>>()?;let job=service.retry_failed_experiment_cases(&user,command.trim_end_matches(":retry-failed"),&ids,&idempotency_key).await?.ok_or_else(||CompatError::new(404,"Experiment run not found"))?;return Ok((StatusCode::ACCEPTED,Json(json!({"jobs":[job]}))).into_response());},
            ("GET",["datasets",dataset,"examples:export"])=>{let info=service.get(&user.tenant_id,"datasets",dataset).await?.ok_or_else(||CompatError::new(404,"Dataset not found"))?;let rows=service.list_example_manifest(&user.tenant_id,dataset).await?.into_iter().filter(|v|params.get("split").filter(|s|!s.is_empty()).is_none_or(|split|v["split"]==split.as_str())).collect::<Vec<_>>();service.require_kb_access(actor.as_ref(),&info,&rows).await?;let examples=rows.iter().map(|row|{let mut metadata=object(&row["metadata"]);for key in ["case_id","expected_trajectory","assertions"]{metadata.remove(key);}json!({"case_id":row["metadata"].get("case_id").filter(|v|!v.is_null()&&v.as_str()!=Some("")).unwrap_or(&row["example_id"]),"split":row["split"].as_str().filter(|s|!s.is_empty()).unwrap_or("regression"),"input":object(&row["input"]),"expected_output":object(&row["expected_output"]),"expected_trajectory":object(&row["metadata"]["expected_trajectory"]),"assertions":row["metadata"].get("assertions").filter(|v|v.is_array()).cloned().unwrap_or(json!([])),"metadata":metadata,"source_trace_id":row["source_trace_id"],"source_span_id":row["source_span_id"]})}).collect::<Vec<_>>();json!({"dataset":info,"schema_version":"eval-golden-v1","examples":examples})},
            ("POST",["datasets",dataset,"examples:from-trace"])=>{let info=service.get(&user.tenant_id,"datasets",dataset).await?.ok_or_else(||CompatError::new(404,"Dataset not found"))?;service.require_kb_access(actor.as_ref(),&info,&[]).await?;return Ok((StatusCode::CREATED,Json(service.example_from_trace(&user,dataset,&body,&state.options).await?)).into_response());},
            ("GET",["datasets",dataset,"kb-failures",case])=>{let example=service.latest_kb_failure(&user.tenant_id,dataset,case).await?.ok_or_else(||CompatError::new(404,"KB failure case not found"))?;let info=service.get(&user.tenant_id,"datasets",dataset).await?.ok_or_else(||CompatError::new(404,"Dataset not found"))?;service.require_kb_access(actor.as_ref(),&info,&[example.clone()]).await?;example},
            ("POST",["datasets",dataset,"kb-failures:save"])=>service.save_kb_failure(&user,actor.as_ref().ok_or_else(||CompatError::new(403,"Knowledge dataset is not accessible"))?,dataset,&body,&state.options).await?,
            ("GET",["datasets"])=>{let manifest=service.dataset_manifest(&user.tenant_id).await?;let linked=manifest.iter().any(|d|!text(&d["metadata"],"kb_dataset_id").is_empty());let visible=if linked{actor.as_ref().ok_or_else(||CompatError::new(403,"Knowledge dataset is not accessible"))?.0.visible_dataset_ids().await.map_err(|_|CompatError::new(403,"Knowledge dataset is not accessible"))?}else{BTreeSet::new()};let visible=manifest.into_iter().filter(|d|text(&d["metadata"],"kb_dataset_id").is_empty()||visible.contains(text(&d["metadata"],"kb_dataset_id"))).collect::<Vec<_>>();json!({"datasets":visible.iter().skip(offset as usize).take(limit as usize).collect::<Vec<_>>(),"total":visible.len(),"limit":limit,"offset":offset})},
            ("GET",[resource])=>service.list(&user.tenant_id,resource,limit,offset).await?,
            ("POST",[resource])=>{if *resource=="datasets"{service.require_kb_access(actor.as_ref(),&body,&[]).await?;}let value=service.create(&user,resource,&body).await?;return Ok((StatusCode::CREATED,Json(value)).into_response());},
            ("GET",[resource,id])=>{let value=service.get(&user.tenant_id,resource,id).await?.ok_or_else(||CompatError::new(404,"Eval resource not found"))?;if *resource=="datasets"{service.require_kb_access(actor.as_ref(),&value,&[]).await?;}value},
            ("GET",["datasets",dataset,"examples"])=>{let info=service.get(&user.tenant_id,"datasets",dataset).await?.ok_or_else(||CompatError::new(404,"Dataset not found"))?;let value=service.examples(&user.tenant_id,dataset,params.get("split").map(String::as_str),limit,offset).await?;service.require_kb_access(actor.as_ref(),&info,value["examples"].as_array().map(Vec::as_slice).unwrap_or(&[])).await?;value},
            ("PATCH"|"DELETE",["datasets",dataset,"examples",example])=>{let info=service.get(&user.tenant_id,"datasets",dataset).await?.ok_or_else(||CompatError::new(404,"Example not found"))?;let current=service.example(&user.tenant_id,dataset,example).await?.ok_or_else(||CompatError::new(404,"Example not found"))?;service.require_kb_access(actor.as_ref(),&info,&[current]).await?;if method==Method::DELETE{service.delete_example(&user.tenant_id,dataset,example).await?;return Ok(StatusCode::NO_CONTENT.into_response());}service.update_example(&user,dataset,example,&body).await?},
            ("POST",["datasets",dataset,"examples:import"])=>{let info=service.get(&user.tenant_id,"datasets",dataset).await?.ok_or_else(||CompatError::new(404,"Dataset not found"))?;service.require_kb_access(actor.as_ref(),&info,&[]).await?;let examples=body["examples"].as_array().ok_or_else(||CompatError::new(422,"examples must be an array"))?;let mode=if text(&body,"mode").is_empty(){"skip_duplicates"}else{text(&body,"mode")};let result=service.import_examples(&user,dataset,examples,mode).await?;return Ok((StatusCode::CREATED,Json(result)).into_response());},
            _=>return Err(CompatError::new(404,"Eval route not found")),
        };Ok::<_,CompatError>(Json(value).into_response())
    }.await;
    result.unwrap_or_else(IntoResponse::into_response)
}

pub fn normalize_ragas_results(payload: &Value) -> Result<Value, CompatError> {
    let invalid = || CompatError::new(502, "knowledge-service RAGAS eval returned invalid results");
    let raw = payload["results"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(invalid)?;
    let mut seen = BTreeSet::new();
    let mut results = vec![];
    for item in raw {
        let metric = text(item, "metric").trim();
        let score = item["score"]
            .as_f64()
            .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
            .ok_or_else(invalid)?;
        let label = text(item, "label").trim();
        let failure_kind = item.get("failure_kind").filter(|v| !v.is_null());
        if metric.is_empty()
            || !seen.insert(metric.to_owned())
            || !matches!(label, "pass" | "fail" | "review")
            || (label == "review"
                && !failure_kind
                    .and_then(Value::as_str)
                    .is_some_and(|v| matches!(v.trim(), "semantic_review" | "infrastructure")))
            || (label != "review" && failure_kind.is_some())
        {
            return Err(invalid());
        }
        results.push(json!({"metric":metric,"score":score,"explanation":text(item,"explanation"),"label":label,"judge_model":payload["judge_model"],"failure_kind":failure_kind}));
    }
    Ok(json!({"judge_model":text(payload,"judge_model"),"results":results}))
}
