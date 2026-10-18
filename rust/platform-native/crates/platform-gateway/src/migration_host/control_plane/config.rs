//! Explicit protocol rollout configuration. Factories remain inert by default.
use base64::Engine;
use ed25519_dalek::pkcs8::DecodePrivateKey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Configuration {
    pub cedar: ai_platform_auth::cedar::Configuration,
    pub evaluation: EvaluationPlacement,
    pub mcp: Option<McpConfiguration>,
    pub a2a: Option<A2aConfiguration>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct EvaluationPlacement {
    pub placement: Placement,
    pub concurrency: usize,
}
impl Default for EvaluationPlacement {
    fn default() -> Self {
        Self {
            placement: Placement::Shared,
            concurrency: 2,
        }
    }
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    #[default]
    Shared,
    Dedicated,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpConfiguration {
    pub resource: String,
    pub authorization_servers: Vec<String>,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct A2aConfiguration {
    pub origin: String,
    /// Actual immutable publication/version pairs, never a draft or mutable alias.
    pub publications: Vec<Publication>,
    pub signing_key_id: String,
    /// Resolved by the explicit file-secret config boundary; never Debug.
    pub signing_private_key: String,
    pub runtime_revision: String,
    pub typed_capabilities_revision: String,
    #[serde(default)]
    pub outbound: Vec<OutboundAgent>,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    pub publication_id: String,
    pub version_id: String,
}
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OutboundAgent {
    pub name: String,
    pub endpoint: String,
    pub address: std::net::IpAddr,
    pub bearer_token: String,
    pub agent_card_key_id: String,
    /// Explicit trusted peer Ed25519 key, raw 32 bytes in base64url without padding.
    pub agent_card_public_key: String,
    pub max_calls_per_day: u32,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    pub timeout_seconds: u64,
}
fn https(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|u| {
        u.scheme() == "https"
            && u.host_str().is_some()
            && u.username().is_empty()
            && u.password().is_none()
            && u.query().is_none()
            && u.fragment().is_none()
    })
}
impl Configuration {
    pub fn from_environment(environment: &BTreeMap<String, String>) -> Result<Self, &'static str> {
        let value = environment
            .get("GATEWAY_CONTROL_PLANE_CONFIGURATION")
            .map(|v| serde_json::from_str(v))
            .transpose()
            .map_err(|_| "gateway_control_plane_configuration")?
            .unwrap_or_default();
        Ok(value)
    }
    pub fn validate(&self, kernel_revision: &str) -> Result<(), &'static str> {
        if !(1..=32).contains(&self.evaluation.concurrency) {
            return Err("eval_concurrency");
        }
        if let Some(mcp) = &self.mcp {
            if !https(&mcp.resource)
                || !url::Url::parse(&mcp.resource).is_ok_and(|u| u.path() == "/mcp")
                || mcp.authorization_servers.is_empty()
                || mcp.authorization_servers.iter().any(|s| !https(s))
                || mcp.allowed_origins.iter().any(|s| !https(s))
            {
                return Err("mcp_configuration");
            }
        }
        if let Some(a) = &self.a2a {
            if !https(&a.origin)
                || url::Url::parse(&a.origin).is_ok_and(|u| u.path() != "/")
                || a.runtime_revision != kernel_revision
                || a.typed_capabilities_revision != "capability-contract/v1"
                || a.publications.is_empty()
                || a.signing_key_id.is_empty()
                || a.signing_key_id.len() > 128
                || a.signing_key_id.bytes().any(|b| b <= 32 || b == 127)
                || ed25519_dalek::SigningKey::from_pkcs8_pem(&a.signing_private_key).is_err()
            {
                return Err("a2a_activation_prerequisites");
            }
            let mut ids = std::collections::BTreeSet::new();
            for p in &a.publications {
                if uuid::Uuid::parse_str(&p.publication_id).is_err()
                    || uuid::Uuid::parse_str(&p.version_id).is_err()
                    || !ids.insert(&p.publication_id)
                {
                    return Err("a2a_publication");
                }
            }
            let mut names = std::collections::BTreeSet::new();
            for o in &a.outbound {
                if !https(&o.endpoint)
                    || !url::Url::parse(&o.endpoint)
                        .is_ok_and(|u| u.path().ends_with("/message:send"))
                    || o.agent_card_key_id.is_empty()
                    || o.agent_card_key_id.len() > 128
                    || o.agent_card_key_id.bytes().any(|b| b <= 32 || b == 127)
                    || !base64::engine::general_purpose::URL_SAFE_NO_PAD
                        .decode(&o.agent_card_public_key)
                        .ok()
                        .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                        .is_some_and(|bytes| {
                            ed25519_dalek::VerifyingKey::from_bytes(&bytes).is_ok()
                        })
                    || url::Url::parse(&o.endpoint).is_ok_and(|u| {
                        u.host_str()
                            .and_then(|h| {
                                h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok()
                            })
                            .is_some_and(|ip| ip != o.address)
                    })
                    || !names.insert(&o.name)
                    || o.name.is_empty()
                    || o.name.len() > 64
                    || !o
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                    || !public_address(o.address)
                    || o.bearer_token.is_empty()
                    || o.bearer_token.bytes().any(|b| !(33..=126).contains(&b))
                    || !(1..=10000).contains(&o.max_calls_per_day)
                    || !(1..=1024 * 1024).contains(&o.max_request_bytes)
                    || !(1..=4 * 1024 * 1024).contains(&o.max_response_bytes)
                    || !(1..=120).contains(&o.timeout_seconds)
                {
                    return Err("a2a_outbound_configuration");
                }
            }
        }
        Ok(())
    }
}
pub fn public_address(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v) => {
            let [a, b, _, _] = v.octets();
            !v.is_private()
                && !v.is_loopback()
                && !v.is_link_local()
                && !v.is_unspecified()
                && !v.is_multicast()
                && !v.is_broadcast()
                && !v.is_documentation()
                && a != 0
                && a < 224
                && !(a == 100 && (64..=127).contains(&b))
                && !(a == 198 && (b == 18 || b == 19))
                && !(a == 192 && b == 0)
        }
        std::net::IpAddr::V6(v) => {
            let segments = v.segments();
            segments[0] & 0xe000 == 0x2000
                && !(segments[0] == 0x2001 && (segments[1] < 0x200 || segments[1] == 0xdb8))
                && segments[0] != 0x2002
                && segments[0] != 0x3fff
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn external_peer_pin_cannot_be_private_reserved_or_mapped() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "169.254.169.254",
            "100.64.0.1",
            "198.18.0.1",
            "240.0.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
            "2001:db8::1",
            "2002:7f00:1::1",
        ] {
            assert!(!public_address(ip.parse().unwrap()), "{ip}");
        }
        assert!(public_address("8.8.8.8".parse().unwrap()));
        assert!(public_address("2606:4700:4700::1111".parse().unwrap()));
    }
}
