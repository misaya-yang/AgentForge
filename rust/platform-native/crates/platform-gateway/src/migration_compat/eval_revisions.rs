//! Immutable KB case revisions and idempotent authorized trace imports.
use super::{
    common::{CompatError, CompatUser, object, text},
    eval::{EvalService, reject_reserved_kb_metadata},
    eval_http::EvalHttpOptions,
};
use crate::knowledge::KnowledgeActor;
use serde_json::{Value, json};
fn revision(v: &Value) -> i64 {
    v["metadata"]["case_revision"]
        .as_i64()
        .filter(|v| *v > 0)
        .unwrap_or(1)
}
fn latest(rows: Vec<Value>) -> Option<Value> {
    rows.into_iter().max_by(|a, b| {
        revision(a)
            .cmp(&revision(b))
            .then(text(a, "created_at").cmp(text(b, "created_at")))
            .then(text(a, "example_id").cmp(text(b, "example_id")))
    })
}
fn conflict(current: i64) -> CompatError {
    CompatError::value(
        409,
        json!({"error":"case_revision_conflict","current_revision":current}),
    )
}
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
const CASE_ROWS: &str = "SELECT to_jsonb(e) FROM eval_examples e WHERE tenant_id=$1 AND dataset_id=$2::text::uuid AND metadata->>'source_kind'='kb_failure' AND metadata->>'case_id'=$3 ORDER BY created_at DESC,example_id DESC";
impl EvalService {
    pub async fn latest_kb_failure(
        &self,
        tenant: &str,
        dataset: &str,
        case: &str,
    ) -> Result<Option<Value>, CompatError> {
        Ok(latest(
            sqlx::query_scalar(CASE_ROWS)
                .bind(tenant)
                .bind(dataset)
                .bind(case)
                .fetch_all(&self.pool)
                .await?,
        ))
    }
    pub async fn review_kb_failure(
        &self,
        user: &CompatUser,
        dataset: &str,
        example: &str,
        patch: &Value,
    ) -> Result<Value, CompatError> {
        if object(patch)
            .keys()
            .any(|key| !matches!(key.as_str(), "review_status" | "split" | "metadata"))
            || object(&patch["metadata"])
                .keys()
                .any(|key| key != "reviewed_from")
        {
            return Err(CompatError::new(
                409,
                "KB failure content is immutable; save a new revision",
            ));
        }
        let status = text(patch, "review_status");
        if !matches!(status, "approved" | "rejected" | "needs_fix" | "pending") {
            return Err(CompatError::new(422, "Review status is required"));
        }
        if !patch["split"].is_null() && patch["split"] != "review" {
            return Err(CompatError::new(422, "Review split does not match status"));
        }
        let mut tx = self.pool.begin().await?;
        let target=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(e) FROM eval_examples e WHERE tenant_id=$1 AND dataset_id=$2::text::uuid AND example_id=$3::text::uuid").bind(&user.tenant_id).bind(dataset).bind(example).fetch_optional(&mut *tx).await?.ok_or_else(||CompatError::new(404,"Example not found"))?;
        let case = text(&target["metadata"], "case_id");
        if case.is_empty() || target["metadata"]["source_kind"] != "kb_failure" {
            return Err(CompatError::new(409, "Not a KB failure case"));
        }
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
            .bind(format!(
                "eval-kb-failure:{}:{dataset}:{case}",
                user.tenant_id
            ))
            .execute(&mut *tx)
            .await?;
        let current = latest(
            sqlx::query_scalar(CASE_ROWS)
                .bind(&user.tenant_id)
                .bind(dataset)
                .bind(case)
                .fetch_all(&mut *tx)
                .await?,
        )
        .ok_or_else(|| conflict(0))?;
        if current["example_id"] != example {
            return Err(conflict(revision(&current)));
        }
        if status == "approved" && text(&current, "source_trace_id").is_empty() {
            return Err(CompatError::new(
                409,
                "A confirmed source trace is required before approval",
            ));
        }
        if status == "approved"
            && text(&current["expected_output"], "answer")
                .trim()
                .is_empty()
        {
            return Err(CompatError::new(
                409,
                "An expected answer is required before approval",
            ));
        }
        let reviewed = text(&patch["metadata"], "reviewed_from");
        let value=sqlx::query_scalar::<_,Value>("WITH changed AS (UPDATE eval_examples SET split='review',metadata=metadata||$4::jsonb WHERE tenant_id=$1 AND dataset_id=$2::text::uuid AND example_id=$3::text::uuid RETURNING *) SELECT to_jsonb(changed) FROM changed").bind(&user.tenant_id).bind(dataset).bind(example).bind(json!({"review_status":status,"reviewed_from":if reviewed.is_empty(){"eval_console"}else{reviewed}})).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(value)
    }
    pub async fn save_kb_failure(
        &self,
        user: &CompatUser,
        actor: &KnowledgeActor,
        dataset: &str,
        payload: &Value,
        options: &EvalHttpOptions,
    ) -> Result<Value, CompatError> {
        for key in ["case_id", "kb_dataset_id", "query", "expected_answer"] {
            if payload[key].as_str().is_none_or(|v| v.is_empty())
                || key == "case_id" && text(payload, key).chars().count() > 160
            {
                return Err(CompatError::new(422, format!("Invalid {key}")));
            }
        }
        if !matches!(text(payload, "source"), "kb-hit-test" | "kb-qa")
            || payload["expected_revision"].as_u64().is_none()
        {
            return Err(CompatError::new(422, "Invalid KB failure revision"));
        }
        let info = self
            .get(&user.tenant_id, "datasets", dataset)
            .await?
            .filter(|v| v["metadata"]["kb_dataset_id"] == payload["kb_dataset_id"])
            .ok_or_else(|| CompatError::new(404, "Linked Eval dataset not found"))?;
        let sources = payload.get("source_versions").cloned().unwrap_or(json!([]));
        let observed = payload
            .get("observed_segment_ids")
            .cloned()
            .unwrap_or(json!([]));
        if !sources.is_array()
            || !observed.is_array()
            || arr(&observed).iter().any(|v| !v.is_string())
        {
            return Err(CompatError::new(422, "Invalid source versions"));
        }
        let mut normalized = vec![];
        for source in arr(&sources) {
            if source["kb_dataset_id"] != payload["kb_dataset_id"] {
                return Err(CompatError::new(
                    422,
                    "Source dataset does not match KB case",
                ));
            }
            let hash = text(source, "source_hash");
            if text(source, "document_id").is_empty()
                || text(source, "segment_id").is_empty()
                || source["source_version"].as_u64().is_none_or(|v| v == 0)
                || hash.len() != 64
                || !hash.bytes().all(|b| b.is_ascii_hexdigit())
            {
                return Err(CompatError::new(422, "Invalid source version"));
            }
            let mut value = source.clone();
            value["source_hash"] = json!(hash.to_lowercase());
            normalized.push(value);
        }
        let (mut a, mut b) = (
            arr(&observed)
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>(),
            normalized
                .iter()
                .map(|v| text(v, "segment_id"))
                .collect::<Vec<_>>(),
        );
        a.sort();
        b.sort();
        if a != b {
            return Err(CompatError::new(
                422,
                "Every observed segment needs a stable source version",
            ));
        }
        self.require_kb_access(Some(actor), &info, &[]).await?;
        let synthetic = json!({"metadata":{"source_kind":"kb_failure","kb_dataset_id":payload["kb_dataset_id"],"kb_source_versions":normalized,"kb_observed_segment_ids":observed}});
        self.require_kb_access(Some(actor), &info, &[synthetic])
            .await
            .map_err(|_| {
                CompatError::new(422, "Source version is unavailable or not authorized")
            })?;
        let source = payload["source_trace_id"]
            .as_str()
            .filter(|s| !s.is_empty());
        if let Some(source) = source {
            if !text(payload, "kb_trace_id").is_empty() && payload["kb_trace_id"] != source {
                return Err(CompatError::new(422, "Source trace IDs do not match"));
            }
            let repo = options
                .traces
                .as_ref()
                .ok_or_else(|| CompatError::new(503, "Database not available"))?;
            if repo
                .get_trace_detail(
                    &user.tenant_id,
                    source,
                    if user.operator() {
                        None
                    } else {
                        Some(&user.user_id)
                    },
                    "rag",
                )
                .await
                .map_err(|e| CompatError::new(e.status, e.code))?
                .is_none()
            {
                return Err(CompatError::new(422, "Source trace is not accessible"));
            }
        }
        let case = text(payload, "case_id");
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text,0))")
            .bind(format!(
                "eval-kb-failure:{}:{dataset}:{case}",
                user.tenant_id
            ))
            .execute(&mut *tx)
            .await?;
        let exists=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM eval_datasets WHERE tenant_id=$1 AND dataset_id=$2::text::uuid)").bind(&user.tenant_id).bind(dataset).fetch_one(&mut *tx).await?;
        if !exists {
            return Err(CompatError::new(404, "Eval dataset not found"));
        }
        let previous = latest(
            sqlx::query_scalar(CASE_ROWS)
                .bind(&user.tenant_id)
                .bind(dataset)
                .bind(case)
                .fetch_all(&mut *tx)
                .await?,
        );
        if previous
            .as_ref()
            .is_some_and(|v| v["metadata"]["kb_dataset_id"] != payload["kb_dataset_id"])
        {
            return Err(CompatError::new(
                404,
                "KB case belongs to a different knowledge dataset",
            ));
        }
        let mut metadata = json!({"source":payload["source"],"source_kind":"kb_failure","kb_dataset_id":payload["kb_dataset_id"],"kb_trace_id":payload["kb_trace_id"],"kb_query_fingerprint":payload["query_fingerprint"],"kb_observed_segment_ids":observed,"kb_source_versions":normalized,"kb_source_versions_verified":!arr(&sources).is_empty(),"kb_observed_answer":payload["observed_answer"],"kb_failure_reason":payload["failure_reason"].as_str().map(str::trim).filter(|s|!s.is_empty())});
        let input = json!({"query":text(payload,"query").trim()});
        let output = json!({"answer":text(payload,"expected_answer").trim()});
        if let Some(previous) = &previous {
            if previous["input"] == input
                && previous["expected_output"] == output
                && previous["source_trace_id"] == json!(source)
                && object(&metadata)
                    .iter()
                    .all(|(key, value)| previous["metadata"][key] == *value)
            {
                tx.commit().await?;
                return Ok(
                    json!({"example":previous,"created":false,"revision":revision(previous)}),
                );
            }
        }
        let current = previous.as_ref().map(revision).unwrap_or(0);
        if payload["expected_revision"].as_i64() != Some(current) {
            return Err(conflict(current));
        }
        metadata["case_id"] = json!(case);
        metadata["case_revision"] = json!(current + 1);
        metadata["supersedes_example_id"] = previous
            .as_ref()
            .map(|v| v["example_id"].clone())
            .unwrap_or(Value::Null);
        metadata["review_status"] = json!("pending");
        metadata["behavior_confirmed"] = json!(false);
        let value=sqlx::query_scalar::<_,Value>("WITH saved AS (INSERT INTO eval_examples(dataset_id,tenant_id,split,input,expected_output,metadata,source_trace_id,created_by) VALUES($1::text::uuid,$2,'review',$3,$4,$5,$6::text::uuid,$7) RETURNING *) SELECT to_jsonb(saved) FROM saved").bind(dataset).bind(&user.tenant_id).bind(input).bind(output).bind(metadata).bind(source).bind(&user.user_id).fetch_one(&mut *tx).await?;
        tx.commit().await?;
        Ok(json!({"example":value,"created":true,"revision":current+1}))
    }
    pub async fn example_from_trace(
        &self,
        user: &CompatUser,
        dataset: &str,
        payload: &Value,
        options: &EvalHttpOptions,
    ) -> Result<Value, CompatError> {
        reject_reserved_kb_metadata(&payload["metadata"])?;
        let family = payload["trace_family"].as_str().unwrap_or("assistant");
        if !matches!(family, "assistant" | "langgraph_proxy" | "rag") {
            return Err(CompatError::new(400, "Unsupported trace_family."));
        }
        let source = uuid::Uuid::parse_str(text(payload, "source_trace_id"))
            .map_err(|_| CompatError::new(422, "Invalid source_trace_id"))?
            .to_string();
        let dataset_id = uuid::Uuid::parse_str(dataset)
            .map_err(|_| CompatError::new(422, "Invalid dataset_id"))?
            .to_string();
        let span = payload["source_span_id"]
            .as_str()
            .map(|s| {
                uuid::Uuid::parse_str(s)
                    .map(|v| v.to_string())
                    .map_err(|_| CompatError::new(422, "Invalid source_span_id"))
            })
            .transpose()?;
        let repo = options
            .traces
            .as_ref()
            .ok_or_else(|| CompatError::new(503, "Database not available"))?;
        let detail = repo
            .get_trace_detail(
                &user.tenant_id,
                &source,
                if user.operator() {
                    None
                } else {
                    Some(&user.user_id)
                },
                family,
            )
            .await
            .map_err(|e| CompatError::new(e.status, e.code))?
            .ok_or_else(|| CompatError::new(404, "Trace not found"))?;
        if span.as_ref().is_some_and(|id| {
            !arr(&detail["spans"])
                .iter()
                .any(|s| text(s, "span_id") == id)
        }) {
            return Err(CompatError::new(404, "Trace not found"));
        }
        if let Some(value)=sqlx::query_scalar::<_,Value>("SELECT to_jsonb(e) FROM eval_examples e WHERE tenant_id=$1 AND dataset_id=$2::text::uuid AND source_trace_id=$3::text::uuid AND source_span_id IS NOT DISTINCT FROM $4::text::uuid ORDER BY created_at,example_id LIMIT 1").bind(&user.tenant_id).bind(dataset).bind(&source).bind(span.as_deref()).fetch_optional(&self.pool).await?{return Ok(value);}
        let trace = &detail["trace"];
        let input = json!({"message":text(trace,"input_preview"),"input_preview":text(trace,"input_preview"),"thread_id":trace.get("thread_id").filter(|v|!v.is_null()&&v.as_str()!=Some("")).unwrap_or(&trace["session_id"]),"run_id":trace["run_id"],"request_id":trace["request_id"],"metadata":object(&trace["metadata"])});
        let output = payload
            .get("expected_output")
            .filter(|v| v.is_object() && !object(v).is_empty())
            .cloned()
            .unwrap_or(json!({"output_preview":text(trace,"output_preview")}));
        let mut metadata = object(&payload["metadata"]);
        let span_kinds = arr(&detail["spans"])
            .iter()
            .filter_map(|v| v["span_kind"].as_str().filter(|s| !s.is_empty()))
            .collect::<std::collections::BTreeSet<_>>();
        metadata.entry("expected_trajectory").or_insert(json!({"required_span_kinds":span_kinds,"runtime":{"expected_exit_reason":trace["metadata"]["runtime_trajectory"].get("exit_reason").filter(|v|!v.is_null()&&v.as_str()!=Some("")).unwrap_or(&trace["status"])}}));
        metadata
            .entry("assertions")
            .or_insert(json!([{"type":"no_sensitive_output"}]));
        metadata.insert("behavior_confirmed".into(), json!(false));
        metadata.insert("review_status".into(), json!("pending"));
        let origin = crate::migration_content::python_json(
            &json!([user.tenant_id, dataset_id, family, source, span]),
            false,
            false,
        );
        let id = uuid::Uuid::new_v5(
            &uuid::Uuid::NAMESPACE_URL,
            format!("ai-gateway/trace-example/{origin}").as_bytes(),
        )
        .to_string();
        let row=sqlx::query_scalar::<_,Value>("WITH saved AS (INSERT INTO eval_examples(dataset_id,tenant_id,split,input,expected_output,metadata,source_trace_id,source_span_id,created_by,example_id) VALUES($1::text::uuid,$2,$3,$4,$5,$6,$7::text::uuid,$8::text::uuid,$9,$10::text::uuid) ON CONFLICT(example_id) DO NOTHING RETURNING *) SELECT to_jsonb(saved) FROM saved").bind(dataset).bind(&user.tenant_id).bind(payload["split"].as_str().filter(|s|!s.is_empty()).unwrap_or("regression")).bind(input).bind(output).bind(json!(metadata)).bind(&source).bind(span.as_deref()).bind(&user.user_id).bind(&id).fetch_optional(&self.pool).await?;
        if let Some(row) = row {
            return Ok(row);
        }
        sqlx::query_scalar("SELECT to_jsonb(e) FROM eval_examples e WHERE example_id=$1::text::uuid AND tenant_id=$2 AND dataset_id=$3::text::uuid AND source_trace_id=$4::text::uuid AND source_span_id IS NOT DISTINCT FROM $5::text::uuid").bind(&id).bind(&user.tenant_id).bind(dataset).bind(source).bind(span).fetch_optional(&self.pool).await?.ok_or_else(||CompatError::new(404,"Trace not found"))
    }
}
