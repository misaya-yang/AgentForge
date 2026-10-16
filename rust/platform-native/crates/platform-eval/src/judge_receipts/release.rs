use super::*;
fn load_manifest(root: &Path) -> Result<Value> {
    let path = root.join("src/services/eval/fixtures/real_agent_release_manifest.native-v2.json");
    require(
        !path.is_symlink() && path.is_file(),
        "host release manifest missing or symlinked",
    )?;
    let manifest = load_json(&path)?;
    require(
        constant_eq(&digest(&manifest), MANIFEST_DIGEST),
        "host release manifest digest differs from code-pinned digest",
    )?;
    strict_keys(
        &manifest,
        &["schema_version", "release_id", "suites"],
        &[],
        "release manifest",
    )?;
    require(
        manifest["schema_version"] == "real-agent-release-manifest/v1",
        "unsupported release manifest schema",
    )?;
    identifier(&manifest["release_id"], "release_id")?;
    let suites = manifest["suites"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 16)
        .ok_or("release suites must be bounded nonempty list")?;
    let mut suite_ids = BTreeSet::new();
    let mut scenario_ids = BTreeSet::new();
    let mut domains = vec![];
    for suite in suites {
        strict_keys(
            suite,
            &["suite_id", "contract_sha256", "scenarios"],
            &["coding_host_test_policy"],
            "release suite",
        )?;
        require(
            suite_ids.insert(identifier(&suite["suite_id"], "suite_id")?),
            "duplicate release suite",
        )?;
        hash(&suite["contract_sha256"], "contract digest")?;
        let rows = suite["scenarios"]
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 64)
            .ok_or("release scenarios must be bounded nonempty list")?;
        for scenario in rows {
            strict_keys(
                scenario,
                &["scenario_id", "domain_key", "domain", "repetitions"],
                &[],
                "release scenario",
            )?;
            require(
                scenario_ids.insert(identifier(&scenario["scenario_id"], "scenario_id")?),
                "duplicate release scenario",
            )?;
            domains.push(identifier(&scenario["domain_key"], "domain_key")?);
            bounded(&scenario["domain"], 120, "release domain")?;
            require(
                integer(&scenario["repetitions"], 1, 5, "release repetitions")? == 3,
                "release requires exactly 3 trials",
            )?;
        }
        if rows.iter().any(|s| s["domain_key"] == "coding") {
            let mut policy = crate::coding_receipt::validator_policy(
                &root.join(
                    "tests/fixtures/eval/general_agent/coding_parallel_v2/validator_contract.json",
                ),
                &root.join("rust/platform-native/crates/platform-eval/src/coding_sandbox.rs"),
            )?;
            policy["receipt_schema"] = json!(crate::coding_receipt::RECEIPT_SCHEMA);
            policy["evidence_schema"] = json!("coding-host-test-evidence/v2");
            policy["required_trials"] = json!(3);
            require(
                suite["coding_host_test_policy"] == policy,
                "coding host-test policy is not code pinned",
            )?;
        } else {
            require(
                suite["coding_host_test_policy"].is_null(),
                "non-coding suite declares host-test policy",
            )?;
        }
    }
    require(
        domains.len() == 4
            && domains.iter().map(String::as_str).collect::<BTreeSet<_>>()
                == BTreeSet::from(["coding", "research", "legal", "finance"]),
        "release manifest must contain exactly coding, research, legal, finance",
    )?;
    Ok(manifest)
}
pub fn validate_release_inputs(root: &Path, prepared: &[PreparedInput]) -> Result<Value> {
    let manifest = load_manifest(root)?;
    let expected: BTreeMap<_, _> = arr(&manifest["suites"])
        .iter()
        .map(|s| (text(s, "suite_id"), s))
        .collect();
    let mut actual = BTreeMap::new();
    for p in prepared {
        require(
            actual.insert(p.suite_id.as_str(), p).is_none(),
            "duplicate prepared suite",
        )?;
    }
    require(
        actual.keys().copied().collect::<BTreeSet<_>>() == expected.keys().copied().collect(),
        "release manifest suite set mismatch",
    )?;
    let mut domains = vec![];
    let mut total = 0;
    for (id, suite) in expected.iter() {
        let input = actual[id];
        require(
            constant_eq(
                &input.scenario_contract_sha256,
                text(suite, "contract_sha256"),
            ),
            format!("{id} contract digest is not release-pinned"),
        )?;
        let mut trials = BTreeMap::new();
        for scenario in arr(&suite["scenarios"]) {
            let scenario_id = text(scenario, "scenario_id");
            let domain = text(scenario, "domain");
            domains.push(domain);
            for n in 1..=3 {
                trials.insert((scenario_id, n), domain);
            }
        }
        total += trials.len();
        let mut actual_trials = BTreeMap::new();
        for t in &input.trials {
            require(t.suite_id == *id, "cross-suite trial")?;
            require(
                actual_trials
                    .insert((t.scenario_id(), t.trial()), t)
                    .is_none(),
                "duplicate prepared trial",
            )?;
        }
        require(
            actual_trials.keys().copied().collect::<BTreeSet<_>>()
                == trials.keys().copied().collect(),
            "release scenario/trial set mismatch",
        )?;
        for (identity, domain) in trials {
            require(
                actual_trials[&identity].domain() == domain,
                "domain does not match release manifest",
            )?;
        }
        if let Some(host) = suite
            .get("coding_host_test_policy")
            .filter(|v| !v.is_null())
        {
            let evidence = input
                .coding_host_test_evidence
                .as_ref()
                .ok_or("coding host-test evidence missing")?;
            let mut policy = host.clone();
            for key in ["receipt_schema", "evidence_schema", "required_trials"] {
                policy.as_object_mut().unwrap().remove(key);
            }
            require(
                evidence["schema_version"] == host["evidence_schema"]
                    && evidence["receipt_schema"] == host["receipt_schema"]
                    && evidence["validator_policy"] == policy
                    && arr(&evidence["trials"]).len() as u64
                        == host["required_trials"].as_u64().unwrap_or(0),
                "coding host evidence differs from release policy",
            )?;
        } else {
            require(
                input.coding_host_test_evidence.is_none(),
                "unexpected coding evidence",
            )?;
        }
    }
    require(
        domains.iter().collect::<BTreeSet<_>>().len() == domains.len(),
        "release domains must be unique",
    )?;
    domains.sort();
    Ok(
        json!({"release_id":manifest["release_id"],"manifest_sha256":MANIFEST_DIGEST,"suite_count":expected.len(),"scenario_count":domains.len(),"trial_count":total,"domains":domains}),
    )
}
fn score(dimensions: &Value) -> i64 {
    DIMENSIONS
        .iter()
        .map(|(name, weight, _)| {
            dimensions[name]
                .as_i64()
                .unwrap_or_else(|| dimensions[name].as_f64().unwrap_or(0.) as i64)
                * weight
        })
        .sum()
}
fn audit(call: &JudgeCall) -> Value {
    let dimensions: Value = json!(
        DIMENSIONS
            .iter()
            .map(|(name, _, _)| (
                *name,
                call.result["dimensions"][*name]["score"]
                    .as_f64()
                    .unwrap_or(0.) as i64
            ))
            .collect::<BTreeMap<_, _>>()
    );
    let raw = score(&dimensions) as f64 / 100.;
    json!({"audit_lane":call.audit_lane,"model_requested":call.model_requested,"model_returned":call.model_returned,"request_sha256":call.request_sha256,"response_sha256":call.response_sha256,"content_sha256":call.content_sha256,"usage":call.usage,"raw_score":raw,"score":raw,"dimensions":dimensions,"verdict":call.result["verdict"],"critical_defects":redact(&call.result["critical_defects"]),"unsupported_claims":redact(&call.result["unsupported_claims"]),"confidence":call.result["confidence"],"rationale":redact_text(text(&call.result,"rationale"))})
}
pub async fn evaluate_release(
    root: &Path,
    prepared: &[PreparedInput],
    judge: &dyn DualJudge,
) -> Result<Value> {
    require(
        !prepared.is_empty(),
        "at least one prepared receipt input required",
    )?;
    let manifest = validate_release_inputs(root, prepared)?;
    let trials: Vec<_> = prepared.iter().flat_map(|p| &p.trials).collect();
    let identities: BTreeSet<_> = trials
        .iter()
        .map(|t| (&t.suite_id, t.scenario_id(), t.trial()))
        .collect();
    require(
        identities.len() == trials.len(),
        "duplicate trial identity across inputs",
    )?;
    let mut reports = vec![];
    let mut failures = vec![];
    for t in &trials {
        let mut calls = vec![];
        let mut failure = None;
        for lane in ["A", "B"] {
            match judge.judge(t, lane).await {
                Ok(call) => calls.push(call),
                Err(error) => {
                    failure = Some(format!(
                        "{}:{}:{} judge failed closed: {}",
                        t.suite_id,
                        t.scenario_id(),
                        t.trial(),
                        redact_text(&error.to_string())
                    ));
                    break;
                }
            }
        }
        let mut row = json!({"suite_id":t.suite_id,"scenario_id":t.scenario_id(),"domain":t.domain(),"trial":t.trial(),"answer_sha256":t.observation["candidate_output_sha256"],"merged_receipt_sha256":t.merged_receipt_sha256,"judge_calls":calls.iter().map(audit).collect::<Vec<_>>()});
        if let Some(failure) = failure {
            failures.push(failure.clone());
            row["passed"] = json!(false);
            row["raw_score"] = json!(0.0);
            row["score"] = json!(0.0);
            row["dimensions"] = json!(
                DIMENSIONS
                    .iter()
                    .map(|(n, _, _)| (*n, 0))
                    .collect::<BTreeMap<_, _>>()
            );
            row["failures"] = json!([failure]);
            reports.push(row);
            continue;
        }
        let dimensions: Value = json!(
            DIMENSIONS
                .iter()
                .map(|(n, _, _)| (
                    *n,
                    calls
                        .iter()
                        .map(|c| c.result["dimensions"][*n]["score"].as_f64().unwrap_or(0.) as i64)
                        .min()
                        .unwrap_or(0)
                ))
                .collect::<BTreeMap<_, _>>()
        );
        let raw = score(&dimensions);
        let mut trial_failures = vec![];
        if raw < 9200 {
            trial_failures.push(format!("raw trial score {:.3} < 92.000", raw as f64 / 100.))
        }
        for (name, _, floor) in DIMENSIONS {
            let n = dimensions[name].as_i64().unwrap_or(0);
            if n < floor {
                trial_failures.push(format!("dimension {name} {n} < {floor}"))
            }
        }
        for call in &calls {
            if call.result["verdict"] != "pass" {
                trial_failures.push(format!(
                    "judge lane {} verdict is {}",
                    call.audit_lane,
                    text(&call.result, "verdict")
                ))
            }
            if !arr(&call.result["critical_defects"]).is_empty() {
                trial_failures.push(format!(
                    "judge lane {} reported critical defects",
                    call.audit_lane
                ))
            }
            if !arr(&call.result["unsupported_claims"]).is_empty() {
                trial_failures.push(format!(
                    "judge lane {} reported unsupported claims",
                    call.audit_lane
                ))
            }
        }
        if calls[0].request_sha256 == calls[1].request_sha256 {
            trial_failures.push("independent judge request hashes are identical".into())
        }
        row["passed"] = json!(trial_failures.is_empty());
        row["raw_score"] = json!(raw as f64 / 100.);
        row["score"] = row["raw_score"].clone();
        row["dimensions"] = dimensions;
        row["failures"] = json!(trial_failures);
        failures.extend(
            trial_failures
                .iter()
                .map(|f| format!("{}:{}:{}: {f}", t.suite_id, t.scenario_id(), t.trial())),
        );
        reports.push(row);
    }
    let mut domains = vec![];
    for domain in reports
        .iter()
        .filter_map(|v| v["domain"].as_str())
        .collect::<BTreeSet<_>>()
    {
        let members: Vec<_> = reports.iter().filter(|r| r["domain"] == domain).collect();
        let raw = members
            .iter()
            .map(|r| r["raw_score"].as_f64().unwrap_or(0.))
            .fold(f64::INFINITY, f64::min);
        let mut errors = vec![];
        if members.iter().any(|r| r["passed"] != true) {
            errors.push("one or more domain trials failed".to_owned())
        }
        if raw < 92. {
            errors.push(format!("raw domain score {raw:.3} < 92.000"))
        }
        failures.extend(errors.iter().map(|e| format!("domain {domain}: {e}")));
        domains.push(json!({"domain":domain,"passed":errors.is_empty(),"raw_score":raw,"score":raw,"trial_count":members.len(),"failures":errors}));
    }
    let global = domains
        .iter()
        .map(|d| d["raw_score"].as_f64().unwrap_or(0.))
        .reduce(f64::min)
        .unwrap_or(0.);
    if global < 92. {
        failures.push(format!("raw global score {global:.3} < 92.000"))
    }
    if domains.iter().any(|d| d["passed"] != true) {
        failures.push("one or more domains failed".into())
    }
    Ok(redact(
        &json!({"schema_version":REPORT_SCHEMA,"generated_at":chrono::Utc::now().to_rfc3339(),"status":if failures.is_empty(){"pass"}else{"fail"},"passed":failures.is_empty(),"raw_score":global,"score":global,"threshold":92.0,"aggregation":"minimum dimension across two judges; minimum trial per domain; minimum domain globally","dimension_weights":DIMENSIONS.iter().map(|(n,w,_)|(*n,*w as f64/100.)).collect::<BTreeMap<_,_>>(),"dimension_floors":DIMENSIONS.iter().map(|(n,_,f)|(*n,*f as f64)).collect::<BTreeMap<_,_>>(),"judge":judge.metadata(),"deterministic_preflight":{"passed":true,"release_manifest":manifest,"suite_count":prepared.len(),"trial_count":trials.len(),"scenario_contract_sha256":prepared.iter().map(|p|&p.scenario_contract_sha256).collect::<Vec<_>>(),"merged_receipt_sha256":prepared.iter().map(|p|&p.merged_receipt_sha256).collect::<Vec<_>>()},"failures":failures,"domains":domains,"trials":reports}),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_release_manifest_matches_current_validator_policy() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../..");
        // load_manifest checks the immutable digest and actual native validator policy.
        let manifest = load_manifest(&root).unwrap();
        assert_eq!(
            manifest["release_id"],
            "general-agent-four-domain-release.native-v2"
        );
        let coding = manifest["suites"]
            .as_array()
            .unwrap()
            .iter()
            .find(|suite| suite["suite_id"] == "general-agent.real-coding.v2")
            .unwrap();
        let scenario = load_json(
            &root.join("tests/fixtures/eval/general_agent/coding_parallel_v2/scenario.json"),
        )
        .unwrap();
        assert_eq!(coding["contract_sha256"], digest(&scenario));
        let revision = load_json(
            &root.join("src/services/eval/fixtures/real_agent_release_revision.native-v2.json"),
        )
        .unwrap();
        assert_eq!(revision["manifest"]["canonical_sha256"], MANIFEST_DIGEST);
        assert_ne!(revision["supersedes_manifest_sha256"], MANIFEST_DIGEST);
    }
    #[test]
    fn integer_weighted_score_has_no_threshold_rounding() {
        let mut d = json!({});
        for (n, _, _) in DIMENSIONS {
            d[n] = json!(92);
        }
        assert_eq!(score(&d), 9200);
        d["efficiency"] = json!(91);
        assert_eq!(score(&d), 9195);
    }
}
