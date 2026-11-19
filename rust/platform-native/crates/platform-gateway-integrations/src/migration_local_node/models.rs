use super::{LocalNodeError as Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    time::{SystemTime, UNIX_EPOCH},
};
pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |v| v.as_secs_f64())
}
pub fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
/// Python's sorted-key, compact, ensure_ascii=False JSON representation.
pub fn canonical_json(value: &Value) -> String {
    fn write(value: &Value, out: &mut String) {
        match value {
            Value::Object(map) => {
                out.push('{');
                let mut keys: Vec<_> = map.keys().collect();
                keys.sort();
                for (index, key) in keys.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    out.push_str(&serde_json::to_string(key).unwrap_or_default());
                    out.push(':');
                    write(&map[*key], out);
                }
                out.push('}');
            }
            Value::Array(items) => {
                out.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index > 0 {
                        out.push(',');
                    }
                    write(item, out);
                }
                out.push(']');
            }
            Value::Number(number) if number.is_f64() => {
                out.push_str(&python_float(number.as_f64().unwrap_or(0.0)));
            }
            _ => out.push_str(&value.to_string()),
        }
    }
    let mut out = String::new();
    write(value, &mut out);
    out
}
pub fn digest_payload(value: &Value) -> String {
    sha256(canonical_json(value).as_bytes())
}
pub fn valid_identifier(value: &str, max: usize) -> bool {
    !value.is_empty() && value.chars().count() <= max && !value.chars().any(|c| c < '\u{20}')
}
pub fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionStatus {
    Proposed,
    PolicyCheck,
    AwaitingApproval,
    Dispatched,
    Running,
    Observed,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
    Unknown,
}
impl ActionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::PolicyCheck => "policy_check",
            Self::AwaitingApproval => "awaiting_approval",
            Self::Dispatched => "dispatched",
            Self::Running => "running",
            Self::Observed => "observed",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::Unknown => "unknown",
        }
    }
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Cancelled | Self::Interrupted | Self::Unknown
        )
    }
    pub fn allows(self, next: Self) -> bool {
        match self {
            Self::Proposed => next == Self::PolicyCheck,
            Self::PolicyCheck => matches!(
                next,
                Self::AwaitingApproval
                    | Self::Dispatched
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Interrupted
            ),
            Self::AwaitingApproval => matches!(
                next,
                Self::Dispatched | Self::Failed | Self::Cancelled | Self::Interrupted
            ),
            Self::Dispatched => matches!(
                next,
                Self::Running
                    | Self::Observed
                    | Self::Succeeded
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Interrupted
                    | Self::Unknown
            ),
            Self::Running => matches!(
                next,
                Self::Observed
                    | Self::Succeeded
                    | Self::Failed
                    | Self::Cancelled
                    | Self::Interrupted
                    | Self::Unknown
            ),
            Self::Observed => next.terminal(),
            _ => false,
        }
    }
}
pub trait PlatformSignatureVerifier: Send + Sync {
    fn verify(&self, key_id: &str, payload: &[u8], signature: &str) -> bool;
}
pub trait TrustedLocalApprovalVerifier: Send + Sync {
    fn device_id(&self) -> &str;
    fn verify_and_consume(&self, payload: &[u8], signature: &str, nonce: &str) -> Result<bool>;
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalProof {
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
impl ApprovalProof {
    pub fn canonical_local_payload(&self) -> Result<Vec<u8>> {
        let mut v = serde_json::to_value(self)?;
        v.as_object_mut().unwrap().remove("local_signature");
        Ok(canonical_json(&v).into_bytes())
    }
    pub fn validate_and_consume(
        &self,
        action: &ActionContext,
        target: &str,
        verifier: Option<&dyn TrustedLocalApprovalVerifier>,
    ) -> Result<()> {
        if !self.expires_at.is_finite() || now() >= self.expires_at {
            return Err(Error::approval("approval expired or lifetime invalid"));
        }
        if self.action_id != action.action_id || self.device_id != action.device_id {
            return Err(Error::approval("approval action or device changed"));
        }
        if self.arguments_digest != action.arguments_digest {
            return Err(Error::approval("approval arguments changed"));
        }
        if self.policy_snapshot_digest != action.policy_snapshot_digest {
            return Err(Error::approval("approval policy changed"));
        }
        if self.target_snapshot_digest != target {
            return Err(Error::stale("approval target changed"));
        }
        let verifier = verifier
            .ok_or_else(|| Error::approval("trusted local approval verifier is unavailable"))?;
        if verifier.device_id() != action.device_id
            || !valid_identifier(&self.nonce, 512)
            || self.local_signature.is_empty()
        {
            return Err(Error::approval("trusted local approval receipt is invalid"));
        }
        if !verifier.verify_and_consume(
            &self.canonical_local_payload()?,
            &self.local_signature,
            &self.nonce,
        )? {
            return Err(Error::approval(
                "trusted local approval verification failed",
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionContext {
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
    #[serde(default)]
    pub approval: Option<ApprovalProof>,
    #[serde(default)]
    pub trace_context: Option<BTreeMap<String, String>>,
}
impl ActionContext {
    pub fn canonical_signed_payload(&self) -> Result<Vec<u8>> {
        let mut value = serde_json::to_value(self)?;
        let map = value.as_object_mut().unwrap();
        map.remove("platform_signature");
        map.remove("trace_context");
        map.insert(
            "resource_refs_digest".into(),
            json!(digest_payload(&json!(self.resource_refs))),
        );
        Ok(canonical_json(&value).into_bytes())
    }
    pub fn validate_metadata(&self) -> Result<()> {
        let ids = [
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
        if ids.iter().any(|v| !valid_identifier(v, 512)) || self.envelope_version != 1 {
            return Err(Error::denied(
                "action envelope identity or version is invalid",
            ));
        }
        if self.resource_refs.is_empty()
            || self.resource_refs.len() > 64
            || self
                .resource_refs
                .iter()
                .any(|v| !valid_identifier(v, 1024))
            || self.resource_refs.iter().collect::<HashSet<_>>().len() != self.resource_refs.len()
        {
            return Err(Error::denied(
                "action envelope resource references are invalid",
            ));
        }
        if !self.issued_at.is_finite()
            || !self.expires_at.is_finite()
            || self.issued_at > now() + 30.0
            || self.expires_at <= self.issued_at
            || now() >= self.expires_at
        {
            return Err(Error::denied("action envelope expired or lifetime invalid"));
        }
        if let Some(a) = &self.approval {
            if [
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
            .any(|v| !valid_identifier(v, 512))
                || !a.expires_at.is_finite()
                || a.expires_at > self.expires_at
            {
                return Err(Error::denied("action approval metadata is invalid"));
            }
        }
        Ok(())
    }
    pub fn verify_platform_signature(
        &self,
        verifier: Option<&dyn PlatformSignatureVerifier>,
    ) -> Result<()> {
        self.validate_metadata()?;
        let verifier =
            verifier.ok_or_else(|| Error::denied("platform signature verifier is unavailable"))?;
        if self.platform_signature.is_empty()
            || self.platform_signature.len() > 8192
            || !verifier.verify(
                &self.platform_key_id,
                &self.canonical_signed_payload()?,
                &self.platform_signature,
            )
        {
            return Err(Error::denied(
                "platform action signature verification failed",
            ));
        }
        Ok(())
    }
    pub fn validate_payload(
        &self,
        arguments: &Value,
        verifier: Option<&dyn PlatformSignatureVerifier>,
        lease: Option<&str>,
        refs: Option<&[String]>,
    ) -> Result<()> {
        self.validate_metadata()?;
        if lease.is_some_and(|v| v != self.capability_lease_id)
            || refs.is_some_and(|v| v != self.resource_refs)
        {
            return Err(Error::denied(
                "action lease or resources do not match local scope",
            ));
        }
        if !arguments.is_object() || digest_payload(arguments) != self.arguments_digest {
            return Err(Error::denied(
                "action arguments do not match signed envelope",
            ));
        }
        self.verify_platform_signature(verifier)
    }
    pub fn require_approval(
        &self,
        target: &str,
        verifier: Option<&dyn TrustedLocalApprovalVerifier>,
    ) -> Result<()> {
        if self.target_snapshot_digest != target {
            return Err(Error::stale("action envelope target changed"));
        }
        self.approval
            .as_ref()
            .ok_or_else(|| Error::approval("local approval is required"))?
            .validate_and_consume(self, target, verifier)
    }
}

// Reuse the existing native catalog normalization of Python JSON binary64 spelling.
fn python_float(value: f64) -> String {
    let raw = serde_json::to_string(&value).expect("JSON values contain finite numbers");
    let (sign, unsigned) = if let Some(value) = raw.strip_prefix('-') {
        ("-", value)
    } else {
        ("", raw.as_str())
    };
    let (mantissa, exponent) = unsigned.split_once('e').map_or((unsigned, 0), |(m, e)| {
        (m, e.parse::<i32>().expect("serializer exponent"))
    });
    let decimal_at = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let mut digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let zeros = digits.bytes().take_while(|c| *c == b'0').count();
    if zeros == digits.len() {
        return format!("{sign}0.0");
    }
    let scientific_exponent = exponent + decimal_at - zeros as i32 - 1;
    digits.drain(..zeros);
    while digits.len() > 1 && digits.ends_with('0') {
        digits.pop();
    }
    if !(-4..16).contains(&scientific_exponent) {
        let fraction = if digits.len() == 1 {
            String::new()
        } else {
            format!(".{}", &digits[1..])
        };
        return format!("{sign}{}{fraction}e{scientific_exponent:+03}", &digits[..1]);
    }
    let decimal = scientific_exponent + 1;
    if decimal <= 0 {
        format!("{sign}0.{}{}", "0".repeat((-decimal) as usize), digits)
    } else if decimal as usize >= digits.len() {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(decimal as usize - digits.len())
        )
    } else {
        format!(
            "{sign}{}.{}",
            &digits[..decimal as usize],
            &digits[decimal as usize..]
        )
    }
}
