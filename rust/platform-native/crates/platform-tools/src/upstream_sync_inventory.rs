//! Observations from immutable upstream blobs and the existing source-unit payload.
use super::*;
use std::{os::unix::fs::PermissionsExt, process::Command};

pub(super) fn git(root: &Path, arguments: &[&str]) -> Result<Vec<u8>> {
    let result = Command::new("git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "--no-replace-objects",
            "-c",
            "core.hooksPath=/dev/null",
            "-C",
        ])
        .arg(root)
        .args(arguments)
        .output()?;
    require(result.status.success(), "upstream Git observation failed")?;
    require(
        result.stdout.len() <= 64 * 1024 * 1024,
        "upstream observation too large",
    )?;
    Ok(result.stdout)
}
pub(super) fn git_text(root: &Path, arguments: &[&str]) -> Result<String> {
    Ok(String::from_utf8(git(root, arguments)?)?.trim().into())
}
fn blob(checkout: &Path, commit: &str, path: &str) -> Result<Option<Vec<u8>>> {
    let name = format!("codex-rs/{path}");
    let tree = git(checkout, &["ls-tree", "-z", commit, "--", &name])?;
    if tree.is_empty() {
        return Ok(None);
    }
    let line = std::str::from_utf8(&tree)?.trim_end_matches('\0');
    let (metadata, actual) = line.split_once('\t').ok_or("invalid Git tree")?;
    let fields = metadata.split_whitespace().collect::<Vec<_>>();
    require(
        actual == name && fields.len() == 3 && fields[1] == "blob" && fields[0] != "120000",
        "unsupported upstream entry",
    )?;
    let size = git_text(checkout, &["cat-file", "-s", fields[2]])?.parse::<u64>()?;
    require(size <= 16 * 1024 * 1024, "upstream file too large")?;
    Ok(Some(git(checkout, &["cat-file", "blob", fields[2]])?))
}
fn records(package: &Path) -> Result<BTreeMap<String, Value>> {
    let unit = load(&package.join("source-unit.json"))?;
    require(
        unit["schema"] == "ai-platform/runtime-source-unit/v1",
        "source-unit schema differs",
    )?;
    let mut result = BTreeMap::new();
    for record in unit["records"]
        .as_array()
        .ok_or("source-unit records absent")?
    {
        let path = text(record, "path")?;
        let file = safe(&package.join("payload/kernel-rs"), path)?;
        require(
            digest(&file)? == text(record, "proposed_sha256")?,
            "source-unit payload changed",
        )?;
        let mode = if std::fs::metadata(&file)?.permissions().mode() & 0o111 == 0 {
            "100644"
        } else {
            "100755"
        };
        require(
            record["file_mode"] == mode,
            "payload mode differs from source-unit",
        )?;
        require(
            result.insert(path.into(), record.clone()).is_none(),
            "duplicate source path",
        )?;
    }
    require(
        unit["source_file_count"].as_u64() == Some(result.len() as u64),
        "source count differs",
    )?;
    let actual = walk(&package.join("payload/kernel-rs"), &[], true)?
        .into_iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<BTreeSet<_>>();
    require(
        actual == result.keys().cloned().collect::<BTreeSet<_>>(),
        "payload has unlisted source files",
    )?;
    Ok(result)
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Result<Self> {
        let path =
            std::env::temp_dir().join(format!("agentforge-upstream-diff-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
    fn file(&self, name: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.0.join(name);
        std::fs::write(&path, bytes)?;
        Ok(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub(super) fn numstat(before: &[u8], after: &[u8]) -> Result<Value> {
    let scratch = Scratch::new()?;
    let old = scratch.file("before", before)?;
    let new = scratch.file("after", after)?;
    let output = Command::new("git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args([
            "-c",
            "core.attributesFile=/dev/null",
            "diff",
            "--no-index",
            "--no-ext-diff",
            "--no-textconv",
            "--numstat",
            "--",
        ])
        .arg(old)
        .arg(new)
        .output()?;
    require(
        matches!(output.status.code(), Some(0 | 1)),
        "line diff failed",
    )?;
    if before == after {
        return Ok(json!({"added":0,"removed":0,"binary":false}));
    }
    let line = std::str::from_utf8(&output.stdout)?;
    let fields = line.trim_end().split('\t').collect::<Vec<_>>();
    require(fields.len() >= 3, "line diff absent")?;
    if fields[0] == "-" {
        return Ok(json!({"added":null,"removed":null,"binary":true}));
    }
    Ok(
        json!({"added":fields[0].parse::<u64>()?,"removed":fields[1].parse::<u64>()?,"binary":false}),
    )
}
pub(super) fn conflicts(ours: &[u8], base: &[u8], theirs: &[u8]) -> Result<Value> {
    let scratch = Scratch::new()?;
    let ours = scratch.file("ours", ours)?;
    let base = scratch.file("base", base)?;
    let theirs = scratch.file("theirs", theirs)?;
    let output = Command::new("git")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .args(["merge-file", "--diff3", "-p"])
        .arg(ours)
        .arg(base)
        .arg(theirs)
        .output()?;
    let code = output.status.code().ok_or("conflict probe interrupted")?;
    // merge-file reports its actual conflict count, capped at 127. Binary or
    // other unmergeable input is recorded distinctly, never called clean.
    Ok(
        json!({"exit_code":code,"conflict_hunks":if (0..=127).contains(&code){Some(code)}else{None},"unmergeable":code>127,"count_capped":code==127}),
    )
}
pub(super) fn metrics(package: &Path, baseline: &Path, checkout: &Path) -> Result<Value> {
    let unit = load(&package.join("source-unit.json"))?;
    let old_unit = load(&baseline.join("source-unit.json"))?;
    let target = text(&unit, "target")?;
    let old = text(&old_unit, "target")?;
    require(hex(target, 40) && hex(old, 40), "source commits invalid")?;
    let current = records(package)?;
    let previous = records(baseline)?;
    let mut modified = Vec::new();
    let mut additions = 0usize;
    let mut overlaps = Vec::new();
    for (path, record) in &current {
        let proposed = std::fs::read(safe(&package.join("payload/kernel-rs"), path)?)?;
        if let Some(upstream) = blob(checkout, target, path)? {
            require(
                record["target_preimage_sha256"] == hash(&upstream),
                "upstream preimage differs",
            )?;
            let upstream_mode = git_text(
                checkout,
                &["ls-tree", target, "--", &format!("codex-rs/{path}")],
            )?;
            let mode = upstream_mode
                .split_whitespace()
                .next()
                .ok_or("upstream mode absent")?;
            if upstream != proposed || record["file_mode"] != mode {
                modified.push(json!({"path":format!("codex-rs/{path}"),"upstream_sha256":hash(&upstream),"overlay_sha256":hash(&proposed),"mode_changed":record["file_mode"]!=mode,"lines":numstat(&upstream,&proposed)?}));
            }
        } else {
            require(
                record["target_upstream_blob"].is_null(),
                "new source claims upstream blob",
            )?;
            additions += 1;
        }
    }
    // Measure the old platform patch against the new upstream independently of
    // the already repaired candidate. This does not modify either checkout.
    for (path, record) in &previous {
        let ours = std::fs::read(safe(&baseline.join("payload/kernel-rs"), path)?)?;
        let Some(base) = blob(checkout, old, path)? else {
            if let Some(theirs) = blob(checkout, target, path)? {
                if ours != theirs {
                    overlaps.push(json!({"path":format!("codex-rs/{path}"),"upstream_added_path_collision":true,"probe":conflicts(&ours,b"",&theirs)?}));
                }
            }
            continue;
        };
        require(
            record["target_preimage_sha256"] == hash(&base),
            "baseline preimage differs",
        )?;
        if ours == base {
            continue;
        }
        match blob(checkout,target,path)? {
            Some(theirs) if base!=theirs => overlaps.push(json!({"path":format!("codex-rs/{path}"),"probe":conflicts(&ours,&base,&theirs)?})),
            None => overlaps.push(json!({"path":format!("codex-rs/{path}"),"probe":{"deleted_upstream":true,"unmergeable":true}})),
            _=>{}
        }
    }
    let added: u64 = modified
        .iter()
        .filter_map(|v| v["lines"]["added"].as_u64())
        .sum();
    let removed: u64 = modified
        .iter()
        .filter_map(|v| v["lines"]["removed"].as_u64())
        .sum();
    Ok(
        json!({"method":"immutable Git target vs verified existing source-unit payload; git no-index numstat and merge-file diff3","modified_upstream_files":modified.len(),"modified_upstream":modified,"platform_added_files":additions,"text_lines_added":added,"text_lines_removed":removed,"binary_modified_files":modified.iter().filter(|v|v["lines"]["binary"]==true).count(),"overlap_paths":overlaps,"conflicting_paths":overlaps.iter().filter(|v|v["probe"]["unmergeable"]==true||v["probe"]["conflict_hunks"].as_u64().is_some_and(|v|v>0)).count(),"deletions":"source-unit/v1 has no overlay deletion operation; unrepresented deletes are never inferred"}),
    )
}
pub(super) fn schema_files(path: &Path) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for relative in walk(path, &[], false)? {
        let p = relative.to_string_lossy().into_owned();
        // Validate the source-generated JSON instead of diffing arbitrary text.
        let full = safe(path, &p)?;
        strict_json(&std::fs::read(&full)?)?;
        result.insert(p, digest(&full)?);
    }
    require(!result.is_empty(), "empty App Server schema")?;
    Ok(result)
}
pub(super) fn sbom_components(path: &Path) -> Result<BTreeMap<String, Value>> {
    let doc = load(path)?;
    require(doc["bomFormat"] == "CycloneDX", "CycloneDX SBOM required")?;
    let mut result = BTreeMap::new();
    let mut refs = BTreeSet::new();
    for c in doc["components"]
        .as_array()
        .ok_or("SBOM components missing")?
    {
        let key = text(c, "bom-ref")?;
        require(refs.insert(key.to_owned()), "duplicate SBOM ref")?;
        result.insert(format!("component:{key}"), c.clone());
    }
    require(!refs.is_empty(), "empty SBOM")?;
    if let Some(root) = doc["metadata"]["component"]["bom-ref"].as_str() {
        refs.insert(root.into());
    }
    if let Some(dependencies) = doc.get("dependencies") {
        for entry in dependencies
            .as_array()
            .ok_or("SBOM dependency graph invalid")?
        {
            let key = text(entry, "ref")?;
            require(refs.contains(key), "SBOM dependency owner absent")?;
            let edges = entry["dependsOn"]
                .as_array()
                .ok_or("SBOM edges missing")?
                .iter()
                .map(|v| v.as_str().ok_or("SBOM dependency ref invalid"))
                .collect::<std::result::Result<BTreeSet<_>, _>>()?;
            require(
                edges.len() == entry["dependsOn"].as_array().unwrap().len()
                    && edges.iter().all(|r| refs.contains(*r)),
                "duplicate or dangling SBOM dependency",
            )?;
            let mut normalized = entry.clone();
            normalized["dependsOn"] = json!(edges);
            require(
                result
                    .insert(format!("dependency:{key}"), normalized)
                    .is_none(),
                "duplicate SBOM dependency owner",
            )?;
        }
    }
    let mut metadata = doc;
    metadata.as_object_mut().unwrap().remove("components");
    metadata.as_object_mut().unwrap().remove("dependencies");
    result.insert("document".into(), metadata);
    Ok(result)
}
pub(super) fn changed<T: PartialEq + serde::Serialize>(
    before: &BTreeMap<String, T>,
    after: &BTreeMap<String, T>,
) -> Value {
    json!({"added":after.keys().filter(|k|!before.contains_key(*k)).collect::<Vec<_>>(),"removed":before.keys().filter(|k|!after.contains_key(*k)).collect::<Vec<_>>(),"changed":after.iter().filter(|(k,v)|before.get(*k).is_some_and(|old|old!=*v)).map(|(k,_)|k).collect::<Vec<_>>()})
}
