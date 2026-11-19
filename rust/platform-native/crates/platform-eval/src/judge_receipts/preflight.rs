use super::*;
pub struct PrepareOptions<'a> {
    pub repository: &'a Path,
    pub scenario_directory: &'a Path,
    pub expected_receipt_sha256: &'a str,
    pub collector_hmac_key: Option<&'a str>,
    pub golden_hmac_key: Option<&'a str>,
    pub coding_host_test_hmac_key: Option<&'a str>,
    pub coding_host_test_receipt_path: Option<&'a Path>,
    pub expected_suite_nonce: Option<&'a str>,
}
fn key(value: Option<&str>, env: &str) -> Result<String> {
    let value = value
        .map(str::to_owned)
        .unwrap_or_else(|| std::env::var(env).unwrap_or_default());
    require(
        value.len() >= 32,
        format!("verification requires at least 32 bytes from {env}"),
    )?;
    Ok(value)
}
fn attestation(v: &Value, payload: &Value, key: &str) -> Result<String> {
    strict_keys(
        v,
        &["algorithm", "key_id", "digest"],
        &[],
        "HMAC attestation",
    )?;
    require(
        v["algorithm"] == "hmac-sha256",
        "attestation must use hmac-sha256",
    )?;
    let id = sha256(key.as_bytes())[..24].to_owned();
    require(
        constant_eq(text(v, "key_id"), &id),
        "attestation key identity mismatch",
    )?;
    require(
        constant_eq(
            &hash(&v["digest"], "HMAC digest")?,
            &crate::seals::hmac_hex(payload, key)?,
        ),
        "HMAC attestation does not match receipt bindings",
    )?;
    Ok(id)
}
fn scenarios(contract: &Value) -> Result<BTreeMap<String, Value>> {
    strict_keys(
        contract,
        &["schema_version", "suite_id", "scenarios"],
        &[],
        "scenario contract",
    )?;
    require(
        contract["schema_version"] == "real-agent-scenarios/v1",
        "unsupported scenario schema",
    )?;
    identifier(&contract["suite_id"], "scenario suite_id")?;
    let rows = contract["scenarios"]
        .as_array()
        .filter(|a| !a.is_empty() && a.len() <= 64)
        .ok_or("scenario contract must contain 1-64 scenarios")?;
    let mut map = BTreeMap::new();
    for raw in rows {
        strict_keys(
            raw,
            &[
                "scenario_id",
                "domain",
                "prompt",
                "repetitions",
                "required_agent_ids",
                "require_parallel",
                "expected_assertions",
            ],
            &[
                "answer_locator",
                "model_id",
                "max_tokens",
                "execution_profile",
                "source_artifacts",
                "delegation_task_requirements",
                "canonical_delegation",
                "output_conformance_hints",
            ],
            "scenario",
        )?;
        let id = identifier(&raw["scenario_id"], "scenario_id")?;
        require(!map.contains_key(&id), "duplicate scenario_id")?;
        bounded(&raw["domain"], 120, "domain")?;
        bounded(&raw["prompt"], 40000, "prompt")?;
        require(
            integer(&raw["repetitions"], 1, 5, "repetitions")? == 3,
            "release evaluation requires exactly 3 trials",
        )?;
        if let Some(hints) = raw.get("output_conformance_hints") {
            let hints = hints
                .as_array()
                .filter(|a| a.len() <= 32)
                .ok_or("invalid output hints")?;
            let values = hints
                .iter()
                .map(|v| bounded(v, 800, "output hint"))
                .collect::<Result<Vec<_>>>()?;
            require(
                values.iter().collect::<BTreeSet<_>>().len() == values.len(),
                "duplicate output hints",
            )?;
        }
        let agents = raw["required_agent_ids"]
            .as_array()
            .filter(|a| a.len() <= 5)
            .ok_or("required_agent_ids must be a list")?;
        let agents = agents
            .iter()
            .map(|v| identifier(v, "required agent id"))
            .collect::<Result<Vec<_>>>()?;
        require(
            agents.iter().collect::<BTreeSet<_>>().len() == agents.len(),
            "duplicate agent IDs",
        )?;
        let parallel = raw["require_parallel"]
            .as_bool()
            .ok_or("require_parallel must be boolean")?;
        require(
            !parallel || agents.len() >= 2,
            "parallel requirement needs two agents",
        )?;
        let delegation = &raw["canonical_delegation"];
        if !agents.is_empty() {
            strict_keys(
                delegation,
                &["max_concurrency", "tasks", "canonical_sha256"],
                &[],
                "canonical delegation",
            )?;
            require(
                delegation["max_concurrency"].as_u64()
                    == Some(if parallel { agents.len() as u64 } else { 1 }),
                "canonical delegation concurrency must be exact",
            )?;
            let tasks = delegation["tasks"]
                .as_array()
                .filter(|a| a.len() == agents.len())
                .ok_or("one canonical task per agent required")?;
            for (task, agent) in tasks.iter().zip(&agents) {
                strict_keys(
                    task,
                    &["prompt", "description"],
                    &["agent_id", "agent_type"],
                    "canonical task",
                )?;
                let aid = text(task, "agent_id");
                let kind = text(task, "agent_type");
                require(
                    aid.is_empty() != kind.is_empty(),
                    "canonical task needs exactly one agent identity",
                )?;
                let identity = if !aid.is_empty() {
                    identifier(&task["agent_id"], "agent_id")?
                } else {
                    format!("builtin:{}", identifier(&task["agent_type"], "agent_type")?)
                };
                require(
                    &identity == agent,
                    "canonical task order differs from required agents",
                )?;
                bounded(&task["description"], 200, "task description")?;
                bounded(&task["prompt"], 20000, "task prompt")?;
            }
            require(
                hash(
                    &delegation["canonical_sha256"],
                    "canonical delegation digest",
                )? == digest(
                    &json!({"tasks":tasks,"max_concurrency":delegation["max_concurrency"]}),
                ),
                "canonical delegation digest mismatch",
            )?;
        } else {
            require(
                delegation.is_null() || delegation == &json!({}),
                "canonical delegation without agents",
            )?;
        }
        let assertions = raw["expected_assertions"]
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 64)
            .ok_or("expected_assertions must be nonempty")?;
        let mut ids = BTreeSet::new();
        for a in assertions {
            strict_keys(
                a,
                &["assertion_id", "kind", "expected"],
                &[
                    "description",
                    "path",
                    "absolute_tolerance",
                    "relative_tolerance",
                ],
                "assertion",
            )?;
            require(
                ids.insert(identifier(&a["assertion_id"], "assertion_id")?),
                "duplicate assertion IDs",
            )?;
        }
        let mut normalized = raw.clone();
        normalized["allowed_parent_tools"] = json!(["spawn_subagent"]);
        map.insert(id, normalized);
    }
    Ok(map)
}
fn plugins(root: &Path, scenarios: &BTreeMap<String, Value>) -> Result<BTreeMap<String, String>> {
    let root = root.join("agent-plugins").canonicalize()?;
    let mut out = BTreeMap::new();
    let component = regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$")?;
    for s in scenarios.values() {
        for id in arr(&s["required_agent_ids"])
            .iter()
            .filter_map(Value::as_str)
        {
            if !id.contains(':') || id.starts_with("builtin:") {
                continue;
            }
            let parts: Vec<_> = id.split(':').collect();
            require(
                parts.len() == 2
                    && parts
                        .iter()
                        .all(|p| !matches!(*p, "." | "..") && component.is_match(p)),
                "malformed plugin identity",
            )?;
            let dir = root.join(parts[0]).canonicalize()?;
            require(dir.starts_with(&root), "plugin path escapes root")?;
            let definition = dir.join("agents").join(format!("{}.md", parts[1]));
            for file in [dir.join("plugin.json"), definition.clone()] {
                require(
                    !file.is_symlink()
                        && file.is_file()
                        && file.canonicalize()?.starts_with(&dir)
                        && file.metadata()?.len() <= 100000,
                    "plugin definition unavailable",
                )?;
            }
            out.insert(id.into(), sha256(&std::fs::read(definition)?));
        }
    }
    Ok(out)
}
fn sources(s: &Value, directory: &Path) -> Result<Vec<Value>> {
    let root = directory.canonicalize()?;
    let artifacts = s["source_artifacts"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("scenario has no immutable source_artifact provenance")?;
    let mut total = 0;
    let mut out = vec![];
    for a in artifacts {
        strict_keys(
            a,
            &["artifact_id", "path", "sha256"],
            &["description"],
            "source artifact",
        )?;
        identifier(&a["artifact_id"], "artifact_id")?;
        let rel = bounded(&a["path"], usize::MAX, "source path")?;
        require(
            !Path::new(rel).is_absolute()
                && !Path::new(rel)
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir)),
            "source artifact path is not contained",
        )?;
        let unresolved = directory.join(rel);
        require(
            !unresolved.is_symlink(),
            "source artifact symlinks are not allowed",
        )?;
        let path = unresolved.canonicalize()?;
        require(
            path.starts_with(&root) && path.is_file(),
            "source artifact escapes scenario directory",
        )?;
        let size = path.metadata()?.len();
        total += size;
        require(
            size <= 100000 && total <= 200000,
            "source artifact byte limit exceeded",
        )?;
        let bytes = std::fs::read(path)?;
        let expected = hash(&a["sha256"], "source digest")?;
        require(
            sha256(&bytes) == expected,
            "source artifact digest mismatch",
        )?;
        let content = String::from_utf8(bytes)?;
        require(
            !has_secret(&json!(content)),
            "source artifact contains secret-like material",
        )?;
        out.push(json!({"artifact_id":a["artifact_id"],"sha256":expected,"content":content}));
    }
    Ok(out)
}
fn evidence(s: &Value, o: &Value, g: &Value, p: &Value, merged: &str) -> Vec<Value> {
    let mut out = vec![
        json!({"evidence_ref":"provenance:merged_receipt","kind":"provenance","sha256":merged}),
        json!({"evidence_ref":"provenance:scenario_contract","kind":"provenance","sha256":p["scenario_contract_sha256"]}),
        json!({"evidence_ref":"provenance:golden_validation","kind":"provenance","sha256":p["validation_sha256"]}),
        json!({"evidence_ref":"lifecycle:terminal","kind":"lifecycle","event_type":o["terminal_events"][0]["event_type"]}),
        json!({"evidence_ref":"deterministic:safety-preflight","kind":"deterministic","passed":true,"checks":["secret-like-material-absent","no-unknown-side-effect-state-observed","current-attempt-only","tenant-scope-bound"]}),
        json!({"evidence_ref":"candidate:output","kind":"candidate","sha256":o["candidate_output_sha256"]}),
    ];
    for a in arr(&g["assertions"]) {
        out.push(json!({"evidence_ref":format!("golden:{}",text(a,"assertion_id")),"kind":"golden","passed":a["passed"],"actual_sha256":a["actual_sha256"]}));
    }
    for v in arr(&o["subagent_finishes"]) {
        let mut id = lifecycle::identity(v);
        if id.is_empty() {
            id = text(v, "agent_id").into();
            if id.is_empty() {
                id = "unknown".into()
            }
        }
        out.push(json!({"evidence_ref":format!("lifecycle:subagent:{id}"),"kind":"lifecycle","status":v["status"],"definition_sha256":v["definition_sha256"]}));
    }
    for a in arr(&s["source_artifacts"]) {
        out.push(json!({"evidence_ref":format!("source:{}",text(a,"artifact_id")),"kind":"source","sha256":a["sha256"]}));
    }
    out
}
pub fn prepare_input(
    contract: &Value,
    receipt: &Value,
    options: &PrepareOptions<'_>,
) -> Result<PreparedInput> {
    let scenarios = scenarios(contract)?;
    let nonce = options
        .expected_suite_nonce
        .map(str::to_owned)
        .unwrap_or_else(|| std::env::var("GENERAL_AGENT_SUITE_NONCE").unwrap_or_default())
        .trim()
        .to_lowercase();
    hash(&json!(nonce), "independent GENERAL_AGENT_SUITE_NONCE")?;
    let definitions = plugins(options.repository, &scenarios)?;
    let mut contents = BTreeMap::new();
    for (id, s) in &scenarios {
        contents.insert(id.clone(), sources(s, options.scenario_directory)?);
    }
    strict_keys(
        receipt,
        &[
            "schema_version",
            "suite_id",
            "provenance",
            "trials",
            "merged_receipt_sha256",
        ],
        &[],
        "merged receipt",
    )?;
    require(
        receipt["schema_version"] == "real-agent-validated-receipts/v1",
        "unsupported merged schema",
    )?;
    let suite = identifier(&receipt["suite_id"], "receipt suite_id")?;
    require(
        receipt["suite_id"] == contract["suite_id"],
        "merged suite_id differs from contract",
    )?;
    let stored = hash(&receipt["merged_receipt_sha256"], "merged receipt digest")?;
    let expected = hash(
        &json!(options.expected_receipt_sha256),
        "expected receipt digest",
    )?;
    require(
        constant_eq(
            &stored,
            &digest(&unsigned(receipt, "merged_receipt_sha256")),
        ),
        "merged receipt self-digest mismatch",
    )?;
    require(
        constant_eq(&stored, &expected),
        "merged receipt differs from out-of-band expected digest",
    )?;
    let p = &receipt["provenance"];
    strict_keys(
        p,
        &[
            "scenario_contract_sha256",
            "observations_sha256",
            "runtime_binding_sha256",
            "raw_sse_artifact_sha256",
            "provider_observer_sha256",
            "suite_nonce_sha256",
            "validation_sha256",
            "validation_seal_strength",
            "coding_host_test_evidence",
            "collector_attestation",
            "golden_attestation",
        ],
        &[],
        "receipt provenance",
    )?;
    let contract_digest = digest(contract);
    require(
        p["scenario_contract_sha256"] == contract_digest,
        "scenario contract provenance mismatch",
    )?;
    for field in [
        "observations_sha256",
        "runtime_binding_sha256",
        "raw_sse_artifact_sha256",
        "provider_observer_sha256",
        "suite_nonce_sha256",
        "validation_sha256",
    ] {
        hash(&p[field], field)?;
    }
    require(
        p["validation_seal_strength"] == "hmac-sha256",
        "release judgment requires HMAC golden validation",
    )?;
    let collector = key(
        options.collector_hmac_key,
        "GENERAL_AGENT_COLLECTOR_HMAC_KEY",
    )?;
    let golden_key = key(options.golden_hmac_key, "GENERAL_AGENT_GOLDEN_HMAC_KEY")?;
    require(
        !constant_eq(&collector, &golden_key),
        "collector and golden keys must differ",
    )?;
    let collector_id = attestation(
        &p["collector_attestation"],
        &crate::scenario_runner::merged_binding(receipt, false),
        &collector,
    )?;
    let golden_id = attestation(
        &p["golden_attestation"],
        &crate::scenario_runner::merged_binding(receipt, true),
        &golden_key,
    )?;
    require(
        !constant_eq(&collector_id, &golden_id),
        "attestation key identities must differ",
    )?;
    let coding = if scenarios.contains_key(crate::coding_receipt::SCENARIO_ID) {
        let path = options
            .coding_host_test_receipt_path
            .ok_or("coding preflight requires --coding-host-test-receipt")?;
        let coding_key = key(
            options.coding_host_test_hmac_key,
            crate::coding_receipt::HMAC_ENVIRONMENT_NAME,
        )?;
        require(
            !constant_eq(&coding_key, &collector) && !constant_eq(&coding_key, &golden_key),
            "coding, collector and golden keys must differ",
        )?;
        let mut trials = BTreeMap::new();
        for t in arr(&receipt["trials"]) {
            let o = &t["observation"];
            if o["scenario_id"] == crate::coding_receipt::SCENARIO_ID
                && t["golden_validation"].is_object()
            {
                if let Some(n) = o["trial"].as_i64() {
                    trials.insert(n,json!({"observation_sha256":o["observation_sha256"],"execution_checks":t["golden_validation"]["execution_checks"]}));
                }
            }
        }
        let policy = crate::coding_receipt::validator_policy(
            &options.repository.join(
                "tests/fixtures/eval/general_agent/coding_parallel_v2/validator_contract.json",
            ),
            &options
                .repository
                .join("rust/platform-native/crates/platform-eval/src/coding_sandbox.rs"),
        )?;
        let evidence = crate::coding_receipt::verify_receipt(
            path,
            &coding_key,
            &policy,
            &crate::coding_receipt::Expected {
                scenario_contract_sha256: &contract_digest,
                observations_sha256: text(p, "observations_sha256"),
                collector_attestation_key_id: &collector_id,
                trials: &trials,
                source_artifacts: None,
                plugin_definitions: None,
                runtime_binding: None,
                raw_sse_artifact: None,
            },
        )?;
        require(
            p["coding_host_test_evidence"] == evidence,
            "coding host-test receipt differs from double-HMAC provenance",
        )?;
        Some(evidence)
    } else {
        require(
            p["coding_host_test_evidence"].is_null()
                && options.coding_host_test_receipt_path.is_none(),
            "unexpected coding host-test evidence",
        )?;
        None
    };
    let rows = receipt["trials"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("merged trials must be nonempty")?;
    let mut by_key = BTreeMap::new();
    let mut summaries = BTreeMap::new();
    let mut challenges = BTreeSet::new();
    let mut failures = vec![];
    for row in rows {
        strict_keys(
            row,
            &["observation", "golden_validation"],
            &[],
            "merged trial",
        )?;
        let o = &row["observation"];
        let g = &row["golden_validation"];
        strict_keys(
            o,
            &[
                "scenario_id",
                "trial",
                "session_id",
                "suite_nonce",
                "collector_challenge",
                "prompt_sha256",
                "event_counts",
                "stream_sha256",
                "attempt_ids",
                "candidate_output",
                "candidate_output_sha256",
                "text_events",
                "event_sequence",
                "subagent_starts",
                "subagent_finishes",
                "parallel_overlaps",
                "tool_starts",
                "tool_results",
                "terminal_events",
                "observation_sha256",
            ],
            &[],
            "observation",
        )?;
        let id = identifier(&o["scenario_id"], "scenario_id")?;
        let n = integer(&o["trial"], 1, 5, "trial")?;
        let pair = (id.clone(), n);
        require(!by_key.contains_key(&pair), "duplicate merged trial")?;
        let s = scenarios.get(&id).ok_or("unknown merged scenario")?;
        require(
            hash(&o["suite_nonce"], "suite nonce")? == nonce,
            "merged suite_nonce differs from independent external nonce",
        )?;
        require(
            challenges.insert(hash(&o["collector_challenge"], "collector challenge")?),
            "cloned collector challenge",
        )?;
        require(
            o["prompt_sha256"] == sha256(crate::scenario::candidate_prompt(s).as_bytes()),
            "prompt digest differs from scenario",
        )?;
        let output = o["candidate_output"]
            .as_str()
            .filter(|s| s.chars().count() <= 100000)
            .ok_or("candidate_output malformed or oversized")?;
        require(
            o["candidate_output_sha256"] == sha256(output.as_bytes()),
            "candidate output digest mismatch",
        )?;
        require(
            !has_secret(
                &json!({"candidate_output":output,"subagent_finishes":o["subagent_finishes"],"tool_starts":o["tool_starts"],"tool_results":o["tool_results"]}),
            ),
            "secret-like material or collector redaction observed",
        )?;
        hash(&o["stream_sha256"], "stream digest")?;
        require(
            hash(&o["observation_sha256"], "observation digest")?
                == digest(&unsigned(o, "observation_sha256")),
            "observation self-digest mismatch",
        )?;
        validate_golden(s, o, g)?;
        let (mut errors, summary) = lifecycle::validate(s, o, &definitions);
        failures.append(&mut errors);
        summaries.insert(pair.clone(), summary);
        by_key.insert(pair, row);
    }
    require(
        p["suite_nonce_sha256"] == sha256(nonce.as_bytes()),
        "HMAC-bound suite nonce differs from external nonce",
    )?;
    let expected: BTreeSet<_> = scenarios
        .keys()
        .flat_map(|id| (1..=3).map(move |n| (id.clone(), n)))
        .collect();
    require(
        by_key.keys().cloned().collect::<BTreeSet<_>>() == expected,
        "merged scenario/trial set mismatch",
    )?;
    for id in scenarios.keys() {
        let trials: Vec<_> = (1..=3)
            .map(|n| &by_key[&(id.clone(), n)]["observation"])
            .collect();
        for field in ["session_id", "stream_sha256"] {
            let values: Vec<_> = trials.iter().map(|o| text(o, field)).collect();
            require(
                values.iter().all(|s| !s.is_empty())
                    && values.iter().collect::<BTreeSet<_>>().len() == 3,
                format!("{id}: repeated or missing {field}"),
            )?;
        }
        let attempts: Vec<_> = trials
            .iter()
            .map(|o| canonical(&o["attempt_ids"]))
            .collect();
        require(
            trials.iter().all(|o| arr(&o["attempt_ids"]).len() == 1)
                && attempts.iter().collect::<BTreeSet<_>>().len() == 3,
            "repeated or missing attempt receipts",
        )?;
        let tenants: Vec<_> = trials
            .iter()
            .flat_map(|o| arr(&o["terminal_events"]).iter())
            .map(|t| text(&t["terminal_envelope"], "tenant_id"))
            .collect();
        require(
            tenants.len() == 3
                && tenants.iter().all(|s| !s.is_empty())
                && tenants.iter().collect::<BTreeSet<_>>().len() == 1,
            "missing or inconsistent terminal tenant scope",
        )?;
    }
    require(
        failures.is_empty(),
        format!(
            "deterministic release preflight failed: {}",
            failures.join("; ")
        ),
    )?;
    let trials=by_key.into_iter().map(|((id,n),row)|{let s=&scenarios[&id];let o=&row["observation"];let g=&row["golden_validation"];PreparedTrial{suite_id:suite.clone(),scenario:s.clone(),observation:o.clone(),golden_validation:g.clone(),provenance:p.clone(),merged_receipt_sha256:stored.clone(),deterministic_summary:json!({"passed":true,"golden_assertion_count":arr(&g["assertions"]).len(),"execution_check_count":arr(&g["execution_checks"]).len(),"all_assertions_passed":true,"all_execution_checks_passed":true,"provenance_verified":true,"collector_attestation_verified":true,"golden_attestation_verified":true,"runtime_binding_verified":true,"raw_sse_artifact_binding_verified":true,"provider_observer_binding_verified":true,"external_suite_nonce_binding_verified":true,"coding_host_tests_verified":if id==crate::coding_receipt::SCENARIO_ID{Some(coding.is_some())}else{None},"validation_seal_strength":p["validation_seal_strength"],"lifecycle":summaries[&(id.clone(),n)]}),evidence_index:evidence(s,o,g,p,&stored),source_contents:contents[&id].clone()}}).collect();
    Ok(PreparedInput {
        suite_id: suite,
        scenario_contract_sha256: contract_digest,
        merged_receipt_sha256: stored,
        trials,
        coding_host_test_evidence: coding,
    })
}
fn validate_golden(s: &Value, o: &Value, g: &Value) -> Result<()> {
    strict_keys(
        g,
        &[
            "scenario_id",
            "trial",
            "observation_sha256",
            "answer_parse_error",
            "candidate_self_assessment_detected",
            "assertions",
            "execution_checks",
            "golden_passed",
            "execution_checks_passed",
            "trial_accepted",
        ],
        &[],
        "golden validation",
    )?;
    require(
        g["scenario_id"] == o["scenario_id"]
            && g["trial"] == o["trial"]
            && g["observation_sha256"] == o["observation_sha256"],
        "golden validation identity mismatch",
    )?;
    require(
        g["answer_parse_error"].is_null()
            && g["candidate_self_assessment_detected"] == false
            && g["golden_passed"] == true
            && g["execution_checks_passed"] == true
            && g["trial_accepted"] == true,
        "golden validation did not accept trial",
    )?;
    let expected: BTreeMap<_, _> = arr(&s["expected_assertions"])
        .iter()
        .map(|a| (text(a, "assertion_id"), a))
        .collect();
    let mut observed = BTreeSet::new();
    for a in g["assertions"]
        .as_array()
        .ok_or("golden assertions must be a list")?
    {
        strict_keys(
            a,
            &[
                "assertion_id",
                "kind",
                "passed",
                "reason",
                "expected_sha256",
                "actual_sha256",
            ],
            &[],
            "golden assertion",
        )?;
        let id = identifier(&a["assertion_id"], "assertion id")?;
        require(observed.insert(id.clone()), "duplicate assertion")?;
        let e = expected.get(id.as_str()).ok_or("unknown assertion")?;
        require(
            a["kind"] == e["kind"]
                && a["expected_sha256"] == digest(&e["expected"])
                && a["passed"] == true
                && a["reason"] == "matched",
            "deterministic assertion mismatch",
        )?;
        hash(&a["actual_sha256"], "actual assertion digest")?;
    }
    require(
        observed.iter().map(String::as_str).collect::<BTreeSet<_>>()
            == expected.keys().copied().collect(),
        "missing assertions",
    )?;
    let mut checks = BTreeSet::new();
    for c in g["execution_checks"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("execution checks must be nonempty")?
    {
        strict_keys(
            c,
            &["check_id", "passed", "observed_sha256"],
            &[],
            "execution check",
        )?;
        let id = identifier(&c["check_id"], "check_id")?;
        require(checks.insert(id.clone()), "duplicate execution check")?;
        hash(&c["observed_sha256"], "execution check digest")?;
        require(
            c["passed"] == true
                && c["observed_sha256"]
                    == digest(
                        &json!({"scenario_id":o["scenario_id"],"observation_sha256":o["observation_sha256"],"check_id":id,"passed":true}),
                    ),
            "execution check failed or digest mismatch",
        )?;
    }
    require(
        checks.iter().map(String::as_str).collect::<BTreeSet<_>>()
            == CHECKS.iter().copied().collect(),
        "execution check set mismatch",
    )
}
