//! Controlled source-batch/v2 producer; eight real Cargo invocations.
//! Original source-unit/v1 producer and its strict 24 commands remain unchanged.
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
type R<T> = Result<T, Box<dyn std::error::Error>>;
include!("/control/steps.rs");
fn q(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c < ' ' => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn arr(v: &[&str]) -> String {
    format!("[{}]", v.iter().map(|s| q(s)).collect::<Vec<_>>().join(","))
}
fn sha(bytes: &[u8]) -> R<String> {
    let mut child = Command::new("sha256sum")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .ok_or("missing hash stdin")?
        .write_all(bytes)?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err("sha256sum failed".into());
    }
    let text = String::from_utf8(out.stdout)?;
    let digest = text.split_whitespace().next().ok_or("hash missing")?;
    if digest.len() != 64 {
        return Err("invalid digest".into());
    }
    Ok(digest.into())
}
fn file_sha(path: impl AsRef<Path>) -> R<String> {
    sha(&fs::read(path)?)
}
fn read(name: &str) -> R<String> {
    Ok(fs::read_to_string(format!("/control/{name}.txt"))?
        .trim()
        .into())
}
fn write(path: impl AsRef<Path>, data: impl AsRef<[u8]>) -> R<()> {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().ok_or("parent missing")?)?;
    fs::write(path, data)?;
    Ok(())
}
fn files(root: &Path) -> R<Vec<PathBuf>> {
    fn walk(base: &Path, p: &Path, out: &mut Vec<PathBuf>) -> R<()> {
        for entry in fs::read_dir(base.join(p))? {
            let e = entry?;
            if e.file_name() == ".git" {
                continue;
            }
            let next = p.join(e.file_name());
            if e.file_type()?.is_dir() {
                walk(base, &next, out)?
            } else {
                out.push(next)
            }
        }
        Ok(())
    }
    let mut out = vec![];
    walk(root, Path::new(""), &mut out)?;
    out.sort();
    Ok(out)
}
fn snapshot(root: &Path) -> R<String> {
    let paths = files(root)?;
    // Hash real regular files in bounded batches rather than launching one
    // process per file. -z preserves exact path bytes without escape rewriting.
    let regular = paths
        .iter()
        .filter(|p| !root.join(p).is_symlink())
        .cloned()
        .collect::<Vec<_>>();
    let mut hashes: BTreeMap<PathBuf, String> = BTreeMap::new();
    for group in regular.chunks(256) {
        let mut command = Command::new("sha256sum");
        command.args(["-z", "--"]);
        for path in group {
            command.arg(root.join(path));
        }
        let output = command.output()?;
        if !output.status.success() {
            return Err("actual source hashing failed".into());
        }
        let rows = output
            .stdout
            .split(|b| *b == 0)
            .filter(|v| !v.is_empty())
            .collect::<Vec<_>>();
        if rows.len() != group.len() {
            return Err("source hash inventory count differs".into());
        }
        for (line, path) in rows.into_iter().zip(group) {
            if line.len() < 66
                || &line[64..66] != b"  "
                || &line[66..] != root.join(path).as_os_str().as_encoded_bytes()
            {
                return Err("source hash filename differs".into());
            }
            let sha = std::str::from_utf8(&line[..64])?;
            if !sha
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
            {
                return Err("source hash malformed".into());
            }
            hashes.insert(path.to_path_buf(), sha.to_owned());
        }
    }
    let mut rows = vec![];
    for path in paths {
        let full = root.join(&path);
        let m = fs::symlink_metadata(&full)?;
        let (mode, digest) = if m.file_type().is_symlink() {
            (
                "120000",
                sha(fs::read_link(&full)?.as_os_str().as_encoded_bytes())?,
            )
        } else {
            (
                if m.permissions().mode() & 0o111 != 0 {
                    "100755"
                } else {
                    "100644"
                },
                hashes.get(&path).ok_or("source hash missing")?.clone(),
            )
        };
        rows.push(format!(
            "{{\"path\":{},\"file_mode\":{},\"sha256\":{}}}",
            q(&path.to_string_lossy()),
            q(mode),
            q(&digest)
        ));
    }
    Ok(format!(
        "{{\"schema\":\"ai-platform/source-export-inventory/v1\",\"records\":[{}]}}\n",
        rows.join(",")
    ))
}
fn copy_tree(from: &Path, to: &Path) -> R<()> {
    for rel in files(from)? {
        let src = from.join(&rel);
        let dst = to.join(rel);
        fs::create_dir_all(dst.parent().ok_or("parent missing")?)?;
        fs::copy(&src, &dst)?;
        fs::set_permissions(&dst, fs::metadata(src)?.permissions())?;
    }
    Ok(())
}
fn run(stem: &str, args: &[&str], binding: &str, outdir: &Path) -> R<()> {
    let stdout = outdir.join(format!("{stem}.stdout"));
    let stderr = outdir.join(format!("{stem}.stderr"));
    let began = std::time::Instant::now();
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_millis();
    let status = Command::new("cargo")
        .args(args)
        .current_dir("/source/codex-rs")
        .stdout(fs::File::create(&stdout)?)
        .stderr(fs::File::create(&stderr)?)
        .status()?;
    let mut command = vec!["cargo"];
    command.extend_from_slice(args);
    let code = status
        .code()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "null".into());
    let stop = if status.code().is_some() {
        "null"
    } else {
        "\"terminated_by_signal\""
    };
    write(
        outdir.join(format!("{stem}.json")),
        format!(
            "{{\"command\":{},\"cwd\":\"/source/codex-rs\",\"exit_code\":{},\"stop_reason\":{},\"stdout_sha256\":{},\"stderr_sha256\":{},\"resource_budget_sha256\":{},\"collector_source_sha256\":{},\"started_at_epoch_ms\":{},\"elapsed_ms\":{},\"source_binding\":{}}}\n",
            arr(&command),
            code,
            stop,
            q(&file_sha(&stdout)?),
            q(&file_sha(&stderr)?),
            q(&file_sha("/control/run-budget.json")?),
            q(&file_sha("/control/collector.rs")?),
            started,
            began.elapsed().as_millis(),
            binding
        ),
    )?;
    println!("{} => {}", command.join(" "), status);
    if !status.success() {
        return Err(format!("actual command failed: {stem}; see preserved stdout/stderr").into());
    }
    Ok(())
}
fn version(program: &str, path: &Path) -> R<()> {
    let out = Command::new(program)
        .args(["+1.95.0", "--version"])
        .current_dir("/source/codex-rs")
        .output()?;
    write(path, out.stdout)?;
    if !out.status.success() {
        return Err("toolchain version command failed".into());
    }
    Ok(())
}
fn schema_bundle(root: &Path) -> R<String> {
    let paths = files(root)?;
    let mut bytes = vec![];
    let mut total = 0;
    for path in &paths {
        let data = fs::read(root.join(path))?;
        bytes.extend_from_slice(path.to_string_lossy().as_bytes());
        bytes.push(0);
        bytes.extend_from_slice(&data);
        bytes.push(0);
        total += data.len();
    }
    if paths.is_empty() {
        return Err("schema bundle empty".into());
    }
    Ok(format!(
        "{{\"sha256\":{},\"file_count\":{},\"total_bytes\":{}}}",
        q(&sha(&bytes)?),
        paths.len(),
        total
    ))
}
fn file_record(root: &Path, relative: &str) -> R<String> {
    Ok(format!(
        "{{\"path\":{},\"sha256\":{}}}",
        q(relative),
        q(&file_sha(root.join(relative))?)
    ))
}
fn collect() -> R<()> {
    let source = Path::new("/source");
    let export = Path::new("/export");
    let package = export.join("source-package");
    let evidence = package.join("evidence");
    fs::create_dir_all(&evidence)?;
    copy_tree(Path::new("/control/source-package"), &package)?;
    fs::copy(
        "/native-validator-linkage.txt",
        evidence.join("native-validator-linkage.txt"),
    )?;
    fs::copy(
        "/control/run-budget.json",
        evidence.join("resource-budget.json"),
    )?;
    fs::copy(
        "/control/collector.rs",
        evidence.join("collector-source.rs"),
    )?;
    fs::copy("/control/steps.rs", evidence.join("collector-steps.rs"))?;
    fs::copy(
        "/control/execution-plan.json",
        evidence.join("prepared-execution-plan.json"),
    )?;
    let before = snapshot(source)?;
    let expected = fs::read_to_string("/control/source-inventory.expected.json")?;
    if before != expected {
        return Err("container source bytes/modes differ from frozen host inventory".into());
    }
    if file_sha("/source/codex-rs/Cargo.lock")? != read("resolved-lock-sha256")? {
        return Err("resolved lock changed".into());
    }
    let target = read("target-commit")?;
    let tree = read("target-tree")?;
    let candidate = read("candidate-sha256")?;
    let lock = read("resolved-lock-sha256")?;
    if file_sha(evidence.join("candidate-source.json"))? != candidate {
        return Err("candidate manifest changed".into());
    }
    let binding = format!(
        "{{\"target_commit\":{},\"target_tree\":{},\"candidate_source_sha256\":{},\"resolved_lock_sha256\":{}}}",
        q(&target),
        q(&tree),
        q(&candidate),
        q(&lock)
    );
    for (stem, args) in EXECUTION_STEPS {
        run(stem, args, &binding, &evidence)?;
    }
    // One receipt per real batched invocation. No legacy per-package receipts.
    fs::copy(
        evidence.join("batch-metadata.stdout"),
        evidence.join("resolved-metadata.json"),
    )?;
    fs::copy(
        "/control/required-service-tests.tsv",
        evidence.join("required-service-tests.tsv"),
    )?;
    let output = Command::new("/control/native-validator/platform-tools")
        .args([
            "source-batch",
            "summarize",
            "--package",
            "/export/source-package",
            "--template",
            "/control/source-unit-template.json",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()?;
    write(
        evidence.join("batch-inventory-validator.stderr"),
        &output.stderr,
    )?;
    if !output.status.success() {
        return Err("actual native batch inventory validator failed; inspect stderr".into());
    }
    write(evidence.join("batch-test-inventory.json"), &output.stdout)?;
    fs::copy(
        "/control/protocol-equivalence.json",
        evidence.join("protocol-equivalence.json"),
    )?;
    fs::copy(
        "/control/native-validator/identity.json",
        evidence.join("native-validator-identity.json"),
    )?;
    let mut binaries = vec![];
    for name in [
        "ai-platform-agent-runtime",
        "ai-platform-history-gate",
        "ai-platform-capability-worker",
        "ai-platform-capability-worker-health",
        "ai-platform-python-supervisor",
        "ai-platform-plugin-packs",
        "ai-platform-python-sandbox",
        "ai-platform-python-sandbox-health",
        "ai-platform-sandbox-pool",
        "ai-platform-sandbox-attestor",
        "ai-platform-sandbox-measurement",
    ] {
        let file = Path::new("/cargo-target/debug").join(name);
        let metadata = fs::symlink_metadata(&file)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || fs::read(&file)?.get(..4) != Some(b"\x7fELF")
        {
            return Err(format!("required real normal ELF absent: {name}").into());
        }
        binaries.push(format!(
            "{{\"name\":{},\"sha256\":{},\"bytes\":{}}}",
            q(name),
            q(&file_sha(&file)?),
            fs::metadata(file)?.len()
        ));
    }
    write(
        evidence.join("built-binaries.json"),
        format!(
            "{{\"schema\":\"agentforge/source-bound-built-binaries/v1\",\"source_binding\":{},\"binaries\":[{}],\"services_started\":false,\"images_built\":false}}\n",
            binding,
            binaries.join(",")
        ),
    )?;
    let mut hashes = vec![];
    for rel in files(&evidence)? {
        hashes.push(format!(
            "{}:{}",
            q(&format!("evidence/{}", rel.to_string_lossy())),
            q(&file_sha(evidence.join(rel))?)
        ))
    }
    let unit = fs::read_to_string("/control/source-unit-template.json")?
        .replace("\"__EVIDENCE__\"", &format!("{{{}}}", hashes.join(",")));
    write(package.join("source-unit.json"), unit)?;
    let unit_sha = file_sha(package.join("source-unit.json"))?;
    let export_binding = format!(
        "{{\"target_commit\":{},\"target_tree\":{},\"source_unit_sha256\":{},\"resolved_lock_sha256\":{},\"source_inventory_sha256\":{}}}",
        q(&target),
        q(&tree),
        q(&unit_sha),
        q(&lock),
        q(&sha(before.as_bytes())?)
    );
    write(export.join("source-inventory-before.json"), &before)?;
    run(
        "metadata-command",
        &[
            "+1.95.0",
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
        ],
        &export_binding,
        export,
    )?;
    fs::copy(
        export.join("metadata-command.stdout"),
        export.join("metadata.json"),
    )?;
    fs::create_dir_all(export.join("schema"))?;
    run(
        "schema-command",
        &[
            "+1.95.0",
            "run",
            "--locked",
            "--offline",
            "-p",
            "codex-app-server-protocol",
            "--example",
            "ai_platform_export_schema",
            "--",
            "/export/schema",
        ],
        &export_binding,
        export,
    )?;
    fs::copy(
        export.join("schema-command.stdout"),
        export.join("schema.stdout"),
    )?;
    version("cargo", &export.join("cargo-version.txt"))?;
    version("rustc", &export.join("rustc-version.txt"))?;
    fs::copy("/source/codex-rs/Cargo.lock", export.join("Cargo.lock"))?;
    let after = snapshot(source)?;
    if after != before {
        return Err("source changed during compiler/export commands".into());
    }
    write(export.join("source-inventory-after.json"), &after)?;
    if file_sha(package.join("source-unit.json"))? != unit_sha {
        return Err("source unit changed during export".into());
    }
    let mappings = [
        ("source_inventory_before", "source-inventory-before.json"),
        ("source_inventory_after", "source-inventory-after.json"),
        ("schema_stdout", "schema.stdout"),
        ("schema_command_receipt", "schema-command.json"),
        ("metadata_command_receipt", "metadata-command.json"),
        ("cargo_metadata", "metadata.json"),
        ("cargo_lock", "Cargo.lock"),
        ("cargo_version", "cargo-version.txt"),
        ("rustc_version", "rustc-version.txt"),
    ];
    let mut filemap = vec![];
    for (name, path) in mappings {
        filemap.push(format!("{}:{}", q(name), file_record(export, path)?));
    }
    let host = read("host-composed")?;
    write(
        export.join("export-evidence.json"),
        format!(
            "{{\"schema\":\"ai-platform/composed-source-export/v3\",\"target_commit\":{},\"source_unit_sha256_before\":{},\"source_unit_sha256_after\":{},\"resolved_lock_sha256\":{},\"schema_bundle\":{},\"execution\":{{\"kind\":\"docker\",\"host_source_root\":{},\"host_cwd\":{},\"container_source_root\":\"/source\",\"container_cwd\":\"/source/codex-rs\",\"container_schema_dir\":\"/export/schema\",\"source_binding\":{}}},\"files\":{{{}}}}}\n",
            q(&target),
            q(&unit_sha),
            q(&unit_sha),
            q(&lock),
            schema_bundle(&export.join("schema"))?,
            q(&host),
            q(&format!("{host}/codex-rs")),
            export_binding,
            filemap.join(",")
        ),
    )?;
    write(
        export.join("run-status.json"),
        format!(
            "{{\"success\":true,\"actual_cargo_invocations\":8,\"inventory_sha256\":{},\"source_unit_sha256\":{}}}\n",
            q(&file_sha(evidence.join("batch-test-inventory.json"))?),
            q(&unit_sha)
        ),
    )?;
    Ok(())
}
fn main() {
    if let Err(error) = collect() {
        let message = error.to_string();
        eprintln!("{message}");
        let _ = write(
            "/export/run-status.json",
            format!("{{\"success\":false,\"error\":{}}}\n", q(&message)),
        );
    }
}
