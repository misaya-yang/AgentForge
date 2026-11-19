//! Gateway-owned, credential-free execution/receipt contracts. Distinct from
//! companion ActionContext, which carries the platform and local approval signatures.
use super::{
    LocalNodeError as Error, Result,
    models::{digest_payload, valid_identifier},
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalNodeDeviceScope {
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub device_id: String,
    pub channel_id: String,
}
impl LocalNodeDeviceScope {
    pub fn validate(&self) -> Result<()> {
        if [
            &self.tenant_id,
            &self.user_id,
            &self.session_id,
            &self.device_id,
            &self.channel_id,
        ]
        .iter()
        .any(|s| !valid_identifier(s, 255))
        {
            return Err(Error::denied("local node device scope invalid"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalNodeCapability {
    pub capability: String,
    pub operation: String,
    pub effect: String,
    pub schema: Value,
    pub revision: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalNodeAction {
    pub scope: LocalNodeDeviceScope,
    pub lease_id: String,
    pub execution_id: String,
    pub run_id: String,
    pub tool_call_id: String,
    pub attempt_id: String,
    pub capability_revision: i64,
    pub capability_id: String,
    pub effect: String,
    pub operation: String,
    pub arguments: Value,
    pub arguments_sha256: String,
    pub idempotency_key: String,
    pub approval_id: Option<String>,
    pub grant_id: Option<String>,
    pub grant_revision: Option<i64>,
    pub dispatch_fence: Option<String>,
}
impl LocalNodeAction {
    pub fn validate(&self) -> Result<()> {
        self.scope.validate()?;
        if [
            &self.execution_id,
            &self.lease_id,
            &self.run_id,
            &self.tool_call_id,
            &self.attempt_id,
            &self.operation,
            &self.idempotency_key,
            &self.capability_id,
            &self.effect,
        ]
        .iter()
        .any(|v| !valid_identifier(v, 255))
            || !["read", "write", "unknown"].contains(&self.effect.as_str())
            || self
                .approval_id
                .as_ref()
                .is_some_and(|v| !valid_identifier(v, 255))
            || self.capability_revision < 1
            || self.grant_revision.is_some_and(|v| v < 1)
            || !self.arguments.is_object()
        {
            return Err(Error::denied("local node action invalid"));
        }
        if (self.effect == "read") != self.approval_id.is_none() {
            return Err(Error::denied("local node action approval scope invalid"));
        }
        if self.arguments_sha256 != format!("sha256:{}", digest_payload(&self.arguments)) {
            return Err(Error::denied("local node arguments hash mismatch"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LocalNodeReceiptStatus {
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Timeout,
    SideEffectUnknown,
}
impl LocalNodeReceiptStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Timeout => "timeout",
            Self::SideEffectUnknown => "side_effect_unknown",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LocalNodeReceipt {
    pub execution_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub device_id: String,
    pub dispatch_fence: String,
    pub sequence: i64,
    pub status: LocalNodeReceiptStatus,
    pub event: String,
    pub payload: Value,
    pub channel_id: Option<String>,
}
impl LocalNodeReceipt {
    pub fn validate(&self, after: i64) -> Result<()> {
        if self.execution_id.is_empty()
            || self.device_id.is_empty()
            || self.dispatch_fence.is_empty()
            || self.sequence <= after
            || (self.status == LocalNodeReceiptStatus::SideEffectUnknown
                && self.event != "action.side_effect_unknown")
            || !self.payload.is_object()
        {
            return Err(Error::denied("local node receipt invalid"));
        }
        Ok(())
    }
}
