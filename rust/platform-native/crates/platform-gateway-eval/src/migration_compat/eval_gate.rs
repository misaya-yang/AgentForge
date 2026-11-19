//! Count-backed regression gate; thresholds cannot conceal an invalid metrics receipt.
use super::common::{object, text};
use serde_json::{Value, json};
pub const METRICS_SCHEMA: &str = "eval-gate-metrics/v2";
const RATE_TOLERANCE: f64 = 0.00005;
fn rate(value: &Value, label: &str, failures: &mut Vec<String>) -> Option<f64> {
    let value = value
        .as_f64()
        .filter(|v| v.is_finite() && (0.0..=1.0).contains(v));
    if value.is_none() {
        failures.push(format!("{label} must be a finite number in [0, 1]"));
    }
    value
}
fn count(value: &Value, label: &str, failures: &mut Vec<String>) -> Option<u64> {
    let value = value.as_u64();
    if value.is_none() {
        failures.push(format!("{label} must be a non-negative integer"));
    }
    value
}
fn check_exceeds(left: Option<u64>, right: Option<u64>, message: &str, failures: &mut Vec<String>) {
    if left.zip(right).is_some_and(|(l, r)| l > r) {
        failures.push(message.into());
    }
}
fn reconcile_rate(
    total: Option<u64>,
    failed: Option<u64>,
    observed: Option<f64>,
    name: &str,
    count_message: &str,
    rate_message: &str,
    normalized: &mut Value,
    failures: &mut Vec<String>,
) -> Option<f64> {
    if let Some((total, failed)) = total.zip(failed) {
        if failed > total {
            failures.push(count_message.into());
        } else if let Some(observed) = observed {
            let expected = if total > 0 {
                (total - failed) as f64 / total as f64
            } else {
                0.0
            };
            if (expected - observed).abs() > RATE_TOLERANCE {
                failures.push(rate_message.into());
            }
            normalized[name] = json!(expected);
            return Some(expected);
        }
    }
    observed
}
pub fn apply_gate(
    metrics: &Value,
    thresholds: Option<&Value>,
    baseline: Option<&Value>,
    require_critical_coverage: bool,
    require_stateful_coverage: bool,
) -> Value {
    let mut failures = vec![];
    let mut skipped = vec![];
    let mut normalized = json!(object(metrics));
    let names = [
        "overall_score",
        "trajectory_pass_rate",
        "critical_pass_rate",
        "stateful_pass_rate",
        "baseline_tolerance",
    ];
    let mut gate = json!({"overall_score":0.85,"trajectory_pass_rate":0.95,"critical_pass_rate":1.0,"stateful_pass_rate":1.0,"baseline_tolerance":0.02});
    if let Some(thresholds) = thresholds {
        if let Some(map) = thresholds.as_object() {
            let mut unknown = map
                .keys()
                .filter(|k| !names.contains(&k.as_str()))
                .cloned()
                .collect::<Vec<_>>();
            unknown.sort();
            if !unknown.is_empty() {
                failures.push(format!(
                    "unsupported gate thresholds: {}",
                    unknown.join(", ")
                ));
            }
            for name in names {
                if let Some(value) = map.get(name) {
                    if let Some(value) = rate(value, &format!("threshold {name}"), &mut failures) {
                        gate[name] = json!(value);
                    }
                }
            }
        } else {
            failures.push("thresholds must be an object".into());
        }
    }
    if !metrics.is_object() {
        failures.push("metrics must be an object".into());
    }
    if text(metrics, "schema_version") != METRICS_SCHEMA {
        failures.push(format!("metrics require schema_version {METRICS_SCHEMA}"));
    }
    let overall_value = if metrics["overall_score"].is_null() {
        &metrics["average_score"]
    } else {
        &metrics["overall_score"]
    };
    let mut overall = rate(overall_value, "overall_score", &mut failures);
    let mut trajectory_rate = rate(
        &metrics["trajectory_pass_rate"],
        "trajectory_pass_rate",
        &mut failures,
    );
    let pass_rate = rate(&metrics["pass_rate"], "pass_rate", &mut failures);
    let cases = count(&metrics["case_count"], "case_count", &mut failures);
    let score_sum = metrics["score_sum"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.0);
    if score_sum.is_none() {
        failures.push("score_sum must be a finite non-negative number".into());
    }
    let failed = count(
        &metrics["failed_case_count"],
        "failed_case_count",
        &mut failures,
    );
    reconcile_rate(
        cases,
        failed,
        pass_rate,
        "pass_rate",
        "failed_case_count exceeds case_count",
        "pass_rate is inconsistent with case counts",
        &mut normalized,
        &mut failures,
    );
    if let Some((cases, sum)) = cases.zip(score_sum) {
        if sum > cases as f64 {
            failures.push("score_sum exceeds case_count".into());
        }
        let expected = if cases > 0 { sum / cases as f64 } else { 0.0 };
        if overall.is_some_and(|observed| (observed - expected).abs() > RATE_TOLERANCE) {
            failures.push("overall_score is inconsistent with score_sum and case_count".into());
        }
        normalized["overall_score"] = json!(expected);
        normalized["average_score"] = json!(expected);
        overall = Some(expected);
    }
    let trajectory_cases = count(
        &metrics["trajectory_case_count"],
        "trajectory_case_count",
        &mut failures,
    );
    let trajectory_failed = count(
        &metrics["trajectory_failed_count"],
        "trajectory_failed_count",
        &mut failures,
    );
    trajectory_rate = reconcile_rate(
        trajectory_cases,
        trajectory_failed,
        trajectory_rate,
        "trajectory_pass_rate",
        "trajectory_failed_count exceeds trajectory_case_count",
        "trajectory_pass_rate is inconsistent with trajectory case counts",
        &mut normalized,
        &mut failures,
    );
    if cases.zip(trajectory_cases).is_some_and(|(a, b)| a != b) {
        failures.push("trajectory coverage must equal case_count".into());
    }
    check_exceeds(
        trajectory_failed,
        failed,
        "trajectory_failed_count exceeds failed_case_count",
        &mut failures,
    );
    let critical_cases = count(
        &metrics["critical_case_count"],
        "critical_case_count",
        &mut failures,
    );
    let critical_value = metrics
        .get("critical_pass_rate")
        .unwrap_or(&metrics["pass_rate"]);
    let mut critical_rate = if critical_cases == Some(0) {
        if !critical_value.is_null() {
            failures
                .push("critical_pass_rate must be null when critical_case_count is zero".into());
        }
        None
    } else {
        rate(critical_value, "critical_pass_rate", &mut failures)
    };
    let critical_failed = count(
        &metrics["critical_failed_count"],
        "critical_failed_count",
        &mut failures,
    );
    check_exceeds(
        critical_cases,
        cases,
        "critical_case_count exceeds case_count",
        &mut failures,
    );
    critical_rate = reconcile_rate(
        critical_cases,
        critical_failed,
        critical_rate,
        "critical_pass_rate",
        "critical_failed_count exceeds critical_case_count",
        "critical_pass_rate is inconsistent with critical case counts",
        &mut normalized,
        &mut failures,
    );
    check_exceeds(
        critical_failed,
        failed,
        "critical_failed_count exceeds failed_case_count",
        &mut failures,
    );
    let stateful_present = metrics.get("stateful_case_count").is_some();
    let stateful_cases = if stateful_present {
        count(
            &metrics["stateful_case_count"],
            "stateful_case_count",
            &mut failures,
        )
    } else {
        None
    };
    let mut stateful_rate = if stateful_cases == Some(0) {
        if !metrics["stateful_pass_rate"].is_null() {
            failures
                .push("stateful_pass_rate must be null when stateful_case_count is zero".into());
        }
        None
    } else if stateful_cases.is_some() {
        rate(
            &metrics["stateful_pass_rate"],
            "stateful_pass_rate",
            &mut failures,
        )
    } else {
        if metrics.get("stateful_pass_rate").is_some() {
            failures.push("stateful_pass_rate requires stateful_case_count".into());
        }
        None
    };
    let stateful_failed = if stateful_present {
        count(
            &metrics["stateful_failed_count"],
            "stateful_failed_count",
            &mut failures,
        )
    } else {
        None
    };
    if stateful_present {
        stateful_rate = reconcile_rate(
            stateful_cases,
            stateful_failed,
            stateful_rate,
            "stateful_pass_rate",
            "stateful_failed_count exceeds stateful_case_count",
            "stateful_pass_rate is inconsistent with stateful case counts",
            &mut normalized,
            &mut failures,
        );
    }
    check_exceeds(
        stateful_cases,
        cases,
        "stateful_case_count exceeds case_count",
        &mut failures,
    );
    check_exceeds(
        stateful_failed,
        failed,
        "stateful_failed_count exceeds failed_case_count",
        &mut failures,
    );
    for (name, value) in [
        ("overall_score", overall),
        ("trajectory_pass_rate", trajectory_rate),
    ] {
        let threshold = gate[name].as_f64().unwrap();
        if let Some(value) = value.filter(|value| *value < threshold) {
            failures.push(format!("{name} {value:.4} < {threshold:.4}"));
        }
    }
    for (name, count, failed, value, present) in [
        (
            "critical",
            critical_cases,
            critical_failed,
            critical_rate,
            true,
        ),
        (
            "stateful",
            stateful_cases,
            stateful_failed,
            stateful_rate,
            stateful_present,
        ),
    ] {
        if !present {
            continue;
        }
        let rate_name = format!("{name}_pass_rate");
        let threshold = gate[&rate_name].as_f64().unwrap();
        if count == Some(0) {
            skipped.push(format!("{rate_name}:no_{name}_cases"));
        } else if threshold == 1.0 && failed.is_some() {
            if failed != Some(0) {
                failures.push(format!(
                    "{name} hard gate requires zero failed {name} cases"
                ));
            }
        } else if let Some(value) = value.filter(|value| *value < threshold) {
            failures.push(format!("{rate_name} {value:.4} < {threshold:.4}"));
        }
    }
    if require_critical_coverage && critical_cases.is_none_or(|n| n == 0) {
        failures.push("release gate requires non-zero critical case coverage".into());
    }
    if require_stateful_coverage && stateful_cases.is_none_or(|n| n == 0) {
        failures.push("release gate requires non-zero stateful case coverage".into());
    }
    if let Some(baseline) = baseline {
        let empty = json!({});
        let baseline = if baseline.is_object() {
            baseline
        } else {
            failures.push("baseline_metrics must be an object".into());
            &empty
        };
        let thresholds = json!({"overall_score":0.0,"trajectory_pass_rate":0.0,"critical_pass_rate":0.0,"stateful_pass_rate":0.0,"baseline_tolerance":0.0});
        let receipt = apply_gate(baseline, Some(&thresholds), None, false, false);
        let errors = receipt["failures"].as_array().unwrap();
        for error in errors {
            failures.push(format!("baseline {}", error.as_str().unwrap_or("")));
        }
        if errors.is_empty() {
            if let Some((candidate, baseline_score)) =
                overall.zip(receipt["metrics"]["overall_score"].as_f64())
            {
                let allowed = baseline_score - gate["baseline_tolerance"].as_f64().unwrap();
                if candidate < allowed {
                    failures.push(format!(
                        "candidate score {candidate:.4} < baseline tolerance {allowed:.4}"
                    ));
                }
            }
        }
    }
    json!({"status":if failures.is_empty(){"pass"}else{"fail"},"thresholds":gate,"metrics":normalized,"failures":failures,"skipped_thresholds":skipped,"coverage":{"case_count":cases,"trajectory_case_count":trajectory_cases,"critical_case_count":critical_cases,"stateful_case_count":stateful_cases,"critical_required":require_critical_coverage,"stateful_required":require_stateful_coverage}})
}
