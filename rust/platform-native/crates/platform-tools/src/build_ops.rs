//! Build wrapper operations: bounded isolated protocol probe and verified CLI materials.
use crate::*;
use std::{
    io::{BufRead, Write},
    os::unix::process::CommandExt,
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
pub fn cli_materials(args: &Args) -> Result<Value> {
    let root = args.root();
    let source = Path::new(args.get("--source")?);
    let output = Path::new(args.get("--output")?);
    let vendor = Path::new(args.get("--vendor")?);
    require(
        output.is_dir() && !output.is_symlink(),
        "package output must be real directory",
    )?;
    let lock = object(&root.join("deploy/agent-runtime-source/lock.json"))?;
    let upstream = args.get("--upstream-sha")?;
    let overlay = args.get("--overlay-sha")?;
    require(
        lock["source"]["upstream_sha"] == upstream && lock["build"]["overlay_sha256"] == overlay,
        "package identities differ from lock",
    )?;
    let arch = args.get("--arch")?;
    require(
        ["x64", "arm64"].contains(&arch),
        "unsupported package architecture",
    )?;
    let material_paths = [
        (
            "LICENSE",
            source.join("LICENSE"),
            &lock["license"]["upstream_license_sha256"],
        ),
        (
            "NOTICE",
            source.join("NOTICE"),
            &lock["license"]["upstream_notice_sha256"],
        ),
        (
            "NOTICE.ai-platform.md",
            safe(
                &root,
                lock["license"]["notice"]
                    .as_str()
                    .ok_or("notice path missing")?,
            )?,
            &lock["license"]["notice_sha256"],
        ),
        (
            "sbom.cdx.json",
            safe(
                &root,
                lock["build"]["sbom"].as_str().ok_or("SBOM path missing")?,
            )?,
            &lock["build"]["sbom_sha256"],
        ),
        (
            "source-receipt.json",
            safe(
                &root,
                lock["build"]["source_receipt"]
                    .as_str()
                    .ok_or("receipt path missing")?,
            )?,
            &lock["build"]["source_receipt_sha256"],
        ),
    ];
    let mut materials = BTreeMap::new();
    let mut verified = vec![];
    for (name, path, expected) in material_paths {
        require(
            path.is_file() && !path.is_symlink(),
            "package material must be real file",
        )?;
        let bytes = std::fs::read(path)?;
        let actual = hash(&bytes);
        require(
            *expected == actual,
            format!("native package material hash mismatch: {name}"),
        )?;
        materials.insert(name, actual);
        verified.push((name, bytes));
    }
    for (name, bytes) in verified {
        std::fs::write(safe(output, name)?, bytes)?;
    }
    let receipt = json!({"schema_version":"ai-gateway-cli/native-artifact/v1","upstream_sha":upstream,"overlay_sha256":overlay,"target_system":"linux","target_arch":arch,"binary":"codex","sha256":digest(&safe(output,"codex")?)?,"materials":materials});
    write_json(&safe(output, "artifact.json")?, &receipt)?;
    std::fs::create_dir_all(vendor)?;
    write_json(
        &safe(vendor, "source.json")?,
        &json!({"schema_version":"ai-gateway-cli/native-source/v1","upstream_sha":upstream,"overlay_sha256":overlay}),
    )?;
    Ok(json!({"result":"pass","artifact":receipt}))
}
pub fn image_probe(args: &Args) -> Result<Value> {
    let mut child = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-i",
            "--network",
            "none",
            args.get("--image")?,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()?;
    let outcome = (|| -> Result<()> {
        let mut stdin = child.stdin.take().ok_or("probe stdin unavailable")?;
        writeln!(
            stdin,
            "{}",
            json!({"method":"initialize","id":0,"params":{"clientInfo":{"name":"ai_platform_probe","title":"AI Platform Probe","version":"0.1.0"}}})
        )?;
        stdin.flush()?;
        let stdout = child.stdout.take().ok_or("probe stdout unavailable")?;
        let stderr = child.stderr.take().ok_or("probe stderr unavailable")?;
        let (tx, rx) = mpsc::sync_channel(16);
        std::thread::spawn(move || {
            let mut reader = std::io::BufReader::new(stdout);
            loop {
                let mut line = vec![];
                let result = reader
                    .by_ref()
                    .take(1024 * 1024 + 1)
                    .read_until(b'\n', &mut line);
                match result {
                    Ok(0) | Err(_) => break,
                    Ok(_) if line.len() > 1024 * 1024 => {
                        let _ = tx.send(Err("oversized probe line".to_owned()));
                        break;
                    }
                    _ => {
                        if tx.send(Ok(line)).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        // Drain stderr without unbounded storage or leaking provider credentials.
        std::thread::spawn(move || {
            let _ = std::io::copy(&mut std::io::BufReader::new(stderr), &mut std::io::sink());
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut initialized = false;
        while Instant::now() < deadline {
            let Ok(frame) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            else {
                break;
            };
            let frame = frame.map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
            let text = std::str::from_utf8(&frame)?;
            if !text.trim_start().starts_with('{') {
                continue;
            }
            let item = strict_json(text.as_bytes())?;
            if item["id"] == 0 && item["result"].is_object() {
                initialized = true;
                break;
            }
        }
        drop(stdin);
        let deadline = Instant::now() + Duration::from_secs(10);
        while child.try_wait()?.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        require(
            initialized,
            "image protocol probe did not return initialize result",
        )?;
        Ok(())
    })();
    resource_lock::terminate(&mut child)?;
    outcome?;
    Ok(json!({"result":"pass","initialize_seen":true,"network":"none"}))
}

/// Deterministic request fixture for the isolated runtime container smoke contract.
/// Values come only from the explicitly named smoke environment variables.
pub fn runtime_smoke_payloads() -> Result<Value> {
    let env = |key: &str| -> Result<String> { Ok(std::env::var(format!("SMOKE_{key}"))?) };
    let canonical = |value: &Value| -> Result<String> { Ok(canonical_sorted(value)) };
    let hash = |value: &Value| -> Result<String> {
        Ok(format!(
            "sha256:{:x}",
            Sha256::digest(canonical(value)?.as_bytes())
        ))
    };
    let tenant = env("TENANT")?;
    let user = env("USER")?;
    let session = env("SESSION")?;
    let mut config = json!({"schema_version":"agent-runtime-platform-config/v1","tenant_id":tenant,"user_id":user,"session_id":session,"agent_spec":{"developerInstructions":"runtime smoke"},"instructions":{"prompt_hash":hash(&json!("runtime smoke"))?,"dynamic_data_after_instructions":true},"tenant_grants":[],"skills":[],"plugins":[],"attachments":[]});
    config["config_hash"] = json!(hash(&config)?);
    let readonly = json!({"schema_version":"agent-readonly-capability/v1","tenant_id":tenant,"capability_revision":1,"items":[],"tools":[],"mcp":[],"deferred":[],"attachment_tools":[],"capability_allowlist":[],"platform_config":config});
    let snapshot = json!({"schema_version":"agent-runtime-snapshot/v1","model":{"id":"qwen3.7-plus","provider_id":"dashscope"},"capability_revision":1,"readonly_capabilities":readonly,"platform_config":config});
    let turn = json!({"runId":env("RUN")?,"snapshotId":env("SNAPSHOT")?,"leaseId":env("LEASE")?,"leaseSignature":env("LEASE_SIGNATURE")?,"message":"hello","model":"qwen3.7-plus","effort":"minimal","capabilityRevision":1,"readonly":readonly,"platformConfig":config});
    let snapshot = canonical(&snapshot)?;
    Ok(
        json!({"_plain":format!("{}\n{}\n{}\n{:x}\n{}",canonical(&config)?,canonical(&readonly)?,snapshot,Sha256::digest(snapshot.as_bytes()),canonical(&turn)?)}),
    )
}

#[cfg(test)]
mod canonical_tests {
    use super::*;
    #[test]
    fn wire_fingerprints_sort_nested_objects_and_preserve_python_floats() {
        assert_eq!(
            canonical_sorted(&json!({"z":{"b":1,"a":"中"},"a":1e-7})),
            "{\"a\":1e-07,\"z\":{\"a\":\"中\",\"b\":1}}"
        );
        assert_eq!(canonical_sorted(&json!(-0.0)), "-0.0");
    }
}
