use super::*;
fn section(s: &str, name: &str) -> String {
    let marker = format!("(?m)^  {}:\n", regex::escape(name));
    let start = regex::Regex::new(&marker).unwrap().find(s).unwrap().start();
    let tail = &s[start + name.len() + 4..];
    let end = regex::Regex::new(r"(?m)^  [A-Za-z0-9_-]+:\n")
        .unwrap()
        .find(tail)
        .map(|m| m.start())
        .unwrap_or(tail.len());
    tail[..end].into()
}
fn target(s: &str, name: &str) -> String {
    let marker = format!("{name}:");
    let start = s
        .lines()
        .scan(0, |offset, line| {
            let here = *offset;
            *offset += line.len() + 1;
            Some((here, line))
        })
        .find(|(_, l)| l.starts_with(&marker))
        .unwrap()
        .0;
    let tail = &s[start..];
    let end = regex::Regex::new(r"(?m)^[A-Za-z0-9_.%-]+:")
        .unwrap()
        .find_iter(tail)
        .nth(1)
        .map(|m| m.start())
        .unwrap_or(tail.len());
    tail[..end].into()
}
#[test]
fn private_ports() {
    let c = read("docker-compose.yml");
    lacks(&c, &["8765"]);
    for (service, ports) in [
        ("postgres", vec!["127.0.0.1:${POSTGRES_PORT:-5432}:5432"]),
        ("redis", vec!["127.0.0.1:${REDIS_PORT:-6379}:6379"]),
        (
            "qdrant",
            vec![
                "127.0.0.1:${QDRANT_HTTP_PORT:-6333}:6333",
                "127.0.0.1:${QDRANT_GRPC_PORT:-6334}:6334",
            ],
        ),
        ("tempo", vec!["127.0.0.1:3200:3200", "127.0.0.1:4317:4317"]),
        (
            "knowledge-service",
            vec!["127.0.0.1:${KNOWLEDGE_SERVICE_PORT:-8092}:8092"],
        ),
    ] {
        has(&section(&c, service), &ports);
    }
    for (service, port) in [
        ("agent-runtime", "8094"),
        ("agent-capability-worker", "8095"),
    ] {
        let b = section(&c, service);
        has(&b, &[&format!("\n    expose:\n      - \"{port}\"")]);
        lacks(&b, &["ports:", "8765"]);
    }
}
#[test]
fn frontend_and_models() {
    let c = read("docker-compose.yml");
    has(
        &section(&c, "gateway"),
        &[
            "DEFAULT_MODEL: \"${DEFAULT_MODEL:-qwen3.8-flash}\"",
            "MODEL_SETUP_MODE: \"${MODEL_SETUP_MODE:-ui}\"",
        ],
    );
    lacks(&c, &["GATEWAY_MODEL_SETUP_MODE"]);
    has(&read(".env.example"), &["MODEL_SETUP_MODE=ui"]);
    has(
        &section(&c, "frontend"),
        &[
            "VITE_AGENT_STUDIO_ENABLED: \"${VITE_AGENT_STUDIO_ENABLED:-true}\"",
            "VITE_SUPPORT_EMAIL: \"${VITE_SUPPORT_EMAIL:-admin@${AUTH_ALLOWED_EMAIL_DOMAIN:-example.com}}\"",
        ],
    );
    for p in [
        ".env.example",
        "docs/deployment/operator-guide.md",
        "docker-compose.yml",
        "scripts/new/validate-env.sh",
    ] {
        has(&read(p), &["GOOGLE_CHAT_BACKEND", "VERTEX_CHAT_API_KEY"]);
    }
    let d = read("web/Dockerfile");
    has(
        &d,
        &[
            "ARG VITE_SUPPORT_EMAIL=\n",
            "ARG NODE_BASE_IMAGE=node:26.11.1-alpine3.24",
            "FROM ${NODE_BASE_IMAGE} AS builder",
            "npm install --global pnpm@12.11.2 --allow-scripts=pnpm",
            "COPY package.json pnpm-lock.yaml pnpm-workspace.yaml ./",
        ],
    );
    lacks(
        &d,
        &[
            "ARG VITE_SUPPORT_EMAIL=admin@example.com",
            "corepack enable",
            "corepack prepare",
        ],
    );
    let p: serde_json::Value = serde_json::from_str(&read("web/package.json")).unwrap();
    assert_eq!(p["engines"]["node"], "^26.11.1");
    assert_eq!(p["packageManager"], "pnpm@12.11.2");
}
#[test]
fn topology_first_class_workers() {
    let c = read("scripts/new/common.sh");
    has(
        &c,
        &[
            "knowledge_worker_container()",
            "compose_running_service_containers knowledge-worker",
            "check_knowledge_worker_health()",
            "agent_capability_worker_image_tag()",
            "--service-ids \"$scope\"",
        ],
    );
    let d = read("scripts/new/deploy.sh");
    has(
        &d,
        &[
            "FULL_APP_SERVICES=\"$(topology_service_ids app)\"",
            "INFRA_SERVICES=\"$(topology_service_ids infrastructure)\"",
            "SERVICES=\"$FULL_APP_SERVICES\"",
            "wait_for_healthy \"Knowledge worker\" \"check_knowledge_worker_health\"",
            "if topology_service_present knowledge-worker; then",
            "wait_for_healthy \"Capability worker\"",
        ],
    );
    let out = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
        .current_dir(root())
        .args(["topology", "--mode", "full", "--service-ids", "app"])
        .output()
        .unwrap();
    has(&outcome(out, 0), &["knowledge-worker"]);
    has(
        &read("scripts/new/validate-env.sh"),
        &[
            "wait_for_healthy \"Knowledge worker\" \"check_knowledge_worker_health\" 60 || fail \"Knowledge worker runtime check failed.\"",
        ],
    );
    has(
        &read("scripts/new/status.sh"),
        &["check_and_report \"Knowledge worker\" check_knowledge_worker_health"],
    );
    has(
        &read("scripts/new/init-env.sh"),
        &[
            "existing_worker_image=\"$(awk -F=",
            "AGENT_CAPABILITY_WORKER_IMAGE=%s\\n",
        ],
    );
    let m = read("Makefile");
    for t in ["dev-compose", "dev-compose-logs"] {
        let b = target(&m, t);
        assert!(
            regex::Regex::new(r"(?s)knowledge-service knowledge-worker .*?frontend")
                .unwrap()
                .is_match(&b)
        );
    }
}
#[test]
fn native_hot_update_release_unit() {
    // Compiled images replace Python module copies; release locking, health and image identity assertions remain.
    let h = read("scripts/new/hot-update.sh");
    has(
        &h,
        &[
            "services+=(knowledge-service)",
            "services+=(knowledge-worker)",
            "build \"${services[@]}\"",
            "replace_services+=(agent-runtime agent-capability-worker)",
            "--force-recreate \"${replace_services[@]}\"",
            "assert_runtime_release_unit_locked \"$AI_PLATFORM_AGENT_RUNTIME_IMAGE\" \"$AGENT_CAPABILITY_WORKER_IMAGE\"",
            "wait_for_healthy \"Agent Runtime\" check_agent_runtime_health",
            "wait_for_healthy \"Knowledge worker\" check_knowledge_worker_health",
            "{{.Image}}",
            "$worker_digest",
            "$runtime_digest",
        ],
    );
}
#[test]
fn deploy_order_and_scope() {
    let d = read("scripts/new/deploy.sh");
    assert!(
        d.find("# -- Determine services to deploy").unwrap()
            < d.find("# -- Pull base images").unwrap()
    );
    has(
        &d,
        &[
            "pull $SERVICES",
            "SHOULD_MIGRATE=false",
            "if [ \"$SKIP_MIGRATE\" != true ] && [ \"$INFRA_ONLY\" != true ]; then\n    SHOULD_MIGRATE=true\nfi",
            "if [ \"$APP_ONLY\" != true ] || [ \"$SHOULD_MIGRATE\" = true ]; then\n    wait_for_healthy \"PostgreSQL\" \"check_postgres_health\" 30\nfi",
            "if [ \"$SHOULD_MIGRATE\" = true ]; then",
        ],
    );
    let steps = [
        "validate-env.sh\" --env \"$ENV_FILE\" --config-only",
        "compose up -d --remove-orphans $START_SERVICES",
        "migrate.sh\" --auto",
        "compose up -d --remove-orphans $FULL_APP_SERVICES",
        "validate-env.sh\" --env \"$ENV_FILE\" --runtime",
    ];
    let positions: Vec<_> = steps.iter().map(|s| d.find(s).unwrap()).collect();
    assert!(positions.windows(2).all(|v| v[0] < v[1]));
    let q = target(&read("Makefile"), "quickstart");
    assert!(q.find("init-env.sh").unwrap() < q.find("deploy.sh").unwrap());
    has(&q, &["--pull"]);
    lacks(&q, &["--build"]);
}
#[test]
fn metrics_readiness_contract() {
    let c = read("scripts/new/common.sh");
    has(
        &c,
        &[
            "check_gateway_metrics()",
            "http://localhost:${GATEWAY_PORT:-8080}/metrics",
            "grep -Eq '^# HELP |^# TYPE '",
            "grep -Eq '^gateway_up($|[ {])'",
            "http://127.0.0.1:8092/health/ready",
            "http://127.0.0.1:8094/health/ready",
            "assert_agent_runtime_image_locked()",
            "--require-artifact agent_runtime",
            "docker image inspect \"$task_image\" --format '{{.Id}}'",
        ],
    );
    lacks(
        &c,
        &[
            "[ -z \"$body\" ] && return 0",
            "127.0.0.1:8765",
            "http://127.0.0.1:8092/health\" &>/dev/null",
            "http://127.0.0.1:8094/health\" &>/dev/null",
        ],
    );
    has(
        &read("scripts/new/validate-env.sh"),
        &[
            "wait_for_healthy \"Gateway metrics endpoint\" \"check_gateway_metrics\" 60",
            "fail \"Gateway metrics check failed.\"",
        ],
    );
    has(
        &read("scripts/new/status.sh"),
        &["check_and_report \"Gateway metrics\" check_gateway_metrics"],
    );
}
#[test]
fn environment_forwarding() {
    let c = read("scripts/new/common.sh");
    has(
        &c,
        &[
            "ENV_FILE=\"${ENV_FILE:-$DEFAULT_ENV_FILE}\"",
            "env_file=\"$(env_file_path)\"",
            "Env file not found: $env_file",
        ],
    );
    let d = read("scripts/new/deploy.sh");
    has(
        &d,
        &[
            "--env FILE   Use a specific env file instead of .env",
            "--env)",
            "log_error \"--env requires a file path\"",
            "ENV_FILE=\"$2\"; shift 2 ;;",
            "validate-env.sh\" --env \"$ENV_FILE\" --config-only",
            "validate-env.sh\" --env \"$ENV_FILE\" --infra-only --config-only",
            "--env-file \"$ENV_FILE\"",
            "ENV_FILE=\"$ENV_FILE\" \"$(dirname \"$0\")/migrate.sh\" --auto",
            "validate-env.sh\" --env \"$ENV_FILE\" --runtime",
            "\"${COMPOSE_CMD[@]}\" \"${COMPOSE_FILES[@]}\" --env-file \"$ENV_FILE\"",
            "compose ps",
        ],
    );
    lacks(&d, &["\n$COMPOSE_CMD ps\n"]);
    let m = read("scripts/new/migrate.sh");
    has(
        &m,
        &[
            "--env FILE   Use a specific env file instead of .env",
            "--env)",
            "log_error \"--env requires a file path\"",
            "ENV_FILE=\"$2\"",
            "database-authority",
        ],
    );
    assert!(m.find("ENV_FILE=\"$2\"").unwrap() < m.find("load_env").unwrap());
    lacks(&m, &["ensure_base_schema", "run_sql", "schema_migrations"]);
}
#[test]
fn make_forwarding() {
    let m = read("Makefile");
    has(
        &m,
        &[
            "ENV_FILE ?= .env",
            "validate-env.sh --env \"$(ENV_FILE)\" --config-only",
            "validate-env.sh --env \"$(ENV_FILE)\" --runtime",
            "init-env.sh --env \"$(ENV_FILE)\" --if-missing",
            "deploy.sh --env \"$(ENV_FILE)\" --pull",
            "deploy.sh --env \"$(ENV_FILE)\" --build",
            "BUILD_COMPOSE := $(COMPOSE) -f docker-compose.yml -f deploy/compose/docker-compose.build.yml",
            "@ENV_FILE=\"$(ENV_FILE)\" bash $(SCRIPTS)/status.sh",
            "@bash $(SCRIPTS)/migrate.sh --env \"$(ENV_FILE)\"",
            "@bash $(SCRIPTS)/deploy.sh --env \"$(ENV_FILE)\" $(ARGS)",
            "$(COMPOSE) --env-file \"$(ENV_FILE)\" stop",
            "$(COMPOSE) --env-file \"$(ENV_FILE)\" restart",
            "$(COMPOSE) --env-file \"$(ENV_FILE)\" logs -f",
            "@ENV_FILE=\"$(ENV_FILE)\" bash $(SCRIPTS)/backup.sh",
            "@ENV_FILE=\"$(ENV_FILE)\" bash $(SCRIPTS)/setup-dev.sh",
        ],
    );
    for t in [
        "deploy",
        "deploy-build",
        "deploy-cn",
        "deploy-infra",
        "deploy-app",
    ] {
        let b = target(&m, t).replace("\\\n", " ");
        let line = b
            .lines()
            .find(|l| l.contains("bash $(SCRIPTS)/deploy.sh"))
            .unwrap();
        has(line, &["$(ARGS)"]);
    }
}
