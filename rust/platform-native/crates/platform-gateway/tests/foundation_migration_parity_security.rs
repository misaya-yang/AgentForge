//! Source tests/security/test_skill_entrypoint_policy.py and config security defaults.
use ai_platform_gateway::{
    migration_content::{skill_executor::SkillExecutor, skills::parse_user_skill_md},
    migration_host::{
        resources::{DatabaseSettings, RedisSettings},
        settings::{ApiKeySettings, AuthenticationSettings, JwtSettings},
    },
};
use serde_json::json;
use std::collections::BTreeSet;
fn content(field: &str, name: &str) -> String {
    format!(
        "---\nname: {name}\ntitle: Safe Helper\ndescription: Instruction-only helper\ngenerated: false\nenabled: true\n{field}---\n# Instructions\nOnly return a bounded answer.\n"
    )
}
#[test]
fn tenant_skill_rejects_executable_aliases_and_reserved_platform_names() {
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
    let error = parse_user_skill_md(
        &content("", "skill-create"),
        &BTreeSet::from(["skill-create".into()]),
    )
    .unwrap_err();
    assert_eq!(error.detail["code"], "SKILL_NAME_RESERVED");
}
#[tokio::test]
async fn tenant_instruction_execution_requires_exact_database_artifact_identity() {
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
        SkillExecutor
            .execute(&manifest, json!({"input":"test"}))
            .await,
        json!({"success":true,"result":"# Instructions\nOnly return a bounded answer.","type":"skill_instructions"})
    );
    manifest["entrypoint"] = json!("db://other/version");
    let result = SkillExecutor
        .execute(&manifest, json!({"input":"test"}))
        .await;
    assert_eq!(result["success"], false);
    assert_eq!(result["error"], "Skill artifact identity is invalid");
}
#[test]
fn settings_defaults_do_not_enable_auth_storage_or_seed_credentials() {
    let database = DatabaseSettings::default();
    assert!(!database.dsn.contains("postgres:postgres"));
    assert!(!database.auto_init);
    assert!(!database.enabled);
    let jwt = JwtSettings::default();
    assert!(jwt.secret.is_empty());
    assert!(!jwt.enabled);
    let redis = RedisSettings::default();
    assert!(redis.url.contains("localhost") || redis.url.contains("127.0.0.1"));
    assert!(!redis.enabled);
    let key = ApiKeySettings::default();
    assert!(key.keys.is_empty());
    assert!(!key.enabled);
    let auth = AuthenticationSettings::default();
    assert!(!auth.jwt.enabled);
    assert!(!auth.api_key.enabled);
}
