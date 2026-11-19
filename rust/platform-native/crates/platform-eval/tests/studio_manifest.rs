//! Native command manifest keeps the finite phase gate inventory and required checks.
use serde_json::Value;
use std::{collections::BTreeSet, path::PathBuf};
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .unwrap()
        .to_path_buf()
}
fn manifest() -> Value {
    serde_json::from_slice(
        &std::fs::read(root().join("tests/fixtures/agent-studio/regression_manifest.json"))
            .unwrap(),
    )
    .unwrap()
}
fn required(m: &Value) -> Vec<(String, String, String, String)> {
    m["gates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["required"] == true)
        .map(|g| {
            (
                g["phase"].as_str().unwrap().into(),
                g["id"].as_str().unwrap().into(),
                g["cwd"].as_str().unwrap().into(),
                g["command"].as_str().unwrap().into(),
            )
        })
        .collect()
}
#[test]
fn manifest_exactly_covers_every_required_phase_gate() {
    let m = manifest();
    assert_eq!(m["schema_version"], "agent-studio-regression/v1");
    let actual = required(&m);
    let mut files = std::fs::read_dir(root().join("deploy/runbooks/agent-studio-prd"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_str().unwrap();
            n.starts_with("phase-0")
                && n.as_bytes()
                    .get(7)
                    .is_some_and(|b| matches!(b, b'0'..=b'8'))
                && n.ends_with(".md")
        })
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files.len(), 9);
    let re = regex::Regex::new(r"(?s)```json\s*(\{.*?\})\s*```").unwrap();
    let mut expected = vec![];
    for file in files {
        let s = std::fs::read_to_string(&file).unwrap();
        let c: Value = serde_json::from_str(&re.captures(&s).unwrap()[1]).unwrap();
        for g in c["validation"]["commands"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|g| g["required"] == true)
        {
            expected.push((
                c["phase"]["id"].as_str().unwrap().to_string(),
                g["id"].as_str().unwrap().to_string(),
                g["cwd"].as_str().unwrap().to_string(),
                g["command"].as_str().unwrap().to_string(),
            ));
        }
    }
    assert_eq!(actual, expected);
    assert_eq!(
        actual.len(),
        actual
            .iter()
            .map(|g| (&g.0, &g.1))
            .collect::<BTreeSet<_>>()
            .len()
    );
}
#[test]
fn manifest_keeps_named_critical_and_assistant_gates() {
    let m = manifest();
    let command = |phase: &str, id: &str| {
        m["gates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|g| g["phase"] == phase && g["id"] == id)
            .unwrap()["command"]
            .as_str()
            .unwrap()
    };
    for (phase, id, needle) in [
        ("AS-00", "capability-tests", "agent_studio_capability_proof"),
        ("AS-00", "capability-tests", "runtime_control_plane"),
        ("AS-02", "gateway-envelope", "agent_studio_envelope"),
        ("AS-03", "mcp-api-runtime", "agent_studio_mcp"),
        (
            "AS-04",
            "skill-api-isolation",
            "agent_studio_skill_entrypoint",
        ),
        ("AS-06", "publish-api-atomicity", "agent_studio_publication"),
        ("AS-06", "agent-eval", "make verify-eval-dev"),
        ("AS-07", "built-nginx-header-smoke", "--built-image"),
    ] {
        assert!(command(phase, id).contains(needle));
    }
    let all = m.to_string();
    assert!(all.contains("make test-isolation"));
    assert!(all.contains("make verify-assistant-runtime-dev"));
}
#[test]
fn manifest_has_no_optional_or_shell_placeholder_entries() {
    let m = manifest();
    ai_platform_eval::studio::validate_manifest(&m, &root()).unwrap();
    for g in m["gates"].as_array().unwrap() {
        assert_eq!(g["required"], true);
        assert_eq!(g["cwd"], ".");
        assert!(!g["command"].as_str().unwrap().contains("TODO"));
        if g["command"].as_str().unwrap().contains("cargo test") {
            assert!(g["command"].as_str().unwrap().contains("--include-ignored"));
        }
    }
}
#[test]
fn required_gate_rejects_skipped_and_native_ignored_tests() {
    use ai_platform_eval::studio::skipped_count;
    assert_eq!(skipped_count("4 passed, 2 skipped in 0.01s"), 2);
    assert_eq!(
        skipped_count("test result: ok. 4 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out"),
        2
    );
    assert_eq!(
        skipped_count("test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out"),
        0
    );
}
#[test]
fn workspace_hash_excludes_generated_evidence() {
    let s = std::fs::read_to_string(
        root().join("rust/platform-native/crates/platform-eval/src/studio.rs"),
    )
    .unwrap();
    for p in [
        ":(exclude)reports/**",
        ":(exclude)web/playwright-report/**",
        ":(exclude)web/test-results/**",
    ] {
        assert!(s.contains(p));
    }
}
