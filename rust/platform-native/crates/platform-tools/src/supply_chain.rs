//! Critical vulnerability predicates and deployment admission. Never prints rendered secrets.
use crate::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use std::process::Command;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema_version: String,
    issuer: String,
    certificate_identity_regexp: String,
    predicate_type: String,
    trivy_version: String,
    cosign_version: String,
    max_database_age_hours: i64,
    max_scan_age_hours: i64,
    platforms: Vec<String>,
    services: BTreeMap<String, String>,
}
fn policy(root: &Path) -> Result<(Policy, String)> {
    let path = root.join("deploy/security/supply-chain-policy.json");
    let p: Policy = serde_json::from_value(strict_json(&std::fs::read(&path)?)?)?;
    require(
        p.schema_version == "agentforge/supply-chain-policy/v1",
        "unsupported supply-chain policy",
    )?;
    require(
        p.issuer == "https://token.actions.githubusercontent.com"
            && p.certificate_identity_regexp.starts_with('^')
            && p.certificate_identity_regexp.ends_with('$'),
        "issuer or identity policy is not constrained",
    )?;
    require(
        (1..=72).contains(&p.max_database_age_hours) && (1..=24).contains(&p.max_scan_age_hours),
        "unsafe supply-chain freshness bounds",
    )?;
    require(
        p.platforms == ["linux/amd64", "linux/arm64"],
        "both native release platforms are required",
    )?;
    regex::Regex::new(&p.certificate_identity_regexp)?;
    Ok((p, digest(&path)?))
}
fn pinned(image: &str) -> Result<(&str, &str)> {
    let (name, digest) = image
        .split_once("@sha256:")
        .ok_or("image must use an immutable SHA256 digest")?;
    require(
        !name.is_empty()
            && name.starts_with("ghcr.io/")
            && !name.contains([':', '@', '$', ' ', '\n'])
            && digest.len() == 64
            && digest
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "invalid canonical image digest reference",
    )?;
    Ok((name, digest))
}
fn fresh(value: &Value, now: i64, hours: i64) -> Result<()> {
    let t = DateTime::parse_from_rfc3339(value.as_str().ok_or("missing scan/database timestamp")?)?
        .timestamp();
    require(
        t <= now + 300 && now - t <= hours * 3600,
        "scan/database timestamp is stale or in the future",
    )
}
fn scan(
    p: &Policy,
    policy_sha: &str,
    image: &str,
    platform: &str,
    report: &Value,
    db: &Value,
    now: i64,
) -> Result<Value> {
    pinned(image)?;
    require(
        p.platforms.iter().any(|v| v == platform),
        "unsupported scan platform",
    )?;
    require(
        report["SchemaVersion"] == 2
            && report["ArtifactType"] == "container_image"
            && report["ArtifactName"] == image,
        "scan report schema or immutable image identity mismatch",
    )?;
    require(
        report["Metadata"]["ImageConfig"]["architecture"] == platform.trim_start_matches("linux/")
            && report["Metadata"]["ImageConfig"]["os"] == "linux",
        "scan report platform mismatch",
    )?;
    require(db["Version"] == p.trivy_version, "unapproved Trivy version")?;
    require(
        db["VulnerabilityDB"]["Version"]
            .as_u64()
            .is_some_and(|v| v > 0),
        "vulnerability database metadata is absent",
    )?;
    fresh(
        &db["VulnerabilityDB"]["UpdatedAt"],
        now,
        p.max_database_age_hours,
    )?;
    let results = report["Results"]
        .as_array()
        .ok_or("scan results are absent")?;
    require(
        !results.is_empty() && results.iter().any(|r| r["Class"] == "os-pkgs"),
        "OS vulnerability scan coverage is absent",
    )?;
    for result in results {
        require(
            matches!(result["Class"].as_str(), Some("os-pkgs" | "lang-pkgs")),
            "unknown vulnerability scan class",
        )?;
        if let Some(v) = result.get("Vulnerabilities").filter(|v| !v.is_null()) {
            for vulnerability in v.as_array().ok_or("invalid vulnerability list")? {
                let severity = vulnerability["Severity"]
                    .as_str()
                    .ok_or("missing vulnerability severity")?;
                require(
                    ["UNKNOWN", "LOW", "MEDIUM", "HIGH", "CRITICAL"].contains(&severity),
                    "unknown vulnerability severity",
                )?;
                require(
                    severity != "CRITICAL",
                    "critical vulnerability blocks image admission",
                )?;
            }
        }
    }
    Ok(
        json!({"schema_version":"agentforge/critical-scan/v1","result":"pass","image_ref":image,"platform":platform,"policy_sha256":policy_sha,"critical_count":0,"scanner_version":p.trivy_version,"scanner_db_updated_at":db["VulnerabilityDB"]["UpdatedAt"],"scanned_at":DateTime::from_timestamp(now,0).ok_or("invalid scan time")?.to_rfc3339()}),
    )
}
// Strict RFC4648 decoding for cosign's verified DSSE payload; no new dependency.
fn decode(input: &str) -> Result<Vec<u8>> {
    let bytes = input.as_bytes();
    require(
        !bytes.is_empty() && bytes.len() % 4 == 0,
        "invalid DSSE base64 length",
    )?;
    let digit = |b: u8| -> Result<u8> {
        match b {
            b'A'..=b'Z' => Ok(b - b'A'),
            b'a'..=b'z' => Ok(b - b'a' + 26),
            b'0'..=b'9' => Ok(b - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err("invalid DSSE base64 alphabet".into()),
        }
    };
    let mut out = vec![];
    for (index, q) in bytes.chunks_exact(4).enumerate() {
        let (a, b) = (digit(q[0])?, digit(q[1])?);
        out.push(a << 2 | b >> 4);
        if q[2] == b'=' {
            require(
                index == bytes.len() / 4 - 1 && q[3] == b'=' && b & 15 == 0,
                "invalid DSSE base64 padding",
            )?;
            continue;
        }
        let c = digit(q[2])?;
        out.push(b << 4 | c >> 2);
        if q[3] == b'=' {
            require(
                index == bytes.len() / 4 - 1 && c & 3 == 0,
                "invalid DSSE base64 padding",
            )?;
            continue;
        }
        out.push(c << 6 | digit(q[3])?);
    }
    Ok(out)
}
fn predicate(p: &Policy, hash: &str, image: &str, statement: &Value, now: i64) -> Result<String> {
    let (name, digest) = pinned(image)?;
    require(
        statement["predicateType"] == p.predicate_type,
        "unapproved scan predicate type",
    )?;
    let subjects = statement["subject"]
        .as_array()
        .ok_or("missing attestation subjects")?;
    require(
        subjects.len() == 1
            && subjects[0]["name"] == name
            && subjects[0]["digest"]["sha256"] == digest,
        "attestation digest subject mismatch",
    )?;
    let v = &statement["predicate"];
    require(
        v["schema_version"] == "agentforge/critical-scan/v1"
            && v["result"] == "pass"
            && v["critical_count"].as_u64() == Some(0)
            && v["image_ref"] == image
            && v["policy_sha256"] == hash
            && v["scanner_version"] == p.trivy_version,
        "scan predicate did not pass the current critical policy",
    )?;
    fresh(&v["scanned_at"], now, p.max_scan_age_hours)?;
    fresh(&v["scanner_db_updated_at"], now, p.max_database_age_hours)?;
    let platform = v["platform"].as_str().ok_or("scan platform missing")?;
    require(
        p.platforms.iter().any(|v| v == platform),
        "unknown scan attestation platform",
    )?;
    Ok(platform.into())
}
fn cosign(args: &[&str]) -> Result<Vec<u8>> {
    let mut command = Command::new("cosign");
    command.args(args);
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy();
        if name.starts_with("COSIGN_") || name.starts_with("SIGSTORE_") {
            command.env_remove(key);
        }
    }
    let output = command.output()?;
    require(
        output.status.success() && !output.stdout.is_empty(),
        "cosign verification failed; deployment blocked",
    )?;
    Ok(output.stdout)
}
fn verified_statements(bytes: &[u8]) -> Result<Vec<Value>> {
    let text = std::str::from_utf8(bytes)?;
    let envelopes = match strict_json(bytes) {
        Ok(Value::Array(v)) => v,
        Ok(v) => vec![v],
        Err(_) => text
            .lines()
            .filter(|s| !s.trim().is_empty())
            .map(|s| strict_json(s.as_bytes()))
            .collect::<Result<Vec<_>>>()?,
    };
    require(
        !envelopes.is_empty(),
        "cosign returned no scan attestations",
    )?;
    envelopes
        .into_iter()
        .map(|v| {
            require(
                v["payloadType"] == "application/vnd.in-toto+json",
                "unexpected DSSE payload type",
            )?;
            strict_json(&decode(
                v["payload"]
                    .as_str()
                    .ok_or("missing verified DSSE payload")?,
            )?)
        })
        .collect()
}
fn verify_image(p: &Policy, hash: &str, image: &str, now: i64) -> Result<()> {
    pinned(image)?;
    let signatures = cosign(&[
        "verify",
        "--output",
        "json",
        "--certificate-identity-regexp",
        &p.certificate_identity_regexp,
        "--certificate-oidc-issuer",
        &p.issuer,
        image,
    ])?;
    require(
        strict_json(&signatures)?
            .as_array()
            .is_some_and(|v| !v.is_empty()),
        "cosign returned no verified image signatures",
    )?;
    let payload = cosign(&[
        "verify-attestation",
        "--type",
        &p.predicate_type,
        "--certificate-identity-regexp",
        &p.certificate_identity_regexp,
        "--certificate-oidc-issuer",
        &p.issuer,
        image,
    ])?;
    let mut platforms = std::collections::BTreeSet::new();
    for statement in verified_statements(&payload)? {
        if let Ok(platform) = predicate(p, hash, image, &statement, now) {
            platforms.insert(platform);
        }
    }
    require(
        p.platforms.iter().all(|v| platforms.contains(v)),
        "fresh signed critical-scan evidence for both platforms is required",
    )
}
pub fn run(args: &Args) -> Result<Value> {
    let root = args.root();
    let (p, hash) = policy(&root)?;
    let now = Utc::now().timestamp();
    match args.positionals.get(1).map(String::as_str) {
        Some("scan-report") => scan(
            &p,
            &hash,
            args.get("--image-ref")?,
            args.get("--platform")?,
            &json_input(args.get("--report")?)?,
            &json_input(args.get("--scanner-metadata")?)?,
            now,
        ),
        Some("verify-image") => {
            let image = args.get("--image-ref")?;
            require(
                p.services
                    .values()
                    .any(|name| pinned(image).is_ok_and(|v| v.0 == name.as_str())),
                "image repository has no signing policy",
            )?;
            let version: Value = strict_json(&cosign(&["version", "--json"])?)?;
            require(
                version["gitVersion"] == p.cosign_version,
                "unapproved cosign version",
            )?;
            verify_image(&p, &hash, image, now)?;
            Ok(
                json!({"gate":"supply-chain-image","result":"pass","policy_sha256":hash,"image_ref":image}),
            )
        }
        Some("verify-compose") => {
            let config = json_input(args.get("--config")?)?;
            let services = config["services"]
                .as_object()
                .ok_or("rendered Compose services missing")?;
            let selected = args
                .get("--service-ids")?
                .split_whitespace()
                .collect::<Vec<_>>();
            require(
                !selected.is_empty(),
                "deployment signature service selection must not be empty",
            )?;
            let mut images = std::collections::BTreeSet::new();
            for service in &selected {
                let expected = p
                    .services
                    .get(*service)
                    .ok_or("selected service has no signing policy")?;
                let image = services
                    .get(*service)
                    .and_then(|v| v["image"].as_str())
                    .ok_or("selected service has no image")?;
                require(
                    pinned(image)?.0 == expected.as_str(),
                    "service image repository is not approved",
                )?;
                images.insert(image.to_string());
            }
            let version: Value = strict_json(&cosign(&["version", "--json"])?)?;
            require(
                version["gitVersion"] == p.cosign_version,
                "unapproved cosign version",
            )?;
            for image in &images {
                verify_image(&p, &hash, image, now)?;
            }
            Ok(
                json!({"gate":"supply-chain-deploy","result":"pass","policy_sha256":hash,"services_verified":selected.len(),"immutable_images_verified":images.len(),"image_refs":images,"predicate_type":p.predicate_type}),
            )
        }
        _ => Err("expected supply-chain scan-report, verify-image or verify-compose".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const NOW: i64 = 1_800_000_000;
    const IMAGE: &str = "ghcr.io/misaya-yang/ai-gateway@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fn fixtures() -> (Policy, String, Value, Value) {
        let (p, h) = policy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..")).unwrap();
        let report = json!({"SchemaVersion":2,"ArtifactType":"container_image","ArtifactName":IMAGE,"Metadata":{"ImageConfig":{"os":"linux","architecture":"amd64"}},"Results":[{"Class":"os-pkgs","Vulnerabilities":[]}]});
        let db = json!({"Version":p.trivy_version,"VulnerabilityDB":{"Version":2,"UpdatedAt":DateTime::from_timestamp(NOW-3600,0).unwrap().to_rfc3339()}});
        (p, h, report, db)
    }
    #[test]
    fn scan_rejects_criticals_unfixed_included_and_unknown_severities() {
        let (p, h, report, db) = fixtures();
        assert!(scan(&p, &h, IMAGE, "linux/amd64", &report, &db, NOW).is_ok());
        for severity in ["CRITICAL", "critical", "TYPO"] {
            let mut r = report.clone();
            r["Results"][0]["Vulnerabilities"] = json!([{"Severity":severity,"Status":"unfixed"}]);
            assert!(scan(&p, &h, IMAGE, "linux/amd64", &r, &db, NOW).is_err());
        }
    }
    #[test]
    fn wrong_identity_missing_coverage_wrong_tools_and_stale_database_fail() {
        let (p, h, report, db) = fixtures();
        for field in [
            "SchemaVersion",
            "ArtifactName",
            "ArtifactType",
            "Metadata",
            "Results",
        ] {
            let mut r = report.clone();
            r.as_object_mut().unwrap().remove(field);
            assert!(scan(&p, &h, IMAGE, "linux/amd64", &r, &db, NOW).is_err());
        }
        let mut r = report.clone();
        r["Results"] = json!([]);
        assert!(scan(&p, &h, IMAGE, "linux/amd64", &r, &db, NOW).is_err());
        assert!(scan(&p, &h, IMAGE, "linux/arm64", &report, &db, NOW).is_err());
        assert!(
            scan(
                &p,
                &h,
                "ghcr.io/misaya-yang/ai-gateway:latest",
                "linux/amd64",
                &report,
                &db,
                NOW
            )
            .is_err()
        );
        for delta in [p.max_database_age_hours * 3600 + 1, -301] {
            let mut d = db.clone();
            d["VulnerabilityDB"]["UpdatedAt"] = json!(
                DateTime::from_timestamp(NOW - delta, 0)
                    .unwrap()
                    .to_rfc3339()
            );
            assert!(scan(&p, &h, IMAGE, "linux/amd64", &report, &d, NOW).is_err());
        }
        let mut d = db.clone();
        d["Version"] = json!("dev");
        assert!(scan(&p, &h, IMAGE, "linux/amd64", &report, &d, NOW).is_err());
        let mut d = db;
        d.as_object_mut().unwrap().remove("VulnerabilityDB");
        assert!(scan(&p, &h, IMAGE, "linux/amd64", &report, &d, NOW).is_err());
    }
    #[test]
    fn critical_stale_wrong_subject_and_wrong_policy_attestations_are_rejected() {
        let (p, h, r, d) = fixtures();
        let v = scan(&p, &h, IMAGE, "linux/amd64", &r, &d, NOW).unwrap();
        let statement = json!({"predicateType":p.predicate_type,"subject":[{"name":"ghcr.io/misaya-yang/ai-gateway","digest":{"sha256":"a".repeat(64)}}],"predicate":v});
        assert_eq!(
            predicate(&p, &h, IMAGE, &statement, NOW).unwrap(),
            "linux/amd64"
        );
        for field in [
            "critical_count",
            "policy_sha256",
            "image_ref",
            "scanner_version",
            "scanned_at",
            "scanner_db_updated_at",
            "platform",
            "result",
        ] {
            let mut s = statement.clone();
            s["predicate"][field] = json!("invalid");
            assert!(predicate(&p, &h, IMAGE, &s, NOW).is_err(), "{field}");
        }
        let mut s = statement.clone();
        s["predicate"]["critical_count"] = json!(1);
        assert!(predicate(&p, &h, IMAGE, &s, NOW).is_err());
        let mut s = statement.clone();
        s["subject"][0]["digest"]["sha256"] = json!("b".repeat(64));
        assert!(predicate(&p, &h, IMAGE, &s, NOW).is_err());
        let mut s = statement;
        s["predicate"]["scanned_at"] = json!(
            DateTime::from_timestamp(NOW - p.max_scan_age_hours * 3600 - 1, 0)
                .unwrap()
                .to_rfc3339()
        );
        assert!(predicate(&p, &h, IMAGE, &s, NOW).is_err());
    }
    #[test]
    fn dsse_decoding_rejects_malformed_noncanonical_and_untyped_payloads() {
        assert_eq!(decode("eyJhIjoxfQ==").unwrap(), b"{\"a\":1}");
        for s in ["", "a", "AA=A", "AR==", "AAB=", "====", "AA==AAAA", "!!!!"] {
            assert!(decode(s).is_err(), "{s}");
        }
        assert!(
            verified_statements(
                br#"{"payloadType":"application/vnd.in-toto+json","payload":"eyJhIjoxfQ=="}"#
            )
            .is_ok()
        );
        assert!(
            verified_statements(br#"{"payloadType":"text/plain","payload":"eyJhIjoxfQ=="}"#)
                .is_err()
        );
        assert!(verified_statements(b"[]").is_err());
        assert!(
            verified_statements(
                br#"{"payloadType":"application/vnd.in-toto+json","payload":"!!!="}"#
            )
            .is_err()
        );
    }
    #[test]
    fn signing_identity_does_not_accept_forks_branches_other_workflows_or_issuers() {
        let (p, _, _, _) = fixtures();
        let re = regex::Regex::new(&p.certificate_identity_regexp).unwrap();
        assert!(re.is_match("https://github.com/misaya-yang/AgentForge/.github/workflows/docker-publish.yml@refs/tags/v2.0.0"));
        for identity in [
            "https://github.com/attacker/AgentForge/.github/workflows/docker-publish.yml@refs/tags/v2.0.0",
            "https://github.com/misaya-yang/AgentForge/.github/workflows/docker-publish.yml@refs/heads/main",
            "https://github.com/misaya-yang/AgentForge/.github/workflows/other.yml@refs/tags/v2.0.0",
        ] {
            assert!(!re.is_match(identity));
        }
        assert_eq!(p.issuer, "https://token.actions.githubusercontent.com");
    }
}
