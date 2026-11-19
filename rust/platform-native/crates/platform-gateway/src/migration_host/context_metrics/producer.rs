//! Measures the actual provider-bound context after preparation and guardrails.
//! This is an explicit codepoint estimate, not billed usage or a tokenizer claim.
//! Non-text modalities and unobserved Runtime compaction/cache effects are unknown.
use super::*;
use crate::migration_agent::{AgentError, model::AuthorizedCall};

fn project(value: &Value, media: &mut bool) -> Value {
    match value {
        Value::Object(fields) => {
            let kind = fields.get("type").and_then(Value::as_str).unwrap_or("");
            if [
                "input_image",
                "image",
                "image_url",
                "input_audio",
                "audio",
                "input_file",
                "file",
                "document",
                "input_video",
                "video",
            ]
            .contains(&kind)
            {
                *media = true;
                return json!({"type":kind});
            }
            Value::Object(
                fields
                    .iter()
                    .filter_map(|(key, value)| {
                        if ["image_url", "audio", "file_data", "image_data", "video_url"]
                            .contains(&key.as_str())
                        {
                            *media = true;
                            None
                        } else {
                            Some((key.clone(), project(value, media)))
                        }
                    })
                    .collect(),
            )
        }
        Value::Array(values) => Value::Array(values.iter().map(|v| project(v, media)).collect()),
        _ => value.clone(),
    }
}
pub fn calculate(
    tenant: &str,
    session: &str,
    context_window: i64,
    prepared: &Value,
    now: DateTime<Utc>,
) -> Result<ContextMetric, AgentError> {
    if tenant.is_empty() || session.is_empty() || context_window <= 0 || !prepared.is_object() {
        return Err(AgentError::code(503, "CONTEXT_OBSERVATION_INVALID"));
    }
    let mut media = false;
    let mut projection = serde_json::Map::new();
    for key in ["instructions", "system", "messages", "input", "tools"] {
        if let Some(value) = prepared.get(key) {
            // Tool schemas are text context even when a schema property happens
            // to be named image_url; only actual input modalities are omitted.
            projection.insert(
                key.into(),
                if key == "tools" {
                    value.clone()
                } else {
                    project(value, &mut media)
                },
            );
        }
    }
    if !projection.contains_key("messages") && !projection.contains_key("input") {
        return Err(AgentError::code(503, "CONTEXT_OBSERVATION_MISSING_INPUT"));
    }
    let wire = serde_json::to_string(&projection)
        .map_err(|_| AgentError::code(503, "CONTEXT_OBSERVATION_INVALID"))?;
    let tokens = crate::usage_parser::estimate_tokens(&wire);
    Ok(ContextMetric {
        tenant_id: tenant.into(),
        session_id: session.into(),
        timestamp: now,
        total_tokens: tokens,
        utilization: tokens as f64 / context_window as f64,
        compression_ratio: None,
        cache_hit_rate: None,
        calculation: Some(Calculation {
            source: "prepared_provider_request".into(),
            token_kind: "codepoint_estimate".into(),
            context_window,
            non_text_tokens_unknown: media,
        }),
    })
}
impl ContextMetricsCollector {
    pub async fn observe_prepared(
        &self,
        call: &AuthorizedCall,
        prepared: &Value,
    ) -> Result<(), AgentError> {
        let window = call.snapshot["limits"]["context_window"]
            .as_i64()
            .ok_or_else(|| AgentError::code(503, "CONTEXT_WINDOW_MISSING"))?;
        let metric = calculate(
            &call.scope.tenant_id,
            &call.scope.session_id,
            window,
            prepared,
            Utc::now(),
        )?;
        self.observe(&call.call_id, metric)
            .await
            .map_err(|_| AgentError::code(503, "CONTEXT_METRICS_UNAVAILABLE"))?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn estimates_final_request_context_and_never_usage_or_reservation() {
        let now = Utc::now();
        let body = json!({"instructions":"actual prepared instructions", "input":[{"role":"user","content":"actual retained query"}], "tools":[{"name":"lookup","parameters":{"type":"object"}}]});
        let first = calculate("t", "s", 128000, &body, now).unwrap();
        let mut polluted = body.clone();
        polluted["usage"] = json!({"input_tokens":9999999,"cached_tokens":1234567});
        polluted["api_key"] = json!("secret-never-measured");
        polluted["max_output_tokens"] = json!(1000000);
        let second = calculate("t", "s", 128000, &polluted, now).unwrap();
        assert_eq!(first.total_tokens, second.total_tokens);
        assert!(first.total_tokens > 0);
        assert_eq!(first.utilization, first.total_tokens as f64 / 128000.0);
        assert!(first.compression_ratio.is_none() && first.cache_hit_rate.is_none());
        let mut changed = body.clone();
        changed["input"] = json!("a".repeat(1000));
        assert!(
            calculate("t", "s", 128000, &changed, now)
                .unwrap()
                .total_tokens
                > first.total_tokens
        );
        let record = serde_json::to_string(&second).unwrap();
        assert!(!record.contains("actual retained") && !record.contains("secret-never"));
    }
    #[test]
    fn non_text_cost_is_explicitly_unknown_and_binary_data_not_counted() {
        let body = json!({"input":[{"type":"input_image","image_url":"data:image/png;base64,AAAAAAAA"},{"type":"input_text","text":"read diagram"}]});
        let a = calculate("t", "s", 100, &body, Utc::now()).unwrap();
        let mut b = body;
        b["input"][0]["image_url"] = json!("A".repeat(10000));
        let b = calculate("t", "s", 100, &b, Utc::now()).unwrap();
        assert_eq!(a.total_tokens, b.total_tokens);
        assert!(a.calculation.unwrap().non_text_tokens_unknown);
    }
    #[test]
    fn no_unsigned_default_window_or_missing_input() {
        for (t, s, w, b) in [
            ("", "s", 10, json!({"input":"x"})),
            ("t", "", 10, json!({"input":"x"})),
            ("t", "s", 0, json!({"input":"x"})),
            ("t", "s", 10, json!({"usage":{"input_tokens":200}})),
        ] {
            assert!(calculate(t, s, w, &b, Utc::now()).is_err());
        }
    }
}

#[cfg(test)]
mod observation_tests {
    use super::*;
    #[tokio::test]
    async fn calculated_context_reaches_real_readers_without_fabricating_unknown_ratios() {
        let collector = ContextMetricsCollector::default();
        let now = Utc::now();
        let metric = calculate(
            "tenant-a",
            "shared-session",
            1000,
            &json!({"messages":[{"role":"user","content":"actual retained conversation"}]}),
            now,
        )
        .unwrap();
        let total = metric.total_tokens;
        collector.observe("call", metric).await.unwrap();
        let session = collector
            .read_session("tenant-a", "shared-session", 50)
            .await
            .unwrap();
        assert_eq!(session["total_tokens_used"], total);
        assert_eq!(session["avg_compression_ratio"], Value::Null);
        assert_eq!(session["avg_cache_hit_rate"], Value::Null);
        assert_eq!(
            session["calculation"]["source"],
            "prepared_provider_request"
        );
        assert_eq!(session["calculation"]["token_kind"], "codepoint_estimate");
        assert_eq!(
            collector
                .read_session("tenant-b", "shared-session", 50)
                .await
                .unwrap()["request_count"],
            0
        );
        assert_eq!(
            collector.read_tenant("tenant-a", 24, now).await.unwrap()["total_tokens"],
            total
        );
    }
    #[test]
    fn old_numeric_payloads_remain_readable_and_invalid_observations_are_rejected() {
        let legacy = json!({"tenant_id":"t","session_id":"s","timestamp":Utc::now(),"total_tokens":4,"utilization":0.2,"compression_ratio":1.5,"cache_hit_rate":0.7});
        let mut metric: ContextMetric = serde_json::from_value(legacy).unwrap();
        assert_eq!(metric.compression_ratio, Some(1.5));
        assert!(metric.calculation.is_none());
        let collector = ContextMetricsCollector::default();
        metric.utilization = f64::NAN;
        assert!(collector.record(metric).is_err());
        assert_eq!(collector.session("s", 100)["request_count"], 0);
    }
    #[tokio::test]
    #[ignore = "explicit private disposable Valkey file required; run serially outside application coordination"]
    async fn actual_context_producer_is_shared_across_independent_process_collectors() {
        let path = std::env::var_os("AGENTFORGE_HA_TEST_REDIS_URL_FILE")
            .expect("private disposable Valkey file");
        let secret = ai_platform_service_primitives::workload_identity::secrets::read_file(
            std::path::Path::new(&path),
            true,
        )
        .expect("private fixture file");
        let url = std::str::from_utf8(secret.expose()).expect("fixture encoding");
        let a = redis::aio::ConnectionManager::new(
            redis::Client::open(url.trim())
                .map_err(|_| "fixture URL invalid")
                .unwrap(),
        )
        .await
        .unwrap();
        let b = redis::aio::ConnectionManager::new(
            redis::Client::open(url.trim())
                .map_err(|_| "fixture URL invalid")
                .unwrap(),
        )
        .await
        .unwrap();
        let a = ContextMetricsCollector::shared(a);
        let b = ContextMetricsCollector::shared(b);
        let tenant = format!("ha-test-{}", uuid::Uuid::new_v4());
        let event = uuid::Uuid::new_v4().to_string();
        let metric = calculate(
            &tenant,
            "same-session",
            128000,
            &json!({"instructions":"prepared system","input":"actual computed context"}),
            Utc::now(),
        )
        .unwrap();
        assert!(a.record(metric.clone()).is_err()); // no synchronous/local fallback
        assert!(a.observe(&event, metric.clone()).await.unwrap());
        assert!(!b.observe(&event, metric.clone()).await.unwrap());
        let snapshot = b.read_session(&tenant, "same-session", 100).await.unwrap();
        assert_eq!(snapshot["request_count"], 1);
        assert_eq!(snapshot["total_tokens_used"], metric.total_tokens);
        let mut changed = metric;
        changed.total_tokens += 1;
        assert!(b.observe(&event, changed).await.is_err());
        assert_eq!(
            b.read_session("different-tenant", "same-session", 100)
                .await
                .unwrap()["request_count"],
            0
        );
    }
}
