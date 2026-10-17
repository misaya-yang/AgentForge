//! Native ports of the four deployment script contract suites. All external runtime
//! commands are local fixtures; these tests never start Docker or contact providers.
use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};
#[path = "support/compose.rs"]
mod compose;
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
fn read(p: &str) -> String {
    fs::read_to_string(root().join(p)).unwrap()
}
fn has(s: &str, needles: &[&str]) {
    for n in needles {
        assert!(s.contains(n), "missing {n:?}");
    }
}
fn lacks(s: &str, needles: &[&str]) {
    for n in needles {
        assert!(!s.contains(n), "unexpected {n:?}: {s}");
    }
}
struct Fixture {
    dir: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("deployment-contract-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("bin")).unwrap();
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_platform-tools"),
            dir.join("bin/platform-tools"),
        )
        .unwrap();
        let f = Self { dir };
        f.executable("docker", DOCKER);
        f.executable("curl", CURL);
        fs::write(
            f.dir.join("compose.json"),
            serde_json::to_vec(&compose::rendered(&root())).unwrap(),
        )
        .unwrap();
        f
    }
    fn executable(&self, name: &str, text: &str) {
        let p = self.dir.join("bin").join(name);
        fs::write(&p, text).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn command(&self, script: &str, args: &[&str]) -> Command {
        let mut c = Command::new("bash");
        c.current_dir(root())
            .arg(script)
            .args(args)
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.dir.join("bin").display()),
            )
            .env("ENV_FILE", self.dir.join(".env"))
            .env("FAKE_COMPOSE_JSON", self.dir.join("compose.json"));
        for (k, _) in std::env::vars() {
            if k.starts_with("VITE_")
                || k.ends_with("API_KEY")
                || k.ends_with("PASSWORD")
                || k.ends_with("TOKEN")
                || k.ends_with("DSN")
                || k == "AI_PLATFORM_BACKUP_DIR"
            {
                c.env_remove(k);
            }
        }
        c
    }
    fn run(&self, script: &str, args: &[&str]) -> Output {
        self.command(script, args).output().unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn outcome(o: Output, code: i32) -> String {
    let s = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert_eq!(o.status.code(), Some(code), "{s}");
    s
}
const DOCKER: &str = r#"#!/bin/sh
if [ "$1" = compose ]; then
 case "$*" in *" config"*)
  # Validate referenced files too: an old overlay location must not false-green.
  previous=""
  for arg in "$@"; do
   if [ "$previous" = -f ] && [ ! -f "$arg" ]; then exit 1; fi
   previous="$arg"
  done
  case "$*" in *"--format json"*) cat "$FAKE_COMPOSE_JSON";; esac
  exit 0;;
 esac
 case "$2" in version|--env-file) exit 0;; esac
fi
if [ "$1" = info ]; then exit 0; fi
if [ "$1" = ps ]; then echo ai-gateway-pg; exit 0; fi
if [ "$1" = exec ]; then case "$*" in
 *redis-cli*) echo PONG; exit 0;;
 *psql*) case "$*" in *qwen3.7-plus*) echo "${FAKE_DEFAULT_QWEN37_COUNT:-1}";; *provider_id*) printf '%s\n' "$FAKE_ENABLED_MODEL_PROVIDERS";; *) echo 1;; esac; exit 0;;
 *curl*|*ai-platform-capability-worker-health*) exit 0;; esac; fi
exit 1
"#;
const CURL: &str = r#"#!/bin/sh
case "$*" in *"/metrics"*) echo '# HELP gateway_up Gateway metrics endpoint availability'; echo '# TYPE gateway_up gauge'; echo 'gateway_up 1';; esac
exit 0
"#;
fn set(s: &str, k: &str, v: &str) -> String {
    regex::Regex::new(&format!(r"(?m)^{}=.*$", regex::escape(k)))
        .unwrap()
        .replace_all(s, regex::NoExpand(&format!("{k}={v}")))
        .into_owned()
}
const SECRET: &str = "72b49ae231a5769984fbd17aaebf624e7e791ee11a17d33e3bd9c487cb24f3a6";
fn valid(chat: &str) -> String {
    let s = read(".env.example")
        .replace("change_me_generate_with_openssl_32_bytes_minimum", SECRET)
        .replace("change_me_generate_with_openssl", SECRET)
        .replace("change_me_embedding_provider_key", "test-embedding-key");
    format!("{}\n{chat}\n", set(&s, "MODEL_SETUP_MODE", "environment"))
}
fn validate(s: &str, args: &[&str], providers: &str, code: i32, yes: &[&str], no: &[&str]) {
    let f = Fixture::new();
    fs::write(f.dir.join(".env"), s).unwrap();
    let mut argv = vec!["--env"];
    let env = f.dir.join(".env");
    argv.push(env.to_str().unwrap());
    argv.extend(args);
    let out = outcome(
        f.command("scripts/new/validate-env.sh", &argv)
            .env("FAKE_ENABLED_MODEL_PROVIDERS", providers)
            .output()
            .unwrap(),
        code,
    );
    has(&out, yes);
    lacks(&out, no);
    lacks(
        &out,
        &[
            SECRET,
            "test-chat-key",
            "test-dashscope-key",
            "test-vertex-chat-key",
            "change_me_generate_with_openssl",
            "change_me_embedding_provider_key",
        ],
    );
}
#[path = "deployment/operations.rs"]
mod operations;
#[path = "deployment/static_contracts.rs"]
mod static_contracts;
#[path = "deployment/validation.rs"]
mod validation;
fn runtime_values(extra: &[(&str, &str)]) -> BTreeMap<String, String> {
    let f = Fixture::new();
    let path = f.dir.join("runtime-config.js");
    let mut c = f.command("web/docker-entrypoint.d/40-runtime-config.sh", &[]);
    c.env("RUNTIME_CONFIG_OUTPUT_PATH", &path);
    for (k, v) in extra {
        c.env(k, v);
    }
    outcome(c.output().unwrap(), 0);
    regex::Regex::new(r#"(?m)^\s+(\w+): "(.*)"[,]?$"#)
        .unwrap()
        .captures_iter(&fs::read_to_string(path).unwrap())
        .map(|c| {
            (
                c[1].into(),
                serde_json::from_str(&format!("\"{}\"", &c[2])).unwrap(),
            )
        })
        .collect()
}
#[test]
fn runtime_config_defaults() {
    let v = runtime_values(&[]);
    for (k, e) in [
        ("apiUrl", ""),
        ("apiBaseUrl", ""),
        ("authEmailDomain", "example.com"),
        ("supportEmail", "admin@example.com"),
        ("telemetryEndpoint", ""),
        ("sseDebug", ""),
        ("agentStudioEnabled", "true"),
    ] {
        assert_eq!(v[k], e);
    }
}
#[test]
fn runtime_config_domain_default() {
    let v = runtime_values(&[("VITE_AUTH_EMAIL_DOMAIN", "myapp.test")]);
    assert_eq!(v["authEmailDomain"], "myapp.test");
    assert_eq!(v["supportEmail"], "admin@myapp.test");
}
#[test]
fn runtime_config_escapes() {
    let v = runtime_values(&[
        ("VITE_API_URL", "https://api.example.com/\"gateway\"\\v1"),
        ("VITE_API_BASE_URL", "https://gateway.example.com/api\nv1"),
        ("VITE_AUTH_EMAIL_DOMAIN", "myapp.test"),
        ("VITE_SUPPORT_EMAIL", r"support\desk@example.com"),
        (
            "VITE_TELEMETRY_ENDPOINT",
            "https://telemetry.example.com/collect?tag=\"release\"",
        ),
        ("VITE_SSE_DEBUG", "true\r\nignored"),
        ("VITE_AGENT_STUDIO_ENABLED", "false"),
    ]);
    for (k, e) in [
        ("apiUrl", "https://api.example.com/\"gateway\"\\v1"),
        ("apiBaseUrl", "https://gateway.example.com/api v1"),
        ("authEmailDomain", "myapp.test"),
        ("supportEmail", r"support\desk@example.com"),
        (
            "telemetryEndpoint",
            "https://telemetry.example.com/collect?tag=\"release\"",
        ),
        ("sseDebug", "true  ignored"),
        ("agentStudioEnabled", "false"),
    ] {
        assert_eq!(v[k], e);
    }
}
#[test]
fn runtime_config_uncached() {
    let n = read("web/nginx.conf");
    let r = regex::Regex::new(r"(?s)location = /runtime-config\.js \{\n(.*?)\n        \}").unwrap();
    let body = &r.captures(&n).unwrap()[1];
    has(
        body,
        &[
            "add_header Cache-Control \"no-store, no-cache, must-revalidate\" always;",
            "add_header Pragma \"no-cache\" always;",
            "try_files $uri =404;",
        ],
    );
}
#[test]
fn seed_dry_run() {
    let f = Fixture::new();
    let s = outcome(f.run("scripts/new/seed-demo-data.sh", &["--dry-run"]), 0);
    has(
        &s,
        &[
            "Dry run complete",
            "No database connection will be opened",
            "/knowledge/demo-kb-ai-gateway",
            "/share/demo-share",
            "/quiz/demo-quiz",
        ],
    );
}
#[test]
fn seed_idempotent() {
    has(
        &read("examples/demo-data/open-source-demo.sql"),
        &[
            "ON CONFLICT",
            "demo-kb-ai-gateway",
            "demo-share",
            "demo-quiz",
            "demo-assistant-session",
        ],
    );
}
