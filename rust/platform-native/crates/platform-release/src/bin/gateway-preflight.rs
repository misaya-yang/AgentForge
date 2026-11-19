//! Static native Gateway/Helm checks. No service mutation or secret inspection.
use ai_platform_release::{Result, require};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
fn item(name: &str, status: &str, message: &str) -> Value {
    json!({"name":name,"status":status,"message":message})
}
fn static_checks(root: &Path) -> Result<Vec<Value>> {
    let identity = std::fs::read_to_string(
        root.join("rust/platform-native/crates/platform-gateway/src/migration_host/identity.rs"),
    )?;
    let ingress =
        std::fs::read_to_string(root.join("deploy/helm/ai-gateway/templates/ingress.yaml"))?;
    let secret =
        std::fs::read_to_string(root.join("deploy/helm/ai-gateway/templates/secret.yaml"))?;
    let whitelist = identity
        .split("whitelist_paths: [")
        .nth(1)
        .ok_or("native identity whitelist missing")?
        .split(']')
        .next()
        .ok_or("native whitelist malformed")?;
    let mut checks = vec![];
    let exposed = whitelist.contains("\"/metrics\"");
    checks.push(item(
        "auth_whitelist_metrics",
        if exposed { "fail" } else { "pass" },
        if exposed {
            "/metrics is still auth-whitelisted"
        } else {
            "/metrics is not auth-whitelisted"
        },
    ));
    let exposed = ingress.contains("path: /metrics");
    checks.push(item(
        "ingress_metrics_exposed",
        if exposed { "fail" } else { "pass" },
        if exposed {
            "ingress still routes /metrics publicly"
        } else {
            "ingress does not expose /metrics"
        },
    ));
    let insecure = secret.contains("default \"change-me\"")
        || secret.contains("default \"change-me-in-production\"");
    checks.push(item(
        "helm_secret_defaults",
        if insecure { "fail" } else { "pass" },
        if insecure {
            "Helm secret template still has insecure defaults"
        } else {
            "Helm secret template requires explicit values"
        },
    ));
    Ok(checks)
}
fn helm(chart: &Path, required: bool) -> Value {
    let exe = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|p| p.join("helm"))
            .find(|p| p.is_file())
    });
    let Some(exe) = exe else {
        return item(
            "helm_available",
            if required { "fail" } else { "warn" },
            "helm binary not found; static checks still ran",
        );
    };
    let mut child = match Command::new(exe)
        .arg("lint")
        .arg(chart)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return item("helm_lint", "fail", "helm lint unavailable"),
    };
    // Drain both streams concurrently so verbose lint output cannot block the child.
    use std::io::Read;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() < Duration::from_secs(60) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    match status {
        Some(s) if s.success() => item("helm_lint", "pass", "helm lint passed"),
        Some(_) => {
            let text = if stderr.is_empty() { stdout } else { stderr };
            item(
                "helm_lint",
                "fail",
                &text.trim().chars().take(500).collect::<String>(),
            )
        }
        None => item("helm_lint", "fail", "helm lint timed out"),
    }
}
fn run() -> Result<i32> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let mut root = std::env::current_dir()?;
    let mut chart = None;
    let mut required = false;
    let mut i = 0;
    while i < a.len() {
        if a[i] == "--require-helm" {
            required = true;
            i += 1;
            continue;
        }
        let value = a.get(i + 1).ok_or("missing value")?;
        match a[i].as_str() {
            "--root" => root = PathBuf::from(value),
            "--chart-dir" => chart = Some(PathBuf::from(value)),
            _ => return Err("unknown preflight option".into()),
        }
        i += 2;
    }
    require(root.is_dir(), "source root missing")?;
    let mut checks = static_checks(&root)?;
    checks.push(helm(
        &chart.unwrap_or(root.join("deploy/helm/ai-gateway")),
        required,
    ));
    let count = |status: &str| checks.iter().filter(|v| v["status"] == status).count();
    let failures = count("fail");
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"schema_version":"gateway-preflight/v1","status":if failures==0{"ok"}else{"fail"},"summary":{"passed":count("pass"),"warnings":count("warn"),"failures":failures},"checks":checks})
        )?
    );
    Ok(i32::from(failures > 0))
}
fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(_) => {
            eprintln!("Gateway preflight failed: check static source and options.");
            std::process::exit(1)
        }
    }
}
