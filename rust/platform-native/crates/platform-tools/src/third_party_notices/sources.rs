//! Exact-source supplements for licenses omitted from published packages.
// SPDX-License-Identifier: Apache-2.0
use super::*;

pub(super) struct Sources {
    pub entries: BTreeMap<String, Value>,
    pub digest: Option<String>,
}
fn relative(path: &str) -> Result<()> {
    require(
        !path.is_empty()
            && Path::new(path)
                .components()
                .all(|p| matches!(p, std::path::Component::Normal(_)))
            && !path.contains('\\'),
        "license source path must be a relative normal path",
    )
}
fn sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn checked_file(root: &Path, path: &str, expected: &str) -> Result<Vec<u8>> {
    relative(path)?;
    require(sha256(expected), "invalid license source SHA256")?;
    let mut current = root.to_path_buf();
    for part in Path::new(path).components() {
        current.push(part);
        require(
            !std::fs::symlink_metadata(&current)?
                .file_type()
                .is_symlink(),
            "license source symlinks are forbidden",
        )?;
    }
    require(
        current.is_file() && current.metadata()?.len() <= 4 * 1024 * 1024,
        "license source must be a file of at most 4MiB",
    )?;
    let raw = std::fs::read(current)?;
    require(
        hash(&raw) == expected,
        format!("license source hash mismatch: {path}"),
    )?;
    Ok(raw)
}
impl Sources {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("docs/security/third-party-license-sources.json");
        if !path.exists() {
            return Ok(Self {
                entries: BTreeMap::new(),
                digest: None,
            });
        }
        let raw = std::fs::read(path)?;
        require(
            raw.len() <= 8 * 1024 * 1024,
            "license source index exceeds limit",
        )?;
        let data: Value = serde_json::from_slice(&raw)?;
        require(
            data["schema"] == "agentforge/third-party-license-sources/v1",
            "unsupported license source index",
        )?;
        let mut entries = BTreeMap::new();
        for item in data["components"]
            .as_array()
            .ok_or("license source components missing")?
        {
            let id = text(item, "id")?;
            require(
                !entries.contains_key(id),
                "duplicate license source component",
            )?;
            let files = item["files"]
                .as_array()
                .ok_or("license source files missing")?;
            require(
                !files.is_empty(),
                format!("empty license source binding: {id}"),
            )?;
            for file in files {
                let path = text(file, "path")?;
                require(
                    path.starts_with("licenses/third-party/materials/"),
                    "license source outside materials directory",
                )?;
                let raw = checked_file(root, path, text(file, "sha256")?)?;
                require(
                    !raw.is_empty() && std::str::from_utf8(&raw).is_ok(),
                    "license source is empty or not UTF-8",
                )?;
                let repository = text(file, "repository")?;
                let repo = repository
                    .strip_prefix("https://github.com/")
                    .ok_or("license source requires official HTTPS repository")?;
                require(
                    repo.split('/').count() == 2
                        && repo.split('/').all(|p| {
                            !p.is_empty()
                                && p.bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
                        }),
                    "invalid source repository",
                )?;
                let revision = text(file, "revision")?;
                require(
                    revision.len() == 40
                        && revision
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                    "license source revision must be an immutable commit",
                )?;
                let upstream_path = text(file, "upstream_path")?;
                relative(upstream_path)?;
                require(
                    file["upstream_url"]
                        == format!(
                            "https://raw.githubusercontent.com/{repo}/{revision}/{upstream_path}"
                        ),
                    "license source URL disagrees with revision/path",
                )?;
                require(
                    matches!(
                        text(file, "binding")?,
                        "exact-cargo-vcs-revision-ancestor-license"
                            | "exact-npm-registry-gitHead-ancestor-license"
                            | "exact-published-source-match"
                            | "bundled-toolchain-license"
                            | "referenced-standard-license"
                    ),
                    "unsupported license source binding",
                )?;
                if file["binding"] == "referenced-standard-license" {
                    let standard = text(file, "standard_license")?;
                    require(
                        matches!(standard, "MPL-2.0" | "CC0-1.0")
                            && item["declared_license"] == standard
                            && repository == "https://github.com/spdx/license-list-data"
                            && upstream_path == format!("text/{standard}.txt"),
                        "standard text must match an explicit supported source grant; no copyright templates",
                    )?;
                    require(
                        item["standard_grant"]["package_path"].is_string()
                            && item["standard_grant"]["sha256"]
                                .as_str()
                                .is_some_and(sha256),
                        "standard terms require original source grant proof",
                    )?;
                }
                if file["binding"] == "exact-published-source-match" {
                    let proofs = item["source_matches"]
                        .as_array()
                        .ok_or("exact published-source binding requires source proofs")?;
                    require(
                        proofs.iter().any(|proof| {
                            proof["repository"] == repository
                                && proof["revision"] == revision
                                && proof["sha256"].as_str().is_some_and(sha256)
                                && proof["package_path"].as_str().is_some_and(|path| {
                                    item["package_files"][path] == proof["sha256"]
                                })
                        }),
                        "license revision is not bound to a published-source proof",
                    )?;
                }
                if file["binding"] == "exact-cargo-vcs-revision-ancestor-license" {
                    require(
                        item["vcs"]["git"]["sha1"] == revision,
                        "Cargo license revision disagrees with package VCS",
                    )?;
                }
                if file["binding"] == "exact-npm-registry-gitHead-ancestor-license" {
                    require(
                        item["npm_registry"]["gitHead"] == revision,
                        "npm license revision disagrees with registry gitHead",
                    )?;
                }
            }
            entries.insert(id.to_owned(), item.clone());
        }
        Ok(Self {
            entries,
            digest: Some(hash(&raw)),
        })
    }
    pub fn supplement(
        &self,
        root: &Path,
        id: &str,
        name: &str,
        version: &str,
        license: &Value,
        manifest: &Path,
        files: &mut BTreeMap<String, Value>,
    ) -> Result<()> {
        let Some(item) = self.entries.get(id) else {
            return Ok(());
        };
        require(
            item["name"] == name
                && item["version"] == version
                && &item["declared_license"] == license,
            format!("license source identity/declaration mismatch: {id}"),
        )?;
        let actual = hash(&std::fs::read(manifest)?);
        let expected = &item["package_manifest_sha256"];
        require(
            expected.as_str() == Some(actual.as_str())
                || expected
                    .as_array()
                    .is_some_and(|xs| xs.iter().any(|x| x.as_str() == Some(actual.as_str()))),
            format!("license source package manifest mismatch: {id}"),
        )?;
        if let Some(vcs) = item.get("vcs").filter(|v| v["git"]["sha1"].is_string()) {
            require(
                object(
                    &manifest
                        .parent()
                        .ok_or("manifest parent missing")?
                        .join(".cargo_vcs_info.json"),
                )? == *vcs,
                format!("license source package VCS mismatch: {id}"),
            )?;
        }
        if let Some(proofs) = item["package_files"].as_object() {
            for (path, expected) in proofs {
                checked_file(
                    manifest.parent().ok_or("manifest parent missing")?,
                    path,
                    expected.as_str().ok_or("source proof hash missing")?,
                )?;
            }
        }
        if let Some(grant) = item.get("standard_grant") {
            let raw = checked_file(
                manifest.parent().ok_or("manifest parent missing")?,
                text(grant, "package_path")?,
                text(grant, "sha256")?,
            )?;
            let source = std::str::from_utf8(&raw)?;
            let valid = match license.as_str() {
                Some("MPL-2.0") => {
                    source.contains("mozilla.org/MPL/2.0/")
                        && source.contains("This Source Code Form is subject to the terms")
                }
                Some("CC0-1.0") => source
                    .lines()
                    .any(|line| line.trim() == "license = \"CC0-1.0\""),
                _ => false,
            };
            require(
                valid,
                "standard license grant is absent from original published source",
            )?;
            files.insert(format!("published-grant/{}", text(grant, "package_path")?), json!({"sha256":hash(&raw),"text":source,"source_file_sha256":hash(&raw),"kind":"original-published-standard-license-grant"}));
        }
        for file in item["files"].as_array().ok_or("source files missing")? {
            let raw = checked_file(root, text(file, "path")?, text(file, "sha256")?)?;
            let key = format!(
                "upstream/{}:{}",
                text(file, "revision")?,
                text(file, "upstream_path")?
            );
            let value = json!({"sha256":file["sha256"],"text":std::str::from_utf8(&raw)?,"provenance":file});
            if let Some(old) = files.insert(key, value.clone()) {
                require(old == value, "conflicting upstream license supplement")?;
            }
        }
        Ok(())
    }
}
