use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn both_cli_selection_paths_refuse_remote_context_and_host_overrides_before_build() {
    let root = std::env::temp_dir().join(format!("buildx-cli-{}", uuid::Uuid::new_v4()));
    fs::create_dir(&root).unwrap();
    let bin = root.join("docker");
    fs::write(&bin, "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$DOCKER_TEST_LOG\"\ncase \"$1 $2\" in\n 'context show') echo remote ;;\n 'context inspect') echo '\"tcp://remote.invalid:2376\"' ;;\n *) echo 'unexpected mutating/network Docker command' >&2; exit 77 ;;\nesac\n").unwrap();
    fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    let log = root.join("calls");
    for explicit in [false, true] {
        for host_override in [false, true] {
            let _ = fs::remove_file(&log);
            let mut cmd = Command::new(env!("CARGO_BIN_EXE_platform-tools"));
            cmd.args(["buildx-resource-builder", "--repo-root"])
                .arg(&root)
                .args(["--memory", "4g", "--cpu-quota", "400000"])
                .env_clear()
                .env("PATH", format!("{}:/usr/bin:/bin", root.display()))
                .env("DOCKER_TEST_LOG", &log);
            if explicit {
                cmd.env("AI_PLATFORM_RUST_EXISTING_BUILDX_BUILDER", "desktop-linux");
            }
            if host_override {
                cmd.env("DOCKER_HOST", "tcp://remote.invalid:2376");
            }
            let output = cmd.output().unwrap();
            assert!(!output.status.success());
            let diagnostic = String::from_utf8_lossy(&output.stdout).to_string()
                + &String::from_utf8_lossy(&output.stderr);
            if host_override {
                assert!(diagnostic.contains("unset DOCKER_HOST"));
                assert!(
                    !log.exists(),
                    "host override must fail before any Docker invocation"
                );
            } else {
                assert!(diagnostic.contains("local Unix-socket"));
                let calls = fs::read_to_string(&log).unwrap();
                assert!(calls.lines().all(|line| line.starts_with("context ")));
                assert_eq!(calls.lines().count(), 2);
            }
        }
    }
    fs::remove_dir_all(root).unwrap();
}
