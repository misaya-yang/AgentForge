use super::*;
#[test]
fn bootstrap_password() {
    validate(
        &set(
            &valid("DASHSCOPE_API_KEY=test-chat-key"),
            "DEFAULT_USER_PASSWORD",
            "ChangeMe-Admin-2026!",
        ),
        &["--config-only"],
        "",
        0,
        &[
            "Configuration validation passed",
            "DEFAULT_USER_PASSWORD uses the documented local bootstrap default",
        ],
        &[],
    );
}
#[test]
fn example_modes() {
    for args in [vec!["--example"], vec!["--example", "--runtime"]] {
        validate(
            &read(".env.example"),
            &args,
            "",
            0,
            &["Example configuration validation passed"],
            &["Validating runtime dependencies"],
        );
    }
}
#[test]
fn example_not_release() {
    validate(
        &read(".env.example"),
        &["--config-only"],
        "",
        1,
        &[
            "POSTGRES_PASSWORD must be set to a non-placeholder secret",
            "REDIS_PASSWORD must be set to a non-placeholder secret",
        ],
        &["Example configuration validation passed"],
    );
}
#[test]
fn default_model_round_trip() {
    let s = valid("DASHSCOPE_API_KEY=test-chat-key");
    for t in [s.clone(), set(&s, "DEFAULT_MODEL", "my-deployment-model")] {
        validate(
            &t,
            &["--config-only"],
            "",
            0,
            &["Configuration validation passed"],
            &[],
        );
    }
    validate(
        &set(&s, "DEFAULT_MODEL", ""),
        &["--config-only"],
        "",
        1,
        &["DEFAULT_MODEL must be non-empty when set"],
        &[],
    );
}
#[test]
fn release_image_required() {
    validate(&set(&valid("DASHSCOPE_API_KEY=test-chat-key"),"GATEWAY_IMAGE","ghcr.io/misaya-yang/ai-gateway:nightly"),&["--config-only"],"",1,&["GATEWAY_IMAGE must use a full semantic release tag (for example 2.0.0) or sha256 digest."],&[]);
}
#[test]
fn example_shape() {
    let s = regex::Regex::new(r"(?m)^AI_PLATFORM_INTERNAL_TOKEN=.*\n")
        .unwrap()
        .replace_all(&read(".env.example"), "")
        .into_owned();
    validate(
        &s,
        &["--example"],
        "",
        1,
        &["AI_PLATFORM_INTERNAL_TOKEN must be declared in the example env file"],
        &[],
    );
}
#[test]
fn ui_bootstrap() {
    validate(
        &set(&valid(""), "MODEL_SETUP_MODE", "ui"),
        &["--config-only"],
        "",
        0,
        &["starting in UI model-setup mode"],
        &[],
    );
}
#[test]
fn runtime_provider_alignment() {
    validate(
        &valid("DASHSCOPE_API_KEY=test-dashscope-key"),
        &["--runtime"],
        "google|5",
        1,
        &[
            "No enabled chat models match configured providers",
            "Configured providers: dashscope",
            "Enabled model providers: google:5",
        ],
        &[],
    );
    validate(
        &valid("DASHSCOPE_API_KEY=test-dashscope-key"),
        &["--runtime"],
        "dashscope|1",
        0,
        &[
            "Chat model/provider alignment is valid (1 available model(s)).",
            "Runtime validation passed",
        ],
        &[],
    );
}
#[test]
fn vertex_provider_aliases() {
    for p in ["dashscope|1\ngoogle-vertex|1", "dashscope|1\ngoogle|1"] {
        validate(&valid("DASHSCOPE_API_KEY=test-dashscope-key\nGOOGLE_CHAT_BACKEND=vertex\nVERTEX_CHAT_API_KEY=test-vertex-chat-key"),&["--runtime"],p,0,&["Chat model/provider alignment is valid (2 available model(s))."],&[]);
    }
}
#[test]
fn vertex_requires_backend() {
    for mode in ["--runtime", "--config-only"] {
        validate(
            &valid("VERTEX_CHAT_API_KEY=test-vertex-chat-key"),
            &[mode],
            "google-vertex|1",
            1,
            &["Set at least one supported chat provider key"],
            &[],
        );
    }
}
#[test]
fn invalid_config_skips_compose() {
    validate(
        &format!(
            "{}\nDEFAULT_USER_PASSWORD=\n",
            valid("DASHSCOPE_API_KEY=test-chat-key")
        ),
        &["--config-only"],
        "",
        1,
        &[
            "DEFAULT_USER_PASSWORD must be set to a non-placeholder secret",
            "Skipping docker compose config validation",
        ],
        &[
            "docker compose config validation failed",
            "DEFAULT_USER_PASSWORD is required",
        ],
    );
}
fn infra() -> String {
    set(
        &set(&read(".env.example"), "POSTGRES_PASSWORD", SECRET),
        "REDIS_PASSWORD",
        SECRET,
    )
}
#[test]
fn infra_secrets_only() {
    validate(
        &infra(),
        &["--infra-only", "--config-only"],
        "",
        0,
        &["Infrastructure configuration validation passed"],
        &["JWT_SECRET", "AI_PLATFORM_INTERNAL_TOKEN"],
    );
    has(
        &read("scripts/new/validate-env.sh"),
        &["validate_compose_config --no-interpolate postgres redis qdrant"],
    );
}
#[test]
fn infra_missing_app_vars() {
    let mut s = infra();
    for k in [
        "DEFAULT_USER_PASSWORD",
        "JWT_SECRET",
        "AI_PLATFORM_INTERNAL_TOKEN",
        "AUTH_ALLOWED_EMAIL_DOMAIN",
    ] {
        s = regex::Regex::new(&format!(r"(?m)^{k}=.*\n?"))
            .unwrap()
            .replace_all(&s, "")
            .into_owned();
    }
    validate(
        &s,
        &["--infra-only", "--config-only"],
        "",
        0,
        &["Infrastructure configuration validation passed"],
        &["DEFAULT_USER_PASSWORD is required"],
    );
}
fn remote() -> String {
    let mut s = valid("DASHSCOPE_API_KEY=test-chat-key");
    for (k, v) in [
        ("AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test"),
        ("VITE_AUTH_EMAIL_DOMAIN", "myapp.test"),
        (
            "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON",
            r#"["https://ai.myapp.test"]"#,
        ),
        (
            "ASSISTANT_CORS_ALLOW_ORIGINS_JSON",
            r#"["https://ai.myapp.test"]"#,
        ),
    ] {
        s = set(&s, k, v);
    }
    s
}
#[test]
fn cors_rejections() {
    let s = valid("DASHSCOPE_API_KEY=test-chat-key");
    validate(
        &set(&s, "AUTH_ALLOWED_EMAIL_DOMAIN", "myapp.test"),
        &["--config-only"],
        "",
        1,
        &["KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON must not include localhost origins"],
        &[],
    );
    for (origin, message) in [
        (r#"["*"]"#, "must not use wildcard origins"),
        (
            r#"["chrome-extension://ai-gateway"]"#,
            "must be a JSON array of explicit http(s) origins",
        ),
        (r#"["http://ai.myapp.test"]"#, "must use https origins"),
    ] {
        validate(
            &set(&remote(), "KNOWLEDGE_CORS_ALLOW_ORIGINS_JSON", origin),
            &["--config-only"],
            "",
            1,
            &[message],
            &[],
        );
    }
}
#[test]
fn remote_cors_valid() {
    validate(
        &remote(),
        &["--config-only"],
        "",
        0,
        &["Configuration validation passed"],
        &[],
    );
}
#[test]
fn remote_frontend_identity() {
    validate(
        &set(&remote(), "VITE_AUTH_EMAIL_DOMAIN", "example.com"),
        &["--config-only"],
        "",
        1,
        &["VITE_AUTH_EMAIL_DOMAIN must match AUTH_ALLOWED_EMAIL_DOMAIN"],
        &[],
    );
    validate(
        &set(&remote(), "VITE_SUPPORT_EMAIL", "admin@example.com"),
        &["--config-only"],
        "",
        1,
        &["VITE_SUPPORT_EMAIL must not use example.com"],
        &[],
    );
}
#[test]
fn remote_frontend_urls() {
    let keys = [
        "VITE_API_URL",
        "VITE_API_BASE_URL",
        "VITE_TELEMETRY_ENDPOINT",
    ];
    let mut s = remote();
    for k in keys {
        s = set(&s, k, "http://api.myapp.test");
    }
    let messages:Vec<String>=keys.iter().map(|k|format!("{k} must use https:// or a same-origin path when AUTH_ALLOWED_EMAIL_DOMAIN is non-local.")).collect();
    validate(
        &s,
        &["--config-only"],
        "",
        1,
        &messages.iter().map(String::as_str).collect::<Vec<_>>(),
        &[],
    );
    let mut s = remote();
    for (k, v) in keys.into_iter().zip([
        "http://localhost:8080",
        "https://[::1]:8080",
        "https://127.0.0.1:8080/telemetry",
    ]) {
        s = set(&s, k, v);
    }
    let mut messages:Vec<String>=keys.iter().map(|k|format!("{k} must not use localhost or loopback when AUTH_ALLOWED_EMAIL_DOMAIN is non-local.")).collect();
    messages.push("VITE_API_URL must use https:// or a same-origin path when AUTH_ALLOWED_EMAIL_DOMAIN is non-local.".into());
    validate(
        &s,
        &["--config-only"],
        "",
        1,
        &messages.iter().map(String::as_str).collect::<Vec<_>>(),
        &[],
    );
}
#[test]
fn same_origin_paths() {
    let mut s = remote();
    for (k, v) in [
        ("VITE_API_URL", "/api"),
        ("VITE_API_BASE_URL", "/api/v1"),
        ("VITE_TELEMETRY_ENDPOINT", "/telemetry"),
    ] {
        s = set(&s, k, v);
    }
    validate(
        &s,
        &["--config-only"],
        "",
        0,
        &["Configuration validation passed"],
        &[],
    );
}
