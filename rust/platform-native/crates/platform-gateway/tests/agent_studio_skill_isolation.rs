//! Source tenant/user/version registry security scenarios, independent of DB acceptance.
use ai_platform_gateway::migration_content::skill_registry::SkillRegistry;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
fn manifest(version: &str, instruction: &str) -> Value {
    let skill = version.replacen('2', "1", 1);
    json!({"name":"same-name","title":"Same Name","description":"Tenant-scoped helper","entrypoint":format!("db://{skill}/{version}"),"instructions":instruction,"source":"user","artifact_type":"tenant_instruction","skill_id":skill,"version_id":version,"content_hash":"a".repeat(64),"generated":false,"enabled":true})
}
#[test]
fn same_name_cache_isolated_by_tenant_user_version() {
    let mut registry = SkillRegistry::new(None);
    let a = "21111111-1111-4111-8111-111111111111";
    let b = "22222222-2222-4222-8222-222222222222";
    registry
        .register_scoped(manifest(a, "TENANT A ONLY"), "tenant-a", "user-a")
        .unwrap();
    registry
        .register_scoped(manifest(b, "TENANT B ONLY"), "tenant-b", "user-b")
        .unwrap();
    let names = BTreeSet::from(["same-name".into()]);
    let av = BTreeMap::from([("same-name".into(), a.into())]);
    let bv = BTreeMap::from([("same-name".into(), b.into())]);
    let aa = registry.list(false, Some(&names), Some(("tenant-a", "user-a")), Some(&av));
    let bb = registry.list(false, Some(&names), Some(("tenant-b", "user-b")), Some(&bv));
    assert_eq!(
        aa.iter()
            .map(|v| v["instructions"].clone())
            .collect::<Vec<_>>(),
        vec![json!("TENANT A ONLY")]
    );
    assert_eq!(
        bb.iter()
            .map(|v| v["instructions"].clone())
            .collect::<Vec<_>>(),
        vec![json!("TENANT B ONLY")]
    );
    assert!(registry.list(false, None, None, None).is_empty());
}
#[test]
fn wrong_user_or_version_cannot_read_scoped_skill() {
    let mut registry = SkillRegistry::new(None);
    let version = "23333333-3333-4333-8333-333333333333";
    registry
        .register_scoped(manifest(version, "PRIVATE"), "tenant-a", "user-a")
        .unwrap();
    assert!(
        registry
            .get_scoped("same-name", "tenant-a", "user-b", Some(version))
            .is_none()
    );
    assert!(
        registry
            .list(
                false,
                None,
                Some(("tenant-a", "user-a")),
                Some(&BTreeMap::from([(
                    "same-name".into(),
                    "24444444-4444-4444-8444-444444444444".into()
                )]))
            )
            .is_empty()
    );
}
#[test]
fn selection_cannot_expand_beyond_exact_scoped_version() {
    let mut registry = SkillRegistry::new(None);
    let allowed = "25555555-5555-4555-8555-555555555555";
    let other = "26666666-6666-4666-8666-666666666666";
    registry
        .register_scoped(manifest(allowed, "ALLOWED"), "tenant-a", "user-a")
        .unwrap();
    registry
        .register_scoped(manifest(other, "FORBIDDEN"), "tenant-a", "user-a")
        .unwrap();
    let selection = registry.select_for_query(
        "tenant scoped helper",
        None,
        Some(&BTreeSet::from(["same-name".into()])),
        Some(("tenant-a", "user-a")),
        Some(&BTreeMap::from([("same-name".into(), allowed.into())])),
    );
    assert_eq!(
        selection
            .iter()
            .map(|s| s.skill["instructions"].clone())
            .collect::<Vec<_>>(),
        vec![json!("ALLOWED")]
    );
}
