//! Signed release provenance and SPDX/license admission, layered over critical scanning.
//! No publication, registry mutation or deployment is performed by this module.
use crate::*;
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;
use std::process::Command;
#[path = "delivery/compose_profiles.rs"]
mod compose_profiles;
const LICENSE_TYPE: &str =
    "https://github.com/misaya-yang/AgentForge/attestations/license-admission/v1";
const RELEASE_TYPE: &str =
    "https://github.com/misaya-yang/AgentForge/attestations/native-release/v1";
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<_, _> =
                map.iter().map(|(k, v)| (k.clone(), canonical(v))).collect();
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        _ => value.clone(),
    }
}
fn document_hash(value: &Value) -> Result<String> {
    Ok(hash(&serde_json::to_vec(&canonical(value))?))
}
fn image_subject(image: &str) -> Result<(&str, &str)> {
    let (name, sha) = image
        .split_once("@sha256:")
        .ok_or("immutable release image required")?;
    require(
        name.starts_with("ghcr.io/") && !name.contains(['@', ':', ' ', '\n']) && hex(sha, 64),
        "invalid release digest",
    )?;
    Ok((name, sha))
}
fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn statement_subject(statement: &Value, image: &str, kind: &str) -> Result<()> {
    let (name, sha) = image_subject(image)?;
    require(
        statement["predicateType"] == kind
            && statement["subject"].as_array().is_some_and(|a| {
                a.len() == 1 && a[0]["name"] == name && a[0]["digest"]["sha256"] == sha
            }),
        "signed artifact subject/type differs from release image",
    )
}
// Strict canonical RFC4648, deliberately independent of additional dependencies.
fn decode(input: &str) -> Result<Vec<u8>> {
    let bytes = input.as_bytes();
    require(
        !bytes.is_empty() && bytes.len() % 4 == 0,
        "invalid DSSE encoding",
    )?;
    let digit = |b: u8| -> Result<u8> {
        match b {
            b'A'..=b'Z' => Ok(b - b'A'),
            b'a'..=b'z' => Ok(b - b'a' + 26),
            b'0'..=b'9' => Ok(b - b'0' + 52),
            b'+' => Ok(62),
            b'/' => Ok(63),
            _ => Err("invalid DSSE alphabet".into()),
        }
    };
    let mut out = Vec::new();
    for (index, q) in bytes.chunks_exact(4).enumerate() {
        let (a, b) = (digit(q[0])?, digit(q[1])?);
        out.push(a << 2 | b >> 4);
        if q[2] == b'=' {
            require(
                index == bytes.len() / 4 - 1 && q[3] == b'=' && b & 15 == 0,
                "invalid DSSE padding",
            )?;
            continue;
        }
        let c = digit(q[2])?;
        out.push(b << 4 | c >> 2);
        if q[3] == b'=' {
            require(
                index == bytes.len() / 4 - 1 && c & 3 == 0,
                "invalid DSSE padding",
            )?;
            continue;
        }
        out.push(c << 6 | digit(q[3])?);
    }
    Ok(out)
}
fn statements(bytes: &[u8]) -> Result<Vec<Value>> {
    let envelopes = match strict_json(bytes) {
        Ok(Value::Array(v)) => v,
        Ok(v) => vec![v],
        Err(_) => std::str::from_utf8(bytes)?
            .lines()
            .filter(|s| !s.trim().is_empty())
            .map(|s| strict_json(s.as_bytes()))
            .collect::<Result<Vec<_>>>()?,
    };
    require(!envelopes.is_empty(), "no verified release artifacts")?;
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
                    .ok_or("verified DSSE payload missing")?,
            )?)
        })
        .collect()
}
fn verified(root: &Path, image: &str, kind: &str) -> Result<Vec<Value>> {
    let p = object(&root.join("deploy/security/supply-chain-policy.json"))?;
    let mut command = Command::new("cosign");
    command.args([
        "verify-attestation",
        "--timeout",
        "45s",
        "--type",
        kind,
        "--certificate-identity-regexp",
        p["certificate_identity_regexp"]
            .as_str()
            .ok_or("signer identity missing")?,
        "--certificate-oidc-issuer",
        p["issuer"].as_str().ok_or("signer issuer missing")?,
        image,
    ]);
    for (key, _) in std::env::vars_os() {
        let key_text = key.to_string_lossy();
        if key_text.starts_with("COSIGN_") || key_text.starts_with("SIGSTORE_") {
            command.env_remove(key);
        }
    }
    let output = command.output()?;
    require(
        output.status.success(),
        "signed release artifact verification failed; diagnostics withheld",
    )?;
    statements(&output.stdout)
}
fn check_artifacts(
    root: &Path,
    image: &str,
    licenses: &[Value],
    sboms: &[Value],
    releases: &[Value],
    now: i64,
) -> Result<String> {
    let license_policy = digest(&root.join("deploy/security/dependency-policy.json"))?;
    let scan_policy = digest(&root.join("deploy/security/supply-chain-policy.json"))?;
    let p = object(&root.join("deploy/security/supply-chain-policy.json"))?;
    let max_age = p["max_scan_age_hours"]
        .as_i64()
        .ok_or("scan age policy missing")?
        * 3600;
    for platform in ["linux/amd64", "linux/arm64"] {
        let valid = licenses.iter().any(|statement| {
            if statement_subject(statement, image, LICENSE_TYPE).is_err() {
                return false;
            }
            let v = &statement["predicate"];
            let fresh = v["generated_at"]
                .as_str()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .is_some_and(|t| t.timestamp() <= now + 300 && now - t.timestamp() <= max_age);
            v["schema_version"] == "agentforge/license-admission/v1"
                && v["result"] == "pass"
                && v["platform"] == platform
                && v["image_ref"] == image
                && v["policy_sha256"] == license_policy
                && fresh
                && sboms.iter().any(|sbom| {
                    statement_subject(sbom, image, "https://spdx.dev/Document").is_ok()
                        && document_hash(&sbom["predicate"]).is_ok_and(|h| v["sbom_sha256"] == h)
                })
        });
        require(
            valid,
            "each platform needs fresh signed current-policy license admission bound to its signed SPDX document",
        )?;
    }
    let release = releases
        .iter()
        .find(|s| {
            statement_subject(s, image, RELEASE_TYPE).is_ok() && {
                let p = &s["predicate"];
                p["schema_version"] == "agentforge/native-release/v1"
                    && p["source_repository"] == "https://github.com/misaya-yang/AgentForge"
                    && p["source_revision"].as_str().is_some_and(|v| hex(v, 40))
                    && p["image_ref"] == image
                    && p["scan_policy_sha256"] == scan_policy
                    && p["license_policy_sha256"] == license_policy
                    && p["platforms"] == json!(["linux/amd64", "linux/arm64"])
                    && p["source_digests"].as_array().is_some_and(|a| {
                        a.len() == 2
                            && a[0] != a[1]
                            && a.iter().all(|v| {
                                v.as_str().is_some_and(|v| {
                                    v.strip_prefix("sha256:").is_some_and(|v| hex(v, 64))
                                })
                            })
                    })
            }
        })
        .ok_or("signed native release provenance for both platforms is required")?;
    Ok(release["predicate"]["source_revision"]
        .as_str()
        .unwrap()
        .to_owned())
}
fn child(args: &Args, command: &str, action: &str) -> Args {
    Args {
        values: args.values.clone(),
        multiple: BTreeMap::new(),
        flags: vec![],
        positionals: vec![command.into(), action.into()],
    }
}
fn verify_image(args: &Args, image: &str) -> Result<String> {
    let mut basic = child(args, "supply-chain", "verify-image");
    basic.values.insert("--image-ref".into(), image.into());
    crate::supply_chain::run(&basic)?;
    check_artifacts(
        &args.root(),
        image,
        &verified(&args.root(), image, LICENSE_TYPE)?,
        &verified(&args.root(), image, "spdxjson")?,
        &verified(&args.root(), image, RELEASE_TYPE)?,
        Utc::now().timestamp(),
    )
}
pub fn run(args: &Args) -> Result<Value> {
    require(
        args.flags.is_empty() && args.positionals.len() == 2,
        "delivery-policy takes one fixed action and no bypass flags",
    )?;
    let action = args.positionals.get(1).map(String::as_str).unwrap_or("");
    let allowed: &[&str] = match action {
        "compose-plan" => &["--mode", "--feature", "--project-name", "--output"],
        "license-predicate" => &["--image-ref", "--platform", "--sbom"],
        "release-predicate" => &["--image-ref", "--revision", "--records"],
        "verify-image" => &["--image-ref"],
        "verify-compose" => &["--config", "--service-ids"],
        "verify-release-set" => &["--images"],
        _ => return Err("unsupported delivery action".into()),
    };
    for (name, values) in &args.multiple {
        require(
            (["--repo-root", "--evidence-out"].contains(&name.as_str())
                || allowed.contains(&name.as_str()))
                && (name == "--feature" || values.len() == 1),
            "unknown or repeated delivery option",
        )?;
    }
    let root = args.root();
    let result=match args.positionals.get(1).map(String::as_str){
        Some("compose-plan") => compose_profiles::plan(args)?,
        Some("license-predicate")=>{
            let image=args.get("--image-ref")?;image_subject(image)?;let platform=args.get("--platform")?;require(["linux/amd64","linux/arm64"].contains(&platform),"invalid SPDX platform")?;
            let admission=crate::dependency_policy::run(&child(args,"dependency-policy","spdx"))?;
            require(admission["result"]=="pass","SPDX license policy rejected release")?;
            json!({"schema_version":"agentforge/license-admission/v1","result":"pass","image_ref":image,"platform":platform,"policy_sha256":digest(&root.join("deploy/security/dependency-policy.json"))?,"sbom_sha256":document_hash(&json_input(args.get("--sbom")?)?)?,"generated_at":Utc::now().to_rfc3339(),"legal_redistribution_acceptance":"not_established"})
        },
        Some("release-predicate")=>{
            let image=args.get("--image-ref")?;let(name,_)=image_subject(image)?;let revision=args.get("--revision")?;require(hex(revision,40),"exact source revision required")?;
            let dir=Path::new(args.get("--records")?);let mut digests=vec![];
            for arch in ["amd64","arm64"]{let path=dir.join(format!("{arch}.json"));require(std::fs::symlink_metadata(&path)?.is_file(),"release digest record must be a regular file")?;let v=json_input(path.to_str().ok_or("non-Unicode record path")?)?;
                let sha=v["digest"].as_str().ok_or("platform digest missing")?;require(sha.strip_prefix("sha256:").is_some_and(|s|hex(s,64))&&v["image"]==name&&v["source_revision"]==revision&&v["platform"]==format!("linux/{arch}")&&v["size_receipt"]["result"]=="pass"&&v["scan_receipt"]["result"]=="pass","release provenance must use admitted immutable native records")?;
                digests.push(sha.to_owned());}
            require(digests[0]!=digests[1],"native architecture digests must differ")?;
            json!({"schema_version":"agentforge/native-release/v1","image_ref":image,"source_repository":"https://github.com/misaya-yang/AgentForge","source_revision":revision,"platforms":["linux/amd64","linux/arm64"],"source_digests":digests,"scan_policy_sha256":digest(&root.join("deploy/security/supply-chain-policy.json"))?,"license_policy_sha256":digest(&root.join("deploy/security/dependency-policy.json"))?,"slsa_level":"not_claimed"})
        },
        Some("verify-release-set") => {
            let images = json_input(args.get("--images")?)?;
            let images = images.as_object().ok_or("release image set must be a service map")?;
            for unit in ["gateway","frontend","knowledge-service","knowledge-worker","migrate","agent-runtime","agent-capability-worker"] {
                require(images.get(unit).and_then(Value::as_str).is_some(),"complete service release image set required")?;
            }
            require(images["knowledge-service"] == images["knowledge-worker"], "Knowledge roles must share exactly one image")?;
            let policy=object(&root.join("deploy/security/supply-chain-policy.json"))?;
            let mut revisions=BTreeMap::new();let mut verified_images:BTreeMap<String,String>=BTreeMap::new();
            for(unit,image)in images {
                let image=image.as_str().ok_or("release image reference missing")?;
                require(policy["services"][unit].as_str()==Some(image_subject(image)?.0),"service image repository differs from its exact signing policy")?;
                let revision=if let Some(revision)=verified_images.get(image) { revision.clone() } else { let revision=verify_image(args,image)?;verified_images.insert(image.to_owned(),revision.clone());revision };
                revisions.insert(unit.to_owned(),revision);
            }
            require(revisions["agent-runtime"] == revisions["agent-capability-worker"], "Runtime and Worker release revisions must remain paired")?;
            json!({"gate":"delivery-release-set","result":"pass","services":images.len(),"immutable_images_verified":verified_images.len(),"runtime_worker_paired":true})
        },
        Some("verify-image")=>{let image=args.get("--image-ref")?;verify_image(args,image)?;json!({"gate":"delivery-artifacts","result":"pass","image_ref":image,"verified":["keyless-signature","both-critical-scans","both-license-policies","signed-SPDX-content-bindings","signed-native-release-provenance"]})},
        Some("verify-compose")=>{let basic=crate::supply_chain::run(&child(args,"supply-chain","verify-compose"))?;let images=basic["image_refs"].as_array().ok_or("admitted immutable images missing")?;for image in images{verify_image(args,image.as_str().ok_or("bad admitted image")?)?;}json!({"gate":"delivery-compose-artifacts","result":"pass","immutable_images_verified":images.len()})},
        _=>return Err("expected delivery-policy license-predicate, release-predicate, verify-image or verify-compose".into()),
    };
    if let Some(path) = args.values.get("--evidence-out") {
        write_json(Path::new(path), &result)?;
    }
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signed_artifact_contract_binds_both_platforms_current_policy_contents_and_revision() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        let image = format!("ghcr.io/misaya-yang/ai-gateway@sha256:{}", "a".repeat(64));
        let subject =
            json!([{"name":"ghcr.io/misaya-yang/ai-gateway","digest":{"sha256":"a".repeat(64)}}]);
        let sbom = json!({"predicateType":"https://spdx.dev/Document","subject":subject,"predicate":{"SPDXID":"SPDXRef-DOCUMENT","spdxVersion":"SPDX-2.3","packages":[{"name":"fixture","licenseDeclared":"MIT"}]}});
        let now = Utc::now().timestamp();
        let licenses=["linux/amd64","linux/arm64"].map(|platform|json!({"predicateType":LICENSE_TYPE,"subject":subject,"predicate":{"schema_version":"agentforge/license-admission/v1","result":"pass","image_ref":image,"platform":platform,"policy_sha256":digest(&root.join("deploy/security/dependency-policy.json")).unwrap(),"sbom_sha256":document_hash(&sbom["predicate"]).unwrap(),"generated_at":DateTime::from_timestamp(now,0).unwrap().to_rfc3339()}}));
        let release = json!({"predicateType":RELEASE_TYPE,"subject":subject,"predicate":{"schema_version":"agentforge/native-release/v1","source_repository":"https://github.com/misaya-yang/AgentForge","source_revision":"b".repeat(40),"image_ref":image,"scan_policy_sha256":digest(&root.join("deploy/security/supply-chain-policy.json")).unwrap(),"license_policy_sha256":digest(&root.join("deploy/security/dependency-policy.json")).unwrap(),"platforms":["linux/amd64","linux/arm64"],"source_digests":[format!("sha256:{}","c".repeat(64)),format!("sha256:{}","d".repeat(64))]}});
        assert_eq!(
            check_artifacts(
                &root,
                &image,
                &licenses,
                &[sbom.clone()],
                &[release.clone()],
                now
            )
            .unwrap(),
            "b".repeat(40)
        );
        assert!(
            check_artifacts(
                &root,
                &image,
                &licenses[..1],
                &[sbom.clone()],
                &[release.clone()],
                now
            )
            .is_err()
        );
        for key in [
            "result",
            "image_ref",
            "platform",
            "policy_sha256",
            "sbom_sha256",
            "generated_at",
        ] {
            let mut bad = licenses.clone();
            bad[1]["predicate"][key] = json!("wrong");
            assert!(
                check_artifacts(
                    &root,
                    &image,
                    &bad,
                    &[sbom.clone()],
                    &[release.clone()],
                    now
                )
                .is_err(),
                "{key}"
            );
        }
        let mut bad_sbom = sbom.clone();
        bad_sbom["predicate"]["packages"][0]["name"] = json!("changed-content");
        assert!(
            check_artifacts(
                &root,
                &image,
                &licenses,
                &[bad_sbom],
                &[release.clone()],
                now
            )
            .is_err()
        );
        let mut bad_release = release.clone();
        bad_release["predicate"]["source_revision"] = json!("branch-main");
        assert!(
            check_artifacts(
                &root,
                &image,
                &licenses,
                &[sbom.clone()],
                &[bad_release],
                now
            )
            .is_err()
        );
        let mut bad = licenses.clone();
        bad[0]["predicate"]["generated_at"] = json!(
            DateTime::from_timestamp(now - 172800, 0)
                .unwrap()
                .to_rfc3339()
        );
        assert!(check_artifacts(&root, &image, &bad, &[sbom], &[release], now).is_err());
    }
    #[test]
    fn canonical_sbom_digest_ignores_object_key_order_but_not_contents() {
        assert_eq!(
            document_hash(&json!({"b":2,"a":{"y":4,"x":3}})).unwrap(),
            document_hash(&json!({"a":{"x":3,"y":4},"b":2})).unwrap()
        );
        assert_ne!(
            document_hash(&json!({"package":"a"})).unwrap(),
            document_hash(&json!({"package":"b"})).unwrap()
        );
    }
    #[test]
    fn malformed_dsse_and_mutable_images_fail() {
        for s in ["", "!!!!", "AR==", "AAB=", "AA==AAAA"] {
            assert!(decode(s).is_err());
        }
        for image in [
            "ghcr.io/a/b:latest",
            "ghcr.io/a/b@sha256:123",
            "docker.io/a/b@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ] {
            assert!(image_subject(image).is_err());
        }
        assert!(statements(b"[]").is_err());
    }
    #[test]
    fn unsigned_or_missing_artifact_sets_never_admit() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        assert!(check_artifacts(&root,"ghcr.io/a/b@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",&[],&[],&[],Utc::now().timestamp()).is_err());
    }
}
