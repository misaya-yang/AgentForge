use super::{Connection, Entry, OAuthState};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use std::sync::Mutex;

#[derive(Debug)]
pub struct StoreError(pub String);

pub enum Store {
    Memory(MemoryStore),
    Postgres(PgStore),
}

impl Store {
    pub async fn list_entries(&self, tenant_id: &str) -> Result<Vec<Entry>, StoreError> {
        match self {
            Self::Memory(store) => store.list_entries(tenant_id),
            Self::Postgres(store) => store.list_entries(tenant_id).await,
        }
    }
    pub async fn upsert_entry(&self, entry: &Entry) -> Result<(), StoreError> {
        match self {
            Self::Memory(store) => store.upsert_entry(entry),
            Self::Postgres(store) => store.upsert_entry(entry).await,
        }
    }
    pub async fn list_connections(&self, tenant_id: &str) -> Result<Vec<Connection>, StoreError> {
        match self {
            Self::Memory(store) => store.list_connections(tenant_id),
            Self::Postgres(store) => store.list_connections(tenant_id).await,
        }
    }
    pub async fn upsert_connection(&self, connection: &Connection) -> Result<(), StoreError> {
        match self {
            Self::Memory(store) => store.upsert_connection(connection),
            Self::Postgres(store) => store.upsert_connection(connection).await,
        }
    }
    pub async fn put_oauth_state(&self, state: &OAuthState) -> Result<(), StoreError> {
        match self {
            Self::Memory(store) => store.put_oauth_state(state),
            Self::Postgres(store) => store.put_oauth_state(state).await,
        }
    }
    pub async fn take_oauth_state(&self, state: &str) -> Result<Option<OAuthState>, StoreError> {
        match self {
            Self::Memory(store) => store.take_oauth_state(state),
            Self::Postgres(store) => store.take_oauth_state(state).await,
        }
    }
}

pub struct MemoryStore {
    entries: Mutex<HashMap<String, Entry>>,
    connections: Mutex<HashMap<String, Connection>>,
    states: Mutex<HashMap<String, OAuthState>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            connections: Mutex::new(HashMap::new()),
            states: Mutex::new(HashMap::new()),
        }
    }
    fn list_entries(&self, tenant_id: &str) -> Result<Vec<Entry>, StoreError> {
        let entries = self.entries.lock().map_err(|_| StoreError("lock".into()))?;
        Ok(entries
            .values()
            .filter(|entry| entry.tenant_id.is_empty() || entry.tenant_id == tenant_id)
            .cloned()
            .collect())
    }
    fn upsert_entry(&self, entry: &Entry) -> Result<(), StoreError> {
        self.entries
            .lock()
            .map_err(|_| StoreError("lock".into()))?
            .insert(entry.entry_id.clone(), entry.clone());
        Ok(())
    }
    fn list_connections(&self, tenant_id: &str) -> Result<Vec<Connection>, StoreError> {
        let connections = self
            .connections
            .lock()
            .map_err(|_| StoreError("lock".into()))?;
        Ok(connections
            .values()
            .filter(|connection| connection.tenant_id == tenant_id)
            .cloned()
            .collect())
    }
    fn upsert_connection(&self, connection: &Connection) -> Result<(), StoreError> {
        self.connections
            .lock()
            .map_err(|_| StoreError("lock".into()))?
            .insert(connection.connection_id.clone(), connection.clone());
        Ok(())
    }
    fn put_oauth_state(&self, state: &OAuthState) -> Result<(), StoreError> {
        self.states
            .lock()
            .map_err(|_| StoreError("lock".into()))?
            .insert(state.state.clone(), state.clone());
        Ok(())
    }
    fn take_oauth_state(&self, state: &str) -> Result<Option<OAuthState>, StoreError> {
        let mut states = self.states.lock().map_err(|_| StoreError("lock".into()))?;
        let Some(found) = states.get(state).cloned() else {
            return Ok(None);
        };
        if found.consumed {
            return Ok(None);
        }
        if let Some(slot) = states.get_mut(state) {
            slot.consumed = true;
        }
        Ok(Some(found))
    }
}

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    async fn list_entries(&self, tenant_id: &str) -> Result<Vec<Entry>, StoreError> {
        let rows = sqlx::query(
            "SELECT entry_id, tenant_id, kind, name, description, category, icon, auth_kind, manifest, source, source_sha256 FROM gateway.catalog_entries WHERE tenant_id = '' OR tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| StoreError(error.to_string()))?;
        Ok(rows.iter().map(entry_from).collect())
    }
    async fn upsert_entry(&self, entry: &Entry) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO gateway.catalog_entries (entry_id, tenant_id, kind, name, description, category, icon, auth_kind, manifest, source, source_sha256) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT (entry_id) DO UPDATE SET tenant_id=EXCLUDED.tenant_id, kind=EXCLUDED.kind, name=EXCLUDED.name, description=EXCLUDED.description, category=EXCLUDED.category, icon=EXCLUDED.icon, auth_kind=EXCLUDED.auth_kind, manifest=EXCLUDED.manifest, source=EXCLUDED.source, source_sha256=EXCLUDED.source_sha256, updated_at=now()",
        )
        .bind(&entry.entry_id)
        .bind(&entry.tenant_id)
        .bind(&entry.kind)
        .bind(&entry.name)
        .bind(&entry.description)
        .bind(&entry.category)
        .bind(&entry.icon)
        .bind(&entry.auth_kind)
        .bind(&entry.manifest)
        .bind(&entry.source)
        .bind(&entry.source_sha256)
        .execute(&self.pool)
        .await
        .map_err(|error| StoreError(error.to_string()))?;
        Ok(())
    }
    async fn list_connections(&self, tenant_id: &str) -> Result<Vec<Connection>, StoreError> {
        let rows = sqlx::query(
            "SELECT connection_id, tenant_id, entry_id, owner_kind, owner_id, status, credential_ciphertext, issuer, resource, error_code, created_by FROM gateway.catalog_connections WHERE tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| StoreError(error.to_string()))?;
        Ok(rows.iter().map(connection_from).collect())
    }
    async fn upsert_connection(&self, connection: &Connection) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO gateway.catalog_connections (connection_id, tenant_id, entry_id, owner_kind, owner_id, status, credential_ciphertext, issuer, resource, error_code, created_by) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT (connection_id) DO UPDATE SET status=EXCLUDED.status, credential_ciphertext=EXCLUDED.credential_ciphertext, issuer=EXCLUDED.issuer, resource=EXCLUDED.resource, error_code=EXCLUDED.error_code, updated_at=now()",
        )
        .bind(&connection.connection_id)
        .bind(&connection.tenant_id)
        .bind(&connection.entry_id)
        .bind(&connection.owner_kind)
        .bind(&connection.owner_id)
        .bind(&connection.status)
        .bind(&connection.credential_ciphertext)
        .bind(&connection.issuer)
        .bind(&connection.resource)
        .bind(&connection.error_code)
        .bind(&connection.created_by)
        .execute(&self.pool)
        .await
        .map_err(|error| StoreError(error.to_string()))?;
        Ok(())
    }
    async fn put_oauth_state(&self, state: &OAuthState) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO gateway.catalog_oauth_states (state, tenant_id, user_id, entry_id, connection_id, code_verifier_ciphertext, issuer, resource, redirect_uri, client_id, token_url, expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,to_timestamp($12))",
        )
        .bind(&state.state)
        .bind(&state.tenant_id)
        .bind(&state.user_id)
        .bind(&state.entry_id)
        .bind(&state.connection_id)
        .bind(&state.code_verifier_ciphertext)
        .bind(&state.issuer)
        .bind(&state.resource)
        .bind(&state.redirect_uri)
        .bind(&state.client_id)
        .bind(&state.token_url)
        .bind(state.expires_at)
        .execute(&self.pool)
        .await
        .map_err(|error| StoreError(error.to_string()))?;
        Ok(())
    }
    async fn take_oauth_state(&self, state: &str) -> Result<Option<OAuthState>, StoreError> {
        let row = sqlx::query(
            "UPDATE gateway.catalog_oauth_states SET consumed_at = now() WHERE state = $1 AND consumed_at IS NULL RETURNING state, tenant_id, user_id, entry_id, connection_id, code_verifier_ciphertext, issuer, resource, redirect_uri, client_id, token_url, EXTRACT(EPOCH FROM expires_at)::bigint AS expires_at",
        )
        .bind(state)
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| StoreError(error.to_string()))?;
        Ok(row.map(|row| OAuthState {
            state: row.get("state"),
            tenant_id: row.get("tenant_id"),
            user_id: row.get("user_id"),
            entry_id: row.get("entry_id"),
            connection_id: row.get("connection_id"),
            code_verifier_ciphertext: row.get("code_verifier_ciphertext"),
            issuer: row.get("issuer"),
            resource: row.get("resource"),
            redirect_uri: row.get("redirect_uri"),
            client_id: row.get("client_id"),
            token_url: row.get("token_url"),
            expires_at: row.get("expires_at"),
            consumed: false,
        }))
    }
}

fn entry_from(row: &sqlx::postgres::PgRow) -> Entry {
    Entry {
        entry_id: row.get("entry_id"),
        tenant_id: row.get("tenant_id"),
        kind: row.get("kind"),
        name: row.get("name"),
        description: row.get("description"),
        category: row.get("category"),
        icon: row.get("icon"),
        auth_kind: row.get("auth_kind"),
        manifest: row.get("manifest"),
        source: row.get("source"),
        source_sha256: row.get("source_sha256"),
    }
}

fn connection_from(row: &sqlx::postgres::PgRow) -> Connection {
    Connection {
        connection_id: row.get("connection_id"),
        tenant_id: row.get("tenant_id"),
        entry_id: row.get("entry_id"),
        owner_kind: row.get("owner_kind"),
        owner_id: row.get("owner_id"),
        status: row.get("status"),
        credential_ciphertext: row.get("credential_ciphertext"),
        issuer: row.get("issuer"),
        resource: row.get("resource"),
        error_code: row.get("error_code"),
        created_by: row.get("created_by"),
    }
}
