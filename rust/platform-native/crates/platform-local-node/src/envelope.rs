use crate::{Error, Result, canonical, digest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
/// Supplied by trusted configuration, never deserialized from a command.
pub trait PlatformVerifier: Send + Sync {
    fn verify(&self, key_id: &str, payload: &[u8], signature: &str) -> bool;
}
/// Implementations must atomically and durably consume a nonce only after verification.
pub trait ApprovalVerifier: Send + Sync {
    fn device_id(&self) -> &str;
    fn verify_and_consume(&self, payload: &[u8], signature: &str, nonce: &str) -> bool;
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Approval {
    pub approval_id: String,
    pub action_id: String,
    pub device_id: String,
    pub arguments_digest: String,
    pub target_snapshot_digest: String,
    pub policy_snapshot_digest: String,
    pub nonce: String,
    pub expires_at: f64,
    pub local_signature: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Action {
    pub action_id: String,
    pub idempotency_key: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub run_id: String,
    pub agent_id: String,
    pub agent_version: String,
    pub call_id: String,
    pub device_id: String,
    pub envelope_version: u32,
    pub capability: String,
    pub tool_name: String,
    pub operation: String,
    pub capability_lease_id: String,
    pub resource_refs: Vec<String>,
    pub arguments_digest: String,
    pub target_snapshot_digest: String,
    pub policy_snapshot_digest: String,
    pub nonce: String,
    pub issued_at: f64,
    pub expires_at: f64,
    pub platform_key_id: String,
    pub platform_signature: String,
    pub approval: Option<Approval>,
    pub trace_context: Option<std::collections::BTreeMap<String, String>>,
}
impl Action {
    pub fn signed_payload(&self) -> Value {
        let mut v = serde_json::to_value(self).expect("finite validated envelope");
        let o = v.as_object_mut().unwrap();
        o.remove("platform_signature");
        o.remove("trace_context");
        o.insert(
            "resource_refs_digest".into(),
            json!(digest(&json!(self.resource_refs))),
        );
        v
    }
    pub fn verify(&self, verifier: &dyn PlatformVerifier, now: f64) -> Result<()> {
        let fields = [
            &self.action_id,
            &self.idempotency_key,
            &self.tenant_id,
            &self.user_id,
            &self.session_id,
            &self.run_id,
            &self.agent_id,
            &self.agent_version,
            &self.call_id,
            &self.device_id,
            &self.capability,
            &self.tool_name,
            &self.operation,
            &self.capability_lease_id,
            &self.nonce,
            &self.platform_key_id,
            &self.arguments_digest,
            &self.target_snapshot_digest,
            &self.policy_snapshot_digest,
        ];
        if fields
            .iter()
            .any(|s| s.is_empty() || s.chars().count() > 512)
            || self.envelope_version != 1
            || self.resource_refs.is_empty()
            || self.resource_refs.len() > 64
            || self
                .resource_refs
                .iter()
                .any(|s| s.is_empty() || s.chars().count() > 1024)
            || self
                .resource_refs
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
                != self.resource_refs.len()
        {
            return Err(Error::Denied("invalid envelope identity"));
        }
        if !now.is_finite()
            || !self.issued_at.is_finite()
            || !self.expires_at.is_finite()
            || self.issued_at > now + 30.0
            || self.expires_at <= self.issued_at
            || now >= self.expires_at
        {
            return Err(Error::Denied("expired or invalid envelope"));
        }
        if let Some(a) = &self.approval {
            if !a.expires_at.is_finite()
                || a.expires_at > self.expires_at
                || [
                    &a.approval_id,
                    &a.action_id,
                    &a.device_id,
                    &a.arguments_digest,
                    &a.target_snapshot_digest,
                    &a.policy_snapshot_digest,
                    &a.nonce,
                    &a.local_signature,
                ]
                .iter()
                .any(|s| s.is_empty() || s.chars().count() > 512)
            {
                return Err(Error::Denied("invalid approval metadata"));
            }
        }
        if self.platform_signature.is_empty()
            || self.platform_signature.len() > 8192
            || !verifier.verify(
                &self.platform_key_id,
                canonical(&self.signed_payload()).as_bytes(),
                &self.platform_signature,
            )
        {
            return Err(Error::Denied("platform signature"));
        }
        Ok(())
    }
    pub fn validate(
        &self,
        args: &Value,
        lease: &str,
        resources: &[String],
        verifier: &dyn PlatformVerifier,
    ) -> Result<()> {
        self.verify(verifier, crate::now())?;
        if self.arguments_digest != digest(args)
            || self.capability_lease_id != lease
            || self.resource_refs != resources
        {
            return Err(Error::Denied("signed scope mismatch"));
        }
        Ok(())
    }
    pub fn approve(&self, target: &str, verifier: &dyn ApprovalVerifier) -> Result<()> {
        let approval = self
            .approval
            .as_ref()
            .ok_or(Error::Denied("approval required"))?;
        self.approve_with(approval, target, verifier)
    }
    /// Verify an operator-approved local receipt without mutating the
    /// platform-signed action envelope. The companion creates this receipt
    /// after its local prompt; it is deliberately separate from `Action` so
    /// adding the device signature cannot invalidate or alter the server
    /// signature over the original command.
    pub fn approve_with(
        &self,
        a: &Approval,
        target: &str,
        verifier: &dyn ApprovalVerifier,
    ) -> Result<()> {
        if target != self.target_snapshot_digest {
            return Err(Error::Stale);
        }
        if !a.expires_at.is_finite()
            || crate::now() >= a.expires_at
            || a.action_id != self.action_id
            || a.device_id != self.device_id
            || verifier.device_id() != self.device_id
            || a.arguments_digest != self.arguments_digest
            || a.policy_snapshot_digest != self.policy_snapshot_digest
        {
            return Err(Error::Denied("approval binding"));
        }
        if a.target_snapshot_digest != target {
            return Err(Error::Stale);
        }
        let mut v = serde_json::to_value(a).map_err(|_| Error::Protocol)?;
        v.as_object_mut().unwrap().remove("local_signature");
        if !verifier.verify_and_consume(canonical(&v).as_bytes(), &a.local_signature, &a.nonce) {
            return Err(Error::Denied("approval signature or replay"));
        }
        Ok(())
    }
}
