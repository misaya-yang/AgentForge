use super::*;
#[test]
fn incompatible_deploy_scope() {
    let f = Fixture::new();
    let s = outcome(f.run("scripts/new/deploy.sh", &["--infra", "--app"]), 2);
    has(&s, &["--infra and --app cannot be used together"]);
    lacks(&s, &["Pre-flight checks"]);
}
#[test]
fn missing_or_unknown_options_fail_before_actions() {
    let f = Fixture::new();
    for (script, next) in [
        ("deploy.sh", "--app"),
        ("validate-env.sh", "--runtime"),
        ("migrate.sh", "--status"),
        ("setup-dev.sh", "--status"),
        ("backup.sh", "--list"),
    ] {
        for args in [vec!["--env"], vec!["--env", next]] {
            let s = outcome(f.run(&format!("scripts/new/{script}"), &args), 2);
            has(&s, &["--env requires a file path"]);
            lacks(
                &s,
                &[
                    "Pre-flight checks",
                    "Validating configuration",
                    "Running database migrations",
                    "Migration status",
                    "Creating database backup",
                    "Restoring from",
                    "Starting development",
                    "password is required",
                ],
            );
        }
    }
    for script in ["migrate.sh", "backup.sh"] {
        let s = outcome(f.run(&format!("scripts/new/{script}"), &["--typo"]), 2);
        has(&s, &["Unknown option: --typo"]);
    }
    for args in [vec!["--backup-dir"], vec!["--backup-dir", "--list"]] {
        let s = outcome(f.run("scripts/new/backup.sh", &args), 2);
        has(&s, &["--backup-dir requires a directory path"]);
        lacks(&s, &["Creating database backup", "Restoring from"]);
    }
}
#[test]
fn explicit_missing_env_fails_before_action() {
    let f = Fixture::new();
    let absent = f.dir.join("missing.env");
    for (script, args) in [
        ("deploy.sh", vec!["--app", "--no-migrate"]),
        ("migrate.sh", vec!["--status"]),
        ("setup-dev.sh", vec!["--status"]),
        ("backup.sh", vec![]),
    ] {
        let mut all = vec!["--env", absent.to_str().unwrap()];
        all.extend(args);
        let s = outcome(f.run(&format!("scripts/new/{script}"), &all), 1);
        has(&s, &["Env file not found"]);
        lacks(
            &s,
            &[
                "Pre-flight checks",
                "Starting services",
                "Migration status",
                "Applied migrations",
                "Pending migrations",
                "Container Status",
                "Creating database backup",
            ],
        );
    }
    assert!(!f.dir.join("backups").exists());
}
#[test]
fn metrics_semantics() {
    let f = Fixture::new();
    f.executable("curl",r#"#!/bin/sh
case "$METRICS_CASE" in
empty) exit 0;;
generic) echo '# HELP other_metric Other'; echo 'other_metric 1'; exit 0;;
gateway) echo '# HELP gateway_up Gateway metrics endpoint availability'; echo 'gateway_up 1'; exit 0;;
protected) case "$*" in *"%{http_code}"*) printf 403; exit 0;; *) exit 22;; esac;;
esac
exit 1
"#);
    for (case, code) in [
        ("empty", 1),
        ("generic", 1),
        ("gateway", 0),
        ("protected", 0),
    ] {
        outcome(
            f.command(
                "-c",
                &["source scripts/new/common.sh; check_gateway_metrics"],
            )
            .env("METRICS_CASE", case)
            .output()
            .unwrap(),
            code,
        );
    }
}
#[test]
fn migrate_authority_exit_and_no_schema_side_effects() {
    for (flag, command, code) in [
        ("--auto", "migrate", 7),
        ("--status", "status", 0),
        ("--init", "init-fresh", 0),
    ] {
        let f = Fixture::new();
        let scripts = f.dir.join("scripts/new");
        fs::create_dir_all(&scripts).unwrap();
        fs::write(scripts.join("migrate.sh"), read("scripts/new/migrate.sh")).unwrap();
        fs::write(
            scripts.join("common.sh"),
            format!(
                r#"#!/bin/bash
set -euo pipefail
PROJECT_ROOT='{}'
log_info() {{ echo "$1"; }}
log_success() {{ echo "$1"; }}
log_warn() {{ echo "$1"; }}
log_error() {{ echo "$1"; }}
log_step() {{ echo "$1"; }}
load_env() {{ :; }}
run_sql() {{ echo forbidden-sql >> "$SQL_LOG"; return 99; }}
run_sql_file() {{ echo forbidden-sql >> "$SQL_LOG"; return 99; }}
"#,
                f.dir.display()
            ),
        )
        .unwrap();
        f.executable(
            "database-authority",
            &format!("#!/bin/sh\ncase \"$1\" in source-kind) echo adopted; exit 0;; provision-roles|provision-extensions) exit 0;; esac\nprintf '%s\\n' \"$*\" > \"$AUTHORITY_LOG\"\nexit {code}\n"),
        );
        let log = f.dir.join("authority.log");
        let sql = f.dir.join("sql.log");
        let s = outcome(
            f.command(scripts.join("migrate.sh").to_str().unwrap(), &[flag])
                .env("AUTHORITY_LOG", &log)
                .env("SQL_LOG", &sql)
                .env(
                    "AI_GATEWAY_DATABASE_MIGRATOR_DSN",
                    "postgresql://fixture@127.0.0.1:9/fixture",
                )
                .env(
                    "AI_GATEWAY_DATABASE_ADMIN_DSN",
                    "postgresql://fixture_admin@127.0.0.1:9/fixture",
                )
                .output()
                .unwrap(),
            code,
        );
        assert_eq!(fs::read_to_string(log).unwrap().trim(), command);
        assert!(!sql.exists());
        lacks(&s, &["CREATE TABLE"]);
    }
}
#[test]
fn backup_external_paths_and_listing() {
    let f = Fixture::new();
    let env = f.dir.join("test.env");
    fs::write(&env, "").unwrap();
    outcome(
        f.command(
            "scripts/new/backup.sh",
            &["--list", "--env", env.to_str().unwrap()],
        )
        .env("XDG_STATE_HOME", f.dir.join("state"))
        .env("AI_PLATFORM_BACKUP_DIR", "")
        .output()
        .unwrap(),
        0,
    );
    let alias = f.dir.join("checkout-alias");
    std::os::unix::fs::symlink(root(), &alias).unwrap();
    for p in [root().join("tmp/../backups"), alias.join("backups")] {
        let s = outcome(
            f.run(
                "scripts/new/backup.sh",
                &[
                    "--backup-dir",
                    p.to_str().unwrap(),
                    "--env",
                    env.to_str().unwrap(),
                ],
            ),
            2,
        );
        has(&s, &["must be outside the source checkout"]);
    }
    let external = f.dir.join("private-backups");
    fs::create_dir(&external).unwrap();
    let name = "gateway_20260829_000000.sql.gz";
    fs::write(external.join(name), b"backup").unwrap();
    fs::write(
        &env,
        format!("AI_PLATFORM_BACKUP_DIR={}\n", external.display()),
    )
    .unwrap();
    let s = outcome(
        f.run(
            "scripts/new/backup.sh",
            &["--list", "--env", env.to_str().unwrap()],
        ),
        0,
    );
    has(&s, &[name]);
}
#[test]
fn backup_sql_fail_fast() {
    let common = read("scripts/new/common.sh");
    assert!(common.matches("psql -v ON_ERROR_STOP=1").count() >= 2);
    has(
        &common,
        &[
            "docker exec -i \"$(pg_container)\" psql -v ON_ERROR_STOP=1",
            "PGPASSWORD=\"$(pg_password)\" psql -v ON_ERROR_STOP=1",
        ],
    );
    let s = read("scripts/new/backup.sh");
    has(
        &s,
        &[
            "--env FILE          Use a specific env file instead of .env",
            "--backup-dir DIR    Store/list backups in an external directory",
            "--env)",
            "log_error \"--env requires a file path\"",
            "ENV_FILE=\"$2\"; shift 2 ;;",
            "psql -v ON_ERROR_STOP=1",
        ],
    );
    assert!(s.find("require_env_file").unwrap() < s.find("# -- Restore").unwrap());
    assert!(s.find("require_env_file").unwrap() < s.find("mkdir -p").unwrap());
}
#[test]
fn status_failure() {
    let f = Fixture::new();
    fs::write(
        f.dir.join(".env"),
        "POSTGRES_PASSWORD=test\nREDIS_PASSWORD=test\n",
    )
    .unwrap();
    f.executable("docker","#!/bin/sh\nif [ \"$1\" = compose ]; then if [ \"$2\" = version ]; then exit 0; fi; if [ \"$2\" = --env-file ]; then echo 'compose ps'; exit 0; fi; fi\nexit 1\n");
    f.executable("curl", "#!/bin/sh\nexit 1\n");
    let s = outcome(f.run("scripts/new/status.sh", &[]), 1);
    has(&s, &["Not available", "health check(s) failed"]);
}
#[test]
fn status_selected_env() {
    let f = Fixture::new();
    let env = f.dir.join(".env.status");
    fs::write(&env, "").unwrap();
    let log = f.dir.join("docker.log");
    f.executable("docker",r#"#!/bin/sh
printf '%s\n' "$*" >> "$DOCKER_CALL_LOG"
if [ "$1" = compose ]; then
 if [ "$2" = version ]; then exit 0; fi
 case " $* " in *" --env-file $EXPECTED_ENV_FILE ps ") echo 'compose ps used selected env file'; exit 0;; esac
fi
if [ "$1" = ps ]; then case "$*" in
 *com.docker.compose.service=gateway*) echo gateway-1;;
 *com.docker.compose.service=agent-runtime*) echo agent-runtime-1;;
 *com.docker.compose.service=knowledge-worker*) echo knowledge-worker-1;;
 *com.docker.compose.service=agent-capability-worker*) echo capability-worker-1;; esac; exit 0; fi
if [ "$1" = exec ]; then exit 0; fi
if [ "$1" = inspect ]; then echo healthy; exit 0; fi
exit 1
"#);
    let s = outcome(
        f.command("scripts/new/status.sh", &[])
            .env("ENV_FILE", &env)
            .env("EXPECTED_ENV_FILE", &env)
            .env("DOCKER_CALL_LOG", &log)
            .output()
            .unwrap(),
        0,
    );
    has(
        &s,
        &["compose ps used selected env file", "Gateway metrics:"],
    );
    has(
        &fs::read_to_string(log).unwrap(),
        &[&format!("--env-file {} ps", env.display())],
    );
}
#[test]
fn setup_status_help_without_secrets() {
    let f = Fixture::new();
    fs::write(
        f.dir.join(".env"),
        "COMPOSE_PROJECT_NAME=agentforge-fixture-stable\n",
    )
    .unwrap();
    f.executable(
        "docker",
        "#!/bin/sh\ncase \"$1\" in ps|info|compose) exit 0;; esac\nexit 1\n",
    );
    for (args, expected) in [
        (vec!["--help"], vec!["--env FILE"]),
        (
            vec!["--status"],
            vec![
                "Infrastructure status",
                "Connection Info",
                "agentforge-fixture-stable",
            ],
        ),
    ] {
        let s = outcome(f.run("scripts/new/setup-dev.sh", &args), 0);
        has(&s, &expected);
        lacks(
            &s,
            &[
                "POSTGRES_PASSWORD is required",
                "REDIS_PASSWORD is required",
            ],
        );
    }
    let env = f.dir.join("selected.env");
    fs::write(&env, "COMPOSE_PROJECT_NAME=agentforge-fixture-selected\nQDRANT_HTTP_PORT=7666\nQDRANT_GRPC_PORT=7667\n").unwrap();
    let s = outcome(
        f.command(
            "scripts/new/setup-dev.sh",
            &["--env", env.to_str().unwrap(), "--status"],
        )
        .env("COMPOSE_PROJECT_NAME", "wrong-ambient-project")
        .output()
        .unwrap(),
        0,
    );
    has(
        &s,
        &[
            "Qdrant:",
            "http://127.0.0.1:7666",
            "agentforge-fixture-selected",
        ],
    );
    lacks(&s, &["wrong-ambient-project"]);
    lacks(
        &s,
        &[
            "POSTGRES_PASSWORD is required",
            "REDIS_PASSWORD is required",
        ],
    );
}
#[test]
fn setup_start_requires_secrets() {
    let f = Fixture::new();
    let env = f.dir.join("empty.env");
    fs::write(&env, "").unwrap();
    let s = outcome(
        f.run(
            "scripts/new/setup-dev.sh",
            &["--env", env.to_str().unwrap(), "--start"],
        ),
        1,
    );
    has(
        &s,
        &[
            "POSTGRES_PASSWORD is required",
            "REDIS_PASSWORD is required",
        ],
    );
    lacks(&s, &["Starting development"]);
}
