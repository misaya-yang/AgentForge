#[path = "../src/platform_catalog.rs"]
mod platform_catalog;

use platform_catalog::{
    PlatformCatalogError, load_platform_catalog, to_capability_descriptor,
    validate_platform_catalog,
};

const EXPECTED_NAMES: &[&str] = &[
    "confluence_read",
    "read_tool_artifact",
    "search_knowledge_base",
    "spawn_subagent",
    "todo_read",
    "tool_call",
    "tool_describe",
    "tool_search",
    "web_fetch",
    "search_web",
    "confluence_write",
    "context_compact",
    "generate_image",
    "generate_quiz",
    "mcp_docgen__generate_document",
    "todo_write",
    "update_user_memory",
    "execute_python_code",
    "read_attachment",
    "local_node_catalog",
    "local_node_describe",
    "local_node_action",
];

#[test]
fn catalog_is_the_frozen_versioned_capability_baseline() {
    let catalog = load_platform_catalog().expect("checked-in catalog must load");
    let names: Vec<_> = catalog
        .capabilities
        .iter()
        .map(|capability| capability.name.as_str())
        .collect();
    assert_eq!(names, EXPECTED_NAMES);
}

#[test]
fn schema_hashes_are_recomputed_by_the_loader() {
    let catalog = load_platform_catalog().expect("checked-in catalog must load");
    for capability in &catalog.capabilities {
        assert!(capability.schema_hash.starts_with("sha256:"));
    }
}

#[test]
fn duplicate_names_fail_closed() {
    let mut catalog = load_platform_catalog().unwrap();
    catalog.capabilities[1].name = catalog.capabilities[0].name.clone();
    let error = validate_platform_catalog(&catalog).unwrap_err();
    assert_eq!(
        error,
        PlatformCatalogError::Duplicate("name", "confluence_read".to_string())
    );
}

#[test]
fn tampered_schema_hash_fails_closed() {
    let mut catalog = load_platform_catalog().unwrap();
    catalog.capabilities[0].schema_hash = "sha256:tampered".to_string();
    let error = validate_platform_catalog(&catalog).unwrap_err();
    assert!(matches!(error, PlatformCatalogError::SchemaHash(..)));
}

#[test]
fn read_and_deferred_write_ownership_is_explicit() {
    let catalog = load_platform_catalog().unwrap();
    let runtime = [
        "spawn_subagent",
        "tool_call",
        "tool_describe",
        "tool_search",
        "context_compact",
    ];
    for capability in &catalog.capabilities {
        if runtime.contains(&capability.name.as_str()) {
            assert_eq!(capability.implementation_owner, "runtime");
        } else {
            assert_eq!(capability.implementation_owner, "worker");
        }
        assert_eq!(capability.id, capability.name);
        if capability.name.starts_with("local_node_") {
            assert_eq!(capability.version.as_deref(), Some("2"));
        } else {
            assert_eq!(capability.version, None);
        }
    }
}

#[test]
fn office_generation_is_a_worker_tool_not_a_nested_mcp_runtime() {
    let catalog = load_platform_catalog().unwrap();
    let office = catalog
        .capabilities
        .iter()
        .find(|capability| capability.name == "mcp_docgen__generate_document")
        .expect("Office generator");
    assert_eq!(office.kind, "tool");
    assert_eq!(office.protocol, "internal");
    assert_eq!(office.category, "generation");
    assert_eq!(office.implementation_owner, "worker");
}

#[test]
fn all_records_convert_without_rewriting_schema_hashes() {
    let catalog = load_platform_catalog().unwrap();
    let descriptors = catalog
        .capabilities
        .iter()
        .map(to_capability_descriptor)
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(descriptors.len(), EXPECTED_NAMES.len());
    for (record, descriptor) in catalog.capabilities.iter().zip(descriptors) {
        assert_eq!(descriptor.id, record.id);
        assert_eq!(descriptor.name, record.name);
        assert_eq!(
            descriptor.version,
            record.version.as_deref().unwrap_or("null")
        );
        assert_eq!(descriptor.schema_hash, record.schema_hash);
        assert_eq!(descriptor.input_schema, record.input_schema);
        assert_eq!(descriptor.protocol, record.protocol);
    }
}

#[test]
fn descriptor_projection_rejects_tampered_record_without_rehashing() {
    let catalog = load_platform_catalog().unwrap();
    let mut record = catalog.capabilities[0].clone();
    record.schema_hash = "sha256:tampered".to_string();
    assert!(matches!(
        to_capability_descriptor(&record),
        Err(PlatformCatalogError::SchemaHash(..))
    ));

    let mut policy_tamper = catalog.capabilities[0].clone();
    policy_tamper.read_only = false;
    assert!(matches!(
        to_capability_descriptor(&policy_tamper),
        Err(PlatformCatalogError::Field(_, "read effect binding"))
    ));
}
