use super::{
    Result,
    config::{Boundary, Case, Edge, Kind},
    ensure,
    identity::Service,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
#[derive(Clone)]
pub struct Span {
    pub service: String,
    pub name: String,
    pub start: u64,
    pub end: u64,
    pub events: Vec<(String, u64)>,
    pub attributes: BTreeMap<String, String>,
}
fn attributes(v: &Value) -> BTreeMap<String, String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some((
                a["key"].as_str()?.to_owned(),
                a["value"]["stringValue"].as_str()?.to_owned(),
            ))
        })
        .collect()
}
fn integer(v: &Value) -> Option<u64> {
    v.as_u64().or_else(|| v.as_str()?.parse().ok())
}
fn id(v: &str, n: usize) -> bool {
    v.len() == n && v.bytes().all(|b| b.is_ascii_hexdigit()) && v.bytes().any(|b| b != b'0')
}
pub fn spans(value: &Value, trace_id: &str, services: &[Service]) -> Result<Vec<Span>> {
    ensure(id(trace_id, 32), "correlation_trace_id_invalid")?;
    let batches = value
        .get("resourceSpans")
        .or_else(|| value.get("batches"))
        .and_then(Value::as_array)
        .ok_or("otlp_json_resource_spans_required")?;
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for resource in batches {
        let attrs = attributes(&resource["resource"]["attributes"]);
        let name = attrs
            .get("service.name")
            .ok_or("trace_service_identity_missing")?;
        let Some(service) = services.iter().find(|s| &s.telemetry_service == name) else {
            continue;
        };
        ensure(
            attrs.get("service.instance.id") == Some(&service.instance_id),
            "trace_instance_not_verified_live_process",
        )?;
        for scope in resource
            .get("scopeSpans")
            .or_else(|| resource.get("instrumentationLibrarySpans"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            for s in scope["spans"].as_array().into_iter().flatten() {
                ensure(
                    s["traceId"]
                        .as_str()
                        .and_then(|v| decode_id(v, 16))
                        .as_deref()
                        == Some(trace_id),
                    "foreign_or_unmatched_trace",
                )?;
                let span_id = s["spanId"]
                    .as_str()
                    .and_then(|v| decode_id(v, 8))
                    .ok_or("span_identity")?;
                ensure(
                    id(&span_id, 16) && seen.insert(span_id.to_owned()),
                    "duplicate_span_identity",
                )?;
                let start = integer(&s["startTimeUnixNano"]).ok_or("trace_clock_missing")?;
                let end = integer(&s["endTimeUnixNano"]).ok_or("trace_clock_missing")?;
                ensure(start > 0 && end >= start, "trace_clock_invalid")?;
                let events = s["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|e| {
                        Some((e["name"].as_str()?.into(), integer(&e["timeUnixNano"])?))
                    })
                    .collect::<Vec<_>>();
                ensure(
                    events.iter().all(|(_, at)| *at >= start && *at <= end),
                    "trace_event_clock_invalid",
                )?;
                result.push(Span {
                    service: service.id.clone(),
                    name: s["name"].as_str().ok_or("trace_span_name_missing")?.into(),
                    start,
                    end,
                    events,
                    attributes: attributes(&s["attributes"]),
                });
                ensure(result.len() <= 10000, "trace_span_budget")?;
            }
        }
    }
    ensure(!result.is_empty(), "correlated_service_trace_missing")?;
    Ok(result)
}
fn boundary(spans: &[Span], b: &Boundary) -> Option<u64> {
    let candidates = spans
        .iter()
        .filter(|s| s.service == b.service && s.name == b.span)
        .flat_map(|s| {
            if let Some(name) = &b.event {
                s.events
                    .iter()
                    .filter(|(n, _)| n == name)
                    .map(|(_, t)| *t)
                    .collect::<Vec<_>>()
            } else {
                vec![match b.edge {
                    Edge::Start => s.start,
                    Edge::End => s.end,
                }]
            }
        })
        .collect::<Vec<_>>();
    (candidates.len() == 1).then(|| candidates[0])
}
pub fn union_ms(mut intervals: Vec<(u64, u64)>) -> f64 {
    intervals.sort_unstable();
    let mut total = 0u128;
    let mut last: Option<(u64, u64)> = None;
    for (a, b) in intervals {
        match last {
            Some((x, y)) if a <= y => last = Some((x, y.max(b))),
            Some((x, y)) => {
                total += (y - x) as u128;
                last = Some((a, b));
            }
            None => last = Some((a, b)),
        }
    }
    if let Some((a, b)) = last {
        total += (b - a) as u128;
    }
    total as f64 / 1e6
}
pub fn metrics(case: &Case, body: &Value, spans: &[Span]) -> Value {
    let mut result = json!({"status":"measured","gateway":null,"retrieval_stages":{},"clock_scope":"per-request spans from verified service instance; no unrelated percentile subtraction"});
    let mut unknown = false;
    if matches!(case.kind, Kind::GatewayMock | Kind::GatewayReal) {
        let names = [
            "receive",
            "admit",
            "upstream_dispatch",
            "upstream_first_byte",
            "upstream_complete",
            "response_complete",
        ];
        let values = names
            .iter()
            .map(|name| case.boundaries.get(*name).and_then(|b| boundary(spans, b)))
            .collect::<Option<Vec<_>>>();
        result["gateway"] = if let Some(v) = values.filter(|v| v.windows(2).all(|a| a[0] <= a[1])) {
            let ms = |a: usize, b: usize| (v[b] - v[a]) as f64 / 1e6;
            json!({"status":"measured","total_server_ms":ms(0,5),"admission_ms":ms(0,1),"post_admit_pre_upstream_ms":ms(1,2),"upstream_first_byte_ms":ms(2,3),"upstream_wait_and_transfer_ms":ms(2,4),"post_upstream_ms":ms(4,5),"gateway_excluding_upstream_interval_ms":ms(0,2)+ms(4,5),"method":"same_request_gateway_boundary_intervals"})
        } else {
            unknown = true;
            json!({"status":"unknown","reason":"missing_ambiguous_or_unordered_gateway_boundaries"})
        };
    }
    if case.kind == Kind::GatewayReal {
        let matched = case.upstream_identity.as_ref().is_some_and(|identity| {
            let selected = spans
                .iter()
                .filter(|s| s.service == identity.service && s.name == identity.span)
                .collect::<Vec<_>>();
            selected.len() == 1
                && identity
                    .attributes
                    .iter()
                    .all(|(key, value)| selected[0].attributes.get(key) == Some(value))
        });
        result["real_provider_identity"] =
            json!({"status":if matched{"verified_from_same_request_service_trace"}else{"unknown"}});
        if !matched {
            unknown = true;
        }
    }
    for (name, stage) in &case.stages {
        let active = stage
            .active_when
            .as_ref()
            .map(|c| body.pointer(&c.pointer).map(|v| v == &c.equals));
        let value = if active == Some(Some(false)) {
            json!({"status":"not_applicable","source":"explicit_correlated_response_condition"})
        } else if active == Some(None) {
            unknown = true;
            json!({"status":"unknown","reason":"stage_condition_missing"})
        } else {
            let selected = spans
                .iter()
                .filter(|s| s.service == stage.service && s.name == stage.span)
                .collect::<Vec<_>>();
            if !selected.is_empty() {
                json!({"status":"measured","source":"correlated_server_spans","span_count":selected.len(),"span_duration_sum_ms":selected.iter().map(|s|(s.end-s.start)as f64/1e6).sum::<f64>(),"union_active_ms":union_ms(selected.iter().map(|s|(s.start,s.end)).collect())})
            } else if spans.iter().any(|s| s.service == stage.service)
                && stage
                    .response_pointer
                    .as_ref()
                    .and_then(|p| body.pointer(p))
                    .and_then(Value::as_f64)
                    .is_some_and(|n| n.is_finite() && n >= 0.0)
            {
                json!({"status":"measured","source":"correlated_service_response_reported_timing","elapsed_ms":body.pointer(stage.response_pointer.as_ref().unwrap())})
            } else {
                unknown = true;
                json!({"status":"unknown","reason":"stage_instrumentation_not_observed"})
            }
        };
        result["retrieval_stages"][name] = value;
    }
    if unknown {
        result["status"] = json!("partial_unknown");
    }
    result
}

fn decode_id(value: &str, bytes: usize) -> Option<String> {
    if id(value, bytes * 2) {
        return Some(value.to_ascii_lowercase());
    }
    let bytes_value = STANDARD.decode(value).ok()?;
    if bytes_value.len() != bytes || bytes_value.iter().all(|b| *b == 0) {
        return None;
    }
    Some(bytes_value.iter().map(|b| format!("{b:02x}")).collect())
}
