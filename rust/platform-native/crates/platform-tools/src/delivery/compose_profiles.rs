//! One include bundle, explicit feature selection, unchanged existing project/volumes.
use super::*;
pub(super) fn matrix(root: &Path) -> Result<Value> {
    let v = object(&root.join("deploy/gitops/compose-profiles.json"))?;
    require(
        v["schema_version"] == "agentforge/compose-profiles/v1"
            && v["automatic_activation"] == false
            && v["default_mode"] == "full",
        "invalid opt-in Compose profile matrix",
    )?;
    let modes = v["modes"].as_object().ok_or("Compose modes missing")?;
    require(
        modes.len() == 3
            && ["full", "compact", "scale"]
                .iter()
                .all(|name| modes.contains_key(*name)),
        "all existing topologies must remain",
    )?;
    let mut names = BTreeSet::new();
    for row in v["features"]
        .as_array()
        .ok_or("Compose feature matrix missing")?
    {
        require(
            names.insert(row["name"].as_str().ok_or("feature name missing")?),
            "duplicate Compose feature",
        )?;
        let path = row["path"].as_str().ok_or("feature source missing")?;
        safe(root, path)?;
        require(
            row["profiles"].is_array() && row["requires"].is_array(),
            "feature profile/dependency arrays required",
        )?;
    }
    for path in modes.values() {
        safe(root, path.as_str().ok_or("mode source missing")?)?;
    }
    for row in v["features"].as_array().unwrap() {
        for dependency in row["requires"].as_array().unwrap() {
            require(
                dependency.as_str().is_some_and(|s| names.contains(s)),
                "unknown Compose feature dependency",
            )?;
        }
    }
    Ok(v)
}
fn selection(
    root: &Path,
    mode: &str,
    features: &[String],
) -> Result<(Vec<String>, BTreeSet<String>)> {
    let v = matrix(root)?;
    let modepath = v["modes"][mode]
        .as_str()
        .ok_or("unsupported Compose topology")?;
    let selected: BTreeSet<_> = features.iter().map(String::as_str).collect();
    require(
        selected.len() == features.len(),
        "duplicate Compose feature",
    )?;
    let rows = v["features"].as_array().unwrap();
    require(
        selected
            .iter()
            .all(|name| rows.iter().any(|r| r["name"] == *name)),
        "unknown Compose feature",
    )?;
    for row in rows
        .iter()
        .filter(|r| selected.contains(r["name"].as_str().unwrap()))
    {
        for dependency in row["requires"].as_array().unwrap() {
            require(
                selected.contains(dependency.as_str().unwrap()),
                "Compose feature prerequisite was not explicitly selected",
            )?;
        }
    }
    for pair in v["incompatible"]
        .as_array()
        .ok_or("incompatibility matrix missing")?
    {
        require(
            !pair
                .as_array()
                .ok_or("bad incompatible pair")?
                .iter()
                .all(|v| v.as_str().is_some_and(|s| selected.contains(s))),
            "incompatible Compose feature selection",
        )?;
    }
    let mut paths = vec!["docker-compose.yml".into(), modepath.into()];
    let mut profiles = BTreeSet::new();
    for row in rows
        .iter()
        .filter(|r| selected.contains(r["name"].as_str().unwrap()))
    {
        paths.push(row["path"].as_str().unwrap().into());
        for profile in row["profiles"].as_array().unwrap() {
            profiles.insert(profile.as_str().ok_or("profile name missing")?.to_owned());
        }
    }
    Ok((paths, profiles))
}
pub(super) fn plan(args: &Args) -> Result<Value> {
    let root = args.root().canonicalize()?;
    let mode = args
        .values
        .get("--mode")
        .map(String::as_str)
        .unwrap_or("full");
    let features = args.multiple.get("--feature").cloned().unwrap_or_default();
    let (paths, profiles) = selection(&root, mode, &features)?;
    let project = args.get("--project-name")?;
    require(
        !project.is_empty()
            && project.len() <= 63
            && project
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
            && project.as_bytes()[0].is_ascii_alphanumeric(),
        "explicit existing Compose project name required",
    )?;
    let mut argv = vec![
        "docker".to_owned(),
        "compose".into(),
        "--project-name".into(),
        project.into(),
        "--project-directory".into(),
        root.to_string_lossy().into_owned(),
    ];
    let bundle = json!({"name":project,"include":[{"path":paths.iter().map(|p|root.join(p)).collect::<Vec<_>>(),"project_directory":root}]});
    if let Some(path) = args.values.get("--output") {
        let path = Path::new(path);
        require(
            path.is_absolute() && !path.starts_with(&root),
            "include bundle must be external and absolute",
        )?;
        let parent = path
            .parent()
            .ok_or("output parent missing")?
            .canonicalize()?;
        require(
            parent.join(path.file_name().ok_or("output name missing")?) == path,
            "output path must not traverse symlinks",
        )?;
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        f.write_all(serde_json::to_string_pretty(&bundle)?.as_bytes())?;
        f.write_all(b"\n")?;
        argv.extend(["-f".into(), path.to_string_lossy().into_owned()]);
    } else {
        for path in &paths {
            argv.extend(["-f".into(), root.join(path).to_string_lossy().into_owned()]);
        }
    }
    for profile in profiles {
        argv.extend(["--profile".into(), profile]);
    }
    argv.extend(["config".into(), "--quiet".into()]);
    Ok(
        json!({"gate":"compose-profile-plan","result":"pass","mode":mode,"features":features,"include_bundle":bundle,"render_command":argv,"docker_executed":false,"runtime_acceptance":false,"project_identity":"caller must match the already owned project"}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")
    }
    #[test]
    fn absent_features_never_enable_optional_services() {
        let (p, f) = selection(&root(), "full", &[]).unwrap();
        assert_eq!(p.len(), 2);
        assert!(f.is_empty());
    }
    #[test]
    fn profile_dependencies_duplicates_unknowns_and_incompatible_builds_fail() {
        for (mode, f) in [
            ("unknown", vec![]),
            ("full", vec!["profiling"]),
            ("full", vec!["sdk-build"]),
            ("full", vec!["observability", "observability"]),
            ("full", vec!["unreviewed"]),
            ("full", vec!["native-candidate", "source-build"]),
        ] {
            assert!(
                selection(
                    &root(),
                    mode,
                    &f.into_iter().map(String::from).collect::<Vec<_>>()
                )
                .is_err()
            );
        }
    }
    #[test]
    fn exact_order_and_profiles_preserve_existing_main_topology() {
        let (p, f) = selection(
            &root(),
            "compact",
            &["profiling".into(), "observability".into()],
        )
        .unwrap();
        assert_eq!(p[0], "docker-compose.yml");
        assert!(p[1].ends_with("compact.yml"));
        assert!(p[2].contains("monitoring.yml"));
        assert!(p[3].contains("profiling.yml"));
        assert_eq!(
            f,
            BTreeSet::from(["observability".into(), "profiling".into()])
        );
    }
}
