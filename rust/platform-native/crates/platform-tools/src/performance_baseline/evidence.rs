use super::{Result, ensure, sha};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
pub fn read(path: &Path, private: bool, limit: u64) -> Result<Vec<u8>> {
    ensure(path.is_absolute(), "absolute_input_required")?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path).map_err(|_| "input_unavailable")?;
    let m = file.metadata().map_err(|_| "input_metadata")?;
    ensure(
        m.is_file() && m.len() > 0 && m.len() <= limit,
        "bounded_regular_input_required",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure(
            m.permissions().mode() & 0o022 == 0,
            "input_not_write_protected",
        )?;
        if private {
            ensure(
                m.uid() == unsafe { libc::geteuid() } && m.permissions().mode() & 0o077 == 0,
                "private_operator_owned_input_required",
            )?;
        }
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "input_read_failed")?;
    ensure(bytes.len() as u64 <= limit, "input_budget")?;
    Ok(bytes)
}
pub fn token(path: &Path) -> Result<String> {
    let value = String::from_utf8(read(path, true, 65536)?).map_err(|_| "token_encoding")?;
    let v = value.trim_end_matches(['\r', '\n']);
    ensure(
        (16..=16384).contains(&v.len()) && v.bytes().all(|b| (33..=126).contains(&b)),
        "token_shape",
    )?;
    Ok(v.into())
}
pub fn file_hash(path: &Path) -> Result<String> {
    let mut f = fs::File::open(path).map_err(|_| "identity_file_unavailable")?;
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = f.read(&mut buf).map_err(|_| "identity_read_failed")?;
        if n == 0 {
            break;
        }
        hash.update(&buf[..n]);
    }
    Ok(format!("{:x}", hash.finalize()))
}
pub fn create_output(path: &Path, root: Option<&Path>) -> Result<()> {
    ensure(
        path.is_absolute() && !path.exists(),
        "fresh_external_output_required",
    )?;
    let parent = path
        .parent()
        .ok_or("output_parent")?
        .canonicalize()
        .map_err(|_| "output_parent_unavailable")?;
    if let Some(root) = root {
        ensure(
            !parent.starts_with(
                root.canonicalize()
                    .map_err(|_| "repository_root_unavailable")?,
            ),
            "external_output_required",
        )?;
    }
    let mut d = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        d.mode(0o700);
    }
    d.create(path).map_err(|_| "fresh_output_creation_failed")?;
    Ok(())
}
pub fn save(root: &Path, name: &str, value: &Value) -> Result<()> {
    ensure(
        [
            "report.json",
            "intent.json",
            "observations.json",
            "resources.json",
            "private-state.json",
            "summary.md",
        ]
        .contains(&name),
        "output_name",
    )?;
    let path = root.join(name);
    if let Ok(m) = fs::symlink_metadata(&path) {
        ensure(
            m.is_file() && !m.file_type().is_symlink(),
            "output_identity_changed",
        )?;
    }
    let temporary = root.join(format!(".receipt-{}", uuid::Uuid::new_v4()));
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&temporary).map_err(|_| "receipt_create")?;
    let bytes = if name.ends_with(".md") {
        value.as_str().ok_or("markdown_shape")?.as_bytes().to_vec()
    } else {
        serde_json::to_vec_pretty(value).map_err(|_| "receipt_encode")?
    };
    f.write_all(&bytes)
        .and_then(|_| f.write_all(b"\n"))
        .and_then(|_| f.sync_all())
        .map_err(|_| "receipt_write")?;
    fs::rename(temporary, path).map_err(|_| "receipt_replace")?;
    Ok(())
}
pub fn markdown(report: &Value) -> String {
    format!(
        "# Native performance baseline\n\nStatus: {}. Release acceptance: false.\n\nProfile SHA256: {}\n\nAccepted-build receipt SHA256: {}\n\nSynthetic upstream and real-provider populations are separate. Missing metrics remain unknown. Read report.json for all failures, warmups, denominators and evidence limits. No SLO or production readiness is inferred.\n",
        report["status"].as_str().unwrap_or("UNKNOWN"),
        report["profile_sha256"].as_str().unwrap_or("unavailable"),
        report["accepted_receipt_sha256"]
            .as_str()
            .unwrap_or("unavailable")
    )
}
pub fn relative(root: &Path, rel: &str) -> Result<PathBuf> {
    ensure(
        !rel.is_empty()
            && !rel.starts_with('/')
            && !rel.contains('\\')
            && rel.split('/').all(|p| !matches!(p, "" | "." | "..")),
        "source_relative_path",
    )?;
    let p = root.join(rel);
    ensure(
        p.canonicalize()
            .map_err(|_| "source_missing")?
            .starts_with(root),
        "source_escape",
    )?;
    ensure(
        !fs::symlink_metadata(&p)
            .map_err(|_| "source_metadata")?
            .file_type()
            .is_symlink(),
        "source_symlink",
    )?;
    Ok(p)
}
pub fn checked_json(path: &Path, expected: &str, private: bool) -> Result<Value> {
    let b = read(path, private, 8 * 1024 * 1024)?;
    ensure(sha(&b) == expected, "evidence_digest_mismatch")?;
    serde_json::from_slice(&b).map_err(|_| "evidence_json_invalid".into())
}
