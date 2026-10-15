//! Canonical connector_mcp.py in-memory tenant/provider registration lifecycle.
//! No subprocess or MCP remote discovery is substituted for the absent source
//! ai_gateway_core.tools.confluence_tool.register_confluence_tools implementation.
//! The default registrar fails explicitly. Injected registrars are a host-owned
//! seam, not evidence that the canonical missing provider implementation exists.
use crate::connector_user_types::*;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub const CANONICAL_REGISTRAR_UNAVAILABLE: &str =
    "Failed to activate Confluence tools: No module named 'ai_gateway_core.tools.confluence_tool'";
/// Synchronous like the source registration function. Do not store credentials
/// outside the gateway or echo credential-bearing failures through this boundary.
pub trait ConnectorConfluenceRegistrar: Send + Sync {
    fn register(
        &self,
        tenant_id: &str,
        domain: &str,
        email: &str,
        api_token: &str,
    ) -> Result<(), ConnectorRegistrationError>;
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorRegistrationError {
    CanonicalUnavailable,
    Failed,
}
#[derive(Default)]
pub struct UnavailableCanonicalConfluenceRegistrar;
impl ConnectorConfluenceRegistrar for UnavailableCanonicalConfluenceRegistrar {
    fn register(
        &self,
        _tenant_id: &str,
        _domain: &str,
        _email: &str,
        _api_token: &str,
    ) -> Result<(), ConnectorRegistrationError> {
        Err(ConnectorRegistrationError::CanonicalUnavailable)
    }
}
pub struct CanonicalConnectorActivation {
    registrar: Arc<dyn ConnectorConfluenceRegistrar>,
    // Keys deliberately use the source colon-concatenation, including collisions
    // if unvalidated identities themselves contain colons. Not user-scoped.
    active: Mutex<Vec<(String, Vec<String>)>>,
}
impl Default for CanonicalConnectorActivation {
    fn default() -> Self {
        Self::new(Arc::new(UnavailableCanonicalConfluenceRegistrar))
    }
}
impl CanonicalConnectorActivation {
    pub fn new(registrar: Arc<dyn ConnectorConfluenceRegistrar>) -> Self {
        Self {
            registrar,
            active: Mutex::new(Vec::new()),
        }
    }
    pub fn active_connections(&self) -> Result<Vec<String>, ConnectorError> {
        Ok(self
            .active
            .lock()
            .map_err(|_| ConnectorError::source())?
            .iter()
            .map(|(key, _)| key.clone())
            .collect())
    }
    pub async fn stop_all(&self) -> Result<(), ConnectorError> {
        self.active
            .lock()
            .map_err(|_| ConnectorError::source())?
            .clear();
        Ok(())
    }
}
impl ConnectorActivation for CanonicalConnectorActivation {
    fn start_confluence<'a>(
        &'a self,
        tenant_id: &'a str,
        domain: &'a str,
        email: &'a str,
        api_token: &'a str,
    ) -> ConnectorFuture<'a, Vec<Value>> {
        Box::pin(async move {
            let key = format!("{tenant_id}:confluence");
            let mut active = self.active.lock().map_err(|_| ConnectorError::source())?;
            active.retain(|(existing, _)| existing != &key);
            // Stop before registration. A failed replacement leaves inactive.
            self.registrar
                .register(tenant_id, domain, email, api_token)
                .map_err(|error| {
                    ConnectorError::detail(
                        500,
                        match error {
                            ConnectorRegistrationError::CanonicalUnavailable => {
                                CANONICAL_REGISTRAR_UNAVAILABLE
                            }
                            // Source RuntimeError includes arbitrary registrar error text. This
                            // native boundary intentionally sanitizes it to prevent token leaks.
                            ConnectorRegistrationError::Failed => {
                                "Failed to activate Confluence tools"
                            }
                        },
                    )
                })?;
            active.push((
                key,
                vec!["confluence_read".into(), "confluence_write".into()],
            ));
            Ok(vec![
                json!({"name":"confluence_read","description":"Read Confluence: search pages, read a page (by id/title/url), list spaces, get a space, list children."}),
                json!({"name":"confluence_write","description":"Write Confluence: create_page / update_page / find_replace (safe partial edit) / comment / delete_page. Requires user approval."}),
            ])
        })
    }
    fn stop<'a>(&'a self, tenant_id: &'a str, provider: &'a str) -> ConnectorFuture<'a, ()> {
        Box::pin(async move {
            self.active
                .lock()
                .map_err(|_| ConnectorError::source())?
                .retain(|(key, _)| key != &format!("{tenant_id}:{provider}"));
            Ok(())
        })
    }
    fn status(
        &self,
        tenant_id: &str,
        provider: &str,
    ) -> Result<(bool, Vec<Value>), ConnectorError> {
        let active = self.active.lock().map_err(|_| ConnectorError::source())?;
        match active
            .iter()
            .find(|(key, _)| key == &format!("{tenant_id}:{provider}"))
        {
            None => Ok((false, vec![])),
            Some((_, names)) => Ok((
                true,
                names
                    .iter()
                    .map(|name| json!({"name":name,"description":""}))
                    .collect(),
            )),
        }
    }
}
