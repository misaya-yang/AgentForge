//! File-mounted workload credentials and exact, TLS-verified SPIFFE identities.
//! No HTTP header can construct VerifiedPeer. User/tenant/signature authorization
//! is deliberately outside this transport trust boundary.
mod der;
pub mod http;
pub mod listener;
pub mod secrets;
pub mod tls;
use std::{collections::BTreeSet, fmt};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Disabled,
    Dual,
    Required,
}
impl Mode {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "disabled" => Ok(Self::Disabled),
            "dual" => Ok(Self::Dual),
            "required" => Ok(Self::Required),
            _ => Err(Error("workload_mode_invalid")),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ServiceIdentity(String);
impl ServiceIdentity {
    pub fn parse(raw: &str) -> Result<Self> {
        if raw.len() > 2048 || raw.bytes().any(|b| !b.is_ascii() || b.is_ascii_control()) {
            return Err(Error("workload_spiffe_invalid"));
        }
        let rest = raw
            .strip_prefix("spiffe://")
            .ok_or(Error("workload_spiffe_invalid"))?;
        let (domain, path) = rest
            .split_once('/')
            .ok_or(Error("workload_spiffe_invalid"))?;
        if domain.is_empty()
            || domain.len() > 253
            || domain.split('.').any(|label| {
                label.is_empty()
                    || label.len() > 63
                    || label.starts_with('-')
                    || label.ends_with('-')
                    || !label
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            })
            || path.is_empty()
            || path.split('/').any(|segment| {
                segment.is_empty()
                    || segment == "."
                    || segment == ".."
                    || !segment
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            return Err(Error("workload_spiffe_invalid"));
        }
        Ok(Self(raw.into()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
#[derive(Clone, Debug)]
pub struct PeerRules {
    identities: BTreeSet<ServiceIdentity>,
}
impl PeerRules {
    pub fn new(identities: impl IntoIterator<Item = ServiceIdentity>) -> Result<Self> {
        let identities = identities.into_iter().collect::<BTreeSet<_>>();
        if identities.is_empty() || identities.len() > 64 {
            return Err(Error("workload_peer_allowlist_invalid"));
        }
        Ok(Self { identities })
    }
    pub fn admits(&self, id: &ServiceIdentity) -> bool {
        self.identities.contains(id)
    }
}
/// Provenance is connection-local TLS state, never client-provided HTTP metadata.
#[derive(Clone, Debug)]
pub struct VerifiedPeer {
    identity: ServiceIdentity,
    leaf_sha256: String,
}
impl VerifiedPeer {
    pub fn identity(&self) -> &ServiceIdentity {
        &self.identity
    }
    pub fn leaf_sha256(&self) -> &str {
        &self.leaf_sha256
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub(crate) &'static str);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
/// Parse unverified certificate metadata for provisioning diagnostics only.
/// This does not create VerifiedPeer or authorize any request.
pub fn certificate_identity(certificate_der: &[u8]) -> Result<ServiceIdentity> {
    der::identity(certificate_der)
}

#[cfg(test)]
mod tests;
