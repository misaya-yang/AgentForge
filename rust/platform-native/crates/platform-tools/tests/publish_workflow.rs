//! Validate the executable workflow's admission filter with no Docker or registry access.
use serde_json::{Value, json};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};
fn workflow(name: &str) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
    serde_yaml::from_str(
        &std::fs::read_to_string(root.join(format!(".github/workflows/{name}.yml"))).unwrap(),
    )
    .unwrap()
}
fn step<'a>(job: &'a Value, name: &str) -> &'a Value {
    job["steps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == name)
        .unwrap()
}
fn filter() -> String {
    let workflow = workflow("docker-publish");
    let run = step(
        &workflow["jobs"]["merge"],
        "Validate admitted pair and merge with attestations",
    )["run"]
        .as_str()
        .unwrap();
    run.split("jq -e '\n")
        .nth(1)
        .unwrap()
        .split("' \"$RUNNER_TEMP/merged-index.json\"")
        .next()
        .unwrap()
        .to_string()
}
fn accepts(value: &Value) -> bool {
    let mut child = Command::new("jq")
        .args(["-e", &filter()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("jq required for release workflow contract");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(value).unwrap())
        .unwrap();
    child.wait_with_output().unwrap().status.success()
}
fn manifest() -> Value {
    let mut rows = vec![];
    for (arch, digest) in [("amd64", "sha256:amd64"), ("arm64", "sha256:arm64")] {
        rows.push(json!({"digest":digest,"platform":{"os":"linux","architecture":arch}}));
        rows.push(json!({"digest":format!("{digest}-attest"),"platform":{"os":"unknown","architecture":"unknown"},"annotations":{"vnd.docker.reference.type":"attestation-manifest","vnd.docker.reference.digest":digest}}));
    }
    json!({"schemaVersion":2,"manifests":rows})
}
#[test]
fn admits_both_native_platforms_and_preserves_their_attestation_descriptors() {
    assert!(accepts(&manifest()));
}
#[test]
fn missing_duplicate_or_unrecognized_platforms_and_lost_attestations_fail() {
    for index in 0..4 {
        let mut value = manifest();
        value["manifests"].as_array_mut().unwrap().remove(index);
        assert!(!accepts(&value), "missing descriptor {index}");
    }
    let mut value = manifest();
    value["manifests"][2]["platform"]["architecture"] = json!("amd64");
    assert!(!accepts(&value));
    let mut value = manifest();
    value["manifests"][2]["platform"]["os"] = json!("windows");
    assert!(!accepts(&value));
    let mut value = manifest();
    value["manifests"][1]["annotations"]["vnd.docker.reference.digest"] = json!("sha256:orphan");
    assert!(!accepts(&value));
    let mut value = manifest();
    value["schemaVersion"] = json!(1);
    assert!(!accepts(&value));
    let mut value = manifest();
    value["manifests"]
        .as_array_mut()
        .unwrap()
        .push(json!({"platform":{"os":"unknown","architecture":"unknown"}}));
    assert!(!accepts(&value));
}
#[test]
fn native_matrix_preserves_publisher_identity_and_enforces_every_size_gate() {
    let workflow = workflow("docker-publish");
    let jobs = &workflow["jobs"];
    let build = &jobs["build-and-push"];
    let matrix = &build["strategy"]["matrix"];
    assert_eq!(build["needs"], json!(["image-size", "supply-chain"]));
    assert_eq!(jobs["merge"]["needs"], "build-and-push");
    assert_eq!(jobs["image-size"]["with"]["policy-only"], true);
    assert_eq!(build["runs-on"], "${{ matrix.host.runner }}");
    assert_eq!(
        matrix["host"],
        json!([{"runner":"ubuntu-24.04","platform":"linux/amd64","arch":"amd64"},{"runner":"ubuntu-24.04-arm","platform":"linux/arm64","arch":"arm64"}])
    );
    let names: Vec<_> = matrix["image"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "ai-gateway",
            "ai-gateway-web",
            "ai-gateway-knowledge-service",
            "ai-gateway-migrate",
            "ai-gateway-code-interpreter"
        ]
    );
    for image in matrix["image"].as_array().unwrap() {
        assert!(image["budget"].as_str().is_some_and(|v| !v.is_empty()));
    }
    for arch in ["amd64", "arm64"] {
        let name = format!("Download exact admitted {arch} digest from this run");
        let download = step(&jobs["merge"], &name);
        assert_eq!(
            download["with"]["name"],
            format!("release-digest-${{{{ matrix.image }}}}-{arch}")
        );
        assert!(download["with"].get("pattern").is_none());
    }
    let build_step = step(build, "Build native image and push by digest");
    assert_eq!(
        build_step["with"]["platforms"],
        "${{ matrix.host.platform }}"
    );
    assert_eq!(build_step["with"]["provenance"], "mode=max");
    assert_eq!(build_step["with"]["sbom"], true);
    assert!(
        build_step["with"]["outputs"]
            .as_str()
            .unwrap()
            .contains("push-by-digest=true")
    );
    let measure = step(build, "Measure the exact pushed platform image");
    let run = measure["run"].as_str().unwrap();
    assert!(run.contains("image_ref=\"$IMAGE_NAME@$IMAGE_DIGEST\""));
    assert!(run.contains("platform-tools image-size-check"));
    assert!(measure.get("continue-on-error").is_none());
    for job in [build, &jobs["merge"]] {
        for s in job["steps"].as_array().unwrap() {
            assert!(!s["uses"].as_str().unwrap_or("").contains("setup-qemu"));
        }
        assert!(
            step(
                job,
                if job == build {
                    "Extract metadata"
                } else {
                    "Extract unchanged release tags"
                }
            )["with"]["tags"]
                .as_str()
                .unwrap()
                .contains("type=sha,format=long")
        );
    }
}
