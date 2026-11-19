//! Versioned takeover state. Deadlines are persisted once, never renewed by a restart.
use super::{transport_budget, turns::StartTurnRequest};
use ai_platform_capability_contract::private_transport::{Budget, Headers};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const SCHEMA: &str = "ai-platform/execution-context/v2";
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DurableContext {
    schema: String,
    request: StartTurnRequest,
    transport: Headers,
}

pub(super) fn encode(request: &StartTurnRequest, budget: &Budget) -> Result<Value, &'static str> {
    let mut transport = vec![];
    budget
        .append(
            &mut transport,
            transport_budget::wall_ms(),
            std::time::Instant::now(),
        )
        .map_err(|_| "runtime_transport_budget_expired")?;
    serde_json::to_value(DurableContext {
        schema: SCHEMA.into(),
        request: request.clone(),
        transport,
    })
    .map_err(|_| "invalid_turn_context")
}

pub(super) fn decode(value: Value) -> Result<(StartTurnRequest, HeaderMap), &'static str> {
    // N-1 contexts predate durable transport attenuation. They remain readable;
    // the database lease still supplies their independent authority ceiling.
    if value.get("schema").is_none() {
        return serde_json::from_value(value)
            .map(|request| (request, HeaderMap::new()))
            .map_err(|_| "recovery_context_invalid");
    }
    let context: DurableContext =
        serde_json::from_value(value).map_err(|_| "recovery_context_invalid")?;
    if context.schema != SCHEMA || context.transport.len() != 3 {
        return Err("recovery_context_invalid");
    }
    let mut headers = HeaderMap::new();
    for (name, value) in context.transport {
        if ![
            ai_platform_capability_contract::private_transport::DEADLINE_HEADER,
            ai_platform_capability_contract::private_transport::BUDGET_HEADER,
            ai_platform_capability_contract::private_transport::HOPS_HEADER,
        ]
        .contains(&name.as_str())
        {
            return Err("recovery_context_invalid");
        }
        let name = axum::http::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| "recovery_context_invalid")?;
        if headers.contains_key(&name) {
            return Err("recovery_context_invalid");
        }
        headers.insert(name, value.parse().map_err(|_| "recovery_context_invalid")?);
    }
    Ok((context.request, headers))
}

/// Snapshot payloads are re-read under live authority; only the admitted deadline
/// is recovered from the immutable execution context.
pub(super) fn restore_budget(context: &Value, verified_expiry: u64) -> Result<Budget, String> {
    let (_, headers) = decode(context.clone()).map_err(str::to_owned)?;
    transport_budget::admit_authenticated(
        &headers,
        transport_budget::ADMISSION_CAP,
        Some(verified_expiry),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> StartTurnRequest {
        serde_json::from_value(serde_json::json!({"runId":uuid::Uuid::from_u128(1),
            "snapshotId":uuid::Uuid::from_u128(2),"leaseId":uuid::Uuid::from_u128(3),
            "leaseSignature":"test","message":"hello","model":"test","effort":null,
            "capabilityRevision":1}))
        .unwrap()
    }
    #[test]
    fn takeover_keeps_original_absolute_deadline_and_hop_ceiling() {
        let now = transport_budget::wall_ms();
        let budget =
            transport_budget::local(std::time::Duration::from_secs(2), Some(now + 2000)).unwrap();
        let value = encode(&request(), &budget).unwrap();
        let (_, headers) = decode(value.clone()).unwrap();
        let deadline: u64 = headers["x-ai-deadline-unix-ms"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!(deadline <= now + 2000);
        let restored = restore_budget(&value, now + 300_000).unwrap();
        let mut outgoing = vec![];
        restored
            .append(
                &mut outgoing,
                transport_budget::wall_ms(),
                std::time::Instant::now(),
            )
            .unwrap();
        assert!(
            outgoing
                .iter()
                .find(|(k, _)| k == "x-ai-deadline-unix-ms")
                .unwrap()
                .1
                .parse::<u64>()
                .unwrap()
                <= deadline
        );
        assert!(
            restored.remaining(std::time::Instant::now()).unwrap()
                <= std::time::Duration::from_secs(2)
        );
    }
    #[test]
    fn corrupt_partial_duplicate_future_and_expired_state_fail_closed() {
        let value = encode(
            &request(),
            &transport_budget::local(transport_budget::ADMISSION_CAP, None).unwrap(),
        )
        .unwrap();
        let mut bad = value.clone();
        bad["schema"] = "future".into();
        assert!(decode(bad).is_err());
        let mut bad = value.clone();
        bad["transport"].as_array_mut().unwrap().pop();
        assert!(decode(bad).is_err());
        let mut bad = value.clone();
        bad["transport"][1] = bad["transport"][0].clone();
        assert!(decode(bad).is_err());
        let mut bad = value;
        bad["transport"][0][1] = "1".into();
        assert!(restore_budget(&bad, transport_budget::wall_ms() + 300_000).is_err());
        assert!(
            decode(serde_json::to_value(request()).unwrap())
                .unwrap()
                .1
                .is_empty()
        );
    }
}
