//! Pinned, content-free GenAI operations and verified accounting observations.
//! Tenant dimensions must come from the authenticated/settled business record.
use opentelemetry::{
    Context, KeyValue, global,
    trace::{SpanKind, Status, TraceContextExt, Tracer},
};
use sha2::{Digest, Sha256};
use std::time::Instant;
pub const SEMCONV_VERSION: &str = "1.37.0";
pub const SCHEMA_URL: &str = "https://opentelemetry.io/schemas/1.37.0";
const SCOPE: &str = "agentforge.gen_ai";
#[derive(Clone, Copy)]
pub enum OperationKind {
    Chat,
    Embeddings,
    InvokeAgent,
    ExecuteTool,
    Retrieval,
    Ingestion,
}
impl OperationKind {
    fn name(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embeddings => "embeddings",
            Self::InvokeAgent => "invoke_agent",
            Self::ExecuteTool => "execute_tool",
            Self::Retrieval => "retrieval",
            Self::Ingestion => "ingestion",
        }
    }
}
/// No arbitrary attribute/event/error API: exported fields cannot capture content.
pub struct Operation {
    context: Context,
    started: Instant,
    kind: OperationKind,
    success: bool,
}
impl Operation {
    pub fn start(kind: OperationKind) -> Self {
        let parent = Context::current();
        let tracer = global::tracer_with_scope(
            opentelemetry::InstrumentationScope::builder(SCOPE)
                .with_schema_url(SCHEMA_URL)
                .build(),
        );
        let span = tracer
            .span_builder(kind.name())
            .with_kind(SpanKind::Client)
            .with_attributes([KeyValue::new("gen_ai.operation.name", kind.name())])
            .start_with_context(&tracer, &parent);
        Self {
            context: parent.with_span(span),
            started: Instant::now(),
            kind,
            success: false,
        }
    }
    pub fn context(&self) -> Context {
        self.context.clone()
    }
    pub fn success(&mut self) {
        self.success = true;
        self.context.span().set_status(Status::Ok);
    }
}
impl Drop for Operation {
    fn drop(&mut self) {
        let _active = self.context.clone().attach();
        if !self.success {
            self.context
                .span()
                .set_status(Status::error("operation_incomplete"));
        }
        global::meter(SCOPE)
            .f64_histogram("gen_ai.client.operation.duration")
            .with_unit("s")
            .build()
            .record(
                self.started.elapsed().as_secs_f64(),
                &[
                    KeyValue::new("gen_ai.operation.name", self.kind.name()),
                    KeyValue::new("outcome", if self.success { "ok" } else { "incomplete" }),
                ],
            );
        self.context.span().end();
    }
}
/// Actual charged USD only, after the durable usage commit; never an estimate.
/// Returns false for invalid dimensions/amounts without affecting accounting.
/// The caller owns deduplication at its existing exactly-once accounting boundary.
pub fn record_settled_usage(tenant: &str, model: &str, input: u64, output: u64, usd: f64) -> bool {
    let Some(attributes) = usage_attributes(tenant, model, usd) else {
        return false;
    };
    let meter = global::meter(SCOPE);
    let tokens = meter
        .u64_counter("agentforge.gen_ai.token.usage")
        .with_unit("{token}")
        .build();
    for (kind, count) in [("input", input), ("output", output)] {
        let mut dimensions = attributes.clone();
        dimensions.push(KeyValue::new("gen_ai.token.type", kind));
        tokens.add(count, &dimensions);
    }
    record_settled_cost(tenant, model, usd)
}

/// Request-metered work has an actual charged amount and no applicable token
/// count. Emit its cost without manufacturing zero-token observations.
pub fn record_settled_cost(tenant: &str, model: &str, usd: f64) -> bool {
    let Some(attributes) = usage_attributes(tenant, model, usd) else {
        return false;
    };
    global::meter(SCOPE)
        .f64_counter("agentforge.gen_ai.cost")
        .with_unit("USD")
        .build()
        .add(usd, &attributes);
    true
}
fn usage_attributes(tenant: &str, model: &str, usd: f64) -> Option<Vec<KeyValue>> {
    let tenant = tenant_dimension(tenant)?;
    if model.is_empty()
        || model.len() > 128
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/-".contains(&b))
        || !usd.is_finite()
        || usd < 0.0
    {
        return None;
    }
    Some(vec![
        KeyValue::new("agentforge.tenant.id", tenant),
        KeyValue::new("gen_ai.request.model", model.to_owned()),
    ])
}
/// Stable opaque dimension across native and Runtime; IDs are case sensitive in PostgreSQL.
/// Hashing supports the actual varchar tenant contract without exporting names or user content.
pub fn tenant_dimension(tenant: &str) -> Option<String> {
    if tenant.trim().is_empty() || tenant.len() > 255 || tenant.chars().any(char::is_control) {
        return None;
    }
    Some(format!("sha256:{:x}", Sha256::digest(tenant.as_bytes())))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn usage_dimensions_are_canonical_and_content_free() {
        let attrs = usage_attributes(
            "550E8400-E29B-41D4-A716-446655440000",
            "provider/model-1",
            0.005,
        )
        .unwrap();
        assert_eq!(attrs.len(), 2);
        assert!(attrs[0].value.to_string().starts_with("sha256:"));
        assert_eq!(tenant_dimension("tenant-a"), tenant_dimension("tenant-a"));
        assert_ne!(tenant_dimension("Tenant-a"), tenant_dimension("tenant-a"));
        assert!(!attrs[0].value.to_string().contains("550E8400"));
        for (tenant, model, cost) in [
            ("", "model", 1.0),
            (
                "550e8400-e29b-41d4-a716-446655440000",
                "prompt with spaces",
                1.0,
            ),
            ("550e8400-e29b-41d4-a716-446655440000", "model", f64::NAN),
            ("550e8400-e29b-41d4-a716-446655440000", "model", -1.0),
        ] {
            assert!(usage_attributes(tenant, model, cost).is_none());
        }
    }
    #[test]
    fn rejected_measurements_do_not_emit_or_fail_accounting() {
        assert!(!record_settled_usage("", "model", 1, 2, f64::INFINITY));
        assert!(record_settled_usage(
            "550e8400-e29b-41d4-a716-446655440000",
            "model",
            1,
            2,
            0.0
        ));
    }
}
