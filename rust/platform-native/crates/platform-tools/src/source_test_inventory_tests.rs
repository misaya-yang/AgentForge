use super::*;

struct Fixture {
    root: PathBuf,
    unit: Value,
    receipt: Value,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("compiled-contract-tests-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("evidence")).unwrap();
        let mut names = REQUIRED.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        names.push("sandbox_job::tests::extra_compiled_regression".into());
        names.sort();
        let count = names.len();
        let list_stdout = format!(
            "{}\n{count} tests, 0 benchmarks\n\n0 tests, 0 benchmarks\n",
            names
                .iter()
                .map(|name| format!("{name}: test"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let test_stdout = format!(
            "running {count} tests\n{}\ntest result: ok. {count} passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s\n\nrunning 0 tests\ntest result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
            names
                .iter()
                .map(|name| format!("test {name} ... ok"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let unit = json!({"target":"a".repeat(40),"target_tree":"b".repeat(40),"input_candidate_manifest_sha256":"c".repeat(64),"resolved_lock_sha256":"d".repeat(64),"evidence":{}});
        let binding = json!({"target_commit":unit["target"],"target_tree":unit["target_tree"],"candidate_source_sha256":unit["input_candidate_manifest_sha256"],"resolved_lock_sha256":unit["resolved_lock_sha256"]});
        let command = json!([
            "cargo",
            "+1.95.0",
            "test",
            "--locked",
            "-p",
            "ai-platform-capability-contract"
        ]);
        let test_run = json!({"command":command,"exit_code":0,"stop_reason":null,"stdout_sha256":hash(test_stdout.as_bytes()),"source_binding":binding,"cwd":"/source/codex-rs"});
        let list_run = json!({"command":["cargo","+1.95.0","test","--locked","-p","ai-platform-capability-contract","--","--list"],"exit_code":0,"stop_reason":null,"stdout_sha256":hash(list_stdout.as_bytes()),"source_binding":binding,"cwd":"/source/codex-rs"});
        let mut inventory = binding;
        inventory["schema"] = json!("ai-platform/compiled-test-inventory/v1");
        inventory["listed_count"] = json!(count);
        inventory["list_command_receipt"] = json!("evidence/contract-list.json");
        inventory["list_stdout"] = json!("evidence/contract-list.stdout");
        inventory["test_stdout"] = json!("evidence/contract-tests.stdout");
        let receipt =
            json!({"contract_unit_tests":{"passed":count,"failed":0,"ignored":0,"filtered_out":0}});
        let mut fixture = Self {
            root,
            unit,
            receipt,
        };
        for (name, bytes) in [
            (
                "contract-test-inventory.json",
                serde_json::to_vec(&inventory).unwrap(),
            ),
            ("contract-list.json", serde_json::to_vec(&list_run).unwrap()),
            (
                "contract-tests.json",
                serde_json::to_vec(&test_run).unwrap(),
            ),
            ("contract-list.stdout", list_stdout.into_bytes()),
            ("contract-tests.stdout", test_stdout.into_bytes()),
            (
                "resolved-metadata.json",
                serde_json::to_vec(&json!({"workspace_root":"/source/codex-rs"})).unwrap(),
            ),
        ] {
            fixture.write(name, &bytes);
        }
        fixture
    }
    fn write(&mut self, name: &str, bytes: &[u8]) {
        let relative = format!("evidence/{name}");
        std::fs::write(self.root.join(&relative), bytes).unwrap();
        self.unit["evidence"][relative] = json!(hash(bytes));
    }
    fn mutate(&mut self, name: &str, operation: impl FnOnce(&mut Value)) {
        let mut value = object(&self.root.join("evidence").join(name)).unwrap();
        operation(&mut value);
        self.write(name, &serde_json::to_vec(&value).unwrap());
    }
    fn verify(&self) -> Result<()> {
        verify(&self.root, &self.unit, &self.receipt)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[test]
fn full_compiled_inventory_accepts_additions_without_losing_original_thirteen() {
    let fixture = Fixture::new();
    fixture.verify().unwrap();
    assert!(
        fixture.receipt["contract_unit_tests"]["passed"]
            .as_u64()
            .unwrap()
            > 13
    );
}
#[test]
fn missing_required_test_duplicate_test_and_false_counts_are_rejected() {
    let mut names = REQUIRED
        .iter()
        .map(|name| format!("{name}: test"))
        .collect::<Vec<_>>();
    for stdout in [
        "0 tests, 0 benchmarks\n".to_string(),
        format!("{}\n13 tests, 0 benchmarks\n", names[1..].join("\n")),
        format!(
            "{}\n{}\n14 tests, 0 benchmarks\n",
            names.join("\n"),
            names[0]
        ),
    ] {
        assert!(listed(&stdout).is_err());
    }
    names.push("new_module::real_test: test".into());
    assert!(listed(&format!("{}\n13 tests, 0 benchmarks\n", names.join("\n"))).is_err());
}
#[test]
fn rehashed_filtered_failed_ignored_or_truncated_test_output_is_rejected() {
    for modification in ["ignored", "filtered", "failed", "truncated", "duplicate"] {
        let mut fixture = Fixture::new();
        let mut stdout =
            std::fs::read_to_string(fixture.root.join("evidence/contract-tests.stdout")).unwrap();
        stdout = match modification {
            "ignored" => stdout.replacen(" ... ok", " ... ignored", 1),
            "filtered" => stdout.replacen("0 filtered out", "1 filtered out", 1),
            "failed" => stdout.replacen("0 failed", "1 failed", 1),
            "truncated" => "running 14 tests\n".into(),
            "duplicate" => format!("{stdout}\ntest {} ... ok\n", REQUIRED[0]),
            _ => unreachable!(),
        };
        fixture.write("contract-tests.stdout", stdout.as_bytes());
        fixture.mutate("contract-tests.json", |run| {
            run["stdout_sha256"] = json!(hash(stdout.as_bytes()))
        });
        assert!(fixture.verify().is_err(), "{modification}");
    }
}
#[test]
fn filters_skip_options_uncompiled_lists_and_wrong_source_are_rejected() {
    for name in ["contract-tests.json", "contract-list.json"] {
        let mut fixture = Fixture::new();
        fixture.mutate(name, |run| {
            run["command"].as_array_mut().unwrap().push(json!("--skip"))
        });
        assert!(fixture.verify().is_err());
        let mut fixture = Fixture::new();
        fixture.mutate(name, |run| {
            run["source_binding"]["target_commit"] = json!("f".repeat(40))
        });
        assert!(fixture.verify().is_err());
        let mut fixture = Fixture::new();
        fixture.mutate(name, |run| run["exit_code"] = json!(1));
        assert!(fixture.verify().is_err());
    }
    let mut fixture = Fixture::new();
    fixture.mutate("contract-test-inventory.json", |inventory| {
        inventory["listed_count"] = json!(1)
    });
    assert!(fixture.verify().is_err());
}
#[test]
fn unbound_changed_stdout_or_missing_full_collection_cannot_pass() {
    let mut fixture = Fixture::new();
    fixture.unit["evidence"]
        .as_object_mut()
        .unwrap()
        .remove("evidence/contract-list.stdout");
    assert!(fixture.verify().is_err());
    let mut fixture = Fixture::new();
    std::fs::write(
        fixture.root.join("evidence/contract-tests.stdout"),
        "fabricated summary",
    )
    .unwrap();
    assert!(fixture.verify().is_err());
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.root.join("evidence/contract-test-inventory.json")).unwrap();
    assert!(fixture.verify().is_err());
}
#[test]
fn reported_counts_cannot_hide_skips_failures_or_missing_tests() {
    for field in ["failed", "ignored", "filtered_out"] {
        let mut fixture = Fixture::new();
        fixture.receipt["contract_unit_tests"][field] = json!(1);
        assert!(fixture.verify().is_err());
    }
    let mut fixture = Fixture::new();
    fixture.receipt["contract_unit_tests"]["passed"] = json!(1);
    assert!(fixture.verify().is_err());
}
#[test]
fn expected_panics_remain_named_successful_test_outcomes() {
    let mut fixture = Fixture::new();
    let stdout = std::fs::read_to_string(fixture.root.join("evidence/contract-tests.stdout"))
        .unwrap()
        .replacen(" ... ok", " - should panic ... ok", 1);
    fixture.write("contract-tests.stdout", stdout.as_bytes());
    fixture.mutate("contract-tests.json", |run| {
        run["stdout_sha256"] = json!(hash(stdout.as_bytes()))
    });
    fixture.verify().unwrap();
}
#[test]
fn historical_thirteen_only_receipt_cannot_admit_a_new_upstream_target() {
    let mut fixture = Fixture::new();
    std::fs::remove_file(fixture.root.join("evidence/contract-test-inventory.json")).unwrap();
    fixture.receipt["contract_unit_tests"] = json!({"passed":13,"failed":0});
    assert!(fixture.verify().is_err());
    fixture.unit["target"] = json!(LEGACY_TARGET);
    fixture.verify().unwrap();
    fixture.receipt["contract_unit_tests"]["passed"] = json!(1);
    assert!(fixture.verify().is_err());
}
