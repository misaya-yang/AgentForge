//! Validate the owner-approved Apache declaration; imported terms stay independent.
// SPDX-License-Identifier: Apache-2.0
use super::*;
pub(super) fn check(root: &Path) -> Result<Value> {
    let license = std::fs::read(root.join("LICENSE"))?;
    require(
        hash(&license) == "8173d5c29b4f956d532781d2b86e4e30f83e6b7878dce18c919451d6ba707c90",
        "root LICENSE must retain the unmodified owner-selected Apache-2.0 text",
    )?;
    let notice = std::fs::read_to_string(root.join("NOTICE"))?;
    for reference in [
        "Apache License, Version 2.0",
        "deploy/agent-runtime-source/NOTICE.md",
        "agent-plugins/community-doublecheck/THIRD_PARTY_NOTICES.md",
        "agent-plugins/community-engineering-reviewers/THIRD_PARTY_NOTICES.md",
    ] {
        require(
            notice.contains(reference),
            format!("root NOTICE attribution missing: {reference}"),
        )?;
    }
    let mut declarations = BTreeMap::new();
    for path in [
        "web/package.json",
        "sdk/cli/package.json",
        "apps/external-a2a-agent/package.json",
    ] {
        let package = object(&root.join(path))?;
        require(
            package["license"] == "Apache-2.0",
            format!("owned package license drift: {path}"),
        )?;
        declarations.insert(path.to_owned(), hash(&std::fs::read(root.join(path))?));
    }
    for path in [
        "sdk/cli/package-lock.json",
        "apps/external-a2a-agent/package-lock.json",
    ] {
        let lock = object(&root.join(path))?;
        require(
            lock["packages"][""]["license"] == "Apache-2.0",
            format!("owned npm lock root license drift: {path}"),
        )?;
        declarations.insert(path.to_owned(), hash(&std::fs::read(root.join(path))?));
    }
    let native: toml::Value = toml::from_str(&std::fs::read_to_string(
        root.join("rust/platform-native/Cargo.toml"),
    )?)?;
    require(
        native
            .get("workspace")
            .and_then(|v| v.get("package"))
            .and_then(|v| v.get("license"))
            .and_then(toml::Value::as_str)
            == Some("Apache-2.0"),
        "native workspace license drift",
    )?;
    for entry in std::fs::read_dir(root.join("rust/platform-native/crates"))? {
        let path = entry?.path().join("Cargo.toml");
        if !path.is_file() {
            continue;
        }
        let package: toml::Value = toml::from_str(&std::fs::read_to_string(&path)?)?;
        let license = package
            .get("package")
            .and_then(|v| v.get("license"))
            .ok_or("owned crate license missing")?;
        require(
            license.as_str() == Some("Apache-2.0")
                || license.get("workspace").and_then(toml::Value::as_bool) == Some(true),
            format!("owned native crate license drift: {}", path.display()),
        )?;
    }
    Ok(
        json!({"license":"Apache-2.0","license_sha256":hash(&license),"notice_sha256":hash(notice.as_bytes()),"declaration_inputs":declarations,"third_party_terms":"retained; not relicensed or policy-approved"}),
    )
}
