//! Fail-closed image size policy. No Docker invocation, startup or publication.
use crate::*;
use serde::Deserialize;

const TARGETS: [&str; 9] = [
    "runtime",
    "knowledge",
    "migrate",
    "sdk-worker",
    "local-node",
    "tools",
    "release",
    "frontend",
    "code-interpreter",
];
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema_version: String,
    metric: String,
    // Integer MiB ceilings, identical on amd64 and arm64; measurements never round.
    targets: BTreeMap<String, u64>,
}
fn valid_id(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn evaluate(
    policy: Value,
    inspect: Value,
    target: &str,
    platform: &str,
    expected_id: &str,
) -> Result<Value> {
    require(TARGETS.contains(&target), "unknown image-size target")?;
    let architecture = match platform {
        "linux/amd64" => "amd64",
        "linux/arm64" => "arm64",
        _ => return Err("unsupported image-size platform".into()),
    };
    require(valid_id(expected_id), "invalid expected image ID")?;
    let policy: Policy = serde_json::from_value(policy)?;
    require(
        policy.schema_version == "ai-platform/image-size-budgets/v1"
            && policy.metric == "docker-image-inspect-size-bytes",
        "unsupported image-size policy schema or metric",
    )?;
    require(
        policy.targets.len() == TARGETS.len()
            && TARGETS.iter().all(|t| policy.targets.contains_key(*t))
            && policy.targets.values().all(|mib| (1..=4096).contains(mib)),
        "image-size policy must cover exactly nine targets with valid MiB ceilings",
    )?;
    let rows = inspect.as_array().ok_or("image inspect must be an array")?;
    require(
        rows.len() == 1,
        "image inspect must contain exactly one image",
    )?;
    let row = rows[0]
        .as_object()
        .ok_or("image inspect row must be an object")?;
    require(
        row.get("Id").and_then(Value::as_str) == Some(expected_id),
        "inspected image ID differs from built image ID",
    )?;
    require(
        row.get("Os").and_then(Value::as_str) == Some("linux")
            && row.get("Architecture").and_then(Value::as_str) == Some(architecture),
        "inspected image platform differs from requested platform",
    )?;
    let size = row
        .get("Size")
        .and_then(Value::as_u64)
        .ok_or("image Size must be unsigned integer bytes")?;
    let ceiling = policy.targets[target] * 1024 * 1024;
    require(
        size > 0 && size <= ceiling,
        format!("{target} image size {size} bytes outside allowed 1..={ceiling}"),
    )?;
    Ok(
        json!({"schema_version":"ai-platform/image-size-check/v1","gate":"image-size-check","result":"pass","target":target,
        "platform":platform,"image_id":expected_id,"metric":"docker-image-inspect-size-bytes",
        "measured_bytes":size,"ceiling_bytes":ceiling,
        "compressed_transfer_bytes":"not_measured","runtime_security_acceptance":"not_checked"}),
    )
}
pub fn run(args: &Args) -> Result<Value> {
    require(args.flags.is_empty(), "image-size-check takes no flags")?;
    require(
        args.positionals.len() == 1,
        "unexpected image-size positional argument",
    )?;
    for (name, values) in &args.multiple {
        require(
            [
                "--repo-root",
                "--target",
                "--platform",
                "--image-id",
                "--inspect",
                "--evidence-out",
            ]
            .contains(&name.as_str())
                && values.len() == 1,
            "unknown or repeated image-size option",
        )?;
    }
    let policy_path = args
        .root()
        .join("deploy/platform-native/image-size-budgets.json");
    let mut receipt = evaluate(
        strict_json(&std::fs::read(&policy_path)?)?,
        json_input(args.get("--inspect")?)?,
        args.get("--target")?,
        args.get("--platform")?,
        args.get("--image-id")?,
    )?;
    receipt["policy_sha256"] = digest(&policy_path)?.into();
    Ok(receipt)
}
#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    fn policy() -> Value {
        json!({"schema_version":"ai-platform/image-size-budgets/v1","metric":"docker-image-inspect-size-bytes","targets":TARGETS.into_iter().map(|t|(t, 1)).collect::<BTreeMap<_,_>>()})
    }
    fn inspect(size: Value) -> Value {
        json!([{"Id":ID,"Size":size,"Os":"linux","Architecture":"amd64"}])
    }
    fn check(policy: Value, inspect: Value) -> Result<Value> {
        evaluate(policy, inspect, "runtime", "linux/amd64", ID)
    }
    #[test]
    fn byte_boundary_is_inclusive_without_rounding() {
        assert_eq!(
            check(policy(), inspect(json!(1048576))).unwrap()["measured_bytes"],
            1048576
        );
        assert!(check(policy(), inspect(json!(1048577))).is_err());
        assert!(check(policy(), inspect(json!(0))).is_err());
    }
    #[test]
    fn malformed_missing_and_overflow_sizes_are_rejected() {
        for size in [
            Value::Null,
            json!(-1),
            json!(1.5),
            json!("100"),
            json!(true),
            json!(u64::MAX),
        ] {
            assert!(check(policy(), inspect(size)).is_err());
        }
        let mut absent = inspect(json!(1));
        absent[0].as_object_mut().unwrap().remove("Size");
        assert!(check(policy(), absent).is_err());
    }
    #[test]
    fn missing_duplicate_and_wrong_platform_images_fail_closed() {
        for rows in [json!([]), json!({}), json!([{}, {}]), json!([null])] {
            assert!(check(policy(), rows).is_err());
        }
        let image = inspect(json!(1));
        assert!(check(policy(), json!([image[0], image[0]])).is_err());
        for field in ["Id", "Os", "Architecture"] {
            let mut wrong = image.clone();
            wrong[0][field] = json!("wrong");
            assert!(check(policy(), wrong).is_err());
        }
        assert!(evaluate(policy(), image.clone(), "unknown", "linux/amd64", ID).is_err());
        assert!(evaluate(policy(), image.clone(), "runtime", "linux/386", ID).is_err());
        assert!(evaluate(policy(), image, "runtime", "linux/amd64", "sha256:invalid").is_err());
    }
    #[test]
    fn arm64_has_the_same_ceiling() {
        let mut image = inspect(json!(1048576));
        image[0]["Architecture"] = json!("arm64");
        assert!(evaluate(policy(), image.clone(), "runtime", "linux/arm64", ID).is_ok());
        image[0]["Size"] = json!(1048577);
        assert!(evaluate(policy(), image, "runtime", "linux/arm64", ID).is_err());
    }
    #[test]
    fn incomplete_typoed_and_invalid_policies_are_rejected() {
        for key in ["schema_version", "metric", "targets"] {
            let mut p = policy();
            p.as_object_mut().unwrap().remove(key);
            assert!(check(p, inspect(json!(1))).is_err());
        }
        for limit in [json!(0), json!(4097), json!("1"), json!(-1), json!(1.5)] {
            let mut p = policy();
            p["targets"]["runtime"] = limit;
            assert!(check(p, inspect(json!(1))).is_err());
        }
        let mut missing = policy();
        missing["targets"].as_object_mut().unwrap().remove("tools");
        assert!(check(missing, inspect(json!(1))).is_err());
        for (field, value) in [
            ("extra", json!(true)),
            ("schema_version", json!("wrong")),
            ("metric", json!("compressed-bytes")),
        ] {
            let mut p = policy();
            p[field] = value;
            assert!(check(p, inspect(json!(1))).is_err());
        }
        let mut extra = policy();
        extra["targets"]["typo"] = json!(1);
        assert!(check(extra, inspect(json!(1))).is_err());
    }
}
