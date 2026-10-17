//! Typed Python work-product validation through an explicit, pinned CPython sandbox.
//! No embedded interpreter, partial Python parser, shell interpolation or host execution.
use crate::{Result, canonical::sha256};
use serde_json::{Value, json};
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
const PROJECT_LABEL: &str = "io.agentforge.project";
const PROJECT: &str = "agentforge-dev-7801d4a9a4";
const OWNER_LABEL: &str = "io.agentforge.execution-owner";
const KIND_LABEL: &str = "io.agentforge.execution-kind";
const KIND: &str = "python-work-product-v1";
const HARNESS: &str =
    include_str!("../../../../../tests/fixtures/eval/parity_python_reserve/harness.py");
#[derive(Clone)]
pub struct PythonWorkProduct {
    contract: Value,
    image_id: String,
}
impl PythonWorkProduct {
    /// Called before any model/provider work. Image inspection never pulls/builds.
    pub async fn prepare(path: &Path) -> Result<Self> {
        let contract = crate::strict_json::parse(&std::fs::read(path)?)?;
        crate::seals::strict_keys(
            &contract,
            &[
                "schema_version",
                "sandbox_image_reference",
                "sandbox_image_config_digest",
                "sandbox_platform",
            ],
            &["python_entrypoint"],
            "Python work-product sandbox",
        )?;
        if contract["schema_version"] != "python-work-product-sandbox/v1"
            || !matches!(
                contract["sandbox_platform"].as_str(),
                Some("linux/amd64" | "linux/arm64")
            )
        {
            return Err("invalid_python_work_product_sandbox_contract".into());
        }
        python_entrypoint(&contract)?;
        let image_id = crate::coding_sandbox::inspect_image(&contract).await?;
        let executor = Self { contract, image_id };
        // Verify the interpreter/harness inside the pinned sandbox before starting
        // any external model turn. This is a readiness probe, not quality evidence.
        let probe = json!({"patched_source":"def reserve(stock, requested):\n    if requested < 0: raise ValueError()\n    return {'accepted': requested <= stock, 'remaining': stock-requested if requested <= stock else stock}\n"});
        let (passed, _, _) = executor
            .validate(
                &probe,
                &json!({"required_cases":[[5,5,true,0],[5,6,false,5],[5,0,true,5]]}),
            )
            .await?;
        if !passed {
            return Err("python_work_product_sandbox_probe_failed".into());
        }
        Ok(executor)
    }
    pub async fn validate(&self, value: &Value, expected: &Value) -> Result<(bool, String, Value)> {
        let input = match prepare_input(value, expected) {
            Ok(v) => v,
            Err(reason) => return Ok((false, reason, json!({"execution_state":"not_started"}))),
        };
        let image = self.contract["sandbox_image_reference"]
            .as_str()
            .ok_or("sandbox image missing")?;
        let platform = self.contract["sandbox_platform"]
            .as_str()
            .ok_or("sandbox platform missing")?;
        let owner = ExecutionOwner::new()?;
        let argv = sandbox_argv(image, platform, python_entrypoint(&self.contract)?, &owner);
        let mut command = crate::coding_sandbox::docker();
        command
            .args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = command.spawn()?;
        let mut stdin = child.stdin.take().ok_or("sandbox stdin unavailable")?;
        let stdout = child.stdout.take().ok_or("sandbox stdout unavailable")?;
        let bytes = serde_json::to_vec(&input)?;
        let execution = async {
            let write = async {
                stdin.write_all(&bytes).await?;
                stdin.shutdown().await?;
                Ok::<_, std::io::Error>(())
            };
            let read = async {
                let mut output = Vec::new();
                stdout.take(65537).read_to_end(&mut output).await?;
                Ok::<_, std::io::Error>(output)
            };
            let ((), output, status) = tokio::try_join!(write, read, child.wait())?;
            Ok::<_, std::io::Error>((output, status))
        };
        let result = tokio::time::timeout(Duration::from_secs(10), execution).await;
        let (output, status) = match result {
            Ok(Ok(v)) => v,
            _ => {
                // Timeout/disconnect is unknown, never a candidate pass or replay.
                let cleanup = cleanup_owned(&owner, &self.image_id).await;
                return Err(if cleanup.is_ok() {
                    "python_work_product_execution_unknown"
                } else {
                    "python_work_product_cleanup_unknown"
                }
                .into());
            }
        };
        if !status.success() || output.len() > 65536 {
            return Err("python_work_product_execution_unknown".into());
        }
        let report = crate::strict_json::parse(&output)
            .map_err(|_| "python_work_product_invalid_sandbox_report")?;
        let (passed, reason) = score_report(&report, expected)?;
        Ok((
            passed,
            reason,
            json!({"execution_state":"completed","backend":"cpython-sandbox/v1","image":image,"image_id":self.image_id,"python_entrypoint":python_entrypoint(&self.contract)?,"project":PROJECT,"execution_owner":owner.token,"container_id":owner.container_id()?,"harness_sha256":sha256(HARNESS.as_bytes()),"source_sha256":sha256(input["source"].as_str().unwrap().as_bytes()),"oracle_sha256":sha256(&serde_json::to_vec(expected)?),"report":report}),
        ))
    }
}
fn python_entrypoint(contract: &Value) -> Result<&str> {
    match contract.get("python_entrypoint") {
        None => Ok("/usr/local/bin/python3"),
        Some(Value::String(path))
            if matches!(path.as_str(), "/usr/local/bin/python3" | "/usr/bin/python3") =>
        {
            Ok(path)
        }
        _ => Err("python_work_product_entrypoint_must_be_known_absolute_python3".into()),
    }
}
struct ExecutionOwner {
    name: String,
    token: String,
    directory: tempfile::TempDir,
}
impl ExecutionOwner {
    fn new() -> Result<Self> {
        let token = uuid::Uuid::new_v4().simple().to_string();
        Ok(Self {
            name: format!("native-python-work-product-{}-{token}", std::process::id()),
            token,
            directory: tempfile::Builder::new()
                .prefix("native-python-execution-")
                .tempdir()?,
        })
    }
    fn cidfile(&self) -> std::path::PathBuf {
        self.directory.path().join("container.id")
    }
    fn container_id(&self) -> Result<String> {
        let id = std::fs::read_to_string(self.cidfile())
            .map_err(|_| "python_work_product_container_identity_unknown")?;
        let id = id.trim();
        if id.len() != 64
            || !id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("python_work_product_container_identity_invalid".into());
        }
        Ok(id.to_owned())
    }
}
fn sandbox_argv(
    image: &str,
    platform: &str,
    entrypoint: &str,
    owner: &ExecutionOwner,
) -> Vec<String> {
    [
        "run",
        "--rm",
        "--pull",
        "never",
        "--platform",
        platform,
        "--name",
        &owner.name,
        "--cidfile",
        &owner.cidfile().to_string_lossy(),
        "--label",
        &format!("{PROJECT_LABEL}={PROJECT}"),
        "--label",
        &format!("{OWNER_LABEL}={}", owner.token),
        "--label",
        &format!("{KIND_LABEL}={KIND}"),
        "--network",
        "none",
        "--read-only",
        "--cap-drop",
        "ALL",
        "--security-opt",
        "no-new-privileges",
        "--pids-limit",
        "32",
        "--memory",
        "128m",
        "--cpus",
        "0.5",
        "--user",
        "65534:65534",
        "--tmpfs",
        "/tmp:rw,noexec,nosuid,size=8m",
        "--log-driver",
        "none",
        "--entrypoint",
        entrypoint,
        "-i",
        image,
        "-I",
        "-S",
        "-c",
        HARNESS,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
fn verify_owned_container(
    value: &Value,
    owner: &ExecutionOwner,
    id: &str,
    image_id: &str,
) -> Result<()> {
    let labels = &value["Config"]["Labels"];
    if value["Id"] != id
        || value["Name"] != format!("/{}", owner.name)
        || value["Image"] != image_id
        || labels[PROJECT_LABEL] != PROJECT
        || labels[OWNER_LABEL] != owner.token
        || labels[KIND_LABEL] != KIND
    {
        return Err("python_work_product_container_ownership_mismatch".into());
    }
    Ok(())
}
async fn cleanup_owned(owner: &ExecutionOwner, image_id: &str) -> Result<()> {
    // CID comes only from this invocation's private cidfile. Never delete by name:
    // a different container may have acquired the name after an auto-removal.
    let id = owner.container_id()?;
    let inspected = tokio::time::timeout(
        Duration::from_secs(10),
        crate::coding_sandbox::docker()
            .args(["container", "inspect", "--format", "{{json .}}", &id])
            .output(),
    )
    .await??;
    if !inspected.status.success() {
        return Err("python_work_product_container_inspection_unknown".into());
    }
    let value = crate::strict_json::parse(&inspected.stdout)?;
    verify_owned_container(&value, owner, &id, image_id)?;
    let removed = tokio::time::timeout(
        Duration::from_secs(10),
        crate::coding_sandbox::docker()
            .args(["rm", "-f", &id])
            .stdout(Stdio::null())
            .status(),
    )
    .await??;
    if !removed.success() {
        return Err("python_work_product_container_cleanup_unknown".into());
    }
    Ok(())
}

fn prepare_input(value: &Value, expected: &Value) -> std::result::Result<Value, String> {
    if !value
        .as_object()
        .is_some_and(|o| o.len() == 1 && o.contains_key("patched_source"))
        || !value["patched_source"].is_string()
    {
        return Err("patched_source_contract".into());
    }
    let source = value["patched_source"].as_str().unwrap();
    if source.len() > 4096 {
        return Err("patched_source_too_large".into());
    }
    let cases = expected["required_cases"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 128)
        .ok_or("reserve_oracle_cases_missing")?;
    if cases.iter().any(|c| {
        c.as_array().is_none_or(|a| a.len() != 4)
            || !c[0].is_i64()
            || !c[1].is_i64()
            || !c[2].is_boolean()
            || !c[3].is_i64()
    }) {
        return Err("reserve_oracle_case_invalid".into());
    }
    Ok(json!({"source":source,"required_cases":cases}))
}
fn python_equal(a: &Value, b: &Value) -> bool {
    let numeric = |v: &Value| {
        v.as_f64()
            .or_else(|| v.as_bool().map(|b| if b { 1.0 } else { 0.0 }))
    };
    if let (Some(a), Some(b)) = (numeric(a), numeric(b)) {
        return a == b;
    }
    match (a, b) {
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|w| python_equal(v, w)))
        }
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| python_equal(a, b))
        }
        _ => a == b,
    }
}
fn score_report(report: &Value, expected: &Value) -> Result<(bool, String)> {
    if let Some(error) = report["validation_error"].as_str() {
        if report.as_object().is_none_or(|o| o.len() != 1)
            || !(matches!(
                error,
                "patched_source_syntax"
                    | "patched_source_must_define_one_function"
                    | "patched_source_function_name"
                    | "patched_source_unsafe_ast"
                    | "patched_source_unsafe_call"
            ) || error.starts_with("patched_source_runtime:"))
        {
            return Err("invalid_python_work_product_diagnostic".into());
        }
        return Ok((false, error.into()));
    }
    crate::seals::strict_keys(
        report,
        &["cases", "negative"],
        &[],
        "Python work-product result",
    )?;
    let expected_cases = expected["required_cases"]
        .as_array()
        .ok_or("reserve oracle missing")?;
    let cases = report["cases"]
        .as_array()
        .filter(|a| a.len() == expected_cases.len())
        .ok_or("sandbox case coverage mismatch")?;
    for (result, case) in cases.iter().zip(expected_cases) {
        if let Some(error) = result["error"].as_str() {
            return Ok((false, format!("patched_source_runtime:{error}")));
        }
        crate::seals::strict_keys(result, &["value"], &[], "Python return value")?;
        if !python_equal(
            &result["value"],
            &json!({"accepted":case[2],"remaining":case[3]}),
        ) {
            return Ok((
                false,
                format!("reserve_case_failed:{}:{}", case[0], case[1]),
            ));
        }
    }
    let negative = &report["negative"];
    if negative == &json!({"error":"ValueError"}) {
        return Ok((true, "passed".into()));
    }
    if let Some(error) = negative["error"].as_str() {
        return Ok((false, format!("patched_source_runtime:{error}")));
    }
    crate::seals::strict_keys(negative, &["value"], &[], "negative Python return")?;
    Ok((false, "negative_request_not_rejected".into()))
}
/// Explicit execution regression gate for the retired interpreter's source cases.
/// No fixture assertion is satisfied without an actual sandbox execution.
pub async fn verify_fixtures(contract: &Path) -> Result<Value> {
    let executor = PythonWorkProduct::prepare(contract).await?;
    let fixtures: Value = serde_json::from_str(include_str!(
        "../../../../../tests/fixtures/eval/parity_python_reserve/cases.json"
    ))?;
    let mut receipts = Vec::new();
    for case in fixtures.as_array().ok_or("reserve fixtures missing")? {
        let (passed, reason, receipt) = executor
            .validate(&case["answer"], &case["expected"])
            .await?;
        if passed
            != case["passed"]
                .as_bool()
                .ok_or("fixture pass oracle missing")?
            || reason != case["reason"].as_str().ok_or("fixture reason missing")?
        {
            return Err(format!("reserve fixture mismatch:{}:{reason}", case["id"]).into());
        }
        receipts.push(json!({"case_id":case["id"],"assertion_passed":true,"execution":receipt}));
    }
    Ok(json!({"passed":true,"cases":receipts,"quality_acceptance":false}))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn worker_entrypoint_is_overridden_and_container_has_execution_ownership() {
        let owner = ExecutionOwner::new().unwrap();
        let image = "example.invalid/python@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let args = sandbox_argv(image, "linux/amd64", "/usr/bin/python3", &owner);
        let entry = args.iter().position(|s| s == "--entrypoint").unwrap();
        assert_eq!(args[entry + 1], "/usr/bin/python3");
        assert!(entry < args.iter().position(|s| s == image).unwrap());
        assert!(!args.iter().any(|s| s == "python3"
            || s == "-v"
            || s == "--mount"
            || s == "--env"
            || s == "-e"));
        for pair in [
            ["--pull", "never"],
            ["--network", "none"],
            ["--pids-limit", "32"],
            ["--memory", "128m"],
            ["--cpus", "0.5"],
            ["--user", "65534:65534"],
        ] {
            assert!(
                args.windows(2)
                    .any(|window| window[0] == pair[0] && window[1] == pair[1])
            );
        }
        for label in [
            format!("{PROJECT_LABEL}={PROJECT}"),
            format!("{OWNER_LABEL}={}", owner.token),
            format!("{KIND_LABEL}={KIND}"),
        ] {
            assert!(args.windows(2).any(|w| w[0] == "--label" && w[1] == label));
        }
        assert!(python_entrypoint(&json!({"python_entrypoint":"python3"})).is_err());
        assert!(python_entrypoint(&json!({"python_entrypoint":"/bin/sh"})).is_err());
        assert_eq!(
            python_entrypoint(&json!({})).unwrap(),
            "/usr/local/bin/python3"
        );
    }
    #[test]
    fn cleanup_authority_requires_exact_cid_name_image_project_and_owner_token() {
        let owner = ExecutionOwner::new().unwrap();
        let id = "a".repeat(64);
        let image = "sha256:fixed-config";
        std::fs::write(owner.cidfile(), &id).unwrap();
        assert_eq!(owner.container_id().unwrap(), id);
        let inspected = json!({"Id":id,"Name":format!("/{}",owner.name),"Image":image,"Config":{"Labels":{(PROJECT_LABEL):PROJECT,(OWNER_LABEL):owner.token,(KIND_LABEL):KIND}}});
        verify_owned_container(&inspected, &owner, &id, image).unwrap();
        for key in ["Id", "Name", "Image"] {
            let mut other = inspected.clone();
            other[key] = json!("different");
            assert!(verify_owned_container(&other, &owner, &id, image).is_err());
        }
        for key in [PROJECT_LABEL, OWNER_LABEL, KIND_LABEL] {
            let mut other = inspected.clone();
            other["Config"]["Labels"][key] = json!("different");
            assert!(verify_owned_container(&other, &owner, &id, image).is_err());
        }
        let mut missing = inspected;
        missing["Config"]["Labels"] = Value::Null;
        assert!(verify_owned_container(&missing, &owner, &id, image).is_err());
        std::fs::write(owner.cidfile(), "name-instead-of-immutable-id").unwrap();
        assert!(owner.container_id().is_err());
    }
    fn oracle() -> Value {
        json!({"required_cases":[[5,2,true,3],[5,5,true,0],[5,6,false,5],[0,0,true,0]]})
    }
    #[test]
    fn typed_scoring_preserves_all_cases_and_negative_exception() {
        let expected = oracle();
        let report = json!({"cases":[{"value":{"accepted":true,"remaining":3}},{"value":{"accepted":true,"remaining":0}},{"value":{"accepted":false,"remaining":5}},{"value":{"accepted":true,"remaining":0}}],"negative":{"error":"ValueError"}});
        assert_eq!(
            score_report(&report, &expected).unwrap(),
            (true, "passed".into())
        );
        let mut bad = report.clone();
        bad["cases"][1]["value"]["accepted"] = json!(false);
        assert!(!score_report(&bad, &expected).unwrap().0);
        let mut bad = report.clone();
        bad["negative"] = json!({"value":{"accepted":true,"remaining":6}});
        assert_eq!(
            score_report(&bad, &expected).unwrap().1,
            "negative_request_not_rejected"
        );
        let mut bad = report;
        bad["cases"].as_array_mut().unwrap().pop();
        assert!(score_report(&bad, &expected).is_err());
    }
    #[test]
    fn no_code_execution_or_score_is_inferred_from_preparation() {
        assert!(prepare_input(&json!({"patched_source":"candidate"}), &oracle()).is_ok());
        assert!(prepare_input(&json!({"patched_source":"x".repeat(4097)}), &oracle()).is_err());
        assert!(prepare_input(&json!({"patched_source":"candidate"}), &json!({})).is_err());
        assert!(score_report(&json!({"passed":true}), &oracle()).is_err());
    }
}
