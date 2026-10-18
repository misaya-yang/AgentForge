use super::*;
fn selection(component: &str) -> Selection {
    parse(
        &[
            "--component",
            component,
            "--output",
            "/external/new",
            "--tag",
            "agentforge-native-local:test",
        ]
        .map(str::to_owned),
    )
    .unwrap()
}
#[test]
fn exact_components_select_only_the_requested_cached_binary_and_real_compose_variable() {
    for (component, binary, variable) in [
        ("gateway", "gateway", "GATEWAY_IMAGE"),
        ("knowledge", "platform-knowledge-service", "KNOWLEDGE_IMAGE"),
        ("migrate", "database-authority", "MIGRATE_IMAGE"),
        ("tools", "platform-tools", "LOCAL_FRESH_TOOLS_IMAGE"),
        ("auth-bootstrap", "admin-create", "ADMIN_BOOTSTRAP_IMAGE"),
        (
            "credentials",
            "workload-credentials",
            "WORKLOAD_CREDENTIALS_IMAGE",
        ),
    ] {
        let chosen = selection(component);
        assert_eq!(chosen.binary, binary);
        assert_eq!(chosen.variable, variable);
        let script = export_script(chosen.binary);
        assert!(script.contains(&format!(
            "cp --no-preserve=ownership \"$source\" /export/bin/{binary}"
        )));
        assert!(!script.contains("cargo"));
        assert!(!script.contains("/source"));
        assert!(script.contains("test ! -L"));
        assert!(script.contains("7f454c46"));
    }
}
#[test]
fn rejects_unknown_options_profile_paths_tags_and_implicit_components() {
    for args in [
        vec![],
        vec!["--component", "../gateway"],
        vec!["--component", "sdk-worker"],
        vec!["--component", "tools", "--component", "gateway"],
        vec![
            "--component",
            "gateway",
            "--output",
            "/external/new",
            "--tag",
            "ghcr.io/production:v1",
        ],
        vec![
            "--component",
            "gateway",
            "--output",
            "/external/new",
            "--tag",
            "agentforge-native-local:$(id)",
        ],
        vec![
            "--component",
            "gateway",
            "--output",
            "/external/new",
            "--tag",
            "agentforge-native-local:ok",
            "--release",
        ],
        vec![
            "--component",
            "gateway",
            "--output",
            "/external/new",
            "--tag",
            "agentforge-native-local:ok",
            "--build",
            "--build",
        ],
    ] {
        assert!(parse(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
    }
}
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "af-package-test-{}-{}-{}",
            std::process::id(),
            crate::now(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
#[test]
fn independent_fixture_cleanup_cannot_remove_another_live_fixture() {
    let first = Fixture::new();
    let second = Fixture::new();
    assert_ne!(first.0, second.0);
    fs::write(second.0.join("live"), b"retained").unwrap();
    drop(first);
    assert_eq!(fs::read(second.0.join("live")).unwrap(), b"retained");
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[test]
fn destination_rejects_existing_paths_symlink_ancestors_and_repository_writes() {
    let fixture = Fixture::new();
    let repo = fixture.0.join("repo");
    fs::create_dir_all(&repo).unwrap();
    assert!(destination(&repo, &fixture.0.join("export")).is_ok());
    for path in [
        &repo,
        &repo.join("out"),
        &fixture.0,
        &fixture.0.join("../out"),
        &PathBuf::from("relative"),
    ] {
        assert!(destination(&repo, path).is_err());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&fixture.0, fixture.0.join("link")).unwrap();
        assert!(destination(&repo, &fixture.0.join("link/out")).is_err());
    }
}
#[test]
fn export_mounts_only_existing_cache_readonly_and_fresh_external_output() {
    let fixture = Fixture::new();
    let mut selection = selection("gateway");
    selection.output = fixture.0.clone();
    let c = exporter(&selection, "sha256:donor").unwrap();
    let args: Vec<_> = c.get_args().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(args.iter().filter(|s| **s == "--mount").count(), 2);
    assert!(args.contains(
        &"type=volume,src=agentforge-dev-7801d4a9a4_native-dev-cache,dst=/native-cache,readonly"
    ));
    for flag in [
        "--pull=never",
        "--network=none",
        "--read-only",
        "--cap-drop=ALL",
        "--security-opt=no-new-privileges",
    ] {
        assert!(args.contains(&flag));
    }
    assert!(
        !args
            .iter()
            .any(|s| s.contains("/source") || s.contains(".env"))
    );
    assert!(
        c.get_envs()
            .all(|(k, _)| !k.to_string_lossy().contains("TOKEN"))
    );
}
#[test]
fn build_uses_local_donor_selected_target_and_external_context_without_compilation() {
    let selection = selection("knowledge");
    let c = build_command(&selection, "sha256:local", "observed", "head", "binary");
    let args: Vec<_> = c.get_args().map(|s| s.to_str().unwrap()).collect();
    assert_eq!(args[0], "build");
    assert!(args.contains(&"--pull=false"));
    assert!(args.contains(&"--network=none"));
    assert!(args.windows(2).any(|s| s == ["--target", "knowledge"]));
    assert_eq!(args.last(), Some(&"/external/new"));
    assert!(
        !args
            .iter()
            .any(|s| s.contains("cargo") || s.contains("--cache-to") || s.contains("--push"))
    );
}
#[test]
fn runtime_recipe_keeps_compiler_in_donor_only_and_preserves_services_contracts() {
    let recipe = include_str!("../../deploy/platform-native/dev/Dockerfile.package");
    assert!(recipe.contains("FROM scratch AS native-base"));
    assert!(recipe.contains("USER 1000:1000"));
    assert!(recipe.contains("/health/ready"));
    assert!(recipe.contains("AI_GATEWAY_AUTHORITY_SOURCE_MODE=distribution"));
    assert!(!recipe.contains("COPY --from=donor /usr"));
    assert!(!recipe.contains("cargo build"));
    assert!(!recipe.contains("apt-get"));
    let collector = include_str!("../../deploy/platform-native/dev/collect-runtime.sh");
    for required in [
        "/usr/bin/curl",
        "/usr/bin/openssl",
        "/usr/bin/pdftotext",
        "/usr/bin/pdfinfo",
        "/etc/ssl/certs",
        "/usr/share/poppler",
        "ossl-modules",
        "sha256sum --check --strict",
        "not found",
    ] {
        assert!(collector.contains(required), "missing {required}");
    }
    assert!(crate::fence::heavy("package"));
}

#[test]
fn elf_dependency_parser_keeps_loader_and_ignores_vdso_addresses() {
    let report = "\tlinux-vdso.so.1 (0x0000ffff)\n\tlibssl.so.3 => /lib/aarch64-linux-gnu/libssl.so.3 (0x0000abcd)\n\tlibc.so.6 => /lib/aarch64-linux-gnu/libc.so.6 (0x00001234)\n\t/lib/ld-linux-aarch64.so.1 (0x00005678)\n";
    let mut child = Command::new("awk")
        .arg("{ for(i=1;i<=NF;i++) if ($i ~ /^\\//) print $i }")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(report.as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert_eq!(
        String::from_utf8(result.stdout).unwrap(),
        "/lib/aarch64-linux-gnu/libssl.so.3\n/lib/aarch64-linux-gnu/libc.so.6\n/lib/ld-linux-aarch64.so.1\n"
    );
}

#[test]
fn public_payload_copy_rejects_secret_names_and_symlink_files() {
    let fixture = Fixture::new();
    let input = fixture.0.join("payload");
    fs::create_dir(&input).unwrap();
    fs::write(input.join("schema.sql"), "select 1;").unwrap();
    let copy = fixture.0.join("good");
    copy_public(&input, &copy).unwrap();
    assert_eq!(fs::read(copy.join("schema.sql")).unwrap(), b"select 1;");
    fs::write(input.join(".env.dev"), "secret").unwrap();
    assert!(copy_public(&input, &fixture.0.join("bad")).is_err());
    fs::remove_file(input.join(".env.dev")).unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(input.join("schema.sql"), input.join("escape.sql")).unwrap();
        assert!(copy_public(&input, &fixture.0.join("link-bad")).is_err());
    }
}

#[test]
fn source_identity_is_deterministic_and_changes_with_public_bytes() {
    let fixture = Fixture::new();
    for source in policy::SOURCES {
        let path = fixture.0.join(source);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    let first = source_sha(&fixture.0).unwrap();
    assert_eq!(first.len(), 64);
    assert_eq!(source_sha(&fixture.0).unwrap(), first);
    fs::write(fixture.0.join("AGENTS.md"), "changed public source").unwrap();
    assert_ne!(source_sha(&fixture.0).unwrap(), first);
}
