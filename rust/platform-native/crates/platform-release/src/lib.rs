pub mod capacity;
pub mod compatibility;
pub mod evidence;
pub mod integration;
pub mod version;
use serde_json::Value;
use std::path::{Path, PathBuf};
pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
pub fn require(condition: bool, message: impl Into<String>) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(message.into().into())
    }
}
pub fn load(path: &Path) -> Result<Value> {
    let v: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    require(v.is_object(), "expected JSON object")?;
    Ok(v)
}
pub fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    let s = v[key]
        .as_str()
        .ok_or_else(|| format!("missing text {key}"))?;
    require(!s.trim().is_empty(), format!("empty {key}"))?;
    Ok(s)
}
pub fn array<'a>(v: &'a Value, key: &str) -> Result<&'a Vec<Value>> {
    v[key]
        .as_array()
        .ok_or_else(|| format!("missing list {key}").into())
}
pub fn safe(root: &Path, raw: &str) -> Result<PathBuf> {
    require(
        !raw.is_empty()
            && !raw.contains('\\')
            && !raw.starts_with('/')
            && raw
                .split('/')
                .all(|p| !p.is_empty() && p != ".." && p != "."),
        "invalid repository POSIX path",
    )?;
    Ok(root.join(raw))
}
pub fn hex(s: &str, n: usize) -> bool {
    s.len() == n
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn digest(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!("{:x}", Sha256::digest(std::fs::read(path)?)))
}
