//! Tenant publisher policy carried by the immutable deployment projection. Tenant
//! preferences cannot grant a publisher missing from this reviewed trust snapshot.
use serde_json::Value;
use std::collections::BTreeSet;

pub fn publisher_admitted(record: &Value, tenant: &str) -> bool {
    let metadata = &record["plugin_metadata"];
    if metadata["activation_id"].is_null() && metadata["artifact_sha256"].is_null() {
        return true;
    }
    if metadata["worker_active"] != true || tenant.is_empty() || tenant == "*" {
        return false;
    }
    let Some(publisher) = metadata["publisher_id"].as_str() else {
        return false;
    };
    if publisher.is_empty()
        || publisher.len() > 64
        || !publisher
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
    {
        return false;
    }
    let Some(encoded) = metadata["publisher_tenants"]
        .as_str()
        .filter(|v| v.len() <= 70_000)
    else {
        return false;
    };
    let Ok(tenants) = serde_json::from_str::<Vec<String>>(encoded) else {
        return false;
    };
    let mut seen = BTreeSet::new();
    !tenants.is_empty()
        && tenants.len() <= 256
        && tenants.iter().all(|id| {
            !id.is_empty()
                && id != "*"
                && id.len() <= 255
                && !id.chars().any(char::is_control)
                && seen.insert(id.as_str())
        })
        && seen.contains(tenant)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn immutable_publisher_projection_is_exact_and_fails_closed() {
        let mut record = json!({"plugin_metadata":{"activation_id":"a","worker_active":true,
            "publisher_id":"reviewed","publisher_tenants":"[\"tenant-a\"]"}});
        assert!(publisher_admitted(&record, "tenant-a"));
        assert!(!publisher_admitted(&record, "tenant-b"));
        assert!(!publisher_admitted(&record, ""));
        for list in [
            "[]",
            "[\"*\"]",
            "[\"tenant-a\",\"tenant-a\"]",
            "{}",
            "[\"tenant-a\",3]",
        ] {
            record["plugin_metadata"]["publisher_tenants"] = json!(list);
            assert!(!publisher_admitted(&record, "tenant-a"));
        }
        record["plugin_metadata"]
            .as_object_mut()
            .unwrap()
            .remove("publisher_tenants");
        assert!(!publisher_admitted(&record, "tenant-a"));
        assert!(publisher_admitted(&json!({"id":"builtin"}), "tenant-a"));
    }
    #[test]
    fn tenant_catalog_and_registry_hide_unadmitted_publishers_without_changing_revision() {
        use crate::assistant_catalog::{CapabilityCatalog, plugins::project_registry};
        let mut value: Value =
            serde_json::from_slice(include_bytes!("../data/assistant_catalog_v1.json")).unwrap();
        value["capabilities"][0]["plugin_id"] = json!("fixture-plugin");
        value["capabilities"][0]["plugin_version"] = json!("1");
        value["capabilities"][0]["plugin_metadata"] = json!({"activation_id":"fixture",
            "worker_active":true,"publisher_id":"reviewed","publisher_tenants":"[\"tenant-a\"]"});
        let catalog = CapabilityCatalog::from_bytes(&serde_json::to_vec(&value).unwrap()).unwrap();
        let admitted = catalog.for_tenant("tenant-a");
        let denied = catalog.for_tenant("tenant-b");
        assert_eq!(admitted.records().len(), catalog.records().len());
        assert_eq!(denied.records().len() + 1, catalog.records().len());
        assert_eq!(denied.revision(), catalog.revision());
        let registry =
            project_registry(&denied, true, "admin", &["admin".into()], &json!({})).unwrap();
        assert!(!registry.plugins.iter().any(|p| p.id == "fixture-plugin"));
    }
}
