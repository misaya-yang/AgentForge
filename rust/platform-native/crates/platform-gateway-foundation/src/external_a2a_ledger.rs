//! Correlation receipts over existing audit_logs. Never accepts caller usage,
//! pricing, permission or settlement assertions. Authenticated entrypoints pass
//! their already verified actor and actual server/provider result identifiers.
use ai_platform_service_primitives::data_connection::{
    DatabaseRole, TenantScope, append_store, bind_transaction,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgPool;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Model,
    Mcp,
    A2a,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub kind: Kind,
    pub request_id: String,
    pub client_request_id: Option<String>,
    pub request_sha256: Option<String>,
    pub wire_response_id: Option<String>,
    pub tool: Option<String>,
    pub local_message_id: Option<String>,
    pub remote_task_id: Option<String>,
    pub outcome: String,
    pub http_status: u16,
}
fn valid(s: &str) -> bool {
    !s.is_empty() && s.len() <= 255 && !s.chars().any(char::is_control)
}
impl Receipt {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self
            .client_request_id
            .as_ref()
            .is_some_and(|v| uuid::Uuid::parse_str(v).is_err())
            || self.request_sha256.as_ref().is_some_and(|s| {
                s.len() != 64
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err("external_ledger_correlation_invalid");
        }
        if !valid(&self.request_id)
            || !["completed", "denied", "canceled", "unknown"].contains(&self.outcome.as_str())
            || !(100..=599).contains(&self.http_status)
            || [
                &self.wire_response_id,
                &self.tool,
                &self.local_message_id,
                &self.remote_task_id,
            ]
            .iter()
            .any(|x| x.as_ref().is_some_and(|s| !valid(s)))
        {
            return Err("external_ledger_receipt_invalid");
        }
        if self.kind == Kind::Model
            && self.outcome == "completed"
            && self.wire_response_id.is_none()
        {
            return Err("external_ledger_wire_identity_missing");
        }
        if self.kind == Kind::Mcp && self.tool.is_none() {
            return Err("external_ledger_tool_missing");
        }
        Ok(())
    }
}
pub async fn record(
    pool: &PgPool,
    tenant: &str,
    user: &str,
    receipt: &Receipt,
) -> Result<i64, &'static str> {
    receipt.validate()?;
    if !valid(user) {
        return Err("external_ledger_actor_invalid");
    }
    let scope = TenantScope::new(tenant).map_err(|_| "external_ledger_tenant_invalid")?;
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        let mut tx = pool
            .begin()
            .await
            .map_err(|_| "external_ledger_begin_failed")?;
        bind_transaction(&mut tx, &scope, DatabaseRole::Gateway, 2000)
            .await
            .map_err(|_| "external_ledger_context_failed")?;
        let id = record_in(&mut tx, tenant, user, receipt).await?;
        tx.commit()
            .await
            .map_err(|_| "external_ledger_commit_unconfirmed")?;
        Ok(id)
    })
    .await
    .map_err(|_| "external_ledger_timeout")?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_fabricated_completed_identity() {
        let mut r = Receipt {
            kind: Kind::Model,
            request_id: "actual-server-id".into(),
            client_request_id: None,
            request_sha256: None,
            wire_response_id: None,
            tool: None,
            local_message_id: None,
            remote_task_id: None,
            outcome: "completed".into(),
            http_status: 200,
        };
        assert!(r.validate().is_err());
        r.wire_response_id = Some("actual-provider-id".into());
        assert!(r.validate().is_ok());
        r.outcome = "settled_by_client".into();
        assert!(r.validate().is_err());
    }
    #[test]
    fn no_arbitrary_or_secret_payload() {
        assert!(serde_json::from_value::<Receipt>(json!({"kind":"model","request_id":"r","outcome":"denied","http_status":403,"prompt":"secret"})).is_err());
    }
}

pub async fn record_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: &str,
    user: &str,
    receipt: &Receipt,
) -> Result<i64, &'static str> {
    receipt.validate()?;
    if !valid(user) {
        return Err("external_ledger_actor_invalid");
    }
    let scope = TenantScope::new(tenant).map_err(|_| "external_ledger_tenant_invalid")?;
    bind_transaction(tx, &scope, DatabaseRole::Gateway, 2000)
        .await
        .map_err(|_| "external_ledger_context_failed")?;
    append_store::bind_tenant(tx, "gateway.audit_logs", tenant)
        .await
        .map_err(|_| "external_ledger_append_failed")?;
    let summary = serde_json::to_value(receipt).map_err(|_| "external_ledger_encode_failed")?;
    let id:i64=sqlx::query_scalar("INSERT INTO gateway.audit_logs(event_type,user_id,tenant_id,resource_type,resource_id,action,request_summary,response_summary,status) VALUES('external_framework_wire_v1',$1,$2,'external_framework_request',$3,'observe',$4,$5,$6) RETURNING id")
   .bind(user).bind(tenant).bind(&receipt.request_id).bind(json!({"schema":"agentforge.external_framework_wire/v1"})).bind(summary).bind(if receipt.outcome=="completed"{"success"}else{receipt.outcome.as_str()}).fetch_one(&mut **tx).await.map_err(|_|"external_ledger_write_unconfirmed")?;
    Ok(id)
}
