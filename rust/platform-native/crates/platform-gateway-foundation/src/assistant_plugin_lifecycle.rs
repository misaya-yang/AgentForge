//! Tenant-owned admission preferences over deployment-registered capabilities.
//! This is neither a package installer nor a permission grant. Missing migration
//! or unavailable storage fails closed; absent rows preserve deployment defaults.
use super::{CapabilityCatalog, CatalogError, CatalogResult, canonical_json, check, is_name};
use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{FromRow, PgPool};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, FromRow)]
pub struct PluginBinding {
    pub plugin_id: String,
    pub selected_version: String,
    pub enabled: bool,
    pub revision: i64,
    pub selected_catalog_revision: String,
    pub selection_hash: String,
    // Retained names prevent a removed deployment record from escaping a deny.
    pub capability_names: Vec<String>,
    pub updated_at: DateTime<Utc>,
}
pub type PluginBindings = BTreeMap<String, PluginBinding>;

pub use crate::native_contract::responses::PluginLifecycle;

pub fn can_manage(tier: &str, roles: &[String]) -> bool {
    roles.iter().any(|role| role == "admin") || tier.eq_ignore_ascii_case("admin")
}

/// Fingerprint all validated descriptor fields, including admission requirements.
/// A reused version label with changed content requires an explicit reselection.
pub fn selection_hash(catalog: &CapabilityCatalog, plugin: &str, version: &str) -> CatalogResult<Option<String>> {
    let mut records: Vec<_> = catalog.records().iter().filter(|record| {
        record["plugin_id"] == plugin && record["plugin_version"] == version
    }).cloned().collect();
    if records.is_empty() {
        return Ok(None);
    }
    records.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let mut canonical = String::new();
    canonical_json(&json!(records), &mut canonical)?;
    Ok(Some(format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))))
}

pub fn project_binding(catalog: &CapabilityCatalog, binding: &PluginBinding) -> CatalogResult<PluginLifecycle> {
    let current = selection_hash(catalog, &binding.plugin_id, &binding.selected_version)?;
    let selection_status = match current {
        None => "version_unavailable",
        Some(hash) if hash != binding.selection_hash => "catalog_changed",
        Some(_) => "current",
    };
    Ok(PluginLifecycle {
        managed: true,
        enabled: binding.enabled,
        selected_version: Some(binding.selected_version.clone()),
        revision: binding.revision.to_string(),
        selected_catalog_revision: Some(binding.selected_catalog_revision.clone()),
        selection_status,
        updated_at: Some(binding.updated_at),
    })
}

pub async fn load_bindings(pool: &PgPool, tenant_id: &str) -> CatalogResult<PluginBindings> {
    check(!tenant_id.is_empty())?;
    let rows = sqlx::query_as::<_, PluginBinding>(
        "SELECT plugin_id,selected_version,enabled,revision,selected_catalog_revision,selection_hash,capability_names,updated_at FROM gateway.tenant_plugin_bindings WHERE tenant_id=$1 ORDER BY plugin_id LIMIT 1025",
    ).bind(tenant_id).fetch_all(pool).await.map_err(|_| CatalogError::Unavailable)?;
    check(rows.len() <= 1024)?;
    let mut bindings = BTreeMap::new();
    for row in rows {
        check(is_name(&row.plugin_id) && is_name(&row.selected_version) && row.revision > 0)?;
        check(row.selected_catalog_revision.parse::<u64>().is_ok_and(|n| n > 0 && n.to_string() == row.selected_catalog_revision))?;
        check(row.capability_names.len() <= 1024 && row.capability_names.iter().all(|name| is_name(name)))?;
        check(super::valid_hash(&json!(row.selection_hash)))?;
        check(bindings.insert(row.plugin_id.clone(), row).is_none())?;
    }
    Ok(bindings)
}

/// Intersection only: add lifecycle denials to existing policy. Never remove
/// manual blocks, expand allowlists, grant permissions, or alter approval rules.
pub fn denied_capabilities(catalog: &CapabilityCatalog, bindings: &PluginBindings) -> CatalogResult<BTreeSet<String>> {
    let mut blocked = BTreeSet::new();
    for record in catalog.records() {
        check(record["plugin_id"].is_null() == record["plugin_version"].is_null())?;
    }
    for (plugin, binding) in bindings {
        let current = binding.enabled
            && selection_hash(catalog, plugin, &binding.selected_version)?.as_ref() == Some(&binding.selection_hash);
        let records: Vec<_> = catalog.records().iter().filter(|record| record["plugin_id"] == plugin.as_str()).collect();
        let selected: BTreeSet<_> = records.iter().filter(|record| current && record["plugin_version"] == binding.selected_version)
            .filter_map(|record| record["name"].as_str()).collect();
        let names = binding.capability_names.iter().map(String::as_str)
            .chain(records.iter().filter_map(|record| record["name"].as_str()));
        blocked.extend(names.filter(|name| !selected.contains(name)).map(str::to_owned));
    }
    Ok(blocked)
}

