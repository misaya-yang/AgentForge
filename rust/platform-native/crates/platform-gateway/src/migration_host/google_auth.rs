//! Native implementation of provider_service::ProviderGoogleAuth.
//!
//! Mirrors the service's two-stage google.auth credential acquisition/refresh:
//! acquisition validates credential material, then ProviderService checks project,
//! and only refresh exchanges credentials for an access token. Construction is
//! inert. ADC values and metadata-server opt-in are explicit host configuration;
//! this module never discovers files, reads environment variables, executes gcloud,
//! or creates an OAuth grant. All diagnostics and Debug output are secret-free.
//!
//! Supported configured ADC kinds: service_account, authorized_user, and explicitly
//! selected Compute Engine metadata. External-account federation, impersonation,
//! executable suppliers and non-default sovereign universes require their own
//! implementation and fail closed here rather than being treated as API keys.
use crate::provider_service::{
    GoogleCredentialState, ProviderGoogleAuth, ServiceError, ServiceFuture,
};
use chrono::Utc;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use reqwest::{Client, Response, Url};
use serde_json::{Value, json};
use std::{fmt, time::Duration};

const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const MAX_AUTH_RESPONSE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Default)]
pub struct GoogleAuthConfig {
    /// Already loaded by the authorized host configuration layer. Never logged.
    pub application_default_credentials: Option<Value>,
    /// ADC project override (e.g. the host's explicitly captured Google project).
    pub default_project: Option<String>,
    /// None disables metadata discovery entirely on non-Google hosts.
    pub metadata_server: Option<GoogleMetadataConfig>,
}
impl fmt::Debug for GoogleAuthConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GoogleAuthConfig(<internal>)")
    }
}
#[derive(Clone)]
pub struct GoogleMetadataConfig {
    /// Origin/root, normally http://metadata.google.internal/computeMetadata/v1/.
    pub root_url: String,
    /// Compute Engine account alias or email. The canonical default is "default".
    pub service_account: String,
}
impl Default for GoogleMetadataConfig {
    fn default() -> Self {
        Self {
            root_url: "http://metadata.google.internal/computeMetadata/v1/".into(),
            service_account: "default".into(),
        }
    }
}
impl fmt::Debug for GoogleMetadataConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GoogleMetadataConfig(<internal>)")
    }
}

pub struct NativeGoogleAuth {
    config: GoogleAuthConfig,
    token_client: Client,
    metadata_client: Client,
}
impl fmt::Debug for NativeGoogleAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeGoogleAuth(<internal>)")
    }
}
impl NativeGoogleAuth {
    pub fn new(config: GoogleAuthConfig) -> Result<Self, ServiceError> {
        if let Some(metadata) = &config.metadata_server {
            metadata_root(metadata)?;
            if metadata.service_account.is_empty()
                || metadata.service_account.contains(['/', '?', '#'])
            {
                return Err(ServiceError::GoogleAuth);
            }
        }
        let token_client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(120))
            .read_timeout(Duration::from_secs(120))
            .build()
            .map_err(|_| ServiceError::GoogleAuth)?;
        let metadata_client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(3))
            .read_timeout(Duration::from_secs(3))
            .build()
            .map_err(|_| ServiceError::GoogleAuth)?;
        Ok(Self {
            config,
            token_client,
            metadata_client,
        })
    }

    fn account_state(
        &self,
        account: Value,
        scopes: &[&str],
        adc: bool,
    ) -> Result<GoogleCredentialState, ServiceError> {
        if !account.is_object() {
            return Err(ServiceError::GoogleAuth);
        }
        let kind = if adc {
            string(&account, "type")?
        } else {
            "service_account"
        };
        match kind {
            "service_account" => {
                string(&account, "client_email")?;
                token_url(string(&account, "token_uri")?)?;
                EncodingKey::from_rsa_pem(string(&account, "private_key")?.as_bytes())
                    .map_err(|_| ServiceError::GoogleAuth)?;
                if account
                    .get("universe_domain")
                    .and_then(Value::as_str)
                    .is_some_and(|domain| !domain.is_empty() && domain != "googleapis.com")
                {
                    return Err(ServiceError::GoogleAuth);
                }
            }
            "authorized_user" if adc => {
                for field in ["client_id", "client_secret", "refresh_token"] {
                    string(&account, field)?;
                }
            }
            _ => return Err(ServiceError::GoogleAuth),
        }
        let default_project = if adc {
            self.config
                .default_project
                .clone()
                .filter(|v| !v.is_empty())
                .or_else(|| {
                    account
                        .get("project_id")
                        .and_then(Value::as_str)
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                })
        } else {
            account
                .get("project_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        Ok(GoogleCredentialState {
            default_project,
            internal_state: json!({"kind":kind,"account":account,"scopes":scopes}),
        })
    }

    async fn metadata_get(
        &self,
        config: &GoogleMetadataConfig,
        path: &str,
        scopes: Option<&str>,
    ) -> Result<Vec<u8>, ServiceError> {
        let mut url = metadata_root(config)?
            .join(path)
            .map_err(|_| ServiceError::GoogleAuth)?;
        if let Some(scopes) = scopes.filter(|v| !v.is_empty()) {
            url.query_pairs_mut().append_pair("scopes", scopes);
        }
        let response = self
            .metadata_client
            .get(url)
            .header("Metadata-Flavor", "Google")
            .send()
            .await
            .map_err(|_| ServiceError::GoogleAuth)?;
        if response
            .headers()
            .get("Metadata-Flavor")
            .and_then(|v| v.to_str().ok())
            != Some("Google")
        {
            return Err(ServiceError::GoogleAuth);
        }
        bounded_body(response).await
    }

    async fn exchange(
        &self,
        endpoint: &str,
        form: &[(&str, String)],
    ) -> Result<String, ServiceError> {
        let response = self
            .token_client
            .post(token_url(endpoint)?)
            .form(form)
            .send()
            .await
            .map_err(|_| ServiceError::GoogleAuth)?;
        let body = bounded_body(response).await?;
        access_token(&body)
    }
}
impl ProviderGoogleAuth for NativeGoogleAuth {
    fn acquire<'a>(
        &'a self,
        service_account: Option<Value>,
        scopes: &'a [&'a str],
    ) -> ServiceFuture<'a, GoogleCredentialState> {
        Box::pin(async move {
            if let Some(account) = service_account {
                return self.account_state(account, scopes, false);
            }
            if let Some(account) = &self.config.application_default_credentials {
                return self.account_state(account.clone(), scopes, true);
            }
            let metadata = self
                .config
                .metadata_server
                .as_ref()
                .ok_or(ServiceError::GoogleAuth)?;
            // ADC determines the default project before ProviderService performs
            // its required-project check. No access token is requested here.
            let project = if let Some(project) = self
                .config
                .default_project
                .as_ref()
                .filter(|v| !v.is_empty())
            {
                Some(project.clone())
            } else {
                let bytes = self
                    .metadata_get(metadata, "project/project-id", None)
                    .await?;
                let project = String::from_utf8(bytes).map_err(|_| ServiceError::GoogleAuth)?;
                (!project.trim().is_empty()).then(|| project.trim().to_owned())
            };
            Ok(GoogleCredentialState {
                default_project: project,
                internal_state: json!({"kind":"metadata","scopes":scopes}),
            })
        })
    }

    fn refresh<'a>(&'a self, credentials: GoogleCredentialState) -> ServiceFuture<'a, String> {
        Box::pin(async move {
            let state = credentials.internal_state;
            let scopes = state
                .get("scopes")
                .and_then(Value::as_array)
                .ok_or(ServiceError::GoogleAuth)?
                .iter()
                .map(|v| v.as_str().ok_or(ServiceError::GoogleAuth))
                .collect::<Result<Vec<_>, _>>()?
                .join(" ");
            match string(&state, "kind")? {
                "service_account" => {
                    let account = &state["account"];
                    let key = EncodingKey::from_rsa_pem(string(account, "private_key")?.as_bytes())
                        .map_err(|_| ServiceError::GoogleAuth)?;
                    let mut header = Header::new(Algorithm::RS256);
                    header.kid = account
                        .get("private_key_id")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    let now = Utc::now().timestamp();
                    let claims = json!({"iss":string(account,"client_email")?,"scope":scopes,"aud":GOOGLE_TOKEN_ENDPOINT,"iat":now,"exp":now+3600});
                    let assertion = jsonwebtoken::encode(&header, &claims, &key)
                        .map_err(|_| ServiceError::GoogleAuth)?;
                    self.exchange(
                        string(account, "token_uri")?,
                        &[
                            (
                                "grant_type",
                                "urn:ietf:params:oauth:grant-type:jwt-bearer".into(),
                            ),
                            ("assertion", assertion),
                        ],
                    )
                    .await
                }
                "authorized_user" => {
                    let account = &state["account"];
                    let mut form = vec![
                        ("grant_type", "refresh_token".into()),
                        ("client_id", string(account, "client_id")?.into()),
                        ("client_secret", string(account, "client_secret")?.into()),
                        ("refresh_token", string(account, "refresh_token")?.into()),
                    ];
                    if !scopes.is_empty() {
                        form.push(("scope", scopes));
                    }
                    // google.oauth2.credentials.from_authorized_user_info uses
                    // Google's token endpoint rather than trusting token_uri.
                    self.exchange(GOOGLE_TOKEN_ENDPOINT, &form).await
                }
                "metadata" => {
                    let config = self
                        .config
                        .metadata_server
                        .as_ref()
                        .ok_or(ServiceError::GoogleAuth)?;
                    let path =
                        format!("instance/service-accounts/{}/token", config.service_account);
                    let metadata_scopes = scopes.replace(' ', ",");
                    let body = self
                        .metadata_get(config, &path, Some(&metadata_scopes))
                        .await?;
                    access_token(&body)
                }
                _ => Err(ServiceError::GoogleAuth),
            }
        })
    }
}
fn string<'a>(value: &'a Value, key: &str) -> Result<&'a str, ServiceError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
        .ok_or(ServiceError::GoogleAuth)
}
fn token_url(raw: &str) -> Result<Url, ServiceError> {
    let url = Url::parse(raw).map_err(|_| ServiceError::GoogleAuth)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(ServiceError::GoogleAuth);
    }
    Ok(url)
}
fn metadata_root(config: &GoogleMetadataConfig) -> Result<Url, ServiceError> {
    let url = Url::parse(&format!("{}/", config.root_url.trim_end_matches('/')))
        .map_err(|_| ServiceError::GoogleAuth)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(ServiceError::GoogleAuth);
    }
    Ok(url)
}
async fn bounded_body(mut response: Response) -> Result<Vec<u8>, ServiceError> {
    if !response.status().is_success() {
        return Err(ServiceError::GoogleAuth);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ServiceError::GoogleAuth)?
    {
        if bytes.len().saturating_add(chunk.len()) > MAX_AUTH_RESPONSE_BYTES {
            return Err(ServiceError::GoogleAuth);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn access_token(bytes: &[u8]) -> Result<String, ServiceError> {
    let response: Value = serde_json::from_slice(bytes).map_err(|_| ServiceError::GoogleAuth)?;
    if response.get("error").is_some() {
        return Err(ServiceError::GoogleAuth);
    }
    Ok(string(&response, "access_token")?.to_owned())
}
