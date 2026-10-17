//! Container path mapping is admitted only after the entire source tree is bound.
use super::*;

pub(super) struct ExportPaths {
    pub cwd: PathBuf,
    pub schema_output: PathBuf,
    docker: bool,
    source: PathBuf,
}

fn snapshot(root: &Path) -> Result<Value> {
    let mut records = Vec::new();
    for relative in walk(root, &[".git"], true)? {
        let path = root.join(&relative);
        let metadata = std::fs::symlink_metadata(&path)?;
        let (mode, bytes) = if metadata.file_type().is_symlink() {
            // Hash the link itself, never read through a source-controlled link.
            let parent = relative.parent().unwrap_or(Path::new(""));
            if !parent.as_os_str().is_empty() {
                safe(root, &parent.to_string_lossy())?;
            }
            (
                "120000",
                std::fs::read_link(&path)?
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
            )
        } else {
            let checked = safe(root, &relative.to_string_lossy())?;
            (
                if metadata.permissions().mode() & 0o111 == 0 {
                    "100644"
                } else {
                    "100755"
                },
                std::fs::read(checked)?,
            )
        };
        records.push(
            json!({"path":relative.to_string_lossy(),"file_mode":mode,"sha256":hash(&bytes)}),
        );
    }
    Ok(json!({"schema":"ai-platform/source-export-inventory/v1","records":records}))
}

pub(super) fn paths(
    evidence: &Value,
    unit: &Value,
    unit_sha: &str,
    source: &Path,
    schema: &Path,
    exported: &impl Fn(&str) -> Result<PathBuf>,
    runs: &[&Value],
) -> Result<ExportPaths> {
    let cwd = source.join("codex-rs");
    if evidence["schema"] == "ai-platform/composed-source-export/v1" {
        require(
            evidence.get("execution").is_none(),
            "v1 cannot declare a path alias",
        )?;
        return Ok(ExportPaths {
            cwd,
            schema_output: schema.into(),
            docker: false,
            source: source.into(),
        });
    }
    require(
        evidence["schema"] == "ai-platform/composed-source-export/v2",
        "unsupported export evidence",
    )?;
    let execution = &evidence["execution"];
    require(
        execution["kind"] == "docker"
            && execution["container_source_root"] == "/source"
            && execution["container_cwd"] == "/source/codex-rs"
            && execution["container_schema_dir"] == "/export/schema"
            && execution["host_source_root"] == source.to_string_lossy().as_ref()
            && execution["host_cwd"] == cwd.to_string_lossy().as_ref(),
        "Docker source mapping must be explicit and exact",
    )?;
    let before = exported("source_inventory_before")?;
    let after = exported("source_inventory_after")?;
    let expected = snapshot(source)?;
    let inventory_sha = digest(&before)?;
    require(
        object(&before)? == expected
            && object(&after)? == expected
            && digest(&after)? == inventory_sha,
        "Docker source inventories differ from immutable composed source",
    )?;
    let binding = json!({"target_commit":unit["target"],"target_tree":unit["target_tree"],
        "source_unit_sha256":unit_sha,"resolved_lock_sha256":unit["resolved_lock_sha256"],
        "source_inventory_sha256":inventory_sha});
    require(
        execution["source_binding"] == binding,
        "Docker export source binding differs",
    )?;
    for run in runs {
        require(
            run["source_binding"] == binding && run["command"][0] == "cargo",
            "Docker command source binding or executable differs",
        )?;
    }
    require(
        runs.first().ok_or("schema command missing")?["stdout_sha256"]
            == digest(&exported("schema_stdout")?)?,
        "schema command stdout not hash bound",
    )?;
    Ok(ExportPaths {
        cwd: "/source/codex-rs".into(),
        schema_output: "/export/schema".into(),
        docker: true,
        source: source.into(),
    })
}

impl ExportPaths {
    pub fn validate_runs(&self, runs: &[&Value]) -> Result<()> {
        for run in runs {
            require(
                run["exit_code"] == 0
                    && run["stop_reason"].is_null()
                    && run["command"].is_array()
                    && run["cwd"] == self.cwd.to_string_lossy().as_ref(),
                "export command not successful or wrong cwd",
            )?;
        }
        Ok(())
    }
    pub fn workspace_matches(&self, raw: &str) -> Result<bool> {
        if self.docker {
            Ok(raw == "/source/codex-rs")
        } else {
            Ok(std::fs::canonicalize(raw)? == self.cwd)
        }
    }
    pub fn manifest(&self, raw: &str) -> Result<PathBuf> {
        let path = if self.docker {
            let relative = raw
                .strip_prefix("/source/")
                .ok_or("metadata manifest outside Docker source")?;
            safe(&self.source, relative)?
        } else {
            std::fs::canonicalize(raw)?
        };
        require(
            path.starts_with(&self.source) && path.is_file(),
            "metadata path package outside source",
        )?;
        if self.docker {
            require(
                path.file_name().is_some_and(|name| name == "Cargo.toml"),
                "metadata manifest must be Cargo.toml",
            )?;
        }
        Ok(path)
    }
}

#[cfg(test)]
#[path = "source_export_tests.rs"]
mod tests;
