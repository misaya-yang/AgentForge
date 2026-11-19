//! Source-ordered orchestration for src/api/v1/connectors.py.
//! The callback is authorized by issued Redis state, never a bearer dependency.
//! GET then DELETE deliberately retains source replay races. Refresh is also not
//! singleflight. Database statements commit independently; deleting state, refresh
//! failure writes and a callback upsert are not rolled back by a later error.
//! Secrets and credential records have no Debug, logging or public serialization.
use crate::{
    connector_user_types::*, protocol::ProtocolTimestamp, service_registry::python_truthy,
    usage_parser::python_value_to_string,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Datelike, Duration, Utc};
use serde_json::{Map, Value, json};
use std::{collections::HashMap, io::Read, sync::Arc};

pub const OAUTH_STATE_TTL_SECONDS: u64 = 600;
pub const OAUTH_STATE_KEY_PREFIX: &str = "oauth:state:";

#[derive(Default)]
pub struct SystemConnectorClock;
impl ConnectorClock for SystemConnectorClock {
    fn now(&self) -> DateTime<Utc> {
        let now = Utc::now();
        DateTime::from_timestamp_micros(now.timestamp_micros()).expect("current UTC timestamp")
    }
}
/// Same 16 random bytes and unpadded URL-safe Base64 as secrets.token_urlsafe(16).
/// This Linux adapter opens the OS CSPRNG only when explicitly called.
#[derive(Default)]
pub struct OsConnectorNonce;
impl ConnectorNonce for OsConnectorNonce {
    fn nonce(&self) -> Result<String, ConnectorError> {
        let mut bytes = [0; 16];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(&mut bytes))
            .map_err(|_| ConnectorError::source())?;
        Ok(URL_SAFE_NO_PAD.encode(bytes))
    }
}
pub struct ConnectorServiceDependencies {
    pub store: Option<Arc<dyn ConnectorUserStore>>,
    pub state: Option<Arc<dyn OAuthStateStore>>,
    pub clock: Arc<dyn ConnectorClock>,
    pub nonce: Arc<dyn ConnectorNonce>,
    pub cipher: Arc<dyn ConnectorTokenCipher>,
    pub form_http: Arc<dyn OAuthFormHttp>,
    pub provider_http: Arc<dyn ConnectorProviderHttp>,
    pub search: Arc<dyn ConnectorSearchProviders>,
    pub activation: Arc<dyn ConnectorActivation>,
    /// Explicit trusted host setting. CORS and request state are never consulted.
    pub console_public_base_url: String,
}
pub struct ConnectorService {
    dependencies: ConnectorServiceDependencies,
}
/// This value is gateway-internal only. Deliberately neither Debug nor Serialize.
pub struct ConnectorCatalogCredential {
    pub access_token: String,
    pub provider_metadata: Value,
}
impl ConnectorService {
    pub fn new(dependencies: ConnectorServiceDependencies) -> Self {
        Self { dependencies }
    }
    fn db(&self) -> Result<&dyn ConnectorUserStore, ConnectorError> {
        self.dependencies
            .store
            .as_deref()
            .ok_or_else(|| ConnectorError::detail(500, "Database not available"))
    }
    async fn config(
        &self,
        db: &dyn ConnectorUserStore,
        provider: &str,
        tenant_id: &str,
    ) -> Result<ConnectorRecord, ConnectorError> {
        db.run(ConnectorUserOperation::EffectiveEnabledConfig {
            provider: provider.into(),
            tenant_id: tenant_id.into(),
        })
        .await?
        .into_iter()
        .next()
        .filter(|row| python_truthy(&row.0))
        .ok_or_else(|| ConnectorError::detail(404, format!("Connector not configured: {provider}")))
    }
    async fn user_row(
        &self,
        db: &dyn ConnectorUserStore,
        user: &ConnectorUser,
        provider: &str,
    ) -> Result<Option<ConnectorRecord>, ConnectorError> {
        Ok(db
            .run(ConnectorUserOperation::GetUser {
                user: user.clone(),
                provider: provider.into(),
            })
            .await?
            .into_iter()
            .next()
            .filter(|row| python_truthy(&row.0)))
    }
    fn decrypt_config(
        &self,
        mut config: ConnectorRecord,
    ) -> Result<ConnectorRecord, ConnectorError> {
        let secret = py_string_or_empty(config.get("client_secret"));
        if secret.starts_with("enc:") {
            let secret = self.dependencies.cipher.decrypt_config(&secret)?;
            config
                .0
                .as_object_mut()
                .ok_or_else(ConnectorError::source)?
                .insert("client_secret".into(), json!(secret));
        }
        Ok(config)
    }
    fn decrypt_connector(
        &self,
        mut row: ConnectorRecord,
    ) -> Result<ConnectorRecord, ConnectorError> {
        // Both tokens are decrypted even when the access token has not expired.
        let access = self.decrypt_token_value(row.get("access_token"))?;
        let refresh = self.decrypt_token_value(row.get("refresh_token"))?;
        let fields = row.0.as_object_mut().ok_or_else(ConnectorError::source)?;
        fields.insert("access_token".into(), json!(access));
        fields.insert("refresh_token".into(), json!(refresh));
        Ok(row)
    }
    fn decrypt_token_value(&self, value: &Value) -> Result<Value, ConnectorError> {
        if !python_truthy(value) {
            return Ok(value.clone());
        }
        Ok(json!(
            self.dependencies
                .cipher
                .decrypt_user(optional_token(value)?)?
        ))
    }
    pub async fn available(&self, user: &ConnectorUser) -> Result<Value, ConnectorError> {
        let db = self.db()?;
        let configs = db
            .run(ConnectorUserOperation::AvailableConfigs {
                tenant_id: user.tenant_id.clone(),
            })
            .await?;
        let connections = db
            .run(ConnectorUserOperation::UserStatuses { user: user.clone() })
            .await?;
        let mut statuses = HashMap::new();
        for row in connections {
            statuses.insert(
                required_string(&row.0, "provider")?.to_owned(),
                required(&row.0, "status")?.clone(),
            );
        }
        let mut result = Vec::new();
        for config in configs {
            let provider = required_string(&config.0, "provider")?;
            let status = statuses.get(provider).cloned().unwrap_or(Value::Null);
            // ConnectorInfo's required string/bool response validation is retained.
            let display_name = required_string(&config.0, "display_name")?;
            let enabled = required(&config.0, "enabled")?
                .as_bool()
                .ok_or_else(ConnectorError::source)?;
            for key in ["description", "icon_url"] {
                if !config.get(key).is_null() && !config.get(key).is_string() {
                    return Err(ConnectorError::source());
                }
            }
            if !status.is_null() && !status.is_string() {
                return Err(ConnectorError::source());
            }
            result.push(json!({"provider":provider,"display_name":display_name,"description":config.get("description"),"icon_url":config.get("icon_url"),"enabled":enabled,"connected":status == "connected","status":status}));
        }
        Ok(Value::Array(result))
    }
    pub async fn mine(&self, user: &ConnectorUser) -> Result<Value, ConnectorError> {
        let rows = self
            .db()?
            .run(ConnectorUserOperation::ListMine { user: user.clone() })
            .await?;
        Ok(Value::Array(rows.into_iter().map(|r| r.0).collect()))
    }
    pub async fn begin(
        &self,
        user: &ConnectorUser,
        provider: &str,
        base_url: &str,
    ) -> Result<Value, ConnectorError> {
        let config = self.config(self.db()?, provider, &user.tenant_id).await?;
        if !python_truthy(config.get("client_id")) {
            return Err(ConnectorError::detail(
                400,
                format!("OAuth not configured for {provider}. Admin must set client_id."),
            ));
        }
        let nonce = self.dependencies.nonce.nonce()?;
        let state = format!("{}:{}:{provider}:{nonce}", user.tenant_id, user.user_id);
        let store = self
            .dependencies
            .state
            .as_deref()
            .ok_or_else(state_unavailable)?;
        let expiry = self
            .dependencies
            .clock
            .now()
            .checked_add_signed(Duration::seconds(600))
            .ok_or_else(ConnectorError::source)?;
        let entry = json!({"tenant_id":user.tenant_id,"user_id":user.user_id,"provider":provider,"nonce":nonce,"expires_at":ProtocolTimestamp::from(expiry.fixed_offset()).to_isoformat()});
        store
            .save(
                &format!("{OAUTH_STATE_KEY_PREFIX}{state}"),
                entry,
                OAUTH_STATE_TTL_SECONDS,
            )
            .await
            .map_err(|_| state_unavailable())?;
        // Save is committed before malformed URL/config fields can fail.
        let redirect_uri = redirect_uri(&config, provider, base_url);
        let extra = json_object(config.get("extra_config"));
        let mut params = vec![
            (
                "client_id".into(),
                python_value_to_string(required(&config.0, "client_id")?),
            ),
            ("redirect_uri".into(), redirect_uri),
            (
                "scope".into(),
                python_value_to_string(required(&config.0, "scopes")?),
            ),
            ("response_type".into(), "code".into()),
            ("state".into(), state.clone()),
        ];
        if provider == "confluence" {
            params.push((
                "audience".into(),
                python_value_to_string(
                    extra.get("audience").unwrap_or(&json!("api.atlassian.com")),
                ),
            ));
            params.push(("prompt".into(), "consent".into()));
        }
        if matches!(provider, "outlook" | "gmail" | "google_drive") {
            params.push(("access_type".into(), "offline".into()));
            params.push(("prompt".into(), "consent".into()));
        }
        let auth_url = format!(
            "{}?{}",
            python_value_to_string(required(&config.0, "auth_url")?),
            urlencode(&params)
        );
        Ok(json!({"auth_url":auth_url,"state":state}))
    }
    async fn consume_state(&self, state: &str) -> Result<Value, ConnectorError> {
        let store = self
            .dependencies
            .state
            .as_deref()
            .ok_or_else(invalid_state)?;
        let key = format!("{OAUTH_STATE_KEY_PREFIX}{state}");
        let raw = store.consume(&key).await.map_err(|_| state_unavailable())?;
        // Atomic consumption burns malformed records before decoding as well.
        let entry = decode_state(raw).map_err(|_| state_unavailable())?;
        let Some(entry) = entry.filter(python_truthy) else {
            return Err(invalid_state());
        };
        if !entry.is_object() {
            return Err(ConnectorError::source());
        }
        let Some(expiry) = entry.get("expires_at").and_then(Value::as_str) else {
            return Err(invalid_state());
        };
        let expiry = aware_time(expiry)?;
        if expiry <= self.dependencies.clock.now() {
            return Err(invalid_state());
        }
        Ok(entry)
    }
    pub async fn callback(
        &self,
        provider: &str,
        code: Option<&str>,
        error: Option<&str>,
        state: &str,
        base_url: &str,
    ) -> Result<String, ConnectorError> {
        if error.is_some_and(|value| value.is_empty() || code.is_some()) {
            return Err(ConnectorError::detail(400, "Invalid OAuth callback"));
        }
        let parts: Vec<_> = state.split(':').collect();
        if parts.len() < 4 {
            return Err(ConnectorError::detail(400, "Invalid state parameter"));
        }
        let (tenant_id, user_id, state_provider, nonce) = (parts[0], parts[1], parts[2], parts[3]);
        if state_provider != provider {
            return Err(ConnectorError::detail(400, "State provider mismatch"));
        }
        if tenant_id.is_empty() || user_id.is_empty() || nonce.is_empty() {
            return Err(ConnectorError::detail(400, "Incomplete state parameter"));
        }
        let issued = self.consume_state(state).await?;
        for (name, value) in [
            ("tenant_id", tenant_id),
            ("user_id", user_id),
            ("provider", provider),
            ("nonce", nonce),
        ] {
            if required(&issued, name)? != value {
                return Err(ConnectorError::detail(400, "Invalid state parameter"));
            }
        }
        if error.is_some() {
            return oauth_console_return(
                &self.dependencies.console_public_base_url,
                "/connections?status=authorization_failed",
            );
        }
        let code = code.ok_or_else(|| ConnectorError::detail(422, "Missing code"))?;
        let db = self.db()?;
        let config = self.decrypt_config(self.config(db, provider, tenant_id).await?)?;
        let mut fields = vec![
            ("grant_type".into(), "authorization_code".into()),
            (
                "client_id".into(),
                form_value(required(&config.0, "client_id")?)?,
            ),
            ("code".into(), code.into()),
            (
                "redirect_uri".into(),
                redirect_uri(&config, provider, base_url),
            ),
        ];
        if python_truthy(config.get("client_secret")) {
            fields.push((
                "client_secret".into(),
                form_value(config.get("client_secret"))?,
            ));
        }
        let response = self
            .dependencies
            .form_http
            .post(OAuthFormRequest {
                url: required_string(&config.0, "token_url")?.into(),
                fields,
                headers: vec![("Accept".into(), "application/json".into())],
            })
            .await
            .map_err(|error| match error {
                ConnectorHttpError::SourceUnhandled => ConnectorError::source(),
                _ => ConnectorError::detail(400, "Token exchange failed"),
            })?;
        if response.status != 200 {
            return Err(ConnectorError::detail(400, "Token exchange failed"));
        }
        let data = response.json()?;
        let data = data.as_object().ok_or_else(ConnectorError::source)?;
        let access = data.get("access_token").unwrap_or(&Value::Null);
        if !python_truthy(access) {
            return Err(ConnectorError::detail(400, "No access_token in response"));
        }
        let refresh = data.get("refresh_token").unwrap_or(&Value::Null);
        let expiry = expires_at(self.dependencies.clock.now(), data.get("expires_in"))?;
        let mut metadata = Map::new();
        if provider == "confluence" {
            let response = self
                .dependencies
                .provider_http
                .get(ConnectorGetRequest {
                    url: "https://api.atlassian.com/oauth/token/accessible-resources".into(),
                    query: vec![],
                    headers: vec![(
                        "Authorization".into(),
                        format!("Bearer {}", python_value_to_string(access)),
                    )],
                })
                .await
                .map_err(|_| ConnectorError::source())?;
            if response.status == 200 {
                let resources = response.json()?;
                if python_truthy(&resources) {
                    let resource = resources
                        .as_array()
                        .and_then(|r| r.first())
                        .ok_or_else(ConnectorError::source)?;
                    metadata.insert("cloud_id".into(), required(resource, "id")?.clone());
                    let obj = resource.as_object().ok_or_else(ConnectorError::source)?;
                    metadata.insert(
                        "site_url".into(),
                        obj.get("url").cloned().unwrap_or(json!("")),
                    );
                    metadata.insert(
                        "site_name".into(),
                        obj.get("name").cloned().unwrap_or(json!("")),
                    );
                }
            }
        }
        let stored_access = self
            .dependencies
            .cipher
            .encrypt_user(optional_token(access)?)?;
        let stored_refresh = self
            .dependencies
            .cipher
            .encrypt_user(optional_token(refresh)?)?;
        db.run(ConnectorUserOperation::UpsertTokens(ConnectorTokenWrite {
            user: ConnectorUser {
                tenant_id: tenant_id.into(),
                user_id: user_id.into(),
            },
            provider: provider.into(),
            access_token: stored_access,
            refresh_token: stored_refresh,
            expires_at: Some(expiry),
            scopes: required_string(&config.0, "scopes")?.into(),
            metadata: Some(Value::Object(metadata)),
        }))
        .await?;
        // A malformed console origin can still fail after tokens were persisted.
        oauth_console_redirect(&self.dependencies.console_public_base_url, provider)
    }
    async fn refresh(
        &self,
        db: &dyn ConnectorUserStore,
        row: ConnectorRecord,
        config: &ConnectorRecord,
    ) -> Result<Value, ConnectorError> {
        let row = self.decrypt_connector(row)?;
        if python_truthy(row.get("token_expires_at"))
            && aware_time(
                row.get("token_expires_at")
                    .as_str()
                    .ok_or_else(ConnectorError::source)?,
            )? < self.dependencies.clock.now()
        {
            if !python_truthy(row.get("refresh_token")) {
                return Err(ConnectorError::detail(
                    400,
                    "Token expired and no refresh token available. Reconnect.",
                ));
            }
            let request = OAuthFormRequest {
                url: required_string(&config.0, "token_url")?.into(),
                fields: vec![
                    ("grant_type".into(), "refresh_token".into()),
                    (
                        "refresh_token".into(),
                        form_value(row.get("refresh_token"))?,
                    ),
                    (
                        "client_id".into(),
                        form_value(required(&config.0, "client_id")?)?,
                    ),
                    (
                        "client_secret".into(),
                        form_value(config.0.get("client_secret").unwrap_or(&json!("")))?,
                    ),
                ],
                headers: vec![],
            };
            let response = self.dependencies.form_http.post(request).await;
            if matches!(&response, Err(ConnectorHttpError::SourceUnhandled)) {
                return Err(ConnectorError::source());
            }
            if !matches!(&response, Ok(response) if response.status == 200) {
                db.run(ConnectorUserOperation::MarkRefreshFailed {
                    id: required(&row.0, "id")?.clone(),
                })
                .await?;
                return Err(ConnectorError::detail(
                    401,
                    "Token refresh failed. Please reconnect.",
                ));
            }
            let data = response.map_err(|_| ConnectorError::source())?.json()?;
            let object = data.as_object().ok_or_else(ConnectorError::source)?;
            let expiry = expires_at(self.dependencies.clock.now(), object.get("expires_in"))?;
            let access = required(&data, "access_token")?;
            let stored_access = self
                .dependencies
                .cipher
                .encrypt_user(optional_token(access)?)?;
            let rotated = object.get("refresh_token").unwrap_or(&Value::Null);
            let stored_refresh = if python_truthy(rotated) {
                self.dependencies
                    .cipher
                    .encrypt_user(optional_token(rotated)?)?
            } else {
                None
            };
            db.run(ConnectorUserOperation::SaveRefreshed {
                id: required(&row.0, "id")?.clone(),
                access_token: stored_access,
                refresh_token: stored_refresh,
                expires_at: expiry,
            })
            .await?;
            // Invalid token types remain an unhandled source error, never a login.
            return Ok(access.clone());
        }
        Ok(required(&row.0, "access_token")?.clone())
    }
    pub async fn search(
        &self,
        user: &ConnectorUser,
        provider: &str,
        query: &str,
        limit: i64,
    ) -> Result<Value, ConnectorError> {
        self.search_exact(user, provider, query, &ConnectorSearchLimit::from(limit))
            .await
    }
    pub async fn search_exact(
        &self,
        user: &ConnectorUser,
        provider: &str,
        query: &str,
        limit: &ConnectorSearchLimit,
    ) -> Result<Value, ConnectorError> {
        // No explicit source DB-none guard exists for this endpoint.
        let db = self
            .dependencies
            .store
            .as_deref()
            .ok_or_else(ConnectorError::source)?;
        let row = self.user_row(db, user, provider).await?;
        let row = match row {
            Some(row) if required(&row.0, "status")? == "connected" => row,
            _ => {
                return Err(ConnectorError::detail(
                    400,
                    format!("Not connected to {provider}. Connect first."),
                ));
            }
        };
        let config = self.decrypt_config(self.config(db, provider, &user.tenant_id).await?)?;
        let token = self.refresh(db, row.clone(), &config).await?;
        let metadata = if python_truthy(row.get("provider_metadata")) {
            row.get("provider_metadata").clone()
        } else {
            json!({})
        };
        self.dependencies
            .search
            .search_exact(
                provider,
                &python_value_to_string(&token),
                &metadata,
                query,
                limit,
            )
            .await
    }
    pub async fn resolve_catalog_credential(
        &self,
        user: &ConnectorUser,
        provider: &str,
    ) -> Result<ConnectorCatalogCredential, ConnectorError> {
        let db = self
            .dependencies
            .store
            .as_deref()
            .filter(|db| db.enabled())
            .ok_or_else(ConnectorError::source)?;
        let row = self
            .user_row(db, user, provider)
            .await?
            .filter(|row| row.get("status") == "connected")
            .ok_or_else(|| ConnectorError::detail(403, "connected connector unavailable"))?;
        let config = self
            .config(db, provider, &user.tenant_id)
            .await
            .map_err(|error| {
                if error.status == 404 {
                    ConnectorError::detail(403, "connector configuration unavailable")
                } else {
                    error
                }
            })?;
        let config = self.decrypt_config(config)?;
        let access_token = self.refresh(db, row.clone(), &config).await?;
        let mut metadata = if python_truthy(row.get("provider_metadata")) {
            row.get("provider_metadata").clone()
        } else {
            json!({})
        };
        if let Some(text) = metadata.as_str() {
            metadata = serde_json::from_str(text).unwrap_or_else(|_| json!({}));
        }
        if !metadata.is_object() || !python_truthy(&access_token) {
            return Err(ConnectorError::source());
        }
        Ok(ConnectorCatalogCredential {
            access_token: python_value_to_string(&access_token),
            provider_metadata: metadata,
        })
    }
    pub async fn disconnect(
        &self,
        user: &ConnectorUser,
        provider: &str,
    ) -> Result<Value, ConnectorError> {
        self.db()?
            .run(ConnectorUserOperation::Revoke {
                user: user.clone(),
                provider: provider.into(),
            })
            .await?;
        let _ = self
            .dependencies
            .activation
            .stop(&user.tenant_id, provider)
            .await;
        Ok(json!({"status":"disconnected","provider":provider}))
    }
    pub async fn activate(
        &self,
        user: &ConnectorUser,
        provider: &str,
    ) -> Result<Value, ConnectorError> {
        if provider != "confluence" {
            return Err(ConnectorError::detail(
                400,
                format!("MCP activation not supported for: {provider}"),
            ));
        }
        let row = self
            .user_row(self.db()?, user, provider)
            .await?
            .filter(|row| row.get("status") == "connected")
            .ok_or_else(|| {
                ConnectorError::detail(
                    400,
                    "No active Confluence connection. Connect first via Settings.",
                )
            })?;
        let row = self.decrypt_connector(row)?;
        let mut metadata = if python_truthy(row.get("provider_metadata")) {
            row.get("provider_metadata").clone()
        } else {
            json!({})
        };
        if let Some(text) = metadata.as_str() {
            metadata = serde_json::from_str(text).map_err(|_| ConnectorError::source())?;
        }
        let object = metadata.as_object().ok_or_else(ConnectorError::source)?;
        let site = object
            .get("site_url")
            .filter(|v| python_truthy(v))
            .or_else(|| object.get("domain"))
            .unwrap_or(&Value::Null);
        let site = py_string_or_empty(site);
        let domain = activation_domain(&site)?;
        let mut email = py_string_or_empty(object.get("email").unwrap_or(&Value::Null));
        let token = py_string_or_empty(row.get("access_token"));
        if domain.is_empty() || token.is_empty() {
            return Err(ConnectorError::detail(
                400,
                "Connector credentials are incomplete. Reconnect.",
            ));
        }
        if email.is_empty() {
            email = format!("{}@oauth.local", user.user_id);
        }
        let tools = self
            .dependencies
            .activation
            .start_confluence(&user.tenant_id, &domain, &email, &token)
            .await?;
        Ok(json!({"status":"activated","provider":provider,"tool_count":tools.len(),"tools":tools}))
    }
    pub async fn status(
        &self,
        user: &ConnectorUser,
        provider: &str,
    ) -> Result<Value, ConnectorError> {
        let (active, tools) = self
            .dependencies
            .activation
            .status(&user.tenant_id, provider)?;
        Ok(json!({"provider":provider,"mcp_active":active,"tool_count":tools.len(),"tools":tools}))
    }
}
fn required<'a>(object: &'a Value, key: &str) -> Result<&'a Value, ConnectorError> {
    object
        .as_object()
        .and_then(|o| o.get(key))
        .ok_or_else(ConnectorError::source)
}
fn required_string<'a>(object: &'a Value, key: &str) -> Result<&'a str, ConnectorError> {
    required(object, key)?
        .as_str()
        .ok_or_else(ConnectorError::source)
}
fn optional_token(value: &Value) -> Result<Option<&str>, ConnectorError> {
    if value.is_null() {
        Ok(None)
    } else {
        value.as_str().map(Some).ok_or_else(ConnectorError::source)
    }
}
fn py_string_or_empty(value: &Value) -> String {
    if python_truthy(value) {
        python_value_to_string(value)
    } else {
        String::new()
    }
}
fn form_value(value: &Value) -> Result<String, ConnectorError> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Null => Ok(String::new()),
        Value::Bool(v) => Ok(v.to_string()),
        Value::Number(_) => Ok(python_value_to_string(value)),
        _ => Err(ConnectorError::source()),
    }
}
fn state_unavailable() -> ConnectorError {
    ConnectorError::detail(503, "OAuth state store unavailable")
}
fn invalid_state() -> ConnectorError {
    ConnectorError::detail(400, "Invalid or expired state parameter")
}
fn decode_state(raw: Option<OAuthStateRaw>) -> Result<Option<Value>, ()> {
    let value = match raw {
        None => return Ok(None),
        Some(OAuthStateRaw::Bytes(bytes)) => serde_json::from_slice(&bytes).map_err(|_| ())?,
        Some(OAuthStateRaw::Json(Value::String(text))) => {
            serde_json::from_str(&text).map_err(|_| ())?
        }
        Some(OAuthStateRaw::Json(value @ Value::Object(_))) => value,
        Some(OAuthStateRaw::Json(_)) => return Ok(None),
    };
    // Source string JSON may decode any truthy object. Non-object JSON is an
    // unhandled entry.get failure AFTER delete, not an invalid-state response.
    Ok(Some(value))
}
fn aware_time(text: &str) -> Result<DateTime<Utc>, ConnectorError> {
    match ProtocolTimestamp::parse_isoformat(text).map_err(|_| ConnectorError::source())? {
        ProtocolTimestamp::Offset(time) => Ok(time.with_timezone(&Utc)),
        ProtocolTimestamp::Naive(_) => Err(ConnectorError::source()),
    }
}
fn expires_at(now: DateTime<Utc>, value: Option<&Value>) -> Result<DateTime<Utc>, ConnectorError> {
    let duration = match value {
        None => Duration::seconds(3600),
        Some(Value::Bool(v)) => Duration::seconds(if *v { 1 } else { 0 }),
        Some(Value::Number(n)) if n.is_i64() => {
            Duration::try_seconds(n.as_i64().ok_or_else(ConnectorError::source)?)
                .ok_or_else(ConnectorError::source)?
        }
        Some(Value::Number(n)) => {
            let micros =
                (n.as_f64().ok_or_else(ConnectorError::source)? * 1_000_000.0).round_ties_even();
            if !micros.is_finite() || micros < i64::MIN as f64 || micros >= i64::MAX as f64 {
                return Err(ConnectorError::source());
            }
            Duration::microseconds(micros as i64)
        }
        _ => return Err(ConnectorError::source()),
    };
    let expiry = now
        .checked_add_signed(duration)
        .ok_or_else(ConnectorError::source)?;
    if !(1..=9999).contains(&expiry.year()) {
        return Err(ConnectorError::source());
    }
    Ok(expiry)
}
fn json_object(value: &Value) -> Map<String, Value> {
    match value {
        Value::Object(value) => value.clone(),
        Value::String(text) => serde_json::from_str::<Value>(text)
            .ok()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default(),
        _ => Map::new(),
    }
}
fn redirect_uri(config: &ConnectorRecord, provider: &str, base_url: &str) -> String {
    if python_truthy(config.get("redirect_uri")) {
        python_value_to_string(config.get("redirect_uri"))
    } else {
        format!(
            "{}/api/v1/connectors/callback/{provider}",
            base_url.trim_end_matches('/')
        )
    }
}
/// Python quote_plus for auth query fields, unlike WHATWG form encoding (~/*).
pub fn urlencode(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", quote(k, false, true), quote(v, false, true)))
        .collect::<Vec<_>>()
        .join("&")
}
fn quote(value: &str, slash: bool, plus: bool) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => {
                out.push(byte as char)
            }
            b'/' if slash => out.push('/'),
            b' ' if plus => out.push('+'),
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 15) as usize] as char);
            }
        }
    }
    out
}
// Deliberately retains netloc spelling and accepts nonnumeric ports: Python only
// reads urlsplit.netloc here, never .port. It does not canonicalize via WHATWG.
struct SplitUrl {
    scheme: String,
    netloc: String,
    path: String,
    query: String,
    fragment: String,
}
fn split_url(raw: &str) -> Result<SplitUrl, ConnectorError> {
    let clean: String = raw
        .trim_start_matches(|c: char| c <= '\u{20}')
        .chars()
        .filter(|c| !matches!(c, '\r' | '\n' | '\t'))
        .collect();
    let mut rest = clean.as_str();
    let mut scheme = String::new();
    if let Some(colon) = rest.find(':') {
        let candidate = &rest[..colon];
        if candidate
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && candidate
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
        {
            scheme = candidate.to_ascii_lowercase();
            rest = &rest[colon + 1..];
        }
    }
    let mut netloc = String::new();
    if rest.starts_with("//") {
        rest = &rest[2..];
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        netloc = rest[..end].to_owned();
        rest = &rest[end..];
        if netloc.contains('[') || netloc.contains(']') {
            let start = netloc.find('[').ok_or_else(ConnectorError::source)?;
            let end = netloc.find(']').ok_or_else(ConnectorError::source)?;
            if end < start {
                return Err(ConnectorError::source());
            }
            let host = &netloc[start + 1..end];
            let future = host.strip_prefix('v').is_some_and(|s| {
                s.split_once('.').is_some_and(|(v, tail)| {
                    !v.is_empty() && v.bytes().all(|b| b.is_ascii_hexdigit()) && !tail.is_empty()
                })
            });
            let ipv6 = if let Some((address, scope)) = host.split_once('%') {
                !scope.is_empty()
                    && !scope.contains('%')
                    && address.parse::<std::net::Ipv6Addr>().is_ok()
            } else {
                host.parse::<std::net::Ipv6Addr>().is_ok()
            };
            if !future && !ipv6 {
                return Err(ConnectorError::source());
            }
        }
    }
    let (rest, fragment) = rest.split_once('#').unwrap_or((rest, ""));
    let (path, query) = rest.split_once('?').unwrap_or((rest, ""));
    Ok(SplitUrl {
        scheme,
        netloc,
        path: path.into(),
        query: query.into(),
        fragment: fragment.into(),
    })
}
pub fn oauth_console_redirect(configured: &str, provider: &str) -> Result<String, ConnectorError> {
    oauth_console_return(
        configured,
        &format!(
            "/connections?connected={}&status=connected",
            quote(provider, true, false)
        ),
    )
}
fn oauth_console_return(configured: &str, path: &str) -> Result<String, ConnectorError> {
    let configured = configured.trim();
    let mut origin = String::new();
    if !configured.is_empty() {
        let parsed = split_url(configured)?;
        if matches!(parsed.scheme.as_str(), "http" | "https")
            && !parsed.netloc.is_empty()
            && !parsed.netloc.contains('@')
            && matches!(parsed.path.as_str(), "" | "/")
            && parsed.query.is_empty()
            && parsed.fragment.is_empty()
        {
            origin = format!("{}://{}", parsed.scheme, parsed.netloc);
        }
    }
    // Starlette RedirectResponse additionally quotes unsafe URL characters but
    // retains existing percent escapes and standard URL punctuation.
    let target = format!("{}{path}", origin.trim_end_matches('/'));
    let mut out = String::new();
    for ch in target.chars() {
        if ch.is_ascii_alphanumeric() || ":/%#?=@[]!$&'()*+,;~._-".contains(ch) {
            out.push(ch);
        } else {
            out.push_str(&quote(&ch.to_string(), false, false));
        }
    }
    Ok(out)
}
fn activation_domain(site: &str) -> Result<String, ConnectorError> {
    let split = split_url(site)?;
    if !split.netloc.is_empty() {
        Ok(split.netloc)
    } else {
        Ok(site
            .replace("https://", "")
            .replace("http://", "")
            .split('/')
            .next()
            .unwrap_or("")
            .to_owned())
    }
}
