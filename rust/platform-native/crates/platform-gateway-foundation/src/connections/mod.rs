//! Connection state machine and OAuth 2.1 / API-key capture.
use crate::catalog::{
    Actor, CatalogApp, CatalogIdentity, Connection, Entry, OAuthState, allows, encrypt_secret,
    visible_entries,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

#[derive(Clone)]
struct StatePair {
    app: Arc<CatalogApp>,
    identity: Arc<dyn CatalogIdentity>,
}

pub fn router(app: Arc<CatalogApp>, identity: Arc<dyn CatalogIdentity>) -> Router {
    Router::new()
        .route("/api/v1/connections", get(list_connections).post(create_connection))
        .route(
            "/api/v1/connections/{entry_id}/oauth/start",
            post(start_oauth),
        )
        .route("/api/v1/connections/oauth/callback", get(oauth_callback))
        .route(
            "/api/v1/connections/{connection_id}/revoke",
            post(revoke_connection),
        )
        .route(
            "/api/v1/connections/{connection_id}/policy",
            post(policy_connection),
        )
        .with_state(StatePair { app, identity })
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

fn public_connection(connection: &Connection) -> Value {
    json!({
        "connection_id": connection.connection_id,
        "entry_id": connection.entry_id,
        "owner_kind": connection.owner_kind,
        "owner_id": connection.owner_id,
        "status": connection.status,
        "has_credential": connection.credential_ciphertext.as_deref().is_some_and(|value| value.starts_with("enc:")),
        "issuer": connection.issuer,
        "resource": connection.resource,
        "error_code": connection.error_code,
    })
}

fn owner_for(actor: &Actor, kind: &str, requested: Option<&str>) -> Result<String, StatusCode> {
    match kind {
        "user" => Ok(actor.user_id.clone()),
        "tenant" | "space" if allows(actor, "connector:share") => {
            Ok(requested.unwrap_or(actor.tenant_id.as_str()).to_owned())
        }
        _ => Err(StatusCode::FORBIDDEN),
    }
}

async fn list_connections(
    State(state): State<StatePair>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    let actor = state.identity.actor(&headers, "/api/v1/connections").await?;
    if !allows(&actor, "connector:view") {
        return Err(StatusCode::FORBIDDEN);
    }
    let connections = state
        .app
        .store
        .list_connections(&actor.tenant_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let visible: Vec<_> = connections
        .iter()
        .filter(|connection| {
            connection.owner_kind != "user" || connection.owner_id == actor.user_id
        })
        .map(public_connection)
        .collect();
    Ok(Json(json!({"connections": visible})))
}

#[derive(Deserialize)]
struct SecretBody {
    entry_id: String,
    owner_kind: Option<String>,
    owner_id: Option<String>,
    #[serde(default)]
    secret: String,
}

async fn create_connection(
    State(state): State<StatePair>,
    headers: HeaderMap,
    Json(body): Json<SecretBody>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let actor = state.identity.actor(&headers, "/api/v1/connections").await?;
    if !allows(&actor, "connector:connect") {
        return Err(StatusCode::FORBIDDEN);
    }
    let entries = visible_entries(&state.app, &actor.tenant_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let entry = entries
        .iter()
        .find(|entry| entry.entry_id == body.entry_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    if !matches!(entry.auth_kind.as_str(), "api_key" | "pat" | "none") {
        return Err(StatusCode::BAD_REQUEST);
    }
    if entry.auth_kind != "none" && body.secret.trim().is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let owner_kind = body.owner_kind.unwrap_or_else(|| "user".into());
    let owner_id = owner_for(&actor, &owner_kind, body.owner_id.as_deref())?;
    let ciphertext = if entry.auth_kind == "none" {
        None
    } else {
        Some(
            encrypt_secret(&state.app.cipher_key, &body.secret)
                .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?,
        )
    };
    let connection = Connection {
        connection_id: Uuid::new_v4().simple().to_string(),
        tenant_id: actor.tenant_id,
        entry_id: entry.entry_id.clone(),
        owner_kind,
        owner_id,
        status: "connected".into(),
        credential_ciphertext: ciphertext,
        issuer: String::new(),
        resource: entry
            .manifest
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        error_code: String::new(),
        created_by: actor.user_id,
    };
    state
        .app
        .store
        .upsert_connection(&connection)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok((StatusCode::CREATED, Json(public_connection(&connection))))
}

#[derive(Deserialize)]
struct StartBody {
    redirect_uri: Option<String>,
    owner_kind: Option<String>,
    owner_id: Option<String>,
}

async fn start_oauth(
    State(state): State<StatePair>,
    headers: HeaderMap,
    Path(entry_id): Path<String>,
    Json(body): Json<StartBody>,
) -> Result<Json<Value>, StatusCode> {
    let actor = state
        .identity
        .actor(&headers, "/api/v1/connections/{entry_id}/oauth/start")
        .await?;
    if !allows(&actor, "connector:connect") {
        return Err(StatusCode::FORBIDDEN);
    }
    let entries = visible_entries(&state.app, &actor.tenant_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let entry = entries
        .into_iter()
        .find(|entry| entry.entry_id == entry_id)
        .ok_or(StatusCode::NOT_FOUND)?;
    if entry.auth_kind != "oauth" {
        return Err(StatusCode::BAD_REQUEST);
    }
    let owner_kind = body.owner_kind.unwrap_or_else(|| "user".into());
    let owner_id = owner_for(&actor, &owner_kind, body.owner_id.as_deref())?;
    let redirect_uri = body.redirect_uri.unwrap_or_else(|| {
        format!(
            "{}/api/v1/connections/oauth/callback",
            state.app.public_base.trim_end_matches('/')
        )
    });
    let prepared = prepare_oauth(&state.app, &entry, &redirect_uri)
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let verifier_blob = json!({"verifier": prepared.verifier, "client_secret": prepared.client_secret}).to_string();
    let verifier_ciphertext = encrypt_secret(&state.app.cipher_key, &verifier_blob)
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let existing = state
        .app
        .store
        .list_connections(&actor.tenant_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let connection_id = existing
        .into_iter()
        .find(|connection| {
            connection.entry_id == entry.entry_id
                && connection.owner_kind == owner_kind
                && connection.owner_id == owner_id
                && connection.status != "revoked"
        })
        .map(|connection| connection.connection_id)
        .unwrap_or_else(|| Uuid::new_v4().simple().to_string());
    let state_token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let connection = Connection {
        connection_id: connection_id.clone(),
        tenant_id: actor.tenant_id.clone(),
        entry_id: entry.entry_id.clone(),
        owner_kind,
        owner_id,
        status: "connecting".into(),
        credential_ciphertext: None,
        issuer: prepared.issuer.clone(),
        resource: prepared.resource.clone(),
        error_code: String::new(),
        created_by: actor.user_id.clone(),
    };
    state
        .app
        .store
        .upsert_connection(&connection)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    state
        .app
        .store
        .put_oauth_state(&OAuthState {
            state: state_token.clone(),
            tenant_id: actor.tenant_id,
            user_id: actor.user_id,
            entry_id: entry.entry_id,
            connection_id: connection_id.clone(),
            code_verifier_ciphertext: verifier_ciphertext,
            issuer: prepared.issuer.clone(),
            resource: prepared.resource.clone(),
            redirect_uri,
            client_id: prepared.client_id.clone(),
            token_url: prepared.token_url.clone(),
            expires_at: now() + 600,
            consumed: false,
        })
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let authorization_url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&state={}&code_challenge={}&code_challenge_method=S256&resource={}",
        prepared.authorization_endpoint,
        urlencoding(&prepared.client_id),
        urlencoding(&state_redirect(&state.app, &prepared)),
        urlencoding(&state_token),
        urlencoding(&prepared.challenge),
        urlencoding(&prepared.resource),
    );
    Ok(Json(json!({
        "authorization_url": authorization_url,
        "connection_id": connection_id,
        "status": "connecting",
    })))
}

fn state_redirect(app: &CatalogApp, prepared: &Prepared) -> String {
    let _ = (app, prepared);
    format!(
        "{}/api/v1/connections/oauth/callback",
        app.public_base.trim_end_matches('/')
    )
}

struct Prepared {
    issuer: String,
    authorization_endpoint: String,
    token_url: String,
    client_id: String,
    client_secret: String,
    verifier: String,
    challenge: String,
    resource: String,
}

async fn prepare_oauth(app: &CatalogApp, entry: &Entry, redirect_uri: &str) -> Result<Prepared, ()> {
    let issuer = entry
        .manifest
        .get("issuer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim_end_matches('/')
        .to_owned();
    if issuer.is_empty() {
        return Err(());
    }
    let metadata = app
        .http
        .get(format!("{issuer}/.well-known/oauth-authorization-server"))
        .send()
        .await
        .map_err(|_| ())?
        .json::<Value>()
        .await
        .map_err(|_| ())?;
    if metadata.get("issuer").and_then(Value::as_str) != Some(issuer.as_str()) {
        return Err(());
    }
    let authorization_endpoint = metadata["authorization_endpoint"]
        .as_str()
        .ok_or(())?
        .to_owned();
    let token_url = metadata["token_endpoint"].as_str().ok_or(())?.to_owned();
    let metadata_client = format!(
        "{}/api/v1/connections/oauth/client-metadata",
        app.public_base.trim_end_matches('/')
    );
    let (client_id, client_secret) = if metadata["client_id_metadata_document_supported"] == true {
        (metadata_client, String::new())
    } else if let Some(endpoint) = metadata["registration_endpoint"].as_str() {
        let registered = app
            .http
            .post(endpoint)
            .json(&json!({
                "client_name": "AgentForge",
                "redirect_uris": [redirect_uri],
                "grant_types": ["authorization_code"],
                "response_types": ["code"],
                "token_endpoint_auth_method": "none",
            }))
            .send()
            .await
            .map_err(|_| ())?
            .json::<Value>()
            .await
            .map_err(|_| ())?;
        (
            registered["client_id"].as_str().unwrap_or("").to_owned(),
            registered["client_secret"].as_str().unwrap_or("").to_owned(),
        )
    } else {
        (
            entry
                .manifest
                .get("client_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            String::new(),
        )
    };
    if client_id.is_empty() {
        return Err(());
    }
    let verifier = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let challenge =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    Ok(Prepared {
        issuer,
        authorization_endpoint,
        token_url,
        client_id,
        client_secret,
        verifier,
        challenge,
        resource: entry
            .manifest
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    iss: Option<String>,
    error: Option<String>,
}

async fn oauth_callback(
    State(state): State<StatePair>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(token) = query.state else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(saved) = state
        .app
        .store
        .take_oauth_state(&token)
        .await
        .ok()
        .flatten()
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if saved.expires_at < now() {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let mut connections = state
        .app
        .store
        .list_connections(&saved.tenant_id)
        .await
        .unwrap_or_default();
    let Some(connection) = connections
        .iter_mut()
        .find(|connection| connection.connection_id == saved.connection_id)
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if query.iss.as_deref() != Some(saved.issuer.as_str()) || query.error.is_some() {
        connection.status = "error".into();
        connection.error_code = "issuer".into();
        let _ = state.app.store.upsert_connection(connection).await;
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(code) = query.code else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let verifier_plain = decrypt_secret(&state.app.cipher_key, &saved.code_verifier_ciphertext);
    let Ok(verifier_plain) = verifier_plain else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let verifier_json: Value = serde_json::from_str(&verifier_plain).unwrap_or(Value::Null);
    let verifier = verifier_json["verifier"].as_str().unwrap_or("");
    let mut form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", code),
        ("redirect_uri", saved.redirect_uri.clone()),
        ("client_id", saved.client_id.clone()),
        ("code_verifier", verifier.to_owned()),
        ("resource", saved.resource.clone()),
    ];
    if let Some(secret) = verifier_json["client_secret"].as_str() {
        if !secret.is_empty() {
            form.push(("client_secret", secret.to_owned()));
        }
    }
    let exchanged = state
        .app
        .http
        .post(&saved.token_url)
        .form(&form)
        .send()
        .await;
    let Ok(response) = exchanged else {
        connection.status = "error".into();
        let _ = state.app.store.upsert_connection(connection).await;
        return StatusCode::BAD_GATEWAY.into_response();
    };
    let Ok(token_body) = response.json::<Value>().await else {
        connection.status = "error".into();
        let _ = state.app.store.upsert_connection(connection).await;
        return StatusCode::BAD_GATEWAY.into_response();
    };
    if token_body.get("access_token").and_then(Value::as_str).is_none() {
        connection.status = "error".into();
        connection.error_code = "token".into();
        let _ = state.app.store.upsert_connection(connection).await;
        return StatusCode::BAD_GATEWAY.into_response();
    }
    let Ok(ciphertext) = encrypt_secret(&state.app.cipher_key, &token_body.to_string()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    connection.status = "connected".into();
    connection.credential_ciphertext = Some(ciphertext);
    connection.error_code.clear();
    if state.app.store.upsert_connection(connection).await.is_err() {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    Redirect::to(&format!(
        "{}/settings/connectors?connected=1",
        state.app.public_base.trim_end_matches('/')
    ))
    .into_response()
}

fn decrypt_secret(key: &str, ciphertext: &str) -> Result<String, ()> {
    NativeDecrypt::open(key, ciphertext)
}

struct NativeDecrypt;
impl NativeDecrypt {
    fn open(key: &str, ciphertext: &str) -> Result<String, ()> {
        use ai_platform_service_primitives::provider_service::ProviderCipher;
        ai_platform_service_primitives::provider_cipher::NativeProviderCipher
            .decrypt(ciphertext, key)
            .map_err(|_| ())
    }
}

async fn revoke_connection(
    State(state): State<StatePair>,
    headers: HeaderMap,
    Path(connection_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    update_status(&state, &headers, &connection_id, "revoked", "connector:connect").await
}

#[derive(Deserialize)]
struct PolicyBody {
    status: String,
}

async fn policy_connection(
    State(state): State<StatePair>,
    headers: HeaderMap,
    Path(connection_id): Path<String>,
    Json(body): Json<PolicyBody>,
) -> Result<Json<Value>, StatusCode> {
    if body.status != "disabled_by_policy" {
        return Err(StatusCode::BAD_REQUEST);
    }
    update_status(
        &state,
        &headers,
        &connection_id,
        "disabled_by_policy",
        "connector:admin",
    )
    .await
}

async fn update_status(
    state: &StatePair,
    headers: &HeaderMap,
    connection_id: &str,
    status: &str,
    permission: &str,
) -> Result<Json<Value>, StatusCode> {
    let actor = state.identity.actor(headers, "/api/v1/connections").await?;
    if !allows(&actor, permission) {
        return Err(StatusCode::FORBIDDEN);
    }
    let mut connections = state
        .app
        .store
        .list_connections(&actor.tenant_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let Some(connection) = connections
        .iter_mut()
        .find(|connection| connection.connection_id == connection_id)
    else {
        return Err(StatusCode::NOT_FOUND);
    };
    connection.status = status.into();
    if status == "revoked" {
        connection.credential_ciphertext = None;
    }
    state
        .app
        .store
        .upsert_connection(connection)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(public_connection(connection)))
}

pub fn project_connected_tools(
    entries: &[Entry],
    connections: &[Connection],
    tenant_id: &str,
    user_id: &str,
    revision: i64,
) -> Vec<Value> {
    let mut tools = Vec::new();
    for connection in connections {
        if connection.tenant_id != tenant_id || connection.status != "connected" {
            continue;
        }
        let visible = match connection.owner_kind.as_str() {
            "user" => connection.owner_id == user_id,
            "tenant" => connection.owner_id == tenant_id,
            "space" => true,
            _ => false,
        };
        if !visible {
            continue;
        }
        let Some(entry) = entries.iter().find(|entry| entry.entry_id == connection.entry_id) else {
            continue;
        };
        let Some(declared) = entry.manifest.get("tools").and_then(Value::as_array) else {
            continue;
        };
        for tool in declared {
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                continue;
            };
            let schema = tool
                .get("input_schema")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object"}));
            let read_only = tool.get("read_only").and_then(Value::as_bool).unwrap_or(true);
            tools.push(json!({
                "id": format!("{}:{}", entry.entry_id, name),
                "name": name,
                "description": tool.get("description").and_then(Value::as_str).unwrap_or(name),
                "schema": schema,
                "schema_hash": schema_hash(&schema),
                "read_only": read_only,
                "tenant_id": tenant_id,
                "capability_revision": revision,
                "kind": "mcp",
            }));
        }
    }
    tools
}

pub fn tool_search<'a>(tools: &'a [Value], query: &str) -> Vec<&'a Value> {
    let query = query.trim().to_lowercase();
    tools
        .iter()
        .filter(|tool| {
            query.is_empty()
                || tool["name"].as_str().unwrap_or("").to_lowercase().contains(&query)
                || tool["description"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&query)
        })
        .collect()
}

pub fn merge_connected_tools(
    mcp: &mut Vec<Value>,
    deferred: &mut Vec<Value>,
    tools: Vec<Value>,
    writes_enabled: bool,
) {
    for tool in tools {
        if tool["read_only"] == true {
            mcp.push(tool);
        } else if writes_enabled {
            deferred.push(tool);
        }
    }
}

pub fn mcp_registry_record(entry: &Entry) -> Option<Value> {
    if entry.kind != "mcp_server" {
        return None;
    }
    let url = entry.manifest.get("resource").and_then(Value::as_str)?;
    crate::mcp_registry_validation::split_https_url(url).ok()?;
    Some(json!({"name": entry.entry_id, "url": url}))
}

fn schema_hash(schema: &Value) -> String {
    let bytes = serde_json::to_vec(&canonical(schema)).unwrap_or_default();
    format!("sha256:{:x}", Sha256::digest(bytes))
}

fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .iter()
                .map(|(key, value)| (key.clone(), canonical(value)))
                .collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(entries.into_iter().collect::<Map<String, Value>>())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

fn urlencoding(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{FixedIdentity, MemoryStore, PgStore, Store, app};
    use axum::routing::post as route_post;
    use tokio::net::TcpListener;

    fn actor() -> Actor {
        Actor {
            user_id: "user-1".into(),
            tenant_id: "tenant-1".into(),
            permissions: vec![
                "connector:view".into(),
                "connector:connect".into(),
                "connector:share".into(),
                "connector:admin".into(),
            ],
        }
    }

    #[tokio::test]
    async fn connections_oauth_pkce_stores_only_ciphertext() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let issuer = format!("http://{address}");
        let app_for_mock = issuer.clone();
        tokio::spawn(async move {
            let router = Router::new()
                .route(
                    "/.well-known/oauth-authorization-server",
                    get({
                        let issuer = app_for_mock.clone();
                        move || {
                            let issuer = issuer.clone();
                            async move {
                                Json(json!({
                                    "issuer": issuer,
                                    "authorization_endpoint": format!("{issuer}/authorize"),
                                    "token_endpoint": format!("{issuer}/token"),
                                    "registration_endpoint": format!("{issuer}/register"),
                                    "client_id_metadata_document_supported": false,
                                }))
                            }
                        }
                    }),
                )
                .route(
                    "/register",
                    route_post(|| async { Json(json!({"client_id": "mock-client"})) }),
                )
                .route(
                    "/token",
                    route_post(|| async {
                        Json(json!({"access_token": "plaintext-access-token", "token_type": "bearer"}))
                    }),
                );
            axum::serve(listener, router).await.unwrap();
        });
        let catalog = app(
            Store::Memory(MemoryStore::new()),
            "catalog-test-key",
            "http://catalog.test",
        );
        catalog
            .store
            .upsert_entry(&Entry {
                entry_id: "mock".into(),
                tenant_id: "tenant-1".into(),
                kind: "connector".into(),
                name: "Mock".into(),
                description: String::new(),
                category: "development".into(),
                icon: String::new(),
                auth_kind: "oauth".into(),
                manifest: json!({"issuer": issuer, "resource": "https://resource.example/mcp"}),
                source: "tenant".into(),
                source_sha256: String::new(),
            })
            .await
            .unwrap();
        let identity = Arc::new(FixedIdentity(actor()));
        let router = router(catalog.clone(), identity);
        let start = tower::ServiceExt::oneshot(
            router.clone(),
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/connections/mock/oauth/start")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    r#"{"redirect_uri":"http://catalog.test/api/v1/connections/oauth/callback"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
        let bytes = axum::body::to_bytes(start.into_body(), 1024 * 1024)
            .await
            .unwrap();
        let started: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(started["status"], "connecting");
        assert!(!started.to_string().contains("plaintext"));
        let url = started["authorization_url"].as_str().unwrap();
        let state = url
            .split("state=")
            .nth(1)
            .unwrap()
            .split('&')
            .next()
            .unwrap();
        let callback = tower::ServiceExt::oneshot(
            router,
            axum::http::Request::builder()
                .uri(format!(
                    "/api/v1/connections/oauth/callback?code=auth-code&state={state}&iss={issuer}"
                ))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(callback.status(), StatusCode::SEE_OTHER);
        let connections = catalog.store.list_connections("tenant-1").await.unwrap();
        let saved = connections
            .iter()
            .find(|connection| connection.status == "connected")
            .unwrap();
        let ciphertext = saved.credential_ciphertext.as_deref().unwrap();
        assert!(ciphertext.starts_with("enc:"));
        assert!(!ciphertext.contains("plaintext-access-token"));
        assert!(!public_connection(saved).to_string().contains("plaintext-access-token"));
    }

    #[tokio::test]
    async fn connections_api_key_is_encrypted() {
        let catalog = app(
            Store::Memory(MemoryStore::new()),
            "catalog-test-key",
            "http://catalog.test",
        );
        let identity = Arc::new(FixedIdentity(actor()));
        let response = tower::ServiceExt::oneshot(
            router(catalog.clone(), identity),
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/connections")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    r#"{"entry_id":"agentmail","secret":"super-secret-token"}"#,
                ))
                .unwrap(),
        )
        .await
        .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&body).contains("super-secret-token"));
        let saved = catalog.store.list_connections("tenant-1").await.unwrap();
        let ciphertext = saved[0].credential_ciphertext.as_deref().unwrap();
        assert!(ciphertext.starts_with("enc:"));
        assert!(!ciphertext.contains("super-secret-token"));
    }

    #[test]
    fn connections_connected_tools_are_searchable() {
        let entry = Entry {
            entry_id: "github-mcp".into(),
            tenant_id: String::new(),
            kind: "mcp_server".into(),
            name: "GitHub MCP".into(),
            description: String::new(),
            category: "development".into(),
            icon: String::new(),
            auth_kind: "oauth".into(),
            manifest: json!({"resource":"https://api.github.com/mcp","tools":[{"name":"list_issues","description":"List issues in a repository","read_only":true,"input_schema":{"type":"object"}}]}),
            source: "builtin".into(),
            source_sha256: String::new(),
        };
        let connected = Connection {
            connection_id: "c1".into(),
            tenant_id: "tenant-1".into(),
            entry_id: "github-mcp".into(),
            owner_kind: "user".into(),
            owner_id: "user-1".into(),
            status: "connected".into(),
            credential_ciphertext: Some("enc:abc".into()),
            issuer: String::new(),
            resource: "https://api.github.com/mcp".into(),
            error_code: String::new(),
            created_by: "user-1".into(),
        };
        let revoked = Connection {
            status: "revoked".into(),
            connection_id: "c2".into(),
            ..connected.clone()
        };
        let tools = project_connected_tools(&[entry.clone()], &[connected, revoked], "tenant-1", "user-1", 1);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["schema_hash"].as_str().unwrap().len(), 71);
        assert_eq!(tool_search(&tools, "issues").len(), 1);
        assert!(tool_search(&tools, "calendar").is_empty());
        assert!(mcp_registry_record(&entry).is_some());
        let mut mcp = Vec::new();
        let mut deferred = Vec::new();
        merge_connected_tools(&mut mcp, &mut deferred, tools, false);
        assert_eq!(mcp.len(), 1);
        assert!(deferred.is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn connections_postgres_ciphertext_round_trip() {
        let url = std::env::var("AI_GATEWAY_TEST_DATABASE_URL").expect("AI_GATEWAY_TEST_DATABASE_URL");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&url)
            .await
            .expect("postgres");
        let catalog = app(
            Store::Postgres(PgStore::new(pool.clone())),
            "catalog-test-key",
            "http://catalog.test",
        );
        let entry_id = format!("pg-{}", Uuid::new_v4().simple());
        catalog
            .store
            .upsert_entry(&Entry {
                entry_id: entry_id.clone(),
                tenant_id: "tenant-pg".into(),
                kind: "connector".into(),
                name: "PG".into(),
                description: String::new(),
                category: "development".into(),
                icon: String::new(),
                auth_kind: "api_key".into(),
                manifest: json!({}),
                source: "tenant".into(),
                source_sha256: String::new(),
            })
            .await
            .unwrap();
        let secret = "postgres-plain-secret";
        let ciphertext = encrypt_secret("catalog-test-key", secret).unwrap();
        let connection_id = Uuid::new_v4().simple().to_string();
        catalog
            .store
            .upsert_connection(&Connection {
                connection_id: connection_id.clone(),
                tenant_id: "tenant-pg".into(),
                entry_id,
                owner_kind: "user".into(),
                owner_id: "user-pg".into(),
                status: "connected".into(),
                credential_ciphertext: Some(ciphertext.clone()),
                issuer: String::new(),
                resource: String::new(),
                error_code: String::new(),
                created_by: "user-pg".into(),
            })
            .await
            .unwrap();
        let stored: String = sqlx::query_scalar(
            "SELECT credential_ciphertext FROM gateway.catalog_connections WHERE connection_id = $1",
        )
        .bind(&connection_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(stored.starts_with("enc:"));
        assert!(!stored.contains(secret));
        let _ = catalog;
    }
}
