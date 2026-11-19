use serde::Serialize;
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Cadence {
    Day,
    Month,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Family {
    pub name: &'static str,
    pub clock: &'static str,
    pub cadence: Cadence,
}
macro_rules! family {
    ($name:literal,$clock:literal,$cadence:ident) => {
        Family {
            name: $name,
            clock: $clock,
            cadence: Cadence::$cadence,
        }
    };
}
pub const ALL: &[Family] = &[
    family!("gateway.usage_records", "created_at", Month),
    family!("gateway.request_traces", "created_at", Day),
    family!("gateway.audit_logs", "created_at", Month),
    family!("gateway.billing_events", "event_time", Month),
    family!("gateway.agent_trace_spans", "started_at", Day),
    family!("gateway.agent_trace_events", "occurred_at", Day),
    family!("gateway.tool_audit_log", "created_at", Month),
    family!("gateway.login_audit", "created_at", Month),
    family!("gateway.service_health_records", "checked_at", Month),
    family!("assistant.assistant_runtime_items", "created_at", Day),
    family!("assistant.assistant_audit_events", "created_at", Month),
    family!("assistant.assistant_capability_events", "created_at", Day),
    family!("knowledge.kb_document_progress_events", "created_at", Month),
    family!(
        "assistant.assistant_runtime_model_calls",
        "reserved_at",
        Month
    ),
    family!(
        "assistant.assistant_runtime_model_leases",
        "issued_at",
        Month
    ),
    family!("gateway.knowledge_model_calls", "issued_at", Month),
    family!("gateway.task_reconciliation_audit", "resolved_at", Month),
    family!(
        "gateway.price_book_v2_activation_audit",
        "created_at",
        Month
    ),
    family!("gateway.price_book_v2_import_audit", "created_at", Month),
    family!("gateway.price_book_v2_usage_receipts", "created_at", Month),
    family!(
        "gateway.price_book_v2_accounting_checkpoints",
        "updated_at",
        Month
    ),
    family!("gateway.agent_traces", "started_at", Day),
];
pub fn family(name: &str) -> super::Result<Family> {
    ALL.iter()
        .find(|f| f.name == name)
        .copied()
        .ok_or_else(|| "unknown append family".into())
}
