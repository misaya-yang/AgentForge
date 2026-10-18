//! Workload authentication is transport admission, never user/tenant authority.
//! Keep signed actor, tenant, capability and lease verification after this gate.
use ai_platform_service_primitives::workload_identity::{
    Mode, PeerRules, ServiceIdentity, VerifiedPeer,
    secrets::{SecretBytes, environment_secret},
};
use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    MutualTls,
    LegacyBearer,
}
#[derive(Clone, Debug)]
pub struct WorkloadAuthentication {
    identity: Option<ServiceIdentity>,
    kind: CredentialKind,
}
impl WorkloadAuthentication {
    pub fn identity(&self) -> Option<&ServiceIdentity> {
        self.identity.as_ref()
    }
    pub fn kind(&self) -> CredentialKind {
        self.kind
    }
    /// Business handlers require this trusted middleware proof. It contains no
    /// user/tenant grants and cannot be synthesized from request headers.
    pub fn require(extensions: &axum::http::Extensions) -> Result<&Self, Error> {
        extensions.get::<Self>().ok_or(Error::Credentials)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Configuration,
    Credentials,
    IdentityHints,
    PeerDenied,
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let status = match self {
            Self::Configuration => StatusCode::SERVICE_UNAVAILABLE,
            Self::PeerDenied | Self::IdentityHints => StatusCode::FORBIDDEN,
            Self::Credentials => StatusCode::UNAUTHORIZED,
        };
        (
            status,
            axum::Json(serde_json::json!({"detail":"workload_authentication_rejected"})),
        )
            .into_response()
    }
}
pub struct WorkloadAuth {
    mode: Mode,
    legacy: Option<SecretBytes>,
    rules: PeerRules,
    route_peers: BTreeMap<(String, String), std::collections::BTreeSet<ServiceIdentity>>,
}
impl WorkloadAuth {
    /// Gateway's config loader resolves `_FILE` once before process assembly.
    /// This explicit in-memory boundary does not discover an environment secret.
    pub fn from_configuration(
        mode: Mode,
        legacy: Option<Vec<u8>>,
        rules: PeerRules,
    ) -> Result<Self, Error> {
        Self::new(mode, legacy.map(SecretBytes::from_configuration), rules)
    }
    pub fn new(mode: Mode, legacy: Option<SecretBytes>, rules: PeerRules) -> Result<Self, Error> {
        if mode == Mode::Required {
            if legacy.is_some() {
                return Err(Error::Configuration);
            }
        } else if legacy.as_ref().is_none_or(|secret| {
            secret.expose().len() < 32
                || secret
                    .expose()
                    .iter()
                    .any(|b| !b.is_ascii() || b.is_ascii_whitespace() || b.is_ascii_control())
        }) {
            return Err(Error::Configuration);
        }
        Ok(Self {
            mode,
            legacy,
            rules,
            route_peers: BTreeMap::new(),
        })
    }
    /// Existing inline internal bearer settings must be migrated by the config
    /// owner. Required mode rejects even a retained `_FILE` setting explicitly.
    pub fn from_environment(
        env: &BTreeMap<String, String>,
        rules: PeerRules,
    ) -> Result<Self, Error> {
        let mode = Mode::parse(
            env.get("AI_PLATFORM_WORKLOAD_MTLS_MODE")
                .map(String::as_str)
                .unwrap_or("disabled"),
        )
        .map_err(|_| Error::Configuration)?;
        if mode == Mode::Required
            && (env.contains_key("AI_PLATFORM_INTERNAL_TOKEN_FILE")
                || env
                    .get("AI_PLATFORM_INTERNAL_TOKEN")
                    .is_some_and(|v| !v.is_empty()))
        {
            return Err(Error::Configuration);
        }
        let secret = if mode == Mode::Required {
            None
        } else {
            environment_secret(env, "AI_PLATFORM_INTERNAL_TOKEN")
                .map_err(|_| Error::Configuration)?
        };
        Self::new(mode, secret, rules)
    }
    /// An exact route may accept a subset of the already admitted TLS peers.
    /// Empty peers deny certificate callers on that route. Legacy transport in
    /// an explicit dual window still requires the original business signature.
    pub fn with_route_peers(
        mut self,
        routes: impl IntoIterator<Item = (String, String)>,
        peers: impl IntoIterator<Item = ServiceIdentity>,
    ) -> Result<Self, Error> {
        let peers = peers.into_iter().collect::<std::collections::BTreeSet<_>>();
        if peers.iter().any(|peer| !self.rules.admits(peer)) {
            return Err(Error::Configuration);
        }
        for (method, path) in routes {
            if method != "POST"
                || !path.starts_with("/internal/")
                || path.contains(['%', '?', '#', '\\'])
                || path.contains("//")
                || path.split('/').any(|part| part == "." || part == "..")
                || self
                    .route_peers
                    .insert((method, path), peers.clone())
                    .is_some()
            {
                return Err(Error::Configuration);
            }
        }
        Ok(self)
    }
    fn authenticate_route(
        &self,
        method: &str,
        path: &str,
        headers: &HeaderMap,
        peer: Option<&VerifiedPeer>,
    ) -> Result<WorkloadAuthentication, Error> {
        let proof = self.authenticate(headers, peer)?;
        if !self.route_admits(method, path, proof.identity()) {
            return Err(Error::PeerDenied);
        }
        Ok(proof)
    }
    fn route_admits(&self, method: &str, path: &str, identity: Option<&ServiceIdentity>) -> bool {
        // Only TLS carries a service identity; legacy requests are independently
        // signed, caller-scoped and replay-checked by each business endpoint.
        identity.is_none_or(|peer| {
            self.route_peers
                .get(&(method.into(), path.into()))
                .is_none_or(|peers| peers.contains(peer))
        })
    }
    pub fn mode(&self) -> Mode {
        self.mode
    }
    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        peer: Option<&VerifiedPeer>,
    ) -> Result<WorkloadAuthentication, Error> {
        // Reject common identity-hint headers outright, including a proxy's XFCC.
        // A mesh must pass end-to-end TLS or use a separately admitted UDS adapter.
        if [
            "x-spiffe-id",
            "x-workload-identity",
            "x-forwarded-client-cert",
            "x-ssl-client-cert",
            "x-client-cert",
        ]
        .iter()
        .any(|name| headers.contains_key(*name))
        {
            return Err(Error::IdentityHints);
        }
        if let Some(peer) = peer {
            if self.mode == Mode::Disabled || !self.rules.admits(peer.identity()) {
                return Err(Error::PeerDenied);
            }
            return Ok(WorkloadAuthentication {
                identity: Some(peer.identity().clone()),
                kind: CredentialKind::MutualTls,
            });
        }
        if self.mode == Mode::Required {
            return Err(Error::Credentials);
        }
        if headers.get_all("authorization").iter().count() != 1 {
            return Err(Error::Credentials);
        }
        let raw = headers
            .get("authorization")
            .and_then(|h| h.to_str().ok())
            .ok_or(Error::Credentials)?;
        let mut parts = raw.split_whitespace();
        if !parts
            .next()
            .is_some_and(|v| v.eq_ignore_ascii_case("bearer"))
        {
            return Err(Error::Credentials);
        }
        let supplied = parts.next().ok_or(Error::Credentials)?;
        if parts.next().is_some() {
            return Err(Error::Credentials);
        }
        let expected = self.legacy.as_ref().ok_or(Error::Configuration)?.expose();
        let actual = Sha256::digest(supplied.as_bytes());
        let desired = Sha256::digest(expected);
        let mismatch = actual
            .iter()
            .zip(desired.iter())
            .fold(0u8, |sum, (a, b)| sum | (a ^ b));
        if supplied.len() != expected.len() || mismatch != 0 {
            return Err(Error::Credentials);
        }
        Ok(WorkloadAuthentication {
            identity: None,
            kind: CredentialKind::LegacyBearer,
        })
    }
    pub fn protect(self: Arc<Self>, router: Router) -> Router {
        router.layer(middleware::from_fn_with_state(self, admit))
    }
}
async fn admit(
    State(gate): State<Arc<WorkloadAuth>>,
    mut request: Request,
    next: Next,
) -> Response {
    match gate.authenticate_route(
        request.method().as_str(),
        request.uri().path(),
        request.headers(),
        request.extensions().get::<VerifiedPeer>(),
    ) {
        Ok(proof) => {
            request.extensions_mut().insert(proof);
            next.run(request).await
        }
        Err(error) => error.into_response(),
    }
}
#[cfg(test)]
#[path = "workload/tests.rs"]
mod tests;
