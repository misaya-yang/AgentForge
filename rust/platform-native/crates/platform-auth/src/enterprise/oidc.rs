//! OIDC code+PKCE with server-only one-use state, nonce and browser binding.
//! SAML is supported only through an external IdP exposing this OIDC boundary.
// SPDX-License-Identifier: Apache-2.0
use super::*;
use openidconnect::{
    AccessTokenHash, AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce,
    OAuth2TokenResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata},
};
use serde::{Deserialize, Serialize};
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OidcConfiguration {
    pub id: String,
    pub tenant_id: String,
    pub issuer: String,
    pub client_id: String,
    pub redirect_url: String,
    pub return_url: String,
    pub endpoint_hosts: Vec<String>,
    #[serde(default)]
    pub jit_verified_domains: Vec<String>,
    #[serde(default)]
    pub accepted_mfa_acr: Vec<String>,
}
#[derive(Clone)]
pub struct OidcProvider {
    config: OidcConfiguration,
    metadata: CoreProviderMetadata,
    secret: Option<ClientSecret>,
    http: HttpClient,
}
#[derive(Clone)]
struct HttpClient {
    client: reqwest::Client,
    hosts: Arc<Vec<String>>,
}
impl<'c> openidconnect::AsyncHttpClient<'c> for HttpClient {
    type Error = std::io::Error;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<
                    Output = std::result::Result<
                        openidconnect::http::Response<Vec<u8>>,
                        Self::Error,
                    >,
                > + Send
                + 'c,
        >,
    >;
    fn call(&'c self, request: openidconnect::http::Request<Vec<u8>>) -> Self::Future {
        Box::pin(async move {
            let invalid = || std::io::Error::other("OIDC HTTP boundary rejected");
            let (parts, body) = request.into_parts();
            let url = Url::parse(&parts.uri.to_string()).map_err(|_| invalid())?;
            if url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || !url
                    .host_str()
                    .is_some_and(|h| self.hosts.iter().any(|allowed| allowed == h))
            {
                return Err(invalid());
            }
            let mut response = self
                .client
                .request(parts.method, url)
                .headers(parts.headers)
                .body(body)
                .send()
                .await
                .map_err(|_| invalid())?;
            if response.content_length().is_some_and(|n| n > 1024 * 1024) {
                return Err(invalid());
            }
            let status = response.status();
            let headers = response.headers().clone();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| invalid())? {
                if bytes.len().saturating_add(chunk.len()) > 1024 * 1024 {
                    return Err(invalid());
                }
                bytes.extend_from_slice(&chunk);
            }
            let mut result = openidconnect::http::Response::builder().status(status);
            *result.headers_mut().ok_or_else(invalid)? = headers;
            result.body(bytes).map_err(|_| invalid())
        })
    }
}
impl OidcProvider {
    /// Configuration is operator/tenant-administrator authority, never request
    /// issuer/email discovery. Secret comes from the existing runtime secret owner.
    pub async fn discover(config: OidcConfiguration, secret: Option<String>) -> Result<Self> {
        config.validate()?;
        let http = HttpClient {
            client: reqwest::Client::builder()
                .https_only(true)
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .connect_timeout(std::time::Duration::from_secs(5))
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .map_err(|_| EnterpriseError::Unavailable)?,
            hosts: Arc::new(config.endpoint_hosts.clone()),
        };
        let issuer = IssuerUrl::new(config.issuer.clone()).map_err(|_| EnterpriseError::Invalid)?;
        let metadata = CoreProviderMetadata::discover_async(issuer, &http)
            .await
            .map_err(|_| EnterpriseError::Proof)?;
        if metadata.issuer().as_str() != config.issuer {
            return Err(EnterpriseError::Proof);
        }
        Ok(Self {
            config,
            metadata,
            secret: secret.map(ClientSecret::new),
            http,
        })
    }
    pub fn id(&self) -> &str {
        &self.config.id
    }
    pub(super) fn issuer(&self) -> &str {
        &self.config.issuer
    }
    pub(super) fn tenant(&self) -> &str {
        &self.config.tenant_id
    }
    pub(super) fn return_url(&self) -> &str {
        &self.config.return_url
    }
}
impl OidcConfiguration {
    fn validate(&self) -> Result<()> {
        for value in [&self.id, &self.tenant_id, &self.client_id] {
            bounded(value, 255)?;
        }
        let issuer = Url::parse(&self.issuer).map_err(|_| EnterpriseError::Invalid)?;
        let redirect = Url::parse(&self.redirect_url).map_err(|_| EnterpriseError::Invalid)?;
        let target = Url::parse(&self.return_url).map_err(|_| EnterpriseError::Invalid)?;
        for url in [&issuer, &redirect, &target] {
            if url.scheme() != "https"
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(EnterpriseError::Invalid);
            }
        }
        if issuer.query().is_some()
            || redirect.query().is_some()
            || redirect.path() != "/api/auth/enterprise/oidc/callback"
            || self.endpoint_hosts.is_empty()
            || self.endpoint_hosts.len() > 16
            || !issuer
                .host_str()
                .is_some_and(|h| self.endpoint_hosts.iter().any(|v| v == h))
        {
            return Err(EnterpriseError::Invalid);
        }
        for host in &self.endpoint_hosts {
            if host.contains(['*', '/', ':', '@']) || host.is_empty() {
                return Err(EnterpriseError::Invalid);
            }
        }
        for domain in &self.jit_verified_domains {
            if domain.is_empty()
                || domain.contains(['*', '/', '@'])
                || domain != &domain.to_ascii_lowercase()
            {
                return Err(EnterpriseError::Invalid);
            }
        }
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
struct Flow {
    provider: String,
    configuration: String,
    nonce: String,
    verifier: String,
}
pub(super) struct OidcIdentity {
    pub user: String,
    pub tenant: String,
    pub method: &'static str,
    pub return_url: String,
    pub epoch: u64,
}
impl EnterpriseService {
    pub async fn oidc_begin(&self, provider: &str, browser_binding: &str) -> Result<String> {
        let p = self
            .providers
            .get(provider)
            .ok_or(EnterpriseError::NotFound)?;
        self.transient
            .rate("oidc-start", browser_binding, 10, 60)
            .await?;
        let client = CoreClient::from_provider_metadata(
            p.metadata.clone(),
            ClientId::new(p.config.client_id.clone()),
            p.secret.clone(),
        )
        .set_redirect_uri(
            RedirectUrl::new(p.config.redirect_url.clone())
                .map_err(|_| EnterpriseError::Invalid)?,
        );
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let (url, state, nonce) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .add_scope(Scope::new("email".into()))
            .add_scope(Scope::new("profile".into()))
            .set_pkce_challenge(challenge)
            .set_max_age(std::time::Duration::from_secs(300))
            .url();
        self.transient
            .put(
                "oidc",
                state.secret(),
                browser_binding,
                &Flow {
                    provider: provider.into(),
                    configuration: digest(
                        &serde_json::to_vec(&p.config).map_err(|_| EnterpriseError::Invalid)?,
                    ),
                    nonce: nonce.secret().clone(),
                    verifier: verifier.secret().clone(),
                },
            )
            .await?;
        Ok(url.to_string())
    }
    pub(super) async fn oidc_finish(
        &self,
        state: &str,
        code: &str,
        browser_binding: &str,
        response_issuer: Option<&str>,
    ) -> Result<OidcIdentity> {
        bounded(code, 8192)?;
        bounded(state, 2048)?;
        self.transient
            .rate("oidc-finish", browser_binding, 10, 60)
            .await?;
        let flow: Flow = self.transient.take("oidc", state, browser_binding).await?;
        let p = self
            .providers
            .get(&flow.provider)
            .ok_or(EnterpriseError::Proof)?;
        if response_issuer.is_some_and(|issuer| issuer != p.config.issuer) {
            return Err(EnterpriseError::Proof);
        }
        if flow.configuration
            != digest(&serde_json::to_vec(&p.config).map_err(|_| EnterpriseError::Invalid)?)
        {
            return Err(EnterpriseError::Proof);
        }
        let fresh = CoreProviderMetadata::discover_async(
            IssuerUrl::new(p.config.issuer.clone()).map_err(|_| EnterpriseError::Invalid)?,
            &p.http,
        )
        .await
        .map_err(|_| EnterpriseError::Proof)?;
        if fresh.issuer().as_str() != p.config.issuer {
            return Err(EnterpriseError::Proof);
        }
        let client = CoreClient::from_provider_metadata(
            fresh,
            ClientId::new(p.config.client_id.clone()),
            p.secret.clone(),
        )
        .set_redirect_uri(
            RedirectUrl::new(p.config.redirect_url.clone())
                .map_err(|_| EnterpriseError::Invalid)?,
        );
        let tokens = client
            .exchange_code(AuthorizationCode::new(code.into()))
            .map_err(|_| EnterpriseError::Proof)?
            .set_pkce_verifier(PkceCodeVerifier::new(flow.verifier))
            .request_async(&p.http)
            .await
            .map_err(|_| EnterpriseError::Proof)?;
        let id = tokens.id_token().ok_or(EnterpriseError::Proof)?;
        let now = i64::try_from(self.now()?).map_err(|_| EnterpriseError::Invalid)?;
        let verifier = client
            .id_token_verifier()
            .set_auth_time_verifier_fn(move |auth_time| {
                if auth_time.is_some_and(|at| at.timestamp() <= now && now - at.timestamp() <= 300)
                {
                    Ok(())
                } else {
                    Err("OIDC authentication freshness rejected".into())
                }
            });
        let nonce = Nonce::new(flow.nonce);
        let claims = id
            .claims(&verifier, &nonce)
            .map_err(|_| EnterpriseError::Proof)?;
        if let Some(expected) = claims.access_token_hash() {
            let actual = AccessTokenHash::from_token(
                tokens.access_token(),
                id.signing_alg().map_err(|_| EnterpriseError::Proof)?,
                id.signing_key(&verifier)
                    .map_err(|_| EnterpriseError::Proof)?,
            )
            .map_err(|_| EnterpriseError::Proof)?;
            if &actual != expected {
                return Err(EnterpriseError::Proof);
            }
        }
        let subject = claims.subject().as_str();
        bounded(subject, 2048)?;
        let user:Option<String>=sqlx::query_scalar("SELECT user_id FROM gateway.enterprise_identity_links WHERE tenant_id=$1 AND provider_id=$2 AND issuer=$3 AND subject=$4").bind(&p.config.tenant_id).bind(&p.config.id).bind(&p.config.issuer).bind(subject).fetch_optional(&self.pool).await?;
        let user = match user {
            Some(user) => user,
            None => {
                let email = claims
                    .email()
                    .map(|e| e.as_str().to_ascii_lowercase())
                    .ok_or(EnterpriseError::Forbidden)?;
                if claims.email_verified() != Some(true)
                    || !email.rsplit_once('@').is_some_and(|(_, domain)| {
                        p.config
                            .jit_verified_domains
                            .iter()
                            .any(|allowed| allowed == domain)
                    })
                {
                    return Err(EnterpriseError::Forbidden);
                }
                let user = format!("enterprise_{}", uuid::Uuid::new_v4());
                let mut tx = self.pool.begin().await?;
                sqlx::query("INSERT INTO gateway.users(user_id,tenant_id,email,status,roles,permissions,tier,password_hash,force_password_change,email_verified) SELECT $1,$2,$3,'active',ARRAY['user']::varchar[],ARRAY[]::varchar[],'normal',NULL,false,true WHERE EXISTS(SELECT 1 FROM gateway.tenants WHERE tenant_id=$2 AND status='active')").bind(&user).bind(&p.config.tenant_id).bind(email).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO gateway.user_roles(user_id,role_name,granted_by) VALUES($1,'user','enterprise:oidc')").bind(&user).execute(&mut *tx).await?;
                sqlx::query("INSERT INTO gateway.enterprise_identity_links(tenant_id,provider_id,issuer,subject,user_id) VALUES($1,$2,$3,$4,$5)").bind(&p.config.tenant_id).bind(&p.config.id).bind(&p.config.issuer).bind(subject).bind(&user).execute(&mut *tx).await?;
                tx.commit().await?;
                user
            }
        };
        let mfa = claims.auth_context_ref().is_some_and(|acr| {
            p.config
                .accepted_mfa_acr
                .iter()
                .any(|value| value == acr.as_str())
        });
        // Pin the MFA decision and auth revision to one policy read. establish
        // checks both again; a concurrent stricter policy cannot be adopted by
        // a proof that did not meet it.
        let current_policy =
            policy::password_policy(&self.pool, &p.config.tenant_id, &user).await?;
        if current_policy.mfa_required && !mfa {
            return Err(EnterpriseError::Forbidden);
        }
        Ok(OidcIdentity {
            user: user.clone(),
            tenant: p.config.tenant_id.clone(),
            method: if mfa { "oidc_mfa" } else { "oidc" },
            return_url: p.config.return_url.clone(),
            epoch: current_policy.auth_epoch,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tenant_and_redirects_are_fixed_and_jit_domains_never_wildcard() {
        let mut c = OidcConfiguration {
            id: "provider".into(),
            tenant_id: "tenant".into(),
            issuer: "https://idp.example".into(),
            client_id: "client".into(),
            redirect_url: "https://gateway.example/api/auth/enterprise/oidc/callback".into(),
            return_url: "https://console.example/".into(),
            endpoint_hosts: vec!["idp.example".into()],
            jit_verified_domains: vec!["example.com".into()],
            accepted_mfa_acr: vec![],
        };
        assert!(c.validate().is_ok());
        c.redirect_url.push_str("?redirect=evil");
        assert!(c.validate().is_err());
        c.redirect_url = "http://gateway.example/api/auth/enterprise/oidc/callback".into();
        assert!(c.validate().is_err());
        c.redirect_url = "https://gateway.example/api/auth/enterprise/oidc/callback".into();
        c.jit_verified_domains = vec!["*.example.com".into()];
        assert!(c.validate().is_err());
    }
}
