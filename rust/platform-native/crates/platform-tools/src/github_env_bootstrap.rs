//! rustc-only CI command-file writer. Accepts no arbitrary keys or multiline values.
#[path = "ci_capacity_policy.rs"]
mod capacity;
use std::{
    io::Write,
    path::{Component, Path, PathBuf},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub fn clean(value: &str) -> Result<()> {
    if value.chars().any(|c| c.is_control()) {
        return Err("CI environment value contains a control character".into());
    }
    Ok(())
}
pub fn under_temp(temp: &Path, path: &Path) -> Result<PathBuf> {
    clean(path.to_str().ok_or("CI path is not UTF-8")?)?;
    if !path.is_absolute()
        || path
            .components()
            .any(|v| matches!(v, Component::ParentDir | Component::CurDir))
    {
        return Err("CI path must be absolute without relative components".into());
    }
    let root = temp.canonicalize()?;
    // Preserve a not-yet-created suffix while resolving all existing ancestors.
    let mut existing = path;
    let mut suffix = vec![];
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err("CI target may not be a symlink".into());
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                suffix.push(
                    existing
                        .file_name()
                        .ok_or("invalid CI path")?
                        .to_os_string(),
                );
                existing = existing.parent().ok_or("CI path has no parent")?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let mut canonical = existing.canonicalize()?;
    for part in suffix.into_iter().rev() {
        canonical.push(part);
    }
    if !canonical.starts_with(&root) || canonical == root {
        return Err("CI path escapes the runner temp directory".into());
    }
    #[cfg(windows)]
    return Ok(path.to_path_buf());
    #[cfg(not(windows))]
    Ok(canonical)
}
pub fn mode(request: &str, reference: &str) -> Result<bool> {
    match (request, reference.starts_with("refs/tags/")) {
        ("none", _) | ("auto", true) => Ok(false),
        ("auto" | "gha", false) => Ok(true),
        ("gha", true) => Err("release jobs may not restore compiler caches".into()),
        _ => Err("cache-mode must be auto, gha or none".into()),
    }
}
pub fn records(
    target: &Path,
    jobs: usize,
    threads: usize,
    profile: &str,
    cache: bool,
    temp: &Path,
) -> Result<Vec<(String, String)>> {
    if !(1..=4).contains(&jobs)
        || !(1..=2).contains(&threads)
        || !["single", "hosted"].contains(&profile)
    {
        return Err("invalid bounded CI capacity".into());
    }
    let target = under_temp(temp, target)?;
    let cache_dir = under_temp(temp, &temp.join("sccache"))?;
    let mut records = vec![
        ("CARGO_BUILD_JOBS".into(), jobs.to_string()),
        ("NEXTEST_TEST_THREADS".into(), threads.to_string()),
        ("AI_PLATFORM_CI_CAPACITY_PROFILE".into(), profile.into()),
        (
            "CARGO_TARGET_DIR".into(),
            target.to_str().ok_or("non UTF-8 target")?.into(),
        ),
    ];
    for (k, v) in [
        ("CARGO_INCREMENTAL", "0"),
        ("CARGO_PROFILE_DEV_DEBUG", "0"),
        ("CARGO_PROFILE_TEST_DEBUG", "0"),
        ("RUSTC_WRAPPER", if cache { "sccache" } else { "" }),
        ("RUSTC_WORKSPACE_WRAPPER", ""),
        ("SCCACHE_GHA_ENABLED", if cache { "true" } else { "false" }),
        ("SCCACHE_CACHE_SIZE", "2G"),
    ] {
        records.push((k.into(), v.into()));
    }
    records.push((
        "SCCACHE_DIR".into(),
        cache_dir.to_str().ok_or("non UTF-8 cache path")?.into(),
    ));
    for (_, v) in &records {
        clean(v)?;
    }
    Ok(records)
}
fn append(
    temp: &Path,
    envfile: &Path,
    pathfile: &Path,
    records: &[(String, String)],
    target: &Path,
) -> Result<()> {
    for (file, prefix) in [(envfile, "set_env_"), (pathfile, "add_path_")] {
        let canonical = under_temp(temp, file)?;
        let metadata = std::fs::symlink_metadata(file)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || !canonical
                .file_name()
                .and_then(|p| p.to_str())
                .is_some_and(|s| s.starts_with(prefix))
            || canonical
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|p| p.to_str())
                != Some("_runner_file_commands")
        {
            return Err("unexpected GitHub command file".into());
        }
    }
    let mut env = std::fs::OpenOptions::new().append(true).open(envfile)?;
    for (name, value) in records {
        clean(value)?;
        writeln!(env, "{name}={value}")?;
    }
    let debug = under_temp(temp, &target.join("debug"))?;
    let mut path = std::fs::OpenOptions::new().append(true).open(pathfile)?;
    writeln!(path, "{}", debug.to_str().ok_or("non UTF-8 PATH entry")?)?;
    Ok(())
}
fn main() -> Result<()> {
    let temp = PathBuf::from(std::env::var("RUNNER_TEMP")?);
    clean(temp.to_str().ok_or("non UTF-8 runner temp")?)?;
    let kind = std::env::args()
        .nth(1)
        .ok_or("expected native or packaging")?;
    let profile = std::env::var("AI_PLATFORM_CI_CAPACITY_PROFILE").unwrap_or("single".into());
    let (target, jobs, threads, cache) = match kind.as_str() {
        "native" => {
            let observed = capacity::detect()?;
            let cache = mode(
                &std::env::var("CI_CACHE_MODE").unwrap_or("auto".into()),
                &std::env::var("GITHUB_REF").unwrap_or_default(),
            )?;
            (
                temp.join("platform-native-target"),
                observed.build_jobs,
                observed.test_threads,
                cache,
            )
        }
        "packaging" => (
            std::env::var_os("CARGO_TARGET_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| temp.join("platform-client-packaging-target")),
            1,
            1,
            false,
        ),
        _ => return Err("expected native or packaging".into()),
    };
    let target = under_temp(&temp, &target)?;
    let records = records(&target, jobs, threads, &profile, cache, &temp)?;
    append(
        &temp,
        &PathBuf::from(std::env::var("GITHUB_ENV")?),
        &PathBuf::from(std::env::var("GITHUB_PATH")?),
        &records,
        &target,
    )
}
