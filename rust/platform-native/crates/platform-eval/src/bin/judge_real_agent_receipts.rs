use ai_platform_eval::{Result, judge_receipts::*};
use serde_json::Value;
use std::path::PathBuf;
#[tokio::main]
async fn main() {
    let code = match run().await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{}", redact_text(&error.to_string()));
            2
        }
    };
    std::process::exit(code)
}
async fn run() -> Result<i32> {
    let mut scenarios = vec![];
    let mut receipts = vec![];
    let mut digests = vec![];
    let mut output = None;
    let mut coding = None;
    let mut repository = std::env::current_dir()?;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--help" || arg == "-h" {
            println!(
                "judge_real_agent_receipts --scenarios CONTRACT --receipts SEALED --expected-receipt-sha256 SHA [repeat matched inputs] --output REPORT [--coding-host-test-receipt PATH] [--repo-root PATH]\nDeterministic preflight precedes DeepSeek environment credentials and all requests."
            );
            return Ok(0);
        }
        let value = args
            .next()
            .ok_or_else(|| format!("{arg} requires a value"))?;
        match arg.as_str() {
            "--scenarios" => scenarios.push(PathBuf::from(value)),
            "--receipts" => receipts.push(PathBuf::from(value)),
            "--expected-receipt-sha256" => digests.push(value),
            "--output" => output = Some(PathBuf::from(value)),
            "--coding-host-test-receipt" => coding = Some(PathBuf::from(value)),
            "--repo-root" => repository = PathBuf::from(value),
            _ => return Err(format!("unknown option {arg}").into()),
        }
    }
    let output = output.ok_or("--output is required")?;
    let prepare = (|| -> Result<Vec<PreparedInput>> {
        if scenarios.is_empty()
            || scenarios.len() != receipts.len()
            || scenarios.len() != digests.len()
        {
            return Err(
                "--scenarios, --receipts, and --expected-receipt-sha256 counts must match".into(),
            );
        }
        let mut items = vec![];
        for ((scenario, receipt), digest) in scenarios.iter().zip(&receipts).zip(&digests) {
            let contract = load_json(scenario)?;
            let merged = load_json(receipt)?;
            let path = scenario.canonicalize()?;
            let contains_coding = contract["scenarios"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|s| s["scenario_id"] == ai_platform_eval::coding_receipt::SCENARIO_ID);
            items.push(prepare_input(
                &contract,
                &merged,
                &PrepareOptions {
                    repository: &repository,
                    scenario_directory: path.parent().ok_or("scenario parent missing")?,
                    expected_receipt_sha256: digest,
                    collector_hmac_key: None,
                    golden_hmac_key: None,
                    coding_host_test_hmac_key: None,
                    coding_host_test_receipt_path: if contains_coding {
                        coding.as_deref()
                    } else {
                        None
                    },
                    expected_suite_nonce: None,
                },
            )?);
        }
        validate_release_inputs(&repository, &items)?;
        Ok(items)
    })();
    let prepared = match prepare {
        Ok(items) => items,
        Err(error) => {
            let report = failure_report(&error.to_string(), None);
            write_report(&output, &report)?;
            eprintln!(
                "{}",
                report["failures"][0]
                    .as_str()
                    .unwrap_or("deterministic preflight failed")
            );
            return Ok(2);
        }
    };
    let report: Value = match DeepSeekSettings::from_env().and_then(DeepSeekDualJudge::new) {
        Ok(judge) => match evaluate_release(&repository, &prepared, &judge).await {
            Ok(report) => report,
            Err(e) => failure_report(&e.to_string(), Some(&prepared)),
        },
        Err(e) => failure_report(&e.to_string(), Some(&prepared)),
    };
    write_report(&output, &report)?;
    println!(
        "real-agent release judge {}: raw_score={:.3}, report={}",
        report["status"].as_str().unwrap_or("fail"),
        report["raw_score"].as_f64().unwrap_or(0.),
        output.display()
    );
    Ok(if report["passed"] == true { 0 } else { 1 })
}
