//! Eval trace HTTP projection; authority and persistence remain in shared native services.
use super::{
    common::{CompatError, CompatUser, object, text},
    eval_export,
    eval_http::EvalHttpOptions,
};
use crate::migration_trace::{TraceError, TraceFilter};
use serde_json::{Value, json};
use std::collections::HashMap;
fn error(e: TraceError) -> CompatError {
    CompatError::new(e.status, e.code)
}
fn pick(source: &Value, fields: &str, defaults: Value) -> Value {
    let mut target = object(&defaults);
    for key in fields.split_whitespace() {
        target.insert(
            key.into(),
            source
                .get(key)
                .cloned()
                .unwrap_or_else(|| target.get(key).cloned().unwrap_or(Value::Null)),
        );
    }
    json!(target)
}
pub fn trace_summary(source: &Value) -> Value {
    pick(
        source,
        "trace_id trace_family workflow_kind tenant_id user_id thread_id session_id run_id request_id agent_id agent_version_id agent_draft_revision publication_id channel runtime_fingerprint agent_spec_hash model_id provider status started_at ended_at first_token_latency_ms total_latency_ms input_tokens output_tokens total_tokens total_cost_cents input_preview output_preview redaction_state metadata metrics privacy source_adapter scores_count created_at updated_at",
        json!({"first_token_latency_ms":0,"total_latency_ms":0,"input_tokens":0,"output_tokens":0,"total_tokens":0,"total_cost_cents":0,"input_preview":"","output_preview":"","redaction_state":{},"metadata":{},"metrics":{},"privacy":{},"scores_count":0}),
    )
}
fn score(source: &Value) -> Value {
    pick(
        source,
        "score_id trace_id span_id score_name score_type numeric_value boolean_value categorical_value text_value label explanation scorer_type evaluator_version target_type target_id evaluator_id evaluator_name score_source confidence created_by metadata created_at",
        json!({"score_type":"numeric","scorer_type":"human","target_type":"trace","score_source":"human","metadata":{}}),
    )
}
pub fn detail(source: &Value) -> Value {
    json!({"trace":trace_summary(&source["trace"]),"spans":arr(&source["spans"]).iter().map(|span|pick(span,"span_id trace_id parent_span_id span_kind name status sequence_no started_at ended_at duration_ms input_preview output_preview attributes error_type error_message created_at",json!({"sequence_no":0,"duration_ms":0,"input_preview":"","output_preview":"","attributes":{}}))).collect::<Vec<_>>(),"events":arr(&source["events"]).iter().map(|event|pick(event,"event_id trace_id span_id event_type sequence_no occurred_at payload payload_size_bytes redacted created_at",json!({"payload":{},"payload_size_bytes":0,"redacted":true}))).collect::<Vec<_>>(),"scores":arr(&source["scores"]).iter().map(score).collect::<Vec<_>>()})
}
fn arr(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
fn family(value: &str) -> Result<(), CompatError> {
    if matches!(value, "assistant" | "langgraph_proxy" | "rag") {
        Ok(())
    } else {
        Err(CompatError::new(400, "Unsupported trace_family."))
    }
}
fn integer(
    params: &HashMap<String, String>,
    key: &str,
    default: i64,
    min: i64,
    max: i64,
) -> Result<i64, CompatError> {
    params
        .get(key)
        .map(|v| {
            v.parse::<i64>()
                .ok()
                .filter(|v| *v >= min && *v <= max)
                .ok_or_else(|| CompatError::new(422, format!("Invalid {key}")))
        })
        .unwrap_or(Ok(default))
}
fn validate_score(body: &Value) -> Result<Value, CompatError> {
    if !body.is_object() {
        return Err(CompatError::new(422, "Invalid score"));
    }
    for (key, max, required) in [
        ("score_name", 96, true),
        ("categorical_value", 96, false),
        ("text_value", 2000, false),
        ("label", 96, false),
        ("explanation", 2000, false),
        ("evaluator_version", 64, false),
        ("target_id", 128, false),
        ("evaluator_name", 128, false),
        ("score_source", 32, false),
    ] {
        if let Some(value) = body.get(key).filter(|v| !v.is_null()) {
            if value
                .as_str()
                .is_none_or(|s| s.chars().count() > max || required && s.is_empty())
            {
                return Err(CompatError::new(422, format!("Invalid {key}")));
            }
        } else if required {
            return Err(CompatError::new(422, format!("Missing {key}")));
        }
    }
    for (key, allowed, default) in [
        (
            "score_type",
            &["numeric", "categorical", "boolean", "text"][..],
            "numeric",
        ),
        (
            "scorer_type",
            &["human", "llm", "rule", "system"][..],
            "human",
        ),
        (
            "target_type",
            &["trace", "span", "thread", "dataset_run", "example"][..],
            "trace",
        ),
    ] {
        let value = body.get(key).and_then(Value::as_str).unwrap_or(default);
        if !allowed.contains(&value) {
            return Err(CompatError::new(422, format!("Invalid {key}")));
        }
    }
    if body
        .get("confidence")
        .filter(|v| !v.is_null())
        .is_some_and(|v| {
            v.as_f64()
                .is_none_or(|v| !v.is_finite() || !(0.0..=1.0).contains(&v))
        })
    {
        return Err(CompatError::new(422, "Invalid confidence"));
    }
    if body.get("metadata").is_some_and(|v| !v.is_object()) {
        return Err(CompatError::new(422, "Invalid metadata"));
    }
    let mut result = score(body);
    let map = result.as_object_mut().unwrap();
    for key in ["score_id", "trace_id", "created_by", "created_at"] {
        map.remove(key);
    }
    Ok(result)
}
pub async fn handle(
    options: &EvalHttpOptions,
    user: &CompatUser,
    method: &str,
    parts: &[&str],
    params: &HashMap<String, String>,
    body: &Value,
) -> Result<Option<(u16, Value)>, CompatError> {
    let supported = matches!(
        (method, parts),
        ("GET", ["summary" | "dashboard" | "traces"])
            | ("GET", ["traces", _])
            | ("POST", ["traces", _, "scores"])
            | ("POST", ["ingest", "traces"])
            | ("GET", ["threads", _])
            | ("GET", ["traces", _, "export"])
    );
    if !supported {
        return Ok(None);
    }
    let repo = options
        .traces
        .as_ref()
        .ok_or_else(|| CompatError::new(503, "Database not available"))?;
    let requested = params.get("user_id").map(String::as_str);
    let scoped = if user.operator() {
        requested
    } else {
        Some(user.user_id.as_str())
    };
    let family_value = params
        .get("trace_family")
        .map(String::as_str)
        .unwrap_or("assistant");
    let result = match (method, parts) {
        ("GET", ["summary"]) => {
            let days = integer(params, "days", 7, 1, 90)?;
            repo.get_summary(&user.tenant_id, scoped, days as i32)
                .await
                .map_err(error)?
        }
        ("GET", ["dashboard"]) => {
            let days = integer(params, "days", 7, 1, 90)?;
            repo.get_dashboard(&user.tenant_id, days as i32)
                .await
                .map_err(error)?
        }
        ("GET", ["traces"]) => {
            family(family_value)?;
            let limit = integer(params, "limit", 50, 1, 200)?;
            let offset = integer(params, "offset", 0, 0, i64::MAX)?;
            let get = |key: &str| params.get(key).cloned();
            let number = |key: &str| {
                params
                    .get(key)
                    .map(|v| {
                        v.parse::<f64>()
                            .ok()
                            .filter(|n| n.is_finite())
                            .ok_or_else(|| CompatError::new(422, format!("Invalid {key}")))
                    })
                    .transpose()
            };
            let optional_int = |key: &str, min| {
                params
                    .get(key)
                    .map(|_| integer(params, key, 0, min, i64::MAX))
                    .transpose()
            };
            let date = |key: &str| {
                params
                    .get(key)
                    .map(|v| {
                        chrono::DateTime::parse_from_rfc3339(v)
                            .map(|v| v.with_timezone(&chrono::Utc))
                            .map_err(|_| CompatError::new(422, format!("Invalid {key}")))
                    })
                    .transpose()
            };
            let channel = get("channel");
            if channel
                .as_deref()
                .is_some_and(|v| !matches!(v, "preview" | "hosted" | "embed" | "api" | "builtin"))
            {
                return Err(CompatError::new(422, "Invalid channel"));
            }
            let transcript_query = get("transcript_query");
            if transcript_query
                .as_ref()
                .is_some_and(|v| v.is_empty() || v.chars().count() > 500)
            {
                return Err(CompatError::new(422, "Invalid transcript_query"));
            }
            let filter = TraceFilter {
                user_id: scoped.map(str::to_owned),
                trace_family: family_value.into(),
                status: get("status"),
                model_id: get("model_id"),
                session_id: get("session_id"),
                run_id: get("run_id"),
                request_id: get("request_id"),
                agent_id: get("agent_id"),
                agent_version_id: get("agent_version_id"),
                publication_id: get("publication_id"),
                channel,
                transcript_query: transcript_query.map(|v| v.trim().into()),
                turn_index: optional_int("turn_index", 1)?,
                span_kind: get("span_kind"),
                score_name: get("score_name"),
                score_label: get("score_label"),
                min_score: number("min_score")?,
                max_score: number("max_score")?,
                min_latency_ms: optional_int("min_latency_ms", 0)?,
                max_latency_ms: optional_int("max_latency_ms", 0)?,
                dataset_id: get("dataset_id"),
                metadata_dataset_id: get("metadata_dataset_id"),
                started_after: date("started_after")?,
                started_before: date("started_before")?,
                limit,
                offset,
            };
            let (rows, total) = repo
                .list_traces(&user.tenant_id, &filter)
                .await
                .map_err(error)?;
            json!({"traces":rows.iter().map(trace_summary).collect::<Vec<_>>(),"total":total,"limit":limit,"offset":offset})
        }
        ("GET", ["traces", id]) => {
            family(family_value)?;
            let value = repo
                .get_trace_detail(
                    &user.tenant_id,
                    id,
                    if user.operator() {
                        None
                    } else {
                        Some(&user.user_id)
                    },
                    family_value,
                )
                .await
                .map_err(error)?
                .ok_or_else(|| CompatError::new(404, "Trace not found"))?;
            detail(&value)
        }
        ("POST", ["traces", id, "scores"]) => {
            family(family_value)?;
            let body = validate_score(body)?;
            let value = repo
                .create_score(
                    &user.tenant_id,
                    id,
                    &user.user_id,
                    &body,
                    if user.operator() {
                        None
                    } else {
                        Some(&user.user_id)
                    },
                    family_value,
                )
                .await
                .map_err(error)?
                .ok_or_else(|| CompatError::new(404, "Trace not found"))?;
            return Ok(Some((201, score(&value))));
        }
        ("POST", ["ingest", "traces"]) => {
            if !body["trace"].is_object() {
                return Err(CompatError::new(422, "trace is required"));
            }
            let mut trace = body["trace"].clone();
            for (key, default) in [
                ("trace_family", json!("assistant")),
                ("workflow_kind", json!("ai_assistant_chat")),
                ("status", json!("succeeded")),
                ("input_preview", json!("")),
                ("output_preview", json!("")),
                ("source_adapter", json!("api")),
                ("metrics", json!({})),
                ("privacy", json!({})),
                ("redaction_state", json!({})),
                ("metadata", json!({})),
                ("spans", json!([])),
                ("events", json!([])),
                ("scores", json!([])),
                ("artifacts", json!([])),
            ] {
                if trace.get(key).is_none() {
                    trace[key] = default;
                }
            }
            family(text(&trace, "trace_family"))?;
            if !matches!(
                text(&trace, "status"),
                "running" | "succeeded" | "failed" | "cancelled" | "timeout"
            ) {
                return Err(CompatError::new(422, "Invalid trace status"));
            }
            let enqueue = body
                .get("enqueue")
                .map(|v| {
                    v.as_bool()
                        .ok_or_else(|| CompatError::new(422, "Invalid enqueue"))
                })
                .transpose()?
                .unwrap_or(true);
            let value = repo
                .ingest_trace_scoped(
                    &user.tenant_id,
                    &user.user_id,
                    &json!({"trace":trace,"enqueue":enqueue}),
                    enqueue,
                    if user.operator() {
                        None
                    } else {
                        Some(&user.user_id)
                    },
                )
                .await
                .map_err(error)?;
            return Ok(Some((
                202,
                pick(&value, "trace_id status job_id", json!({})),
            )));
        }
        ("GET", ["threads", id]) => {
            let limit = integer(params, "limit", 100, 1, 500)?;
            let mut value = repo
                .get_thread(
                    &user.tenant_id,
                    id,
                    if user.operator() {
                        None
                    } else {
                        Some(&user.user_id)
                    },
                    limit,
                )
                .await
                .map_err(error)?;
            value["traces"] = json!(
                arr(&value["traces"])
                    .iter()
                    .map(trace_summary)
                    .collect::<Vec<_>>()
            );
            value
        }
        ("GET", ["traces", id, "export"]) => {
            family(family_value)?;
            let format = params
                .get("format")
                .map(String::as_str)
                .unwrap_or("openinference");
            if !matches!(format, "otel" | "openinference" | "langsmith-jsonl") {
                return Err(CompatError::new(422, "Invalid format"));
            }
            let value = repo
                .get_trace_detail(
                    &user.tenant_id,
                    id,
                    if user.operator() {
                        None
                    } else {
                        Some(&user.user_id)
                    },
                    family_value,
                )
                .await
                .map_err(error)?
                .ok_or_else(|| CompatError::new(404, "Trace not found"))?;
            json!({"trace_id":id,"format":format,"redaction_policy":eval_export::redaction_policy(),"payload":eval_export::export_trace(&value,format)})
        }
        _ => unreachable!(),
    };
    Ok(Some((200, result)))
}

#[cfg(test)]
mod ingest_authority_tests {
    use super::*;
    use std::sync::Arc;

    async fn options_with_closed_storage() -> EvalHttpOptions {
        // Closed, independent lazy pools cannot attempt a database connection.
        // This isolates the real handler's scope gate from SQL execution.
        let data = sqlx::postgres::PgPoolOptions::new()
            .min_connections(0)
            .connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        let control = sqlx::postgres::PgPoolOptions::new()
            .min_connections(0)
            .connect_lazy_with(sqlx::postgres::PgConnectOptions::new());
        data.close().await;
        control.close().await;
        EvalHttpOptions {
            traces: Some(Arc::new(
                crate::migration_trace::TraceRepository::new(data, control).unwrap(),
            )),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn ordinary_and_platform_roles_cannot_ingest_other_user() {
        let options = options_with_closed_storage().await;
        for role in ["user", "platform_admin", "Admin", "tenant_admin"] {
            let user = CompatUser {
                tenant_id: "tenant-a".into(),
                user_id: "actor".into(),
                roles: vec![role.into()],
                authenticated: true,
                ..Default::default()
            };
            let error = handle(
                &options,
                &user,
                "POST",
                &["ingest", "traces"],
                &HashMap::new(),
                &json!({"trace":{"user_id":"other"}}),
            )
            .await
            .unwrap_err();
            assert_eq!(error.status, 404, "role {role}");
            assert_eq!(error.detail, json!("Trace not found"));
        }
    }

    #[tokio::test]
    async fn owner_and_existing_tenant_operators_reach_storage() {
        let options = options_with_closed_storage().await;
        for (role, owner) in [
            ("user", "actor"),
            ("platform_admin", "actor"),
            ("admin", "other"),
            ("operator", "other"),
        ] {
            let user = CompatUser {
                tenant_id: "tenant-a".into(),
                user_id: "actor".into(),
                roles: vec![role.into()],
                authenticated: true,
                ..Default::default()
            };
            let error = handle(
                &options,
                &user,
                "POST",
                &["ingest", "traces"],
                &HashMap::new(),
                &json!({"trace":{"user_id":owner}}),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.status, 503,
                "authorized role {role} must reach closed storage"
            );
            assert_eq!(error.detail, json!("trace_storage_unavailable"));
        }
    }

    #[tokio::test]
    async fn internal_capture_retains_subject_but_rejects_wrong_claim_tenant() {
        let options = options_with_closed_storage().await;
        let repository = options.traces.as_ref().unwrap();
        let error = repository
            .ingest_trace(
                "tenant-a",
                "eval-worker",
                &json!({"trace":{"user_id":"candidate"}}),
                false,
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.status, 503,
            "trusted attribution must reach closed storage"
        );
        let claimed = repository.with_outbox_claim(&json!({"job_id":"job", "tenant_id":"tenant-b", "owner_id":"worker", "claim_token":"claim"})).unwrap();
        let error = claimed
            .ingest_trace("tenant-a", "eval-worker", &json!({"trace":{}}), false)
            .await
            .unwrap_err();
        assert_eq!(error.code, "eval_outbox_lease_lost");
    }

    #[test]
    fn native_sql_preserves_conflict_parent_fences() {
        let trace = crate::migration_trace::sql::INGEST_TRACE;
        assert!(trace.contains("WHERE agent_traces.tenant_id = EXCLUDED.tenant_id"));
        assert!(trace.contains("AND ($31::text IS NULL OR agent_traces.user_id = $31)"));
        assert!(!trace.contains("tenant_id = EXCLUDED.tenant_id,"));
        assert!(
            crate::migration_trace::sql::UPSERT_SPAN
                .contains("WHERE agent_trace_spans.trace_id = EXCLUDED.trace_id")
        );
    }
}
