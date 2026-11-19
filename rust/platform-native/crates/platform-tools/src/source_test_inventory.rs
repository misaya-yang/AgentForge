//! Complete compiled test collection and per-test outcomes, bound to the source unit.
use super::*;
const LEGACY_TARGET: &str = "092d3acd6bec3e3a14bdc7e7a2810ab628ab759d";
pub(super) const REQUIRED: &[&str] = &[
    "tests::canonical_hash_is_independent_of_object_key_order",
    "tests::lease_signature_binds_scope_and_arguments",
    "tests::write_lease_requires_bound_approval",
    "tests::unknown_fields_fail_closed",
    "tests::signature_matches_the_python_cross_language_fixture",
    "tests::descriptor_binds_schema_hash_and_requires_stable_tags",
    "tests::schema_subset_accepts_nested_object_string_number_and_array",
    "tests::schema_subset_rejects_unknown_keywords_and_unsupported_forms",
    "tests::schema_subset_requires_unique_required_and_enum_values",
    "tests::schema_subset_validates_object_string_number_and_array_instances",
    "tests::safe_pattern_supports_bounded_quantifiers_but_rejects_unsafe_forms",
    "tests::schema_subset_supports_annotations_and_composition",
    "tests::schema_subset_rejects_ambiguous_composition_and_bounds_complexity",
];
fn bound(package: &Path, unit: &Value, relative: &str) -> Result<PathBuf> {
    let path = safe(package, relative)?;
    require(
        unit["evidence"][relative]
            .as_str()
            .is_some_and(|sha| hex(sha, 64))
            && unit["evidence"][relative] == digest(&path)?,
        "test evidence not source-unit hash bound",
    )?;
    Ok(path)
}
fn listed(stdout: &str) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    let summary = regex::Regex::new(r"^(\d+) tests?, 0 benchmarks?$")?;
    let mut count = 0u64;
    let mut summaries = 0;
    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(name) = line.strip_suffix(": test") {
            require(
                !name.is_empty() && names.insert(name.to_owned()),
                "duplicate/empty compiled test name",
            )?;
        } else if let Some(matches) = summary.captures(line) {
            count += matches[1].parse::<u64>()?;
            summaries += 1;
        } else {
            return Err("unexpected compiled test-list output".into());
        }
    }
    require(
        summaries > 0
            && count == names.len() as u64
            && REQUIRED.iter().all(|name| names.contains(*name)),
        "compiled inventory lost required tests or has inconsistent counts",
    )?;
    Ok(names)
}
fn executed(stdout: &str, expected: &BTreeSet<String>) -> Result<()> {
    let running = regex::Regex::new(r"^running (\d+) tests?$")?;
    let summary = regex::Regex::new(
        r"^test result: ok\. (\d+) passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in .+$",
    )?;
    let mut names = BTreeSet::new();
    let mut passed = 0u64;
    let mut announced = 0u64;
    let mut summaries = 0;
    for line in stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        if let Some(matches) = running.captures(line) {
            announced += matches[1].parse::<u64>()?;
        } else if let Some(matches) = summary.captures(line) {
            passed += matches[1].parse::<u64>()?;
            summaries += 1;
        } else if let Some(name) = line
            .strip_prefix("test ")
            .and_then(|line| line.strip_suffix(" ... ok"))
        {
            let name = if expected.contains(name) {
                name
            } else {
                name.strip_suffix(" - should panic").unwrap_or(name)
            };
            require(names.insert(name.to_owned()), "duplicate executed test")?;
        } else {
            return Err("unexpected test outcome, ignored test, filter or failure".into());
        }
    }
    require(
        summaries > 0 && &names == expected && passed == names.len() as u64 && announced == passed,
        "complete test execution differs from compiled inventory",
    )
}

pub(super) fn verify(package: &Path, unit: &Value, receipt: &Value) -> Result<()> {
    let inventory_relative = "evidence/contract-test-inventory.json";
    if unit["target"] == LEGACY_TARGET && !package.join(inventory_relative).exists() {
        return require(
            receipt["contract_unit_tests"]["passed"] == 13
                && receipt["contract_unit_tests"]["failed"] == 0,
            "legacy contract test scope mismatch",
        );
    }
    let inventory = object(&bound(package, unit, inventory_relative)?)?;
    require(
        inventory["schema"] == "ai-platform/compiled-test-inventory/v1",
        "unsupported compiled inventory",
    )?;
    let binding = json!({"target_commit":unit["target"],"target_tree":unit["target_tree"],
        "candidate_source_sha256":unit["input_candidate_manifest_sha256"],"resolved_lock_sha256":unit["resolved_lock_sha256"]});
    for key in [
        "target_commit",
        "target_tree",
        "candidate_source_sha256",
        "resolved_lock_sha256",
    ] {
        require(
            inventory[key] == binding[key] && binding[key].is_string(),
            "compiled inventory source differs",
        )?;
    }
    let list_receipt_path = bound(package, unit, text(&inventory, "list_command_receipt")?)?;
    let list = object(&list_receipt_path)?;
    let test = object(&bound(package, unit, "evidence/contract-tests.json")?)?;
    let metadata = object(&bound(package, unit, "evidence/resolved-metadata.json")?)?;
    let workspace = text(&metadata, "workspace_root")?;
    for run in [&list, &test] {
        require(
            run["exit_code"] == 0
                && run["stop_reason"].is_null()
                && run["source_binding"] == binding
                && run["cwd"] == workspace,
            "compiled inventory command failed or source differs",
        )?;
    }
    let mut command = vec![
        json!("cargo"),
        json!("+1.95.0"),
        json!("test"),
        json!("--locked"),
        json!("-p"),
        json!("ai-platform-capability-contract"),
    ];
    require(
        test["command"] == json!(command),
        "complete test command must not filter or skip",
    )?;
    command.extend([json!("--"), json!("--list")]);
    require(
        list["command"] == json!(command),
        "test collection command must be exact compiled --list",
    )?;
    let list_stdout = bound(package, unit, text(&inventory, "list_stdout")?)?;
    let test_stdout = bound(package, unit, text(&inventory, "test_stdout")?)?;
    require(
        list["stdout_sha256"] == digest(&list_stdout)?
            && test["stdout_sha256"] == digest(&test_stdout)?,
        "test command stdout differs",
    )?;
    let names = listed(&std::fs::read_to_string(list_stdout)?)?;
    require(
        inventory["listed_count"] == names.len() as u64,
        "declared compiled test count differs",
    )?;
    executed(&std::fs::read_to_string(test_stdout)?, &names)?;
    let counts = &receipt["contract_unit_tests"];
    require(
        counts["passed"] == names.len() as u64
            && counts["failed"] == 0
            && counts["ignored"] == 0
            && counts["filtered_out"] == 0,
        "contract results differ from complete compiled inventory",
    )
}

#[cfg(test)]
#[path = "source_test_inventory_tests.rs"]
mod tests;
