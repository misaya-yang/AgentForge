use super::*;
use crate::binary_run::source_tests::{Fixture, invocation};
fn argv(command: &Command) -> Vec<String> {
    command
        .get_args()
        .map(|a| a.to_str().unwrap().to_owned())
        .collect()
}
fn execution(binary: &(String, bool, Vec<String>)) -> Vec<String> {
    let mut args = vec![format!(
        "/native-cache/target/{}/{}",
        if binary.1 { "release" } else { "debug" },
        binary.0
    )];
    args.extend(binary.2.clone());
    args
}
#[test]
fn compose_run_gets_only_exact_child_bind_overrides_before_the_service() {
    let fixture = Fixture::new();
    for kind in ["sync-workload-source", "sync-plugin-source"] {
        let binary = invocation(kind, true);
        let args =
            argv(&command(&fixture.0, &execution(&binary), false, Some(&binary), None).unwrap());
        let service = args.iter().position(|s| s == "native-build").unwrap();
        let mounts: Vec<_> = args
            .iter()
            .enumerate()
            .filter(|(_, v)| v.as_str() == "--volume")
            .map(|(index, _)| {
                assert!(index + 1 < service);
                args[index + 1].clone()
            })
            .collect();
        let selected = crate::binary_run::source_mounts(&fixture.0, Some(&binary)).unwrap();
        let override_file = fixture
            .0
            .join("deploy/platform-native/dev/compose.source-plugin-sync.yml");
        if kind == "sync-plugin-source" {
            assert!(
                mounts.is_empty(),
                "plugin binds use explicit Compose read_only:false, not a short-volume merge"
            );
            assert!(
                args.windows(2)
                    .any(|w| w[0] == "--file" && w[1] == override_file.to_str().unwrap())
            );
        } else {
            assert_eq!(mounts.len(), 2);
            assert!(
                !args
                    .iter()
                    .any(|arg| arg == override_file.to_str().unwrap())
            );
        }
        let script = &args[service + 2];
        for mount in &selected {
            assert!(script.contains(&format!("test -d '{0}' && test -w '{0}'", mount.container)));
        }

        let expected_user = source_writer_identity(
            &fixture.0,
            &crate::binary_run::source_mounts(&fixture.0, Some(&binary)).unwrap(),
        )
        .unwrap();
        assert!(
            args.windows(2)
                .any(|w| w == ["--user", expected_user.as_str()])
        );
        if kind == "sync-workload-source" {
            for mount in selected {
                assert!(mounts.contains(&format!(
                    "{}:{}:rw",
                    mount.host.display(),
                    mount.container
                )));
            }
        }
        assert!(!mounts.iter().any(|m| m.ends_with(":/source:rw")));
        assert!(args.windows(2).any(|w| w == ["--env-file", "/dev/null"]));
        assert!(
            args.iter()
                .any(|v| v.ends_with("deploy/platform-native/dev/compose.yml"))
        );
        assert_eq!(&args[args.len() - 5..], &binary.2);
    }
}
#[test]
fn ordinary_runs_checks_cargo_and_install_shim_equivalent_checks_stay_read_only() {
    let fixture = Fixture::new();
    for kind in ["sync-workload-source", "sync-plugin-source"] {
        let binary = invocation(kind, false);
        let args =
            argv(&command(&fixture.0, &execution(&binary), false, Some(&binary), None).unwrap());
        assert!(!args.iter().any(|arg| arg == "--volume" || arg == "--user"));
        assert!(
            !args
                .iter()
                .any(|arg| arg.ends_with("compose.source-plugin-sync.yml"))
        );
    }
    let binary = ("gateway".into(), false, vec!["--apply".into()]);
    let args = argv(&command(&fixture.0, &execution(&binary), false, Some(&binary), None).unwrap());
    assert!(!args.iter().any(|arg| arg == "--volume" || arg == "--user"));
    let args = argv(&command(&fixture.0, &["test".into()], false, None, None).unwrap());
    assert!(!args.iter().any(|arg| arg == "--volume" || arg == "--user"));
}
#[test]
fn writer_cannot_select_registry_fetch_or_another_executable() {
    let fixture = Fixture::new();
    let binary = invocation("sync-plugin-source", true);
    assert!(command(&fixture.0, &execution(&binary), true, Some(&binary), None).is_err());
    assert!(
        command(
            &fixture.0,
            &execution(&binary),
            false,
            Some(&binary),
            Some(&fixture.0)
        )
        .is_err()
    );
    let mut wrong = execution(&binary);
    wrong[0] = "/bin/sh".into();
    assert!(command(&fixture.0, &wrong, false, Some(&binary), None).is_err());
}
#[test]
fn source_writer_environment_is_the_same_explicit_public_build_environment() {
    let fixture = Fixture::new();
    let binary = invocation("sync-plugin-source", true);
    let built = command(&fixture.0, &execution(&binary), false, Some(&binary), None).unwrap();
    let allowed = [
        "PATH",
        "HOME",
        "DOCKER_HOST",
        "DOCKER_CONTEXT",
        "DOCKER_CONFIG",
        "DOCKER_TLS_VERIFY",
        "DOCKER_CERT_PATH",
        "SSH_AUTH_SOCK",
        "NATIVE_CONTAINER_ROOT",
        "COMPOSE_DISABLE_ENV_FILE",
        "COMPOSE_PARALLEL_LIMIT",
    ];
    for (name, _) in built.get_envs() {
        assert!(
            allowed.contains(&name.to_str().unwrap()),
            "unexpected environment name"
        );
    }
    assert!(
        built.get_envs().any(
            |(key, value)| key == "COMPOSE_DISABLE_ENV_FILE" && value.is_some_and(|v| v == "1")
        )
    );
}

#[test]
fn plugin_override_explicitly_resets_only_the_two_existing_read_only_bind_targets() {
    let text = include_str!("../../deploy/platform-native/dev/compose.source-plugin-sync.yml");
    let fixture = Fixture::new();
    let binary = invocation("sync-plugin-source", true);
    let selected = crate::binary_run::source_mounts(&fixture.0, Some(&binary)).unwrap();
    let targets: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("        target: "))
        .collect();
    let sources: Vec<_> = text
        .lines()
        .filter_map(|line| line.strip_prefix("        source: "))
        .collect();
    assert_eq!(
        targets,
        selected
            .iter()
            .map(|m| m.container.as_str())
            .collect::<Vec<_>>()
    );
    assert_eq!(sources.len(), 2);
    for (source, mount) in sources.iter().zip(selected) {
        assert_eq!(
            *source,
            format!(
                "${{NATIVE_CONTAINER_ROOT:?launcher required}}/{}",
                mount.container.strip_prefix("/source/").unwrap()
            )
        );
    }
    assert_eq!(
        text.lines()
            .filter(|line| line.trim() == "read_only: false")
            .count(),
        2
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.trim() == "create_host_path: false")
            .count(),
        2
    );
    assert!(!text.contains("read_only: true"));
    assert!(!text.contains("target: /source\n"));
    assert!(!text.contains(".env"));
}
#[test]
fn filesystem_preflight_executes_and_stops_before_the_tool_when_a_target_is_missing() {
    let fixture = Fixture::new();
    let directory = fixture.0.join("projection");
    std::fs::create_dir(&directory).unwrap();
    let mount = crate::binary_run::SourceMount {
        host: directory.clone(),
        container: directory.to_str().unwrap().into(),
    };
    let script = source_write_preflight(&[mount]) + "printf '%s' tool-admitted";
    let admitted = Command::new("/bin/sh")
        .args(["-ec", &script])
        .output()
        .unwrap();
    assert!(admitted.status.success());
    assert_eq!(admitted.stdout, b"tool-admitted");
    std::fs::remove_dir(directory).unwrap();
    let rejected = Command::new("/bin/sh")
        .args(["-ec", &script])
        .output()
        .unwrap();
    assert_eq!(rejected.status.code(), Some(2));
    assert!(rejected.stdout.is_empty());
    assert!(
        String::from_utf8(rejected.stderr)
            .unwrap()
            .contains("generated source directory is not writable:")
    );
}
