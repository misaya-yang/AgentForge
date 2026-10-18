//! Preserve actual dependency notices and embedded grants.
// SPDX-License-Identifier: Apache-2.0
use super::*;
pub(super) fn notice_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    let extension = upper.rsplit('.').next().unwrap_or("");
    if [
        "RS", "C", "CPP", "H", "JS", "TS", "TSX", "PY", "WASM", "MAP",
    ]
    .contains(&extension)
    {
        return false;
    }
    upper.contains("LICENSE")
        || upper.contains("LICENCE")
        || ["NOTICE", "COPYING", "COPYRIGHT", "OFL", "UFL", "AUTHORS"]
            .iter()
            .any(|prefix| {
                upper == *prefix
                    || upper.starts_with(&format!("{prefix}."))
                    || upper.starts_with(&format!("{prefix}-"))
            })
}
pub(super) fn embedded_license(content: &str) -> bool {
    let upper = content.to_ascii_uppercase();
    upper.contains("COPYRIGHT")
        && ((upper.contains("PERMISSION IS HEREBY GRANTED")
            && upper.contains("SOFTWARE IS PROVIDED"))
            || (upper.contains("REDISTRIBUTION AND USE IN SOURCE AND BINARY FORMS")
                && upper.contains("SOFTWARE IS PROVIDED")))
}
pub(super) fn data_license_header<'a>(name: &str, content: &'a str) -> Option<&'a str> {
    let end = if name.ends_with(".xml") {
        let mut ends = [
            content.find("</comment>").map(|i| i + 10),
            content.find("-->").map(|i| i + 3),
        ]
        .into_iter()
        .flatten();
        ends.next().map(|first| ends.fold(first, usize::min))?
    } else if name.ends_with(".proto") {
        content.find("syntax =")?
    } else {
        return None;
    };
    if end > 8192 {
        return None;
    }
    let header = &content[..end];
    let upper = header.to_ascii_uppercase();
    (upper.contains("COPYRIGHT")
        && (upper.contains("LICENSE") || upper.contains("PERMISSION IS HEREBY GRANTED")))
    .then_some(header)
}
pub(super) fn notice_files(
    root: &Path,
    dir: &Path,
    depth: usize,
    files: &mut BTreeMap<String, Value>,
) -> Result<()> {
    require(depth <= 32, "dependency notice tree exceeds depth limit")?;
    let mut entries = std::fs::read_dir(dir)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let ty = entry.file_type()?;
        let path = entry.path();
        // Never traverse dependency symlinks or nested dependency graphs here.
        if ty.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if ty.is_dir() && !matches!(name.as_str(), "node_modules" | ".git" | "target") {
            notice_files(root, &path, depth + 1, files)?;
        } else if ty.is_file()
            && (notice_name(&name)
                || name.to_ascii_uppercase().starts_with("README")
                || name.ends_with(".xml")
                || name.ends_with(".proto"))
        {
            let data = name.ends_with(".xml") || name.ends_with(".proto");
            let raw = if data && !notice_name(&name) {
                // Test XML can have a different encoding or a very large body.
                // Only the original UTF-8 legal header is relevant here.
                let mut prefix = Vec::new();
                std::fs::File::open(&path)?
                    .take(8192)
                    .read_to_end(&mut prefix)?;
                prefix
            } else {
                require(
                    entry.metadata()?.len() <= 4 * 1024 * 1024,
                    "dependency notice exceeds 4MiB",
                )?;
                std::fs::read(&path)?
            };
            let content = match std::str::from_utf8(&raw) {
                Ok(content) => content,
                Err(error) if data && !notice_name(&name) => {
                    std::str::from_utf8(&raw[..error.valid_up_to()])?
                }
                Err(_) => return Err("dependency notice is not UTF-8; review required".into()),
            };
            let header = if !notice_name(&name) {
                data_license_header(&name, content)
            } else {
                None
            };
            if !notice_name(&name)
                && (if data {
                    header.is_none()
                } else {
                    !embedded_license(content)
                })
            {
                continue;
            }
            let content = header.unwrap_or(content);
            let relative = path
                .strip_prefix(root)?
                .to_str()
                .ok_or("non-Unicode dependency notice path")?
                .replace('\\', "/");
            let key = if header.is_some() {
                format!("{relative}#license-header")
            } else {
                relative
            };
            files.insert(key, json!({"sha256":hash(content.as_bytes()),"text":content,"source_file_sha256":digest(&path)?}));
        }
    }
    Ok(())
}
