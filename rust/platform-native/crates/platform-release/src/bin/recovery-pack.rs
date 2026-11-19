//! Stable, source-only native delivery snapshot. Refuses any remaining Python source.
use ai_platform_release::{Result, compatibility::canonical_hash, require};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
const DENY_DIR: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".cache",
    ".next",
    "logs",
    "tmp",
    "temp",
    ".aws",
    ".ssh",
    ".codex",
    ".rust-cargo",
    ".rustup",
    "uploads",
    "user-data",
    "test-results",
    "playwright-report",
];
const DENY_EXT: &[&str] = &[
    "pyc", "pyo", "log", "tmp", "bak", "swp", "sqlite", "sqlite3", "db", "pem", "p12", "pfx",
    "key", "jks", "zip", "tar", "gz", "xz", "so", "dylib", "dll", "exe", "o", "rlib", "rmeta",
    "whl",
];
fn valid(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('/')
        && !s.contains(['\\', ':', '\0'])
        && s.split('/').all(|p| !p.is_empty() && p != "." && p != "..")
}
fn allowed(p: &Path) -> bool {
    let n = p
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    if n.starts_with(".env")
        && ![".env.example", ".env.sample", ".env.template"].contains(&n.as_str())
    {
        return false;
    }
    if [
        ".npmrc",
        ".netrc",
        "credentials",
        "credentials.json",
        "auth.json",
        "secrets.json",
        "secrets.yaml",
        "secrets.yml",
        "id_rsa",
        "id_ed25519",
    ]
    .contains(&n.as_str())
    {
        return false;
    }
    let ext = p
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    !DENY_EXT.contains(&ext.as_str())
}
fn scan(root: &Path) -> Result<BTreeMap<String, PathBuf>> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let p = entry.path();
            let ty = entry.file_type()?;
            if ty.is_symlink() {
                continue;
            }
            if ty.is_dir() {
                if !DENY_DIR.contains(&entry.file_name().to_string_lossy().to_lowercase().as_str())
                {
                    stack.push(p);
                }
                continue;
            }
            if !ty.is_file() || !allowed(&p) {
                continue;
            }
            require(
                p.extension().is_none_or(|s| {
                    !["py", "pyw", "pyi"]
                        .contains(&s.to_string_lossy().to_ascii_lowercase().as_str())
                }),
                format!(
                    "Unmigrated Python remains: {}",
                    p.strip_prefix(root)?.display()
                ),
            )?;
            let name = format!(
                "AI--Platfform/{}",
                p.strip_prefix(root)?.to_string_lossy().replace('\\', "/")
            );
            require(valid(&name), "unsafe archive name")?;
            out.insert(name, p);
        }
    }
    Ok(out)
}
fn read(p: &Path) -> Result<(Vec<u8>, Value)> {
    let mut o = OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut f = o.open(p)?;
    let before = f.metadata()?;
    require(
        before.is_file() && before.len() <= 256 * 1024 * 1024,
        "invalid/oversize source member",
    )?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes)?;
    let after = f.metadata()?;
    require(
        before.len() == after.len()
            && before.modified()? == after.modified()?
            && bytes.len() as u64 == after.len(),
        "source changed during read",
    )?;
    require(
        !bytes.starts_with(b"\x7fELF") && !bytes.starts_with(b"MZ"),
        "compiled binary in source snapshot",
    )?;
    let secret = regex::bytes::Regex::new(
        r"-----BEGIN (?:RSA |EC |OPENSSH |DSA )?PRIVATE KEY-----|\bAKIA[0-9A-Z]{16}\b|\bgh[pousr]_[A-Za-z0-9]{30,}|\bsk-(?:proj-)?[A-Za-z0-9_-]{40,}",
    )?;
    for m in secret.find_iter(&bytes) {
        let hash = format!("{:x}", Sha256::digest(m.as_bytes()));
        require(
            [
                "950fbc84c4d5b6ce1b9dc0b6ffc5639d1ffae4d183d2c777f0596f4f9b396bb7",
                "41e78e33993b18daa9e2700b30a026e32e1ed52b98ffaba4fd9071c0e763917b",
            ]
            .contains(&hash.as_str()),
            "Potential embedded credential; snapshot stopped without printing value",
        )?;
    }
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        after.permissions().mode() & 0o777
    };
    #[cfg(not(unix))]
    let mode = 0o644u32;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let size = bytes.len();
    Ok((bytes, json!({"sha256":hash,"size":size,"mode":mode})))
}
fn verify(path: &Path, files: &BTreeMap<String, Value>) -> Result<()> {
    let mut zip = zip::ZipArchive::new(File::open(path)?)?;
    require(zip.len() == files.len() + 1, "ZIP membership mismatch")?;
    for (name, record) in files {
        let mut f = zip.by_name(name)?;
        require(
            f.size() == record["size"].as_u64().unwrap(),
            "ZIP size mismatch",
        )?;
        let mut bytes = vec![];
        f.read_to_end(&mut bytes)?;
        require(
            format!("{:x}", Sha256::digest(&bytes)) == record["sha256"].as_str().unwrap(),
            "ZIP hash mismatch",
        )?;
    }
    let mut manifest = String::new();
    zip.by_name("RECOVERY_MANIFEST.json")?
        .read_to_string(&mut manifest)?;
    let v: Value = serde_json::from_str(&manifest)?;
    require(
        v["files"] == serde_json::to_value(files)?,
        "ZIP manifest mismatch",
    )
}
fn run() -> Result<()> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    require(
        a.len() == 4 && a[0] == "--repo-root" && a[2] == "--output",
        "usage: recovery-pack --repo-root ROOT --output NEW.zip",
    )?;
    let root = Path::new(&a[1]).canonicalize()?;
    let output = PathBuf::from(&a[3]);
    let parent = output.parent().unwrap_or(Path::new(".")).canonicalize()?;
    require(
        !parent.starts_with(&root),
        "archive output must be external",
    )?;
    #[cfg(unix)]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let path = CString::new(parent.as_os_str().as_bytes())?;
        let mut st = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        require(
            unsafe { libc::statvfs(path.as_ptr(), st.as_mut_ptr()) } == 0,
            "disk check failed",
        )?;
        let st = unsafe { st.assume_init() };
        require(
            (st.f_bavail as u128) * (st.f_frsize as u128) >= 3 * 1024 * 1024 * 1024,
            "disk protection: at least3GiB free required",
        )?;
    }
    for p in ["AGENTS.md", "Cargo.lock", "README.md"] {
        let p = if p == "Cargo.lock" {
            "rust/platform-native/Cargo.lock"
        } else {
            p
        };
        require(
            root.join(p).is_file(),
            format!("required recovery file missing: {p}"),
        )?;
    }
    let before = scan(&root)?;
    let mut contents = BTreeMap::new();
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for (n, p) in &before {
        let (bytes, record) = read(p)?;
        total += bytes.len() as u64;
        require(total <= 1024 * 1024 * 1024, "expanded snapshot exceeds1GiB")?;
        contents.insert(n.clone(), bytes);
        files.insert(n.clone(), record);
    }
    let restore=b"Restore AI--Platfform/ to a new directory. Read AGENTS.md and docs/migration/full-rust-status.md first. Install the pinned Rust toolchain, set external CARGO_TARGET_DIR and CARGO_BUILD_JOBS=1, disable debug/incremental, and run cargo check --locked using rust/platform-native/Cargo.toml. Build platform-tools, then run platform-tools recovery-archive verify --archive this.zip --require-zero-python. Secrets, user data, Git remote credentials and build outputs are intentionally excluded. This snapshot is source delivery, not evidence of production release acceptance.\n".to_vec();
    files.insert(
        "RESTORE.md".into(),
        json!({"sha256":format!("{:x}",Sha256::digest(&restore)),"size":restore.len(),"mode":420}),
    );
    contents.insert("RESTORE.md".into(), restore);
    let main: BTreeMap<_, _> = files
        .iter()
        .filter(|(n, _)| n.starts_with("AI--Platfform/"))
        .map(|(n, v)| (n.clone(), v.clone()))
        .collect();
    let manifest = json!({"schema_version":"ai-platform/recovery-manifest/v2","candidate_is_release":false,"zero_python_source":true,"source_identity":{"kind":"sha256_file_inventory","current_commit_verified":false,"main_file_count":main.len(),"main_file_inventory_sha256":canonical_hash(&serde_json::to_value(main)?),"inventory_encoding":"UTF-8 JSON, sorted keys, compact separators; archive-relative path to sha256/size/mode records"},"files":files});
    require(
        scan(&root)? == before,
        "source inventory changed during snapshot",
    )?;
    for (n, p) in &before {
        require(
            read(p)?.1 == files[n],
            "source content changed during snapshot",
        )?;
    }
    let f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    let mut zip = zip::ZipWriter::new(f);
    for (n, b) in &contents {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(files[n]["mode"].as_u64().unwrap_or(420) as u32);
        zip.start_file(n, options)?;
        zip.write_all(b)?;
    }
    zip.start_file(
        "RECOVERY_MANIFEST.json",
        zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o644),
    )?;
    zip.write_all(serde_json::to_string_pretty(&manifest)?.as_bytes())?;
    let f = zip.finish()?;
    f.sync_all()?;
    require(
        f.metadata()?.len() <= 122 * 1024 * 1024,
        "ZIP exceeds122MiB ceiling",
    )?;
    verify(&output, &files)?;
    require(
        scan(&root)? == before,
        "source inventory changed after packing",
    )?;
    for (n, p) in &before {
        require(read(p)?.1 == files[n], "source changed after packing")?;
    }
    println!(
        "{}",
        json!({"result":"pass","archive":output,"archive_sha256":ai_platform_release::digest(&output)?,"file_count":files.len(),"source_identity":manifest["source_identity"],"uploaded":false})
    );
    Ok(())
}
fn main() {
    if let Err(e) = run() {
        eprintln!("Recovery pack failed: {e}");
        std::process::exit(1)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn filters() {
        assert!(!allowed(Path::new(".env")));
        assert!(!allowed(Path::new("secrets.json")));
        assert!(allowed(Path::new(".env.example")));
        assert!(allowed(Path::new("Cargo.lock")));
        assert!(!valid("../escape"));
        assert!(valid("AI--Platfform/src/main.rs"));
    }
}
