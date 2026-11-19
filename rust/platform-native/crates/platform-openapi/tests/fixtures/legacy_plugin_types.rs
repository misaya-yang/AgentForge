// Preimage DTO fixtures: exact existing declarations, not production owners.
#[allow(dead_code)]
mod legacy {
use super::*;
mod lifecycle {
use super::*;
#[derive(Debug, Serialize)]
pub struct PluginLifecycle {
    pub managed: bool,
    pub enabled: bool,
    pub selected_version: Option<String>,
    pub revision: String,
    pub selected_catalog_revision: Option<String>,
    pub selection_status: &'static str,
    pub updated_at: Option<DateTime<Utc>>,
}
impl Default for PluginLifecycle {
    fn default() -> Self {
        Self {
            managed: false,
            enabled: true,
            selected_version: None,
            revision: "0".into(),
            selected_catalog_revision: None,
            selection_status: "deployment_default",
            updated_at: None,
        }
    }
}


}
pub use lifecycle::PluginLifecycle;
#[derive(Debug, Serialize)]
pub struct PluginRegistry {
    pub schema_version: &'static str,
    pub catalog_revision: u64,
    pub catalog_revision_token: String,
    pub tenant_id: String,
    pub can_manage: bool,
    pub package_installation_supported: bool,
    pub plugins: Vec<Plugin>,
}
#[derive(Debug, Serialize)]
pub struct Plugin {
    pub id: String,
    pub versions: Vec<PluginVersion>,
    pub lifecycle: lifecycle::PluginLifecycle,
}
#[derive(Debug, Serialize)]
pub struct PluginVersion {
    pub version: String,
    pub availability: &'static str,
    pub required_permissions: BTreeSet<String>,
    pub declared_permissions: BTreeSet<String>,
    pub capabilities: Vec<PluginCapability>,
}
#[derive(Debug, Serialize)]
pub struct PluginCapability {
    pub id: String,
    pub name: String,
    pub version: String,
    pub schema_hash: String,
    pub category: String,
    pub risk_level: String,
    pub read_only: bool,
    pub required_permissions: Vec<String>,
    pub declared_permissions: Vec<String>,
    pub available: bool,
}

}
