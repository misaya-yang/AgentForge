//! Dependency/build-input lock for the Rust service images; never builds or publishes.
use ai_platform_release::{Result, digest, load, require};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
const INPUTS: &[&str] = &[
    "rust-toolchain.toml",
    "rust/platform-native/Cargo.toml",
    "rust/platform-native/Cargo.lock",
    "rust/platform-native/rust-toolchain.toml",
    "rust/platform-native/crates/platform-rpc/build.rs",
    "rust/platform-native/crates/platform-rpc/proto/knowledge.proto",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/Cargo.toml",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-sdk-agent-engine/Cargo.lock",
    "deploy/platform-native/Dockerfile",
    "deploy/platform-native/Dockerfile.dockerignore",
    "rust/agent-runtime-overlay/kernel-rs/Cargo.toml",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/Cargo.toml",
];
fn expected(root: &Path) -> Result<Value> {
    let mut files = serde_json::Map::new();
    for p in INPUTS {
        let path = root.join(p);
        require(
            path.is_file() && !path.is_symlink(),
            format!("build input missing or symlinked: {p}"),
        )?;
        files.insert(p.to_string(), digest(&path)?.into());
    }
    // Member manifests determine feature resolution and must be covered alongside the lock.
    for entry in std::fs::read_dir(root.join("rust/platform-native/crates"))? {
        let entry = entry?;
        let path = entry.path().join("Cargo.toml");
        if path.is_file() {
            require(!path.is_symlink(), "symlinked member manifest")?;
            let relative = path
                .strip_prefix(root)?
                .to_string_lossy()
                .replace('\\', "/");
            files.insert(relative, digest(&path)?.into());
        }
    }
    let docker = std::fs::read_to_string(root.join("deploy/platform-native/Dockerfile"))?;
    for text in [
        "FROM rust:1.99.0-bookworm AS builder",
        "cargo build --locked --release",
        "CARGO_BUILD_JOBS=1",
        "CARGO_INCREMENTAL=0",
        "CARGO_PROFILE_RELEASE_DEBUG=0",
        "--bin gateway",
        "--bin platform-knowledge-service",
        "--bin database-authority",
        "--bin sdk-agent-worker",
    ] {
        require(
            docker.contains(text),
            format!("native Docker build contract missing: {text}"),
        )?;
    }
    require(
        !docker.contains("pip install")
            && !docker.contains("uv sync")
            && !docker.contains("python:"),
        "native service image has Python build dependency",
    )?;
    let pin = std::fs::read_to_string(root.join("rust/platform-native/rust-toolchain.toml"))?;
    require(
        pin.contains("channel = \"1.99.0\""),
        "native toolchain mismatch",
    )?;
    Ok(
        json!({"schema_version":"ai-platform/native-image-inputs/v1","toolchain":"1.99.0","files":files,"image_execution_verified":false}),
    )
}
fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut root = std::env::current_dir()?;
    let mut output = None;
    let mut write = false;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--write" {
            write = true;
            i += 1;
            continue;
        }
        let v = args.get(i + 1).ok_or("missing option value")?;
        match args[i].as_str() {
            "--repo-root" => root = v.into(),
            "--lock" => output = Some(PathBuf::from(v)),
            _ => return Err("unknown image-input option".into()),
        }
        i += 2;
    }
    let expected = expected(&root)?;
    let output =
        output.unwrap_or_else(|| root.join("deploy/platform-native/image-inputs.lock.json"));
    if write {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(
            &output,
            format!("{}\n", serde_json::to_string_pretty(&expected)?),
        )?;
    } else {
        require(
            load(&output)? == expected,
            "native image-input lock drift; regenerate explicitly after reviewing build inputs",
        )?;
    }
    println!(
        "{}",
        json!({"result":"pass","lock":output,"lock_written":write,"build_inputs":expected["files"].as_object().map_or(0, |v| v.len()),"image_execution_verified":false})
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Native image-input contract failed: {e}");
        std::process::exit(1)
    }
}
