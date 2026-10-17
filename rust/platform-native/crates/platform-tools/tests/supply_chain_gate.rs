//! Real CLI with mock cryptographic verifier only; never accesses credentials/registries.
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
const IMAGE: &str = "ghcr.io/misaya-yang/ai-gateway@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
struct Fixture {
    root: PathBuf,
    hash: String,
}
fn encode(bytes: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in bytes.chunks(3) {
        let a = c[0];
        let b = *c.get(1).unwrap_or(&0);
        let d = *c.get(2).unwrap_or(&0);
        out.push(alphabet[(a >> 2) as usize] as char);
        out.push(alphabet[(((a & 3) << 4) | (b >> 4)) as usize] as char);
        out.push(if c.len() > 1 {
            alphabet[(((b & 15) << 2) | (d >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            alphabet[(d & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("supply-chain-gate-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("deploy/security")).unwrap();
        fs::create_dir_all(root.join("bin")).unwrap();
        let source = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../deploy/security/supply-chain-policy.json");
        let bytes = fs::read(source).unwrap();
        let hash = format!("{:x}", Sha256::digest(&bytes));
        fs::write(root.join("deploy/security/supply-chain-policy.json"), bytes).unwrap();
        let script = root.join("bin/cosign");
        fs::write(
            &script,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$FIXTURE/calls"
case "$1" in
version) printf '{"gitVersion":"v3.1.3"}\n';;
verify) test "${FAIL_SIGNATURE:-0}" = 0 || exit 1; printf '[{}]\n';;
verify-attestation) test "${FAIL_ATTESTATION:-0}" = 0 || exit 1; cat "$FIXTURE/attestations.json";;
*) exit 97;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(script, fs::Permissions::from_mode(0o755)).unwrap();
        let f = Self { root, hash };
        f.attestations(false, false);
        f
    }
    fn attestations(&self, critical: bool, missing_arm: bool) {
        let policy: Value = serde_json::from_slice(
            &fs::read(self.root.join("deploy/security/supply-chain-policy.json")).unwrap(),
        )
        .unwrap();
        let now = chrono::Utc::now().to_rfc3339();
        let mut envelopes = vec![];
        for platform in ["linux/amd64", "linux/arm64"] {
            if missing_arm && platform == "linux/arm64" {
                continue;
            }
            let statement = json!({"predicateType":policy["predicate_type"],"subject":[{"name":"ghcr.io/misaya-yang/ai-gateway","digest":{"sha256":"a".repeat(64)}}],"predicate":{"schema_version":"agentforge/critical-scan/v1","result":"pass","image_ref":IMAGE,"platform":platform,"policy_sha256":self.hash,"critical_count":if critical {1}else{0},"scanner_version":"0.76.0","scanner_db_updated_at":now,"scanned_at":now}});
            envelopes.push(json!({"payloadType":"application/vnd.in-toto+json","payload":encode(&serde_json::to_vec(&statement).unwrap())}));
        }
        fs::write(
            self.root.join("attestations.json"),
            serde_json::to_vec(&envelopes).unwrap(),
        )
        .unwrap();
    }
    fn check(&self, image: &str, signature_fails: bool, attestation_fails: bool, pass: bool) {
        let config = self.root.join("config.json");
        fs::write(&config,serde_json::to_vec(&json!({"services":{"gateway":{"image":image,"environment":{"SECRET":"private-canary"}}}})).unwrap()).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_platform-tools"))
            .args(["supply-chain", "verify-compose", "--repo-root"])
            .arg(&self.root)
            .args(["--config"])
            .arg(config)
            .args(["--service-ids", "gateway"])
            .env(
                "PATH",
                format!("{}:/usr/bin:/bin", self.root.join("bin").display()),
            )
            .env("FIXTURE", &self.root)
            .env("FAIL_SIGNATURE", if signature_fails { "1" } else { "0" })
            .env(
                "FAIL_ATTESTATION",
                if attestation_fails { "1" } else { "0" },
            )
            .output()
            .unwrap();
        assert_eq!(
            output.status.success(),
            pass,
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-canary"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-canary"));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
#[test]
fn signed_fresh_both_platforms_pass_and_both_verifier_commands_bind_identity_and_issuer() {
    let f = Fixture::new();
    f.check(IMAGE, false, false, true);
    let calls = fs::read_to_string(f.root.join("calls")).unwrap();
    assert!(calls
        .contains("--certificate-identity-regexp ^https://github\\.com/misaya-yang/AgentForge/"));
    assert_eq!(
        calls
            .matches("--certificate-oidc-issuer https://token.actions.githubusercontent.com")
            .count(),
        2
    );
    assert!(!calls.contains("--insecure"));
    assert!(!calls.contains("--offline"));
}
#[test]
fn unsigned_invalid_attestation_critical_and_missing_architecture_fail_closed() {
    let f = Fixture::new();
    f.check(IMAGE, true, false, false);
    f.check(IMAGE, false, true, false);
    f.attestations(true, false);
    f.check(IMAGE, false, false, false);
    f.attestations(false, true);
    f.check(IMAGE, false, false, false);
}
#[test]
fn mutable_tag_and_unapproved_repository_fail_before_any_verifier_invocation() {
    let f = Fixture::new();
    f.check("ghcr.io/misaya-yang/ai-gateway:latest", false, false, false);
    f.check("ghcr.io/attacker/ai-gateway@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",false,false,false);
    assert!(!f.root.join("calls").exists());
}
#[test]
fn deployment_hook_precedes_app_stop_and_never_writes_rendered_config() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let deploy = fs::read_to_string(root.join("scripts/new/deploy.sh")).unwrap();
    let gate = deploy
        .find("compose config --format json | platform-tools supply-chain verify-compose")
        .unwrap();
    assert!(gate < deploy.find("compose stop $SERVICES").unwrap());
    assert!(deploy[gate..].contains("--config - --service-ids"));
    assert!(!deploy.contains("SIGNATURE_SKIP"));
}

#[test]
fn all_workflow_actions_match_verified_official_shas_and_have_explicit_permissions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let refs: Value = serde_json::from_slice(
        &fs::read(root.join("deploy/security/official-action-refs.json")).unwrap(),
    )
    .unwrap();
    for entry in fs::read_dir(root.join(".github/workflows")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|v| v.to_str()) != Some("yml") {
            continue;
        }
        let workflow: Value = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(workflow["permissions"].is_object(), "{}", path.display());
        for job in workflow["jobs"].as_object().unwrap().values() {
            for step in job["steps"].as_array().into_iter().flatten() {
                if let Some(used) = step["uses"]
                    .as_str()
                    .filter(|s| !s.starts_with("./") && !s.starts_with("$/"))
                {
                    let (repo, sha) = used.split_once('@').unwrap();
                    assert_eq!(sha.len(), 40);
                    assert_eq!(refs["actions"][repo]["sha"], sha, "{used}");
                }
            }
        }
    }
}
#[test]
fn actual_release_workflow_checks_critical_findings_and_signatures_before_promotion() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let workflow: Value = serde_yaml::from_str(
        &fs::read_to_string(root.join(".github/workflows/docker-publish.yml")).unwrap(),
    )
    .unwrap();
    let jobs = &workflow["jobs"];
    assert_eq!(jobs["merge"]["permissions"]["id-token"], "write");
    assert!(jobs["build-and-push"]["permissions"]
        .get("id-token")
        .is_none());
    let build = jobs["build-and-push"]["steps"].as_array().unwrap();
    let scan = build
        .iter()
        .position(|v| v["name"] == "Enforce critical vulnerability gate on pushed platform digest")
        .unwrap();
    let upload = build
        .iter()
        .position(|v| v["name"] == "Export only admitted digest and public size evidence")
        .unwrap();
    assert!(scan < upload);
    assert!(build[scan].get("continue-on-error").is_none());
    let run = jobs["merge"]["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == "Validate admitted pair and merge with attestations")
        .unwrap()["run"]
        .as_str()
        .unwrap();
    let promote = run.find("imagetools create \"${tag_args[@]}\"").unwrap();
    for gate in [
        "cosign attest --yes",
        "cosign sign --yes",
        "supply-chain verify-image",
    ] {
        assert!(run.find(gate).unwrap() < promote);
    }
    assert!(run.contains("for arch in amd64 arm64"));
}

#[test]
fn actual_scan_wrapper_stops_on_scanner_error_and_rejects_tags_even_on_system_bash() {
    let f = Fixture::new();
    let bin = f.root.join("bin");
    for (name, script) in [
        (
            "trivy",
            "#!/bin/sh\ntest -z \"${TRIVY_IGNORE_UNFIXED:-}\" || exit 99\nexit 42\n",
        ),
        (
            "platform-tools",
            "#!/bin/sh\necho called > \"$FIXTURE/predicate-issued\"\n",
        ),
    ] {
        let path = bin.join(name);
        fs::write(&path, script).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../scripts/harness/scan_release_image.sh");
    for (image, platform, code) in [
        (IMAGE, "linux/amd64", 42),
        ("ghcr.io/misaya-yang/ai-gateway:latest", "linux/amd64", 2),
        (IMAGE, "linux/386", 2),
    ] {
        let output = Command::new("/bin/bash")
            .arg(&script)
            .args([image, platform])
            .arg(f.root.join("predicate.json"))
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("FIXTURE", &f.root)
            .env("TRIVY_IGNORE_UNFIXED", "true")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(code));
        assert!(!f.root.join("predicate.json").exists());
        assert!(!f.root.join("predicate-issued").exists());
    }
}

#[test]
fn sdk_release_uses_only_oidc_publishing_and_disables_release_package_caches() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    let source = fs::read_to_string(root.join(".github/workflows/publish-sdk.yml")).unwrap();
    let workflow: Value = serde_yaml::from_str(&source).unwrap();
    assert!(!source.contains("secrets.NPM_TOKEN"));
    assert!(!source.contains("NODE_AUTH_TOKEN:"));
    let publish = &workflow["jobs"]["publish-cli"];
    assert_eq!(publish["permissions"]["id-token"], "write");
    let steps = publish["steps"].as_array().unwrap();
    assert!(steps
        .iter()
        .any(|s| s["run"] == "node .github/scripts/check-npm-trusted-publisher.mjs"));
    assert!(steps.iter().any(|s| s["run"]
        .as_str()
        .unwrap_or("")
        .contains("npm publish --access public --provenance")));
    for job in workflow["jobs"].as_object().unwrap().values() {
        for step in job["steps"].as_array().into_iter().flatten() {
            let used = step["uses"].as_str().unwrap_or("");
            if used.starts_with("actions/setup-node@") {
                assert_eq!(step["with"]["package-manager-cache"], false);
                assert!(step["with"].get("cache").is_none());
            }
            if used.starts_with("actions/setup-java@") {
                assert!(step["with"].get("cache").is_none());
            }
        }
    }
}
