//! Exercise the same rustc-only helper used by both CI composite actions.
#[allow(dead_code)]
#[path = "../src/github_env_bootstrap.rs"]
mod bootstrap;

struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("github-env-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn release_refs_never_enable_cache_and_invalid_modes_fail() {
    assert!(!bootstrap::mode("auto", "refs/tags/v1.0.0").unwrap());
    assert!(!bootstrap::mode("none", "refs/heads/main").unwrap());
    assert!(bootstrap::mode("gha", "refs/tags/v1.0.0").is_err());
    assert!(bootstrap::mode("unknown", "refs/heads/main").is_err());
    assert!(bootstrap::mode("auto", "refs/pull/1/merge").unwrap());
}

#[test]
fn command_values_refuse_newline_and_control_character_injection() {
    for value in [
        "target\nRUSTC_WRAPPER=evil",
        "target\rPATH=evil",
        "a\0b",
        "a\tb",
    ] {
        assert!(bootstrap::clean(value).is_err());
    }
    assert!(bootstrap::clean("/tmp/runner target/工具").is_ok());
}

#[test]
fn targets_stay_below_runner_temp_and_capacity_is_bounded() {
    let temp = Temp::new();
    let target = temp.0.join("target");
    assert!(bootstrap::under_temp(&temp.0, &target).is_ok());
    assert!(bootstrap::under_temp(&temp.0, &temp.0).is_err());
    assert!(bootstrap::under_temp(&temp.0, &temp.0.join("../escape")).is_err());
    assert!(bootstrap::under_temp(&temp.0, &std::env::temp_dir()).is_err());
    for (jobs, threads) in [(0, 1), (5, 1), (1, 0), (1, 3)] {
        assert!(bootstrap::records(&target, jobs, threads, "single", false, &temp.0).is_err());
    }
    let values = bootstrap::records(&target, 1, 1, "single", false, &temp.0).unwrap();
    assert!(
        values
            .iter()
            .any(|(k, v)| k == "RUSTC_WRAPPER" && v.is_empty())
    );
    assert!(
        values
            .iter()
            .any(|(k, v)| k == "CARGO_INCREMENTAL" && v == "0")
    );
    assert!(values.iter().all(|(_, v)| bootstrap::clean(v).is_ok()));
}

#[cfg(unix)]
#[test]
fn symlinks_cannot_redirect_a_target_outside_runner_temp() {
    let temp = Temp::new();
    let outside = Temp::new();
    std::os::unix::fs::symlink(&outside.0, temp.0.join("link")).unwrap();
    assert!(bootstrap::under_temp(&temp.0, &temp.0.join("link/target")).is_err());
}
