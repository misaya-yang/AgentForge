//! Connector, MCP, skill, and plugin catalog.
mod store;
use ai_platform_service_primitives::provider_cipher::{NativeProviderCipher, is_encrypted};
use ai_platform_service_primitives::provider_service::ProviderCipher;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::sync::Arc;

pub use store::{MemoryStore, PgStore, Store, StoreError};

const BUILTINS: &str = include_str!("../../../../../../config/catalog/builtins.json");

#[derive(Clone, Debug)]
pub struct Entry {
    pub entry_id: String,
    pub tenant_id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub category: String,
    pub icon: String,
    pub auth_kind: String,
    pub manifest: Value,
    pub source: String,
    pub source_sha256: String,
}

#[derive(Clone, Debug)]
pub struct Connection {
    pub connection_id: String,
    pub tenant_id: String,
    pub entry_id: String,
    pub owner_kind: String,
    pub owner_id: String,
    pub status: String,
    pub credential_ciphertext: Option<String>,
    pub issuer: String,
    pub resource: String,
    pub error_code: String,
    pub created_by: String,
}

#[derive(Clone, Debug)]
pub struct OAuthState {
    pub state: String,
    pub tenant_id: String,
    pub user_id: String,
    pub entry_id: String,
    pub connection_id: String,
    pub code_verifier_ciphertext: String,
    pub issuer: String,
    pub resource: String,
    pub redirect_uri: String,
    pub client_id: String,
    pub token_url: String,
    pub expires_at: i64,
    pub consumed: bool,
}

#[derive(Clone, Debug)]
pub struct Actor {
    pub user_id: String,
    pub tenant_id: String,
    pub permissions: Vec<String>,
}

pub trait CatalogIdentity: Send + Sync {
    fn actor<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Actor, StatusCode>> + Send + 'a>,
    >;
}

pub struct FixedIdentity(pub Actor);

impl CatalogIdentity for FixedIdentity {
    fn actor<'a>(
        &'a self,
        _headers: &'a HeaderMap,
        _path: &'a str,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Actor, StatusCode>> + Send + 'a>,
    > {
        let actor = self.0.clone();
        Box::pin(async move { Ok(actor) })
    }
}

pub struct CatalogApp {
    pub store: Store,
    pub cipher_key: String,
    pub public_base: String,
    pub http: reqwest::Client,
}

pub fn router(app: Arc<CatalogApp>, identity: Arc<dyn CatalogIdentity>) -> Router {
    Router::new()
        .route("/api/v1/catalog", get(list_catalog).post(create_entry))
        .route("/api/v1/catalog/{entry_id}", get(get_entry))
        .route(
            "/api/v1/connections/oauth/client-metadata",
            get(client_metadata),
        )
        .with_state(AppState { app: app.clone(), identity: identity.clone() })
        .merge(crate::connections::router(app, identity))
}

#[derive(Clone)]
struct AppState {
    app: Arc<CatalogApp>,
    identity: Arc<dyn CatalogIdentity>,
}

#[derive(Deserialize)]
struct ListQuery {
    q: Option<String>,
    category: Option<String>,
    kind: Option<String>,
}

pub fn builtins() -> Vec<Entry> {
    let value: Value = serde_json::from_str(BUILTINS).expect("builtin catalog");
    value["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(entry_from_value)
        .map(|mut entry| {
            entry.tenant_id.clear();
            entry.source = "builtin".into();
            entry
        })
        .collect()
}

fn entry_from_value(value: &Value) -> Option<Entry> {
    Some(Entry {
        entry_id: value.get("id")?.as_str()?.to_owned(),
        tenant_id: value
            .get("tenant_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        kind: value.get("kind")?.as_str()?.to_owned(),
        name: value.get("name")?.as_str()?.to_owned(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        category: value
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        icon: value
            .get("icon")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        auth_kind: value
            .get("auth_kind")
            .and_then(Value::as_str)
            .unwrap_or("none")
            .to_owned(),
        manifest: value.get("manifest").cloned().unwrap_or_else(|| json!({})),
        source: value
            .get("source")
            .and_then(Value::as_str)
            .unwrap_or("tenant")
            .to_owned(),
        source_sha256: value
            .get("source_sha256")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    })
}

pub fn allows(actor: &Actor, permission: &str) -> bool {
    actor.permissions.iter().any(|held| {
        held == "admin:*"
            || held == permission
            || (held.ends_with(":*") && permission.starts_with(&held[..held.len() - 1]))
    })
}

pub async fn visible_entries(app: &CatalogApp, tenant_id: &str) -> Result<Vec<Entry>, StoreError> {
    let mut entries = builtins();
    for entry in app.store.list_entries(tenant_id).await? {
        if let Some(slot) = entries.iter_mut().find(|item| item.entry_id == entry.entry_id) {
            *slot = entry;
        } else {
            entries.push(entry);
        }
    }
    Ok(entries)
}

pub fn search<'a>(
    entries: &'a [Entry],
    query: &str,
    category: &str,
    kind: &str,
) -> Vec<&'a Entry> {
    let query = query.trim().to_lowercase();
    entries
        .iter()
        .filter(|entry| category.is_empty() || category == "all" || entry.category == category)
        .filter(|entry| kind.is_empty() || entry.kind == kind)
        .filter(|entry| {
            query.is_empty()
                || entry.name.to_lowercase().contains(&query)
                || entry.description.to_lowercase().contains(&query)
                || entry.kind.contains(&query)
                || entry.category.contains(&query)
        })
        .collect()
}

fn public_entry(entry: &Entry) -> Value {
    json!({
        "id": entry.entry_id,
        "kind": entry.kind,
        "name": entry.name,
        "description": entry.description,
        "category": entry.category,
        "icon": entry.icon,
        "auth_kind": entry.auth_kind,
        "source": entry.source,
        "manifest": public_manifest(&entry.manifest),
    })
}

fn public_manifest(manifest: &Value) -> Value {
    let mut copy = manifest.clone();
    if let Some(object) = copy.as_object_mut() {
        object.remove("client_secret");
        object.remove("access_token");
        object.remove("refresh_token");
        object.remove("api_key");
    }
    copy
}

pub fn encrypt_secret(key: &str, secret: &str) -> Result<String, &'static str> {
    if key.is_empty() || secret.is_empty() {
        return Err("cipher unavailable");
    }
    let ciphertext = NativeProviderCipher
        .encrypt(secret, key)
        .map_err(|_| "cipher unavailable")?;
    if !is_encrypted(&ciphertext) || ciphertext.contains(secret) {
        return Err("cipher unavailable");
    }
    Ok(ciphertext)
}

pub fn registry_entries(document: &str) -> Result<Vec<Entry>, &'static str> {
    let value: Value = serde_json::from_str(document).map_err(|_| "registry document")?;
    let sha = format!("{:x}", Sha256::digest(document.as_bytes()));
    let servers = value
        .get("servers")
        .and_then(Value::as_array)
        .ok_or("registry document")?;
    Ok(servers
        .iter()
        .filter_map(|server| {
            let record = server.get("server").unwrap_or(server);
            let name = record.get("name")?.as_str()?;
            let id = name
                .rsplit('/')
                .next()
                .unwrap_or(name)
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() {
                        character.to_ascii_lowercase()
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            if id.is_empty() {
                return None;
            }
            Some(Entry {
                entry_id: format!("registry-{id}"),
                tenant_id: String::new(),
                kind: "mcp_server".into(),
                name: name.to_owned(),
                description: record
                    .get("description")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned(),
                category: "development".into(),
                icon: "/catalog/github.svg".into(),
                auth_kind: "oauth".into(),
                manifest: record.clone(),
                source: "registry".into(),
                source_sha256: sha.clone(),
            })
        })
        .collect())
}

async fn list_catalog(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, StatusCode> {
    let actor = state.identity.actor(&headers, "/api/v1/catalog").await?;
    if !allows(&actor, "connector:view") {
        return Err(StatusCode::FORBIDDEN);
    }
    let entries = visible_entries(&state.app, &actor.tenant_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let found = search(
        &entries,
        query.q.as_deref().unwrap_or(""),
        query.category.as_deref().unwrap_or(""),
        query.kind.as_deref().unwrap_or(""),
    );
    Ok(Json(json!({
        "entries": found.into_iter().map(public_entry).collect::<Vec<_>>()
    })))
}

async fn get_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(entry_id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    let actor = state
        .identity
        .actor(&headers, "/api/v1/catalog/{entry_id}")
        .await?;
    if !allows(&actor, "connector:view") {
        return Err(StatusCode::FORBIDDEN);
    }
    let entries = visible_entries(&state.app, &actor.tenant_id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    entries
        .iter()
        .find(|entry| entry.entry_id == entry_id)
        .map(|entry| Json(public_entry(entry)))
        .ok_or(StatusCode::NOT_FOUND)
}

#[derive(Deserialize)]
struct CreateEntry {
    id: String,
    kind: String,
    name: String,
    description: Option<String>,
    category: Option<String>,
    icon: Option<String>,
    auth_kind: Option<String>,
    manifest: Option<Value>,
}

async fn create_entry(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateEntry>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    let actor = state.identity.actor(&headers, "/api/v1/catalog").await?;
    if !allows(&actor, "connector:admin") {
        return Err(StatusCode::FORBIDDEN);
    }
    if !body
        .id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
        || !matches!(
            body.kind.as_str(),
            "connector" | "mcp_server" | "skill" | "plugin" | "bot_template"
        )
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let entry = Entry {
        entry_id: body.id,
        tenant_id: actor.tenant_id,
        kind: body.kind,
        name: body.name,
        description: body.description.unwrap_or_default(),
        category: body.category.unwrap_or_else(|| "more".into()),
        icon: body.icon.unwrap_or_default(),
        auth_kind: body.auth_kind.unwrap_or_else(|| "none".into()),
        manifest: body.manifest.unwrap_or_else(|| json!({})),
        source: "tenant".into(),
        source_sha256: String::new(),
    };
    state
        .app
        .store
        .upsert_entry(&entry)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok((StatusCode::CREATED, Json(public_entry(&entry))))
}

async fn client_metadata(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "client_id": format!("{}/api/v1/connections/oauth/client-metadata", state.app.public_base.trim_end_matches('/')),
        "client_name": "AgentForge",
        "grant_types": ["authorization_code"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
        "redirect_uris": [format!("{}/api/v1/connections/oauth/callback", state.app.public_base.trim_end_matches('/'))]
    }))
}

pub fn app(
    store: Store,
    cipher_key: impl Into<String>,
    public_base: impl Into<String>,
) -> Arc<CatalogApp> {
    Arc::new(CatalogApp {
        store,
        cipher_key: cipher_key.into(),
        public_base: public_base.into(),
        http: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap_or_else(|_| reqwest::Client::new()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn actor() -> Actor {
        Actor {
            user_id: "user-1".into(),
            tenant_id: "tenant-1".into(),
            permissions: vec!["connector:view".into(), "connector:admin".into()],
        }
    }

    #[test]
    fn catalog_builtins_cover_the_connection_dialog() {
        let entries = builtins();
        for id in [
            "google-calendar",
            "notion",
            "outlook",
            "slack",
            "github",
            "gitlab",
            "jira",
            "confluence",
            "feishu",
            "dingtalk",
            "google-drive",
            "agentmail",
            "figma",
            "onepassword",
            "space-writer",
        ] {
            assert!(entries.iter().any(|entry| entry.entry_id == id), "{id}");
        }
        let found = search(&entries, "飞书", "", "");
        assert_eq!(found.len(), 1);
        assert!(search(&entries, "", "development", "skill").is_empty());
        assert!(search(&entries, "space", "", "skill")
            .iter()
            .any(|entry| entry.entry_id == "space-writer"));
    }

    #[tokio::test]
    async fn catalog_tenant_entries_stay_private() {
        let app = app(Store::Memory(MemoryStore::new()), "catalog-test-key", "http://catalog.test");
        app.store
            .upsert_entry(&Entry {
                entry_id: "private-tool".into(),
                tenant_id: "tenant-1".into(),
                kind: "mcp_server".into(),
                name: "Private tool".into(),
                description: "tenant only".into(),
                category: "development".into(),
                icon: String::new(),
                auth_kind: "none".into(),
                manifest: json!({}),
                source: "tenant".into(),
                source_sha256: String::new(),
            })
            .await
            .unwrap();
        let own = visible_entries(&app, "tenant-1").await.unwrap();
        let other = visible_entries(&app, "tenant-2").await.unwrap();
        assert!(own.iter().any(|entry| entry.entry_id == "private-tool"));
        assert!(!other.iter().any(|entry| entry.entry_id == "private-tool"));
        let _ = actor();
    }

    #[test]
    fn catalog_registry_document_keeps_its_sha() {
        let document = r#"{"servers":[{"name":"io.example/demo","description":"Demo MCP"}]}"#;
        let entries = registry_entries(document).unwrap();
        assert_eq!(entries[0].source, "registry");
        assert_eq!(entries[0].source_sha256.len(), 64);
        assert_eq!(
            entries[0].source_sha256,
            format!("{:x}", Sha256::digest(document.as_bytes()))
        );
    }
}
