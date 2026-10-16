//! Deployment registry and tenant lifecycle projected from validated descriptors.
//! Package text and arbitrary plugin metadata are never exposed as authority.
use super::{
    CapabilityCatalog, CatalogError, CatalogState, authenticate, error, project_tools,
    lifecycle, resolved_policies, user_has_required_permissions,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub use crate::native_contract::responses::{PluginRegistry, Plugin, PluginVersion, PluginCapability};
fn strings(record: &Value, key: &str) -> Vec<String> {
    record[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// Version labels are exact catalog identities, not semver ordering or an upgrade
/// recommendation. A missing pair is rejected here without changing legacy tools.
/// Callers see only capabilities allowed by their existing permission lattice;
/// policy-blocked capabilities remain inspectable so an admin can diagnose policy.
pub fn project_registry(
    catalog: &CapabilityCatalog,
    authenticated: bool,
    tier: &str,
    roles: &[String],
    tenant_policy: &Value,
) -> Result<PluginRegistry, CatalogError> {
    let available: BTreeSet<String> = project_tools(
        catalog, authenticated, tier, roles, tenant_policy,
    )?
    .into_iter()
    .map(|tool| tool.name)
    .collect();
    let mut grouped: BTreeMap<String, BTreeMap<String, PluginVersion>> = BTreeMap::new();
    for record in catalog.records() {
        let (id, version) = match (record["plugin_id"].as_str(), record["plugin_version"].as_str()) {
            (None, None) => continue,
            (Some(id), Some(version)) => (id, version),
            _ => return Err(CatalogError::Malformed),
        };
        let required = strings(record, "required_permissions");
        if !user_has_required_permissions(tier, roles, &required) {
            continue;
        }
        let declared = strings(record, "permissions");
        let entry = grouped.entry(id.to_owned()).or_default()
            .entry(version.to_owned()).or_insert_with(|| PluginVersion {
                version: version.to_owned(),
                availability: "policy_blocked",
                required_permissions: BTreeSet::new(),
                declared_permissions: BTreeSet::new(),
                capabilities: Vec::new(),
            });
        entry.required_permissions.extend(required.iter().cloned());
        entry.declared_permissions.extend(declared.iter().cloned());
        // Every mandatory field was checked by CapabilityCatalog::from_bytes.
        let text = |key: &str| record[key].as_str().expect("validated capability field").to_owned();
        let name = text("name");
        entry.capabilities.push(PluginCapability {
            available: available.contains(&name),
            id: text("id"),
            name,
            version: record["version"].as_str().unwrap_or("null").to_owned(),
            schema_hash: text("schema_hash"),
            category: text("category"),
            risk_level: text("risk"),
            read_only: record["read_only"].as_bool().expect("validated read_only"),
            required_permissions: required,
            declared_permissions: declared,
        });
    }
    let plugins = grouped.into_iter().map(|(id, versions)| {
        let versions = versions.into_values().map(|mut version| {
            version.capabilities.sort_by(|a, b| a.name.cmp(&b.name));
            let available = version.capabilities.iter().filter(|c| c.available).count();
            version.availability = if available == version.capabilities.len() {
                "available"
            } else if available == 0 {
                "policy_blocked"
            } else {
                "partially_available"
            };
            version
        }).collect();
        Plugin { id, versions, lifecycle: lifecycle::PluginLifecycle::default() }
    }).collect();
    Ok(PluginRegistry {
        schema_version: "ai-platform-plugin-registry/v1",
        catalog_revision: catalog.revision(),
        catalog_revision_token: catalog.revision().to_string(),
        tenant_id: String::new(),
        can_manage: false,
        package_installation_supported: false,
        plugins,
    })
}

pub(super) async fn list_plugins(State(state): State<CatalogState>, headers: HeaderMap) -> Response {
    let principal = match authenticate(&state, &headers).await {
        Ok(value) => value,
        Err(response) => return response,
    };
    let catalog = match &state.options.catalog {
        Ok(value) => value,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin catalog is unavailable"),
    };
    let policy = match resolved_policies(&state, &principal, catalog).await
        .and_then(|policy| serde_json::to_value(policy).map_err(|_| CatalogError::Malformed)) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant policy is unavailable"),
    };
    let bindings = match lifecycle::load_bindings(&state.pool, &principal.tenant_id).await {
        Ok(bindings) => bindings,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin policy is unavailable"),
    };
    let denied = match lifecycle::denied_capabilities(catalog, &bindings) {
        Ok(denied) => denied,
        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin policy is unavailable"),
    };
    match project_registry(catalog, true, &principal.tier, &principal.roles, &policy) {
        Ok(mut registry) => {
            registry.tenant_id = principal.tenant_id;
            registry.can_manage = lifecycle::can_manage(&principal.tier, &principal.roles);
            for plugin in &mut registry.plugins {
                if let Some(binding) = bindings.get(&plugin.id) {
                    plugin.lifecycle = match lifecycle::project_binding(catalog, binding) {
                        Ok(lifecycle) => lifecycle,
                        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin policy is unavailable"),
                    };
                }
                if !registry.can_manage && plugin.lifecycle.selected_version.as_ref().is_some_and(|selected| {
                    !plugin.versions.iter().any(|version| &version.version == selected)
                }) {
                    plugin.lifecycle.selected_version = None;
                }
                for version in &mut plugin.versions {
                    for capability in &mut version.capabilities {
                        capability.available &= !denied.contains(&capability.name);
                    }
                    let available = version.capabilities.iter().filter(|c| c.available).count();
                    version.availability = if available == version.capabilities.len() { "available" }
                        else if available == 0 { "policy_blocked" } else { "partially_available" };
                }
            }
            // Only administrators see saved orphan metadata. No hidden current
            // catalog version is added to a caller's permission-filtered registry.
            if registry.can_manage {
                for (id, binding) in &bindings {
                    if catalog.records().iter().any(|r| r["plugin_id"] == id.as_str()) { continue; }
                    let lifecycle = match lifecycle::project_binding(catalog, binding) {
                        Ok(lifecycle) => lifecycle,
                        Err(_) => return error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin policy is unavailable"),
                    };
                    registry.plugins.push(Plugin { id: id.clone(), versions: vec![], lifecycle });
                }
                registry.plugins.sort_by(|a, b| a.id.cmp(&b.id));
            }
            Json(registry).into_response()
        },
        Err(_) => error(StatusCode::SERVICE_UNAVAILABLE, "Assistant plugin catalog is unavailable"),
    }
}
