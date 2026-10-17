//! Dependency and SPDX admission. Policy intentionally blocks unresolved retirements.
use crate::*;
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema_version: String,
    cargo_deny_version: String,
    blocked_packages: Vec<String>,
    blocked_prefixes: Vec<String>,
    registry_sources: Vec<String>,
    yaml_alias: String,
    yaml_package: String,
    yaml_version: String,
    yaml_members: Vec<String>,
    allowed_spdx_expressions: Vec<String>,
}
fn load(root: &Path) -> Result<(Policy, String)> {
    let path = root.join("deploy/security/dependency-policy.json");
    let p: Policy = serde_json::from_value(strict_json(&std::fs::read(&path)?)?)?;
    require(
        p.schema_version == "agentforge/dependency-policy/v1" && !p.cargo_deny_version.is_empty(),
        "invalid dependency policy",
    )?;
    require(
        p.blocked_packages.iter().any(|n| n == "serde_yaml")
            && p.blocked_packages.iter().any(|n| n == "serde_yml")
            && p.blocked_prefixes.iter().any(|n| n == "rustpython-"),
        "retired parser policy may not be omitted",
    )?;
    require(
        p.yaml_package == "serde_norway"
            && p.yaml_alias == "serde_yaml"
            && p.yaml_version == "0.9.42"
            && p.yaml_members.len() == 7,
        "unreviewed YAML replacement policy",
    )?;
    Ok((p, digest(&path)?))
}
fn metadata(p: &Policy, v: &Value) -> Result<Value> {
    require(
        v["version"] == 1
            && v["resolve"]["nodes"]
                .as_array()
                .is_some_and(|v| !v.is_empty()),
        "complete resolved Cargo metadata is required; no-deps input is refused",
    )?;
    let packages = v["packages"]
        .as_array()
        .ok_or("Cargo metadata packages missing")?;
    let workspace = v["workspace_members"]
        .as_array()
        .ok_or("workspace members missing")?;
    let mut ids = std::collections::BTreeSet::new();
    let mut found = std::collections::BTreeSet::new();
    let mut replacement = false;
    let mut replacement_id = None;
    let mut owner_ids = std::collections::BTreeSet::new();
    for package in packages {
        let name = package["name"].as_str().ok_or("package name missing")?;
        let id = package["id"].as_str().ok_or("package identity missing")?;
        require(ids.insert(id), "duplicate package identity")?;
        require(
            !p.blocked_packages.iter().any(|n| n == name)
                && !p.blocked_prefixes.iter().any(|n| name.starts_with(n)),
            "retired YAML/Python parser remains in resolved dependencies",
        )?;
        if let Some(source) = package.get("source").filter(|v| !v.is_null()) {
            require(
                source
                    .as_str()
                    .is_some_and(|s| p.registry_sources.iter().any(|a| a == s)),
                "unapproved dependency registry or git source",
            )?;
        }
        if name == p.yaml_package {
            require(
                package["version"] == p.yaml_version,
                "unreviewed YAML package version",
            )?;
            require(
                package["source"]
                    .as_str()
                    .is_some_and(|s| p.registry_sources.iter().any(|a| a == s)),
                "YAML replacement must come from the approved registry",
            )?;
            replacement = true;
            replacement_id = Some(id);
        }
        if p.yaml_members.iter().any(|n| n == name) {
            require(
                workspace.iter().any(|v| v == id),
                "YAML owner is not in checked workspace",
            )?;
            let deps = package["dependencies"]
                .as_array()
                .ok_or("package dependencies missing")?;
            let aliases: Vec<_> = deps
                .iter()
                .filter(|d| {
                    d["rename"] == p.yaml_alias
                        || (d["name"] == p.yaml_alias && d["rename"].is_null())
                })
                .collect();
            require(
                aliases.len() == 1
                    && aliases[0]["name"] == p.yaml_package
                    && aliases[0]["rename"] == p.yaml_alias
                    && aliases[0]["req"] == format!("={}", p.yaml_version),
                "all seven owners must resolve the pinned maintained YAML alias",
            )?;
            found.insert(name);
            owner_ids.insert(id);
        }
    }
    require(
        replacement && found.len() == p.yaml_members.len(),
        "maintained YAML package or owning members are absent",
    )?;
    for owner in owner_ids {
        require(
            v["resolve"]["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|node| {
                    node["id"] == owner
                        && node["dependencies"]
                            .as_array()
                            .is_some_and(|deps| deps.iter().any(|id| id.as_str() == replacement_id))
                }),
            "declared YAML alias is not connected to the approved resolved package",
        )?;
    }
    for node in v["resolve"]["nodes"].as_array().unwrap() {
        require(
            node["id"].as_str().is_some_and(|id| ids.contains(id)),
            "unrecognized resolved node",
        )?;
        for dependency in node["dependencies"]
            .as_array()
            .ok_or("resolved dependency IDs missing")?
        {
            require(
                dependency.as_str().is_some_and(|id| ids.contains(id)),
                "unrecognized dependency ID",
            )?;
        }
    }
    Ok(
        json!({"gate":"dependency-policy","result":"pass","packages_checked":packages.len(),"yaml_members_checked":found.len(),"yaml_package":p.yaml_package,"yaml_version":p.yaml_version}),
    )
}
fn sbom(p: &Policy, v: &Value, image: &str) -> Result<Value> {
    let (name, hash) = image
        .split_once("@sha256:")
        .ok_or("SPDX admission requires an immutable image")?;
    require(
        name.starts_with("ghcr.io/")
            && hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid image digest",
    )?;
    require(
        matches!(v["spdxVersion"].as_str(), Some("SPDX-2.2" | "SPDX-2.3"))
            && v["dataLicense"] == "CC0-1.0",
        "unsupported SPDX document",
    )?;
    require(
        v["SPDXID"] == "SPDXRef-DOCUMENT"
            && v["name"].as_str().is_some_and(|s| !s.is_empty())
            && v["documentNamespace"]
                .as_str()
                .is_some_and(|s| s.starts_with("https://") || s.starts_with("http://"))
            && v["creationInfo"]["creators"]
                .as_array()
                .is_some_and(|v| !v.is_empty()),
        "SPDX document identity or creator metadata missing",
    )?;
    let packages = v["packages"].as_array().ok_or("SPDX packages missing")?;
    require(!packages.is_empty(), "empty SPDX package coverage")?;
    let mut ids = std::collections::BTreeSet::new();
    let mut components = 0usize;
    let mut aggregates = 0usize;
    for package in packages {
        require(
            package["name"].as_str().is_some_and(|n| !n.is_empty()),
            "SPDX package name missing",
        )?;
        let id = package["SPDXID"]
            .as_str()
            .ok_or("SPDX package identity missing")?;
        require(ids.insert(id), "duplicate SPDX package identity")?;
        let aggregate = package["primaryPackagePurpose"] == "CONTAINER"
            && package["name"] == image
            && v["documentDescribes"]
                .as_array()
                .is_some_and(|items| items.iter().any(|value| value == id));
        if aggregate {
            aggregates += 1;
            require(aggregates <= 1, "multiple SPDX container aggregates")?;
        } else {
            components += 1;
        }
        let mut expressions = vec![];
        for key in ["licenseDeclared", "licenseConcluded"] {
            if let Some(value) = package.get(key) {
                let expr = value.as_str().ok_or("malformed SPDX license expression")?;
                if expr != "NOASSERTION" && expr != "NONE" {
                    expressions.push(expr);
                }
            }
        }
        if let Some(values) = package.get("licenseInfoFromFiles") {
            for value in values
                .as_array()
                .ok_or("malformed SPDX file-license list")?
            {
                let expr = value.as_str().ok_or("malformed SPDX file license")?;
                if expr != "NOASSERTION" && expr != "NONE" {
                    expressions.push(expr);
                }
            }
        }
        require(
            aggregate || !expressions.is_empty(),
            "package license is unidentified; owner review required",
        )?;
        for expr in expressions {
            let normalized = expr.split_whitespace().collect::<Vec<_>>().join(" ");
            require(
                p.allowed_spdx_expressions.contains(&normalized),
                "SPDX license is outside the reviewed permissive baseline; owner review required",
            )?;
        }
    }
    require(components > 0, "SPDX has no licensed component coverage")?;
    Ok(
        json!({"gate":"image-license-policy","result":"pass","image_ref":image,"spdx_packages_checked":components,"container_aggregates":aggregates,"legal_redistribution_acceptance":"not_established"}),
    )
}
pub fn run(args: &Args) -> Result<Value> {
    let (p, hash) = load(&args.root())?;
    let mut result = match args.positionals.get(1).map(String::as_str) {
        Some("metadata") => metadata(&p, &json_input(args.get("--metadata")?)?)?,
        Some("spdx") => sbom(
            &p,
            &json_input(args.get("--sbom")?)?,
            args.get("--image-ref")?,
        )?,
        _ => return Err("expected dependency-policy metadata or spdx".into()),
    };
    result["policy_sha256"] = hash.into();
    Ok(result)
}
