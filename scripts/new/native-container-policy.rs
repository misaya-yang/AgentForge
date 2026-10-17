use std::{
    fs,
    path::{Path, PathBuf},
};
pub const PROJECT: &str = "agentforge-dev-7801d4a9a4";
pub const IMAGE: &str = "agentforge-native-dev:rust1.99.0";
pub const BASE: &str = "rust:1.99.0-bookworm";
pub const VOLUME: &str = "agentforge-dev-7801d4a9a4_native-dev-cache";
pub const SOURCES: &[&str] = &[
    "rust/platform-native",
    "rust/agent-runtime-overlay/kernel-rs/Cargo.toml",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine",
    "database",
    "tests",
    "docs",
    "deploy",
    "scripts",
    "sdk/fixtures",
    "sdk/openapi",
    "sdk/openapi.json",
    "sdk/openapi-profiles.json",
    "sdk/native-common-contracts.json",
    "apps/local-node/native/macos/LocalNodeMacOSHelper.swift",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-agent-runtime/src/schema_admission_expected.json",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/schema_admission_expected.json",
    "src/services/eval/fixtures/real_finance_salesforce_fy26_q1/golden.v1.json",
    "src/services/eval/fixtures/real_research/cra_excerpt_provenance.v1.json",
    "web/src/pages/knowledge/sources/sourceSyncContract.fixture.json",
    "reports/dots-source-import-20261015.json",
    "AGENTS.md",
    "Makefile",
    "harness.yml",
    "rust-toolchain.toml",
];
pub fn scan(path: &Path) -> Result<(), String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("non-UTF8 source path")?;
    if name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".env")
        || [".secrets", "target", "node_modules", ".git"].contains(&name)
    {
        return Err(format!(
            "source mount contains forbidden configuration/cache: {}",
            path.display()
        ));
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("source {}: {e}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "source symlink requires an explicit safe mount instead: {}",
            path.display()
        ));
    }
    if metadata.is_dir() {
        for child in fs::read_dir(path).map_err(|e| e.to_string())? {
            scan(&child.map_err(|e| e.to_string())?.path())?;
        }
    }
    Ok(())
}
pub fn sources(root: &Path) -> Result<(), String> {
    for source in SOURCES {
        scan(&root.join(source))?;
    }
    Ok(())
}
pub fn cargo_args(action: &str, args: &[String]) -> Result<Vec<String>, String> {
    let mut result = vec![action.to_owned(), "--locked".into(), "--offline".into()];
    let mut i = 0;
    let mut package = false;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--" && action == "test" {
            result.extend(args[i..].iter().cloned());
            break;
        }
        if [
            "-p",
            "--package",
            "--bin",
            "--test",
            "--example",
            "--features",
            "--exclude",
        ]
        .contains(&arg)
        {
            let value = args
                .get(i + 1)
                .filter(|v| !v.is_empty() && !v.starts_with('-'))
                .ok_or_else(|| format!("value required: {arg}"))?;
            if value.chars().any(char::is_control) {
                return Err("control character in target option".into());
            }
            package |= matches!(arg, "-p" | "--package");
            result.extend([arg.to_owned(), value.clone()]);
            i += 2;
        } else if [
            "--workspace",
            "--all-targets",
            "--lib",
            "--bins",
            "--tests",
            "--examples",
            "--all-features",
            "--no-default-features",
            "--release",
        ]
        .contains(&arg)
        {
            result.push(arg.to_owned());
            i += 1;
        } else {
            return Err(format!(
                "unsupported Cargo option (no environment/target-dir/config overrides): {arg}"
            ));
        }
    }
    if !package && !args.iter().any(|v| v == "--workspace") {
        result.push("--workspace".into());
    }
    Ok(result)
}
pub const ENGINE_MANIFEST: &str =
    "/source/rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/Cargo.toml";
pub fn engine_args(action: &str, args: &[String]) -> Result<Vec<String>, String> {
    let mut command = if action == "engine-fetch" {
        if !args.is_empty() {
            return Err("engine-fetch takes no Cargo target overrides".into());
        }
        vec!["fetch".into(), "--locked".into()]
    } else {
        cargo_args("build", args)?
    };
    command.splice(1..1, ["--manifest-path".into(), ENGINE_MANIFEST.into()]);
    Ok(command)
}
pub fn external_cache() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".cache")))
        .ok_or("HOME or XDG_CACHE_HOME required")?;
    Ok(base.join("agentforge-native-launcher"))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn args_preserve_real_targets_and_test_filter() {
        let input = [
            "-p",
            "ai-platform-primitives",
            "--lib",
            "--",
            "tls",
            "--exact",
        ]
        .map(str::to_owned);
        assert_eq!(
            cargo_args("test", &input).unwrap(),
            [
                "test",
                "--locked",
                "--offline",
                "-p",
                "ai-platform-primitives",
                "--lib",
                "--",
                "tls",
                "--exact"
            ]
        );
    }
    #[test]
    fn overrides_and_other_workspaces_rejected() {
        for flag in [
            "--target-dir",
            "--manifest-path",
            "--config",
            "-j",
            "--jobs",
            "--target",
            "--profile",
            "--frozen",
        ] {
            assert!(cargo_args("check", &[flag.into(), "x".into()]).is_err());
        }
    }
    #[test]
    fn private_paths_and_host_targets_rejected_without_reading_values() {
        for name in [
            ".env",
            ".env.dev",
            ".secrets",
            "target",
            "node_modules",
            ".git",
        ] {
            assert!(scan(Path::new(name)).unwrap_err().contains("forbidden"));
        }
    }
    #[test]
    fn every_public_source_has_an_explicit_readonly_compose_mount() {
        let compose = include_str!("../../deploy/platform-native/dev/compose.yml");
        for path in SOURCES {
            assert!(
                compose.contains(&format!("/{path}:/source/{path}:ro")),
                "missing readonly mount: {path}"
            );
        }
        assert!(!compose.contains("/var/run/docker.sock"));
        assert!(!compose.contains("env_file:"));
    }
    #[test]
    fn current_public_mounts_have_no_secrets_symlinks_or_host_targets() {
        let root = Path::new(file!())
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        sources(root).unwrap();
    }
    #[test]
    fn engine_uses_fixed_manifest_and_the_existing_target_cache() {
        let command = engine_args("engine-build", &["--release".into()]).unwrap();
        assert_eq!(command[0], "build");
        assert_eq!(command[1], "--manifest-path");
        assert_eq!(command[2], ENGINE_MANIFEST);
        assert!(command.iter().any(|a| a == "--offline"));
        let fetch = engine_args("engine-fetch", &[]).unwrap();
        assert_eq!(
            fetch,
            ["fetch", "--manifest-path", ENGINE_MANIFEST, "--locked"]
        );
        assert!(engine_args("engine-fetch", &["--manifest-path".into(), "other".into()]).is_err());
        assert!(!command.iter().any(|a| a == "--target-dir"));
    }
}
