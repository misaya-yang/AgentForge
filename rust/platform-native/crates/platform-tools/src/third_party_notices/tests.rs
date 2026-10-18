// SPDX-License-Identifier: Apache-2.0
use super::*;
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let f =
            Self(std::env::temp_dir().join(format!("agentforge-notice-{}", uuid::Uuid::new_v4())));
        for dir in [
            "web/node_modules",
            "sdk/cli/node_modules",
            "rust/platform-native",
            "docs/security",
            "licenses/third-party/materials",
        ] {
            std::fs::create_dir_all(f.0.join(dir)).unwrap();
        }
        for p in [
            "web/pnpm-lock.yaml",
            "sdk/cli/package-lock.json",
            "rust/platform-native/Cargo.lock",
        ] {
            std::fs::write(f.0.join(p), "fixture").unwrap();
        }
        std::fs::write(f.0.join("cargo.json"), r#"{"packages":[]}"#).unwrap();
        f
    }
    fn args(&self) -> Args {
        let mut a = Args::default();
        for (key, value) in [
            ("--repo-root", self.0.clone()),
            ("--cargo-metadata", self.0.join("cargo.json")),
            ("--output", self.0.join("out")),
        ] {
            a.values
                .insert(key.into(), value.to_string_lossy().into_owned());
        }
        a
    }
    fn package(&self) -> PathBuf {
        let p = self.0.join("web/node_modules/example");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(
            p.join("package.json"),
            r#"{"name":"example","version":"1.0.0","license":"MIT"}"#,
        )
        .unwrap();
        p
    }
    fn supplement(&self, package: &Path) -> Value {
        let text = "Actual upstream fixture license, not a product license grant.\n";
        let sha = hash(text.as_bytes());
        let path = format!("licenses/third-party/materials/{sha}.txt");
        std::fs::write(self.0.join(&path), text).unwrap();
        json!({"schema":"agentforge/third-party-license-sources/v1","components":[{
            "id":"npm:example@1.0.0","name":"example","version":"1.0.0","declared_license":"MIT",
            "package_manifest_sha256":hash(&std::fs::read(package.join("package.json")).unwrap()),
            "npm_registry":{"gitHead":"1111111111111111111111111111111111111111"},
            "files":[{"path":path,"sha256":sha,"repository":"https://github.com/example/example","revision":"1111111111111111111111111111111111111111","upstream_path":"LICENSE","upstream_url":"https://raw.githubusercontent.com/example/example/1111111111111111111111111111111111111111/LICENSE","binding":"exact-npm-registry-gitHead-ancestor-license"}]
        }]})
    }
    fn save(&self, value: &Value) {
        std::fs::write(
            self.0
                .join("docs/security/third-party-license-sources.json"),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn strict_missing_terms_fail_before_any_output() {
    let f = Fixture::new();
    f.package();
    let mut a = f.args();
    a.flags.push("--require-zero".into());
    assert!(run(&a).unwrap_err().to_string().contains("1 components"));
    assert!(!f.0.join("out").exists());
}

#[test]
fn declared_external_agent_cannot_silently_disappear_from_notices() {
    let f = Fixture::new();
    let package = f.package();
    std::fs::write(package.join("LICENSE"), "License fixture").unwrap();
    let external = f.0.join("apps/external-a2a-agent");
    std::fs::create_dir_all(&external).unwrap();
    std::fs::write(
        external.join("package.json"),
        r#"{"name":"external-agent"}"#,
    )
    .unwrap();
    assert!(
        run(&f.args())
            .unwrap_err()
            .to_string()
            .contains("node_modules")
    );
    assert!(!f.0.join("out").exists());
    std::fs::create_dir(external.join("node_modules")).unwrap();
    assert!(
        run(&f.args())
            .unwrap_err()
            .to_string()
            .contains("dependency lock missing")
    );
    assert!(!f.0.join("out").exists());
    std::fs::write(external.join("package-lock.json"), "fixture").unwrap();
    run(&f.args()).unwrap();
    let inventory = object(&f.0.join("out/inventory.json")).unwrap();
    assert!(
        inventory["javascript_scopes"]
            .as_array()
            .unwrap()
            .contains(&json!("apps/external-a2a-agent"))
    );
    assert_eq!(
        inventory["inputs"]["apps/external-a2a-agent/package-lock.json"],
        hash(b"fixture")
    );
}
#[test]
fn exact_source_supplement_is_included_and_identity_drift_is_rejected() {
    let f = Fixture::new();
    let p = f.package();
    let source = f.supplement(&p);
    f.save(&source);
    let mut a = f.args();
    a.flags.push("--require-zero".into());
    assert_eq!(run(&a).unwrap()["missing_declaration_or_text_count"], 0);
    let result = object(&f.0.join("out/inventory.json")).unwrap();
    assert!(
        result["components"]["npm:example@1.0.0"]["notice_files"]
            .as_object()
            .unwrap()
            .values()
            .any(|v| v["provenance"]["revision"] == "1111111111111111111111111111111111111111")
    );
    std::fs::write(
        p.join("package.json"),
        r#"{"name":"example","version":"1.0.0","license":"Apache-2.0"}"#,
    )
    .unwrap();
    a.values.insert(
        "--output".into(),
        f.0.join("two").to_string_lossy().into_owned(),
    );
    assert!(
        run(&a)
            .unwrap_err()
            .to_string()
            .contains("identity/declaration mismatch")
    );
}
#[test]
fn tampered_material_mutable_revision_and_path_escape_fail_closed() {
    let f = Fixture::new();
    let p = f.package();
    let original = f.supplement(&p);
    for (key, value) in [
        (
            "path",
            json!("licenses/third-party/materials/../outside.txt"),
        ),
        ("revision", json!("main")),
        ("sha256", json!("0".repeat(64))),
        ("upstream_url", json!("https://untrusted.example/LICENSE")),
    ] {
        let mut v = original.clone();
        v["components"][0]["files"][0][key] = value;
        f.save(&v);
        assert!(Sources::load(&f.0).is_err(), "{key}");
    }
    let mut v = original.clone();
    v["components"][0]["npm_registry"]["gitHead"] = json!("2".repeat(40));
    f.save(&v);
    assert!(Sources::load(&f.0).is_err());
    f.save(&original);
    std::fs::write(
        f.0.join(
            original["components"][0]["files"][0]["path"]
                .as_str()
                .unwrap(),
        ),
        "altered",
    )
    .unwrap();
    assert!(Sources::load(&f.0).is_err());
}
#[cfg(unix)]
#[test]
fn supplement_symlink_is_never_followed() {
    let f = Fixture::new();
    let p = f.package();
    let v = f.supplement(&p);
    f.save(&v);
    let source =
        f.0.join(v["components"][0]["files"][0]["path"].as_str().unwrap());
    let other = f.0.join("elsewhere");
    std::fs::rename(&source, &other).unwrap();
    std::os::unix::fs::symlink(other, source).unwrap();
    assert!(Sources::load(&f.0).is_err());
}
#[test]
fn embedded_readme_license_is_collected_but_badge_is_not() {
    assert!(!embedded_license(
        "Copyright Alice. Licensed MIT; see external URL."
    ));
    let f = Fixture::new();
    let p = f.package();
    let content = "Copyright Alice\nPermission is hereby granted\nTHE SOFTWARE IS PROVIDED AS IS\n";
    std::fs::write(p.join("README.md"), content).unwrap();
    let mut a = f.args();
    a.flags.push("--require-zero".into());
    assert_eq!(run(&a).unwrap()["missing_declaration_or_text_count"], 0);
    a.values.insert(
        "--check-bundle".into(),
        f.0.join("out").to_string_lossy().into_owned(),
    );
    a.values.insert(
        "--output".into(),
        f.0.join("verified").to_string_lossy().into_owned(),
    );
    run(&a).unwrap();
    std::fs::write(p.join("README.md"), format!("{content}changed")).unwrap();
    a.values.insert(
        "--output".into(),
        f.0.join("stale").to_string_lossy().into_owned(),
    );
    assert!(run(&a).unwrap_err().to_string().contains("stale"));
    assert!(!f.0.join("stale").exists());
}
#[test]
fn vendored_third_party_path_is_not_silently_omitted() {
    let f = Fixture::new();
    let p = f.0.join("rust/platform-native/vendor/demo");
    std::fs::create_dir_all(&p).unwrap();
    std::fs::write(
        p.join("Cargo.toml"),
        "[package]\nname='demo'\nversion='1.0.0'\nlicense='MIT'\n",
    )
    .unwrap();
    std::fs::write(p.join("LICENSE"), "Upstream license fixture").unwrap();
    std::fs::write(f.0.join("cargo.json"), serde_json::to_vec(&json!({"workspace_members":[],"packages":[{"id":"path+demo","name":"demo","version":"1.0.0","license":"MIT","source":null,"manifest_path":p.join("Cargo.toml")}]})).unwrap()).unwrap();
    let r = run(&f.args()).unwrap();
    assert_eq!(r["component_count"], 1);
    let inv = object(&f.0.join("out/inventory.json")).unwrap();
    assert!(
        inv["components"]
            .as_object()
            .unwrap()
            .contains_key("cargo:demo@1.0.0#path:rust/platform-native/vendor/demo")
    );
}

#[cfg(test)]
mod filename_tests {
    use super::*;
    #[test]
    fn recognizes_license_material_without_matching_implementation_files() {
        for name in [
            "LICENSE",
            "LICENSE-MIT",
            "LICENSE.Apache-2.0",
            "NOTICE.md",
            "OFL.txt",
            "COPYING.LESSER",
            "UFL.txt",
            "emoji-icon-font-mit-license.txt",
        ] {
            assert!(notice_name(name));
        }
        for name in ["license.rs", "main.rs", "package.json", "node_modules"] {
            assert!(!notice_name(name));
        }
    }
}

#[cfg(test)]
mod deterministic_tests {
    use super::*;
    #[test]
    fn notice_inventory_is_deterministic_and_never_approves_unknown_terms() {
        let root =
            std::env::temp_dir().join(format!("agentforge-notice-test-{}", uuid::Uuid::new_v4()));
        for path in [
            "rust/platform-native",
            "web/node_modules/demo",
            "sdk/cli/node_modules/demo",
        ] {
            std::fs::create_dir_all(root.join(path)).unwrap();
        }
        for path in [
            "rust/platform-native/Cargo.lock",
            "web/pnpm-lock.yaml",
            "sdk/cli/package-lock.json",
        ] {
            std::fs::write(root.join(path), b"fixture lock").unwrap();
        }
        let mut scopes = vec!["web", "sdk/cli"];
        if root.join("apps/external-a2a-agent/node_modules").exists() {
            scopes.push("apps/external-a2a-agent");
        }
        for scope in scopes {
            let package = root.join(scope).join("node_modules/demo");
            std::fs::write(
                package.join("package.json"),
                r#"{"name":"demo","version":"1.0.0","license":"LicenseRef-Needs-Review"}"#,
            )
            .unwrap();
            std::fs::write(package.join("LICENSE"), "Third-party rights retained.\n").unwrap();
        }
        let metadata = root.join("cargo.json");
        std::fs::write(&metadata, r#"{"packages":[]}"#).unwrap();
        let mut args = Args::default();
        args.values
            .insert("--repo-root".into(), root.to_string_lossy().into_owned());
        args.values.insert(
            "--cargo-metadata".into(),
            metadata.to_string_lossy().into_owned(),
        );
        for output in ["one", "two"] {
            args.values.insert(
                "--output".into(),
                root.join(output).to_string_lossy().into_owned(),
            );
            assert_eq!(run(&args).unwrap()["result"], "generated_not_approved");
        }
        for name in ["inventory.json", "THIRD_PARTY_NOTICES.txt"] {
            assert_eq!(
                std::fs::read(root.join("one").join(name)).unwrap(),
                std::fs::read(root.join("two").join(name)).unwrap()
            );
        }
        let inventory = object(&root.join("one/inventory.json")).unwrap();
        assert_eq!(
            inventory["components"]["npm:demo@1.0.0"]["status"],
            "collected_not_approved"
        );
        assert!(run(&args).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn standard_terms_require_the_original_explicit_grant_and_never_invent_mit_copyright() {
    let f = Fixture::new();
    let package = f.package();
    let mut source = f.supplement(&package);
    let item = &mut source["components"][0];
    item["declared_license"] = json!("CC0-1.0");
    let grant = "[package]\nlicense = \"CC0-1.0\"\n";
    std::fs::write(package.join("Cargo.toml.orig"), grant).unwrap();
    item["standard_grant"] =
        json!({"package_path":"Cargo.toml.orig","sha256":hash(grant.as_bytes())});
    let file = &mut item["files"][0];
    file["repository"] = json!("https://github.com/spdx/license-list-data");
    file["upstream_path"] = json!("text/CC0-1.0.txt");
    file["upstream_url"] = json!(
        "https://raw.githubusercontent.com/spdx/license-list-data/1111111111111111111111111111111111111111/text/CC0-1.0.txt"
    );
    file["binding"] = json!("referenced-standard-license");
    file["standard_license"] = json!("CC0-1.0");
    // This fixture only tests source binding. It is never a product license receipt.
    std::fs::write(
        package.join("package.json"),
        r#"{"name":"example","version":"1.0.0","license":"CC0-1.0"}"#,
    )
    .unwrap();
    source["components"][0]["package_manifest_sha256"] =
        json!(hash(&std::fs::read(package.join("package.json")).unwrap()));
    f.save(&source);
    assert_eq!(
        run(&f.args()).unwrap()["missing_declaration_or_text_count"],
        0
    );
    std::fs::write(package.join("Cargo.toml.orig"), "license = \"MIT\"\n").unwrap();
    let mut a = f.args();
    a.values.insert(
        "--output".into(),
        f.0.join("bad").to_string_lossy().into_owned(),
    );
    assert!(run(&a).is_err());
    source["components"][0]["files"][0]["standard_license"] = json!("MIT");
    source["components"][0]["declared_license"] = json!("MIT");
    f.save(&source);
    assert!(Sources::load(&f.0).is_err());
}

#[test]
fn data_headers_retain_attribution_without_copying_the_entire_registry() {
    let xml = "<?xml version='1.0'?><registry><comment>Copyright Khronos\nPermission is hereby granted\nTHE MATERIALS ARE PROVIDED AS IS</comment><types>large body</types>";
    let h = data_license_header("gl.xml", xml).unwrap();
    assert!(h.contains("Copyright Khronos"));
    assert!(!h.contains("large body"));
    assert!(
        data_license_header(
            "gl.xml",
            "<registry><comment>no legal grant</comment></registry>"
        )
        .is_none()
    );
    assert!(data_license_header("impl.rs", xml).is_none());
}

#[test]
fn non_utf8_test_xml_does_not_hide_or_corrupt_real_license_files() {
    let f = Fixture::new();
    let package = f.package();
    std::fs::write(package.join("LICENSE"), "Retained upstream terms fixture").unwrap();
    std::fs::write(package.join("encoded-test.xml"), [b'<', b'x', b'>', 0xff]).unwrap();
    let legal_xml =
        "<license><!--Copyright Alice; license--><terms>Complete terms</terms></license>";
    std::fs::write(package.join("LICENSE.xml"), legal_xml).unwrap();
    let mut registry =
        b"<registry><comment>Copyright Khronos. Licensed under Apache License 2.0.</comment>"
            .to_vec();
    registry.extend_from_slice(&[0xff, 0xfe]);
    std::fs::write(package.join("registry.xml"), &registry).unwrap();
    let mut a = f.args();
    a.flags.push("--require-zero".into());
    run(&a).unwrap();
    let inv = object(&f.0.join("out/inventory.json")).unwrap();
    let notices = inv["components"]["npm:example@1.0.0"]["notice_files"]
        .as_object()
        .unwrap();
    assert!(notices.contains_key("LICENSE"));
    assert_eq!(notices["LICENSE.xml"]["text"], legal_xml);
    assert!(!notices.contains_key("encoded-test.xml"));
    assert_eq!(
        notices["registry.xml#license-header"]["source_file_sha256"],
        hash(&registry)
    );
    assert!(
        !notices["registry.xml#license-header"]["text"]
            .as_str()
            .unwrap()
            .contains('\u{fffd}')
    );
}
