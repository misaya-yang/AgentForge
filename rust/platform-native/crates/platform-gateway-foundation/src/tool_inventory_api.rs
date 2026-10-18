//! Complete inert route library for `src/api/v1/tool_inventory.py`.
//!
//! Host supplies canonical JWT/API-key/anonymous get_user_context resolution and
//! its actual authentication bit. No admission, tenant-policy, capability or
//! worker call is added. The already validated native catalog is projected with
//! the source's default empty tenant policy, never the JWT-only catalog router.
//! An invalid selected snapshot is retained as an error, not replaced by bundled
//! data; endpoint authentication precedes its 500 shared host-error rendering.
//!
//! HEAD/other methods are rejected before identity; GET ignores request bodies,
//! media types and query parameters. Cancellation is delegated, never rendered
//! as ordinary failure. Dropping the request drops the awaited identity future.
//! Production mounting, credential verification, global middleware/body limits,
//! trusted trace-state injection and slash/encoded-path normalization are host
//! boundaries. Merge users must reset this factory's fallback before merging.
use crate::{
    assistant_catalog::{CapabilityCatalog, CatalogError, ToolInfoResponse, project_tools},
    config_audit_writer::ConfigAuditRequestContext,
    service_config_api::{ServiceConfigCancellation, ServiceConfigError, ServiceConfigFuture},
};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::json;
use std::sync::Arc;

pub const TOOL_INVENTORY_ROUTES: &[(&str, &str, &str)] =
    &[("GET", "/debug/tools", "list_registered_tools")];

/// Trusted resolver output. A user ID or role must never imply authentication.
#[derive(Clone, Debug, Default)]
pub struct ToolInventoryIdentity {
    pub is_authenticated: bool,
    pub tier: String,
    pub roles: Vec<String>,
}
pub trait ToolInventoryIdentityResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        headers: &'a HeaderMap,
        path: &'a str,
    ) -> ServiceConfigFuture<'a, ToolInventoryIdentity>;
    /// Canonical tenant authority for activated publisher filtering. Existing
    /// resolvers without a tenant expose only the builtin inventory.
    fn tenant_id<'a>(&'a self, _headers: &'a HeaderMap) -> ServiceConfigFuture<'a, String> {
        Box::pin(async { Ok(String::new()) })
    }
}

/// Preserve Python dictionary first-occurrence order as well as name order.
#[derive(Debug, Default)]
pub struct InventoryCategories(pub Vec<(String, Vec<String>)>);
impl Serialize for InventoryCategories {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, names) in &self.0 {
            map.serialize_entry(key, names)?;
        }
        map.end()
    }
}
#[derive(Debug, Serialize)]
pub struct ToolInventoryResponse {
    pub total: usize,
    pub by_category: InventoryCategories,
    pub confluence: ConfluenceInventory,
}
#[derive(Debug, Serialize)]
pub struct ConfluenceInventory {
    pub tools: Vec<String>,
    pub new_meta_tools_present: bool,
    pub stale_single_purpose_tools_present: bool,
    pub status: &'static str,
}
/// Projection only, including defensive empty-category fallback from the source.
/// Valid CapabilityCatalog records currently always have nonempty categories.
pub fn summarize_tools(tools: &[ToolInfoResponse]) -> ToolInventoryResponse {
    let mut by_category = InventoryCategories::default();
    let mut confluence = Vec::new();
    for tool in tools {
        let category = if tool.category.is_empty() {
            "unknown"
        } else {
            &tool.category
        };
        if let Some((_, names)) = by_category.0.iter_mut().find(|(key, _)| key == category) {
            names.push(tool.name.clone());
        } else {
            by_category
                .0
                .push((category.to_owned(), vec![tool.name.clone()]));
        }
        if tool.name.contains("confluence") {
            confluence.push(tool.name.clone());
        }
    }
    let has_new = confluence.iter().any(|name| name == "confluence_read")
        && confluence.iter().any(|name| name == "confluence_write");
    let has_old = confluence.iter().any(|name| {
        [
            "search_confluence",
            "read_confluence_page",
            "create_confluence_page",
            "update_confluence_page",
            "delete_confluence_page",
            "add_confluence_comment",
            "list_confluence_spaces",
            "get_confluence_space",
        ]
        .contains(&name.as_str())
    });
    ToolInventoryResponse {
        total: tools.len(),
        by_category,
        confluence: ConfluenceInventory {
            tools: confluence,
            new_meta_tools_present: has_new,
            stale_single_purpose_tools_present: has_old,
            status: if has_new && !has_old {
                "OK — new meta-tools loaded"
            } else if has_old {
                "STALE — deprecated single-purpose capabilities are still published"
            } else {
                "MISSING — Confluence capabilities are not visible to this tenant."
            },
        },
    }
}

pub struct ToolInventoryApi {
    pub identity: Arc<dyn ToolInventoryIdentityResolver>,
    catalog: Result<Arc<CapabilityCatalog>, CatalogError>,
}
impl ToolInventoryApi {
    /// Loading/refreshing the selected immutable catalog is an assembly concern.
    /// Retaining a failed load preserves anonymous 401 before catalog errors.
    pub fn new(
        identity: Arc<dyn ToolInventoryIdentityResolver>,
        catalog: Result<Arc<CapabilityCatalog>, CatalogError>,
    ) -> Self {
        Self { identity, catalog }
    }
    pub fn execute(
        &self,
        identity: &ToolInventoryIdentity,
    ) -> Result<ToolInventoryResponse, ServiceConfigError> {
        self.execute_for_tenant(identity, "")
    }
    pub fn execute_for_tenant(
        &self, identity: &ToolInventoryIdentity, tenant: &str,
    ) -> Result<ToolInventoryResponse, ServiceConfigError> {
        if !identity.is_authenticated {
            return Err(ServiceConfigError::detail(401, "authentication required"));
        }
        let catalog = self
            .catalog
            .as_ref()
            .map_err(|_| ServiceConfigError::Internal)?;
        let tenant_catalog = catalog.for_tenant(tenant);
        let tools = project_tools(
            &tenant_catalog,
            identity.is_authenticated,
            &identity.tier,
            &identity.roles,
            &json!({}),
        )
        .map_err(|_| ServiceConfigError::Internal)?;
        Ok(summarize_tools(&tools))
    }
}
#[derive(Clone)]
struct HttpState {
    api: Arc<ToolInventoryApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
}
pub fn router(
    api: Arc<ToolInventoryApi>,
    cancellation: Arc<dyn ServiceConfigCancellation>,
) -> Router {
    Router::new()
        .route("/api/v1/debug/tools", any(dispatch))
        .fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
        .with_state(HttpState { api, cancellation })
}
async fn dispatch(State(state): State<HttpState>, request: Request) -> Response {
    if request.method() != axum::http::Method::GET {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [("allow", "GET")],
            Json(json!({"detail":"Method Not Allowed"})),
        )
            .into_response();
    }
    let headers = request.headers();
    let result = match state
        .api
        .identity
        .resolve(headers, request.uri().path())
        .await
    {
        Ok(identity) if identity.is_authenticated => match state.api.identity.tenant_id(headers).await {
            Ok(tenant) => state.api.execute_for_tenant(&identity, &tenant),
            Err(error) => Err(error),
        },
        Ok(identity) => state.api.execute(&identity),
        Err(error) => Err(error),
    };
    match result {
        Ok(value) => Json(value).into_response(),
        Err(ServiceConfigError::Cancelled) => state.cancellation.cancelled().await,
        Err(error) => {
            let (status, body) = error
                .response(&ConfigAuditRequestContext {
                    headers: headers.clone(),
                    ..Default::default()
                })
                .expect("cancellation handled separately");
            (
                StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(body),
            )
                .into_response()
        }
    }
}
