//! U-PE-04: explicit existing-fixture controller. Import/plan does no networking.
//! No provisioning, signing, deployment/configuration mutation or write retries.
mod config;
mod controller;
mod evidence;
mod identity;
mod mock;
#[cfg(test)]
mod tests;
mod trace;
pub use config::{Profile, validate_profile};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
pub type Result<T> = std::result::Result<T, String>;
pub(crate) fn ensure(ok: bool, code: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(code.into()) }
}
pub(crate) fn sha(bytes: &[u8]) -> String {
    crate::data_security::append_partitions::sha256(bytes)
}
pub fn plan() -> Value {
    json!({"schema":"agentforge-performance-baseline/v1","status":"BLOCKED","findings":["accepted_build_and_owned_profile_required"],"network_executed":false,"acceptance":false,"required":["externally_accepted_build_with_pinned_authority_signature","unchanged_profile_source_binary_image_process_and_privilege_evidence","existing_actor_and_resource_authorized_fixture","correlated_server_trace_boundaries","actual_service_process_samples"],"modes":["deterministic_synthetic_upstream","separately_authorized_real_provider"],"scope":"serial Gateway overhead, Knowledge stages, ingestion throughput and each declared live service RSS; no SLO is invented"})
}
/// Standalone binary and root platform-tools dispatch share this exact parser.
/// Only `run` can create the bounded mock listener or perform local HTTP/inspect.
pub fn command(args: &[String]) -> Result<Value> {
    let command = args.first().map(String::as_str).unwrap_or("plan");
    ensure(
        matches!(command, "plan" | "run"),
        "unknown_performance_command",
    )?;
    let mut flags = BTreeMap::new();
    let mut real = false;
    let mut i = 1;
    while i < args.len() {
        let key = &args[i];
        if key == "--allow-real-provider" {
            ensure(!real, "duplicate_option")?;
            real = true;
            i += 1;
            continue;
        }
        ensure(
            [
                "--profile",
                "--acceptance",
                "--signature",
                "--trust-key",
                "--trust-key-sha256",
                "--repo-root",
                "--output",
            ]
            .contains(&key.as_str()),
            "unknown_option",
        )?;
        let value = args.get(i + 1).ok_or("missing_option_value")?;
        ensure(
            flags.insert(key.clone(), value.clone()).is_none(),
            "duplicate_option",
        )?;
        i += 2;
    }
    let mut report = plan();
    let output = flags.get("--output").map(PathBuf::from);
    if command == "run" {
        ensure(output.is_some(), "fresh_external_output_directory_required")?;
    }
    let complete = [
        "--profile",
        "--acceptance",
        "--signature",
        "--trust-key",
        "--trust-key-sha256",
        "--repo-root",
    ]
    .iter()
    .all(|k| flags.contains_key(*k));
    if output.is_some() {
        ensure(
            flags.contains_key("--repo-root"),
            "repo_root_required_for_external_output",
        )?;
    }
    if !complete {
        if let Some(output) = output {
            evidence::create_output(&output, Some(Path::new(&flags["--repo-root"])))?;
            evidence::save(&output, "report.json", &report)?;
        }
        return Ok(report);
    }
    let root = Path::new(&flags["--repo-root"]);
    if let Some(output) = output.as_ref() {
        evidence::create_output(output, Some(root))?;
        evidence::save(output, "report.json", &report)?;
    }
    let prepared = identity::prepare(&flags);
    match prepared {
        Err(code) => {
            report["findings"] = json!([code]);
        }
        Ok(prepared) => {
            report["profile_sha256"] = json!(prepared.profile_sha256);
            report["accepted_receipt_sha256"] = json!(prepared.receipt_sha256);
            if command == "plan" {
                report["status"] = json!("PLANNED_NOT_EXECUTED");
                report["findings"] =
                    json!(["live_process_image_listener_and_trace_checks_not_run"]);
            } else {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .map_err(|_| "controller_runtime_unavailable")?;
                let directory = output.as_ref().unwrap();
                report = runtime.block_on(controller::run(prepared, directory, real));
            }
        }
    }
    if let Some(output) = output {
        evidence::save(&output, "report.json", &report)?;
        evidence::save(&output, "summary.md", &json!(evidence::markdown(&report)))?;
    }
    Ok(report)
}
