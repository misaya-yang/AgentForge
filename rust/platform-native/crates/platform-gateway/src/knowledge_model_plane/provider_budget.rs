//! Bind the reservation ceiling to the selected vendor's actual generation cap.
use super::*;
fn denied() -> PlaneError {
    PlaneError::new(422, "knowledge_provider_output_budget_required")
}
pub(super) fn validate(call: &ModelCall) -> PlaneResult<()> {
    let payload = &call.payload;
    let limits = [
        payload.pointer("/max_tokens"),
        payload.pointer("/max_completion_tokens"),
        payload.pointer("/max_output_tokens"),
        payload.pointer("/generationConfig/maxOutputTokens"),
        payload.pointer("/parameters/max_tokens"),
    ];
    // A different codec's cap must not hide an invalid or higher effective cap.
    // Reject conflicting aliases rather than relying on a vendor's precedence.
    let mut declared = None;
    for value in limits.into_iter().flatten() {
        let limit = value.as_u64().filter(|n| *n > 0).ok_or_else(denied)?;
        if limit > call.budget.max_output_tokens
            || declared.is_some_and(|previous| previous != limit)
        {
            return Err(denied());
        }
        declared = Some(limit);
    }
    let (limit, multiplicities): (_, &[&str]) = match call.wire {
        ModelWire::ChatCompletions => (
            payload
                .get("max_completion_tokens")
                .or_else(|| payload.get("max_tokens")),
            &["/n", "/best_of"],
        ),
        ModelWire::GeminiGenerateContent => (
            payload.pointer("/generationConfig/maxOutputTokens"),
            &["/generationConfig/candidateCount"],
        ),
        ModelWire::DashscopeVision => (
            payload.pointer("/parameters/max_tokens"),
            &["/parameters/n", "/parameters/best_of"],
        ),
        _ => return Err(denied()),
    };
    let limit = limit.and_then(Value::as_u64).ok_or_else(denied)?;
    let mut count = 1;
    for pointer in multiplicities {
        if let Some(value) = payload.pointer(pointer) {
            count = count.max(value.as_u64().filter(|n| *n > 0).ok_or_else(denied)?);
        }
    }
    if limit
        .checked_mul(count)
        .is_none_or(|total| total > call.budget.max_output_tokens)
    {
        return Err(denied());
    }
    Ok(())
}
#[cfg(test)]
#[path = "budget_tests.rs"]
mod tests;
