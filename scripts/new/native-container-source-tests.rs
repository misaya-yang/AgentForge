use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct Fixture(pub(crate) PathBuf);
impl Fixture {
    pub(crate) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "native-source-mount-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        let fixture = Self(path.canonicalize().unwrap());
        for relative in WORKLOAD_PROJECTIONS.iter().chain(PLUGIN_PROJECTIONS) {
            fs::create_dir_all(fixture.0.join(relative)).unwrap();
        }
        fixture
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
pub(crate) fn invocation(kind: &str, apply: bool) -> (String, bool, Vec<String>) {
    let mut args = vec![
        "source-lock".into(),
        kind.into(),
        "--repo-root".into(),
        "/source".into(),
    ];
    if apply {
        args.push("--apply".into());
    }
    ("platform-tools".into(), false, args)
}

#[test]
fn exact_syncs_grant_only_their_two_generated_directories() {
    let fixture = Fixture::new();
    for (kind, expected) in [
        ("sync-workload-source", WORKLOAD_PROJECTIONS),
        ("sync-plugin-source", PLUGIN_PROJECTIONS),
    ] {
        let binary = invocation(kind, true);
        let mounts = source_mounts(&fixture.0, Some(&binary)).unwrap();
        assert_eq!(mounts.len(), 2);
        for (mount, relative) in mounts.iter().zip(expected) {
            assert_eq!(mount.host, fixture.0.join(relative));
            assert_eq!(mount.container, format!("/source/{relative}"));
        }
        let mut host_root = binary.clone();
        host_root.1 = true;
        host_root.2[3] = fixture.0.to_str().unwrap().into();
        assert_eq!(source_mounts(&fixture.0, Some(&host_root)).unwrap(), mounts);
        assert!(
            source_mounts(&fixture.0, Some(&invocation(kind, false)))
                .unwrap()
                .is_empty()
        );
    }
}
#[test]
fn alternate_roots_options_outputs_and_apply_spellings_are_rejected() {
    let fixture = Fixture::new();
    let valid = invocation("sync-workload-source", true);
    for root in [
        ".",
        "/",
        "/source/..",
        "/source/subdir",
        "/source/",
        "/outside",
    ] {
        let mut bad = valid.clone();
        bad.2[3] = root.into();
        assert!(source_mounts(&fixture.0, Some(&bad)).is_err(), "{root}");
    }
    for suffix in [
        vec!["--apply", "--apply"],
        vec!["--apply", "--output", "/source"],
        vec!["--apply=true"],
        vec!["--evidence-out"],
        vec!["--help"],
    ] {
        let mut bad = invocation("sync-workload-source", false);
        bad.2.extend(suffix.into_iter().map(str::to_owned));
        assert!(
            source_mounts(&fixture.0, Some(&bad)).is_err(),
            "{:?}",
            bad.2
        );
    }
    let mut swapped = valid.clone();
    swapped.2.swap(2, 4);
    assert!(source_mounts(&fixture.0, Some(&swapped)).is_err());
    for command in [
        "validate",
        "sync-workload-source-extra",
        "sync-unknown-source",
    ] {
        assert!(source_mounts(&fixture.0, Some(&invocation(command, true))).is_err());
    }
}
#[test]
fn arbitrary_tools_and_read_only_commands_never_gain_writes() {
    let fixture = Fixture::new();
    assert!(source_mounts(&fixture.0, None).unwrap().is_empty());
    let mut other = invocation("sync-plugin-source", true);
    other.0 = "gateway".into();
    assert!(source_mounts(&fixture.0, Some(&other)).unwrap().is_empty());
    let other = (
        "platform-tools".into(),
        false,
        vec!["seed-demo".into(), "--apply".into()],
    );
    assert!(source_mounts(&fixture.0, Some(&other)).unwrap().is_empty());
    let other = (
        "platform-tools".into(),
        false,
        vec!["source-lock".into(), "validate".into()],
    );
    assert!(source_mounts(&fixture.0, Some(&other)).unwrap().is_empty());
}
#[test]
fn missing_or_non_directory_projection_never_widens_the_mount() {
    let fixture = Fixture::new();
    let target = fixture.0.join(WORKLOAD_PROJECTIONS[1]);
    fs::remove_dir(&target).unwrap();
    let binary = invocation("sync-workload-source", true);
    assert!(source_mounts(&fixture.0, Some(&binary)).is_err());
    assert!(!target.exists());
    fs::write(target, b"ordinary fixture file").unwrap();
    assert!(source_mounts(&fixture.0, Some(&binary)).is_err());
}
#[cfg(unix)]
#[test]
fn leaf_parent_and_repository_symlinks_cannot_escape_to_another_directory() {
    use std::os::unix::fs::symlink;
    for leaf in [true, false] {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        let relative = if leaf {
            WORKLOAD_PROJECTIONS[0]
        } else {
            "rust/agent-runtime-overlay"
        };
        fs::remove_dir_all(fixture.0.join(relative)).unwrap();
        symlink(outside.0.join(relative), fixture.0.join(relative)).unwrap();
        assert!(
            source_mounts(&fixture.0, Some(&invocation("sync-workload-source", true))).is_err()
        );
    }
    let fixture = Fixture::new();
    let outside = Fixture::new();
    let alias = outside.0.join("repo-alias");
    symlink(&fixture.0, &alias).unwrap();
    assert!(source_mounts(&alias, Some(&invocation("sync-plugin-source", true))).is_err());
}
#[test]
fn writable_receiver_must_match_the_fixed_executable_and_mapped_argv() {
    let fixture = Fixture::new();
    let binary = invocation("sync-plugin-source", true);
    let mut command = vec!["/native-cache/target/debug/platform-tools".into()];
    command.extend(binary.2.clone());
    verify_source_command(&fixture.0, &binary, &command).unwrap();
    let mut mismatch = command.clone();
    mismatch[0] = "/bin/sh".into();
    assert!(verify_source_command(&fixture.0, &binary, &mismatch).is_err());
    mismatch = command.clone();
    mismatch.push("--output".into());
    assert!(verify_source_command(&fixture.0, &binary, &mismatch).is_err());
    let mut host = binary;
    host.2[3] = fixture.0.to_str().unwrap().into();
    verify_source_command(&fixture.0, &host, &command).unwrap();
}
#[test]
fn volume_delimiters_cannot_be_injected_through_a_repository_name() {
    let mut fixture = Fixture::new();
    let renamed = fixture.0.with_file_name(format!(
        "{}:rw",
        fixture.0.file_name().unwrap().to_str().unwrap()
    ));
    fs::rename(&fixture.0, &renamed).unwrap();
    fixture.0 = renamed;
    assert!(source_mounts(&fixture.0, Some(&invocation("sync-plugin-source", true))).is_err());
}
