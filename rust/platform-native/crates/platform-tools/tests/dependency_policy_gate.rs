//! Real dependency/license CLI against controlled fixtures. No Cargo/registry/Docker calls.
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
const IMAGE: &str = "ghcr.io/misaya-yang/ai-gateway@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("dependency-policy-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(p.join("deploy/security")).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        fs::copy(
            root.join("deploy/security/dependency-policy.json"),
            p.join("deploy/security/dependency-policy.json"),
        )
        .unwrap();
        Self(p)
    }
    fn metadata(&self) -> Value {
        let p: Value = serde_json::from_slice(
            &fs::read(self.0.join("deploy/security/dependency-policy.json")).unwrap(),
        )
        .unwrap();
        let mut packages = vec![];
        let mut members = vec![];
        let mut nodes = vec![];
        for name in p["yaml_members"].as_array().unwrap() {
            let id = format!("member:{}", name.as_str().unwrap());
            members.push(id.clone());
            packages.push(json!({"name":name,"id":id,"version":"0.1.0","source":null,"dependencies":[{"name":"serde_yaml_ng","rename":"serde_yaml","req":"=0.10.0"}]}));
            nodes.push(json!({"id":id,"dependencies":["yaml"]}));
        }
        packages.push(json!({"name":"serde_yaml_ng","id":"yaml","version":"0.10.0","source":"registry+https://github.com/rust-lang/crates.io-index","dependencies":[]}));
        nodes.push(json!({"id":"yaml","dependencies":[]}));
        json!({"version":1,"workspace_members":members,"packages":packages,"resolve":{"nodes":nodes}})
    }
    fn check(&self, mode: &str, input: &Value, pass: bool) {
        let path = self.0.join("input.json");
        fs::write(&path, serde_json::to_vec(input).unwrap()).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_platform-tools"));
        command
            .args(["dependency-policy", mode, "--repo-root"])
            .arg(&self.0)
            .args([if mode == "metadata" {
                "--metadata"
            } else {
                "--sbom"
            }])
            .arg(path);
        if mode == "spdx" {
            command.args(["--image-ref", IMAGE]);
        }
        let out = command.output().unwrap();
        assert_eq!(
            out.status.success(),
            pass,
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!String::from_utf8_lossy(&out.stderr).contains("private-canary"));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn sbom(license: Value) -> Value {
    json!({"spdxVersion":"SPDX-2.3","dataLicense":"CC0-1.0","SPDXID":"SPDXRef-DOCUMENT","name":IMAGE,"documentNamespace":"https://example.test/spdx/fixture","creationInfo":{"creators":["Tool: fixture"]},"packages":[{"SPDXID":"SPDXRef-one","name":"fixture","licenseDeclared":license}]})
}
#[test]
fn maintained_alias_complete_resolved_graph_passes() {
    let f = Fixture::new();
    f.check("metadata", &f.metadata(), true);
}
#[test]
fn retired_parsers_any_future_rustpython_and_unknown_sources_fail() {
    let f = Fixture::new();
    for name in [
        "serde_yaml",
        "serde_yml",
        "rustpython-parser",
        "rustpython-future-unlisted-crate",
    ] {
        let mut v = f.metadata();
        v["packages"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name":name,"id":"retired","source":null}));
        f.check("metadata", &v, false);
    }
    let mut v = f.metadata();
    v["packages"][7]["source"] = json!("git+https://private-canary@example.test/repo");
    f.check("metadata", &v, false);
}
#[test]
fn no_deps_omitted_member_unpinned_alias_and_inconsistent_resolve_fail() {
    let f = Fixture::new();
    let mut v = f.metadata();
    v["resolve"] = Value::Null;
    f.check("metadata", &v, false);
    let mut v = f.metadata();
    v["packages"].as_array_mut().unwrap().remove(0);
    f.check("metadata", &v, false);
    let mut v = f.metadata();
    v["packages"][0]["dependencies"][0]["req"] = json!("^0.10");
    f.check("metadata", &v, false);
    let mut v = f.metadata();
    v["resolve"]["nodes"][0]["dependencies"] = json!(["missing"]);
    f.check("metadata", &v, false);
}
#[test]
fn permissive_spdx_passes_but_gpl_unknown_and_hidden_licenses_do_not() {
    let f = Fixture::new();
    f.check("spdx", &sbom(json!("MIT")), true);
    for license in [
        "GPL-2.0-or-later",
        "GPL-3.0-only",
        "AGPL-3.0-only",
        "NOASSERTION",
        "NONE",
        "LicenseRef-unknown",
        "MIT OR GPL-3.0-only",
    ] {
        f.check("spdx", &sbom(json!(license)), false);
    }
    let mut v = sbom(json!("MIT"));
    v["packages"][0]["licenseInfoFromFiles"] = json!(["GPL-2.0-only"]);
    f.check("spdx", &v, false);
    let mut v = sbom(json!("MIT"));
    let duplicate = v["packages"][0].clone();
    v["packages"].as_array_mut().unwrap().push(duplicate);
    f.check("spdx", &v, false);
}
#[test]
fn unsigned_container_metadata_cannot_replace_licensed_component_coverage() {
    let f = Fixture::new();
    let mut v = sbom(json!("MIT"));
    v["documentDescribes"] = json!(["SPDXRef-image"]);
    v["packages"].as_array_mut().unwrap().push(json!({"SPDXID":"SPDXRef-image","name":IMAGE,"primaryPackagePurpose":"CONTAINER","licenseDeclared":"NOASSERTION"}));
    f.check("spdx", &v, true);
    v["packages"].as_array_mut().unwrap().remove(0);
    f.check("spdx", &v, false);
}
#[test]
fn cargo_deny_has_no_advisory_license_or_retired_parser_waivers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let p: toml::Value = fs::read_to_string(root.join("deploy/security/deny.toml"))
        .unwrap()
        .parse()
        .unwrap();
    for (table, key) in [
        ("advisories", "ignore"),
        ("licenses", "exceptions"),
        ("graph", "exclude"),
        ("bans", "skip"),
        ("bans", "skip-tree"),
    ] {
        assert!(p[table][key].as_array().unwrap().is_empty());
    }
    assert_eq!(p["advisories"]["unmaintained"].as_str(), Some("all"));
    assert_eq!(p["advisories"]["unsound"].as_str(), Some("all"));
    assert_eq!(p["advisories"]["yanked"].as_str(), Some("deny"));
    let bans = p["bans"]["deny"].as_array().unwrap();
    for name in [
        "serde_yaml",
        "serde_yml",
        "rustpython-ast",
        "rustpython-parser",
        "rustpython-parser-core",
        "rustpython-parser-vendored",
    ] {
        assert!(bans.iter().any(|v| v.as_str() == Some(name)));
    }
}
