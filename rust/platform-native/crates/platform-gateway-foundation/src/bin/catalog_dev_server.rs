//! Local catalog and MCP server for the M3 smoke scripts.
//! Tokens and cipher keys come from the environment and are never logged.
use ai_platform_gateway_foundation::catalog::{self, Actor, FixedIdentity, Store};
use ai_platform_gateway_foundation::mcp_server::{self, McpConfig};
use std::env;
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr: SocketAddr = env::var("CATALOG_DEV_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8096".into())
        .parse()?;
    let public_base = env::var("CATALOG_PUBLIC_BASE")
        .unwrap_or_else(|_| format!("http://{addr}"));
    let cipher_key = env::var("CATALOG_CIPHER_KEY").unwrap_or_else(|_| "catalog-test-key".into());
    let bearer = env::var("CATALOG_DEV_BEARER").unwrap_or_else(|_| "catalog-dev-bearer".into());
    let knowledge = env::var("KNOWLEDGE_BASE_URL").unwrap_or_else(|_| public_base.clone());
    let store = match env::var("AI_GATEWAY_CATALOG_DATABASE_URL") {
        Ok(url) if !url.is_empty() => {
            let pool = sqlx::postgres::PgPoolOptions::new()
                .max_connections(4)
                .connect(&url)
                .await?;
            Store::Postgres(catalog::PgStore::new(pool))
        }
        _ => Store::Memory(catalog::MemoryStore::new()),
    };
    let app = catalog::app(store, cipher_key, public_base.clone());
    let identity = Arc::new(FixedIdentity(Actor {
        user_id: "catalog-dev".into(),
        tenant_id: "tenant-dev".into(),
        permissions: vec![
            "connector:view".into(),
            "connector:connect".into(),
            "connector:share".into(),
            "connector:admin".into(),
        ],
    }));
    let router = catalog::router(app, identity).merge(mcp_server::router(McpConfig {
        knowledge_base: knowledge,
        bearer,
        resource: format!("{}/mcp", public_base.trim_end_matches('/')),
        http: reqwest::Client::new(),
    }));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("catalog-dev-server {addr}");
    axum::serve(listener, router).await?;
    Ok(())
}
