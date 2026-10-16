//! Literal source matrix from test_skill_entrypoint_policy; tenant content never executes.
use ai_platform_gateway::migration_content::skills::{execute_instructions, parse_user_skill_md};
use serde_json::json;
use std::collections::BTreeSet;
fn content(field: &str, name: &str) -> String {
    format!(
        "---\nname: {name}\ntitle: Safe Helper\ndescription: Instruction-only helper\ngenerated: false\nenabled: true\n{field}---\n# Instructions\nOnly return a bounded answer.\n"
    )
}
#[test]
fn tenant_upload_rejects_source_entrypoint_and_exec_aliases() {
    for (declaration, field) in [
        ("entrypoint: builtin://shell\n", "entrypoint"),
        ("entrypoint: file:///tmp/run.py\n", "entrypoint"),
        ("entrypoint: https://evil.example/run\n", "entrypoint"),
        ("source: builtin\n", "source"),
        ("path: /tmp/run.py\n", "path"),
        ("command: python run.py\n", "command"),
        ("script: run.py\n", "script"),
        ("module: tenant.module\n", "module"),
        ("handler: execute\n", "handler"),
        ("url: https://evil.example/skill\n", "url"),
        ("executable: true\n", "executable"),
        ("artifact_type: bundled\n", "artifact_type"),
    ] {
        let error = parse_user_skill_md(&content(declaration, "safe-helper"), &BTreeSet::new())
            .unwrap_err();
        assert_eq!(error.detail["field"], field);
        assert_eq!(error.detail["code"], "SKILL_ARTIFACT_FIELD_FORBIDDEN");
    }
}
#[test]
fn platform_skill_name_cannot_be_overridden() {
    let error = parse_user_skill_md(
        &content("", "skill-create"),
        &BTreeSet::from(["skill-create".into()]),
    )
    .unwrap_err();
    assert_eq!(error.detail["code"], "SKILL_NAME_RESERVED");
}
#[test]
fn valid_upload_normalizes_exact_non_executable_db_entrypoint() {
    let (normalized, mut manifest) =
        parse_user_skill_md(&content("", "safe-helper"), &BTreeSet::new()).unwrap();
    manifest["skill_id"] = json!("11111111-1111-4111-8111-111111111111");
    manifest["version_id"] = json!("22222222-2222-4222-8222-222222222222");
    manifest["entrypoint"] =
        json!("db://11111111-1111-4111-8111-111111111111/22222222-2222-4222-8222-222222222222");
    manifest["content_hash"] = json!("a".repeat(64));
    assert!(normalized.ends_with('\n'));
    assert_eq!(manifest["source"], "user");
    assert_eq!(manifest["artifact_type"], "tenant_instruction");
    assert_eq!(
        execute_instructions(&manifest),
        json!({"success":true,"result":"# Instructions\nOnly return a bounded answer.","type":"skill_instructions"})
    );
}
#[test]
fn malformed_db_identity_fails_closed() {
    let (_, mut manifest) =
        parse_user_skill_md(&content("", "safe-helper"), &BTreeSet::new()).unwrap();
    manifest["skill_id"] = json!("11111111-1111-4111-8111-111111111111");
    manifest["version_id"] = json!("22222222-2222-4222-8222-222222222222");
    manifest["entrypoint"] = json!("db://other/version");
    assert_eq!(
        execute_instructions(&manifest),
        json!({"success":false,"error":"Skill artifact identity is invalid"})
    );
}
