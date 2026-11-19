//! Source bilingual fixture/metric/gate assertions against the production private metric helper.
use super::*;
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .unwrap()
}
fn fixture() -> PathBuf {
    root().join("tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl")
}
fn scores(cases: &[Case], oracle: bool) -> BTreeMap<String, Vec<f64>> {
    cases
        .iter()
        .map(|c| {
            (
                c.id.clone(),
                c.candidates
                    .iter()
                    .enumerate()
                    .map(|(i, (id, _))| {
                        if oracle {
                            *c.relevance.get(id).unwrap_or(&0) as f64
                        } else {
                            (c.candidates.len() - i) as f64
                        }
                    })
                    .collect(),
            )
        })
        .collect()
}
fn result(cases: &[Case], values: &BTreeMap<String, Vec<f64>>, name: &str) -> Value {
    let all = cases.iter().collect::<Vec<_>>();
    let zh = all
        .iter()
        .copied()
        .filter(|c| c.language == "zh")
        .collect::<Vec<_>>();
    let en = all
        .iter()
        .copied()
        .filter(|c| c.language == "en")
        .collect::<Vec<_>>();
    json!({"adapter":name,"eligible":true,"errors":[],"slices":{"overall":metrics(&all,values,10),"language:zh":metrics(&zh,values,10),"language:en":metrics(&en,values,10)}})
}
#[test]
fn committed_fixture_slices_and_grade_three_targets() {
    let cases = load(&fixture()).unwrap();
    assert_eq!(cases.len(), 12);
    let zh = cases
        .iter()
        .filter(|c| c.language == "zh")
        .collect::<Vec<_>>();
    let en = cases
        .iter()
        .filter(|c| c.language == "en")
        .collect::<Vec<_>>();
    assert_eq!(zh.len(), 6);
    assert_eq!(en.len(), 6);
    assert!(zh.iter().all(|c| c.cross));
    assert!(en.iter().all(|c| !c.cross));
    for c in &cases {
        assert!(!c.candidates.is_empty() && !c.relevance.is_empty());
        let top = c
            .relevance
            .iter()
            .filter(|(_, g)| **g >= 3)
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert!(!top.is_empty(), "{}", c.id);
        assert!(
            top.iter()
                .all(|id| c.candidates.iter().any(|(cid, _)| cid == *id)),
            "{}",
            c.id
        );
    }
}
#[test]
fn identity_chinese_headroom_and_oracle_gate() {
    let cases = load(&fixture()).unwrap();
    let baseline = result(&cases, &scores(&cases, false), "identity");
    let ndcg = baseline["slices"]["language:zh"]["ndcg_at_k"]
        .as_f64()
        .unwrap();
    assert!(ndcg > 0. && ndcg < 0.9);
    let oracle = result(&cases, &scores(&cases, true), "oracle");
    let d = gate(&[baseline, oracle]);
    assert_eq!(d["promotable"], true, "{d}");
    assert_eq!(d["winner"], "oracle");
}
#[test]
fn builder_is_byte_deterministic() {
    let rows = crate::build_rerank_rows().unwrap();
    let rendered = rows
        .iter()
        .map(ai_platform_eval::canonical::canonical)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    assert_eq!(rendered, std::fs::read_to_string(fixture()).unwrap());
}
fn row() -> Value {
    json!({"case_id":"zh-1","query":"报销额度","language":"zh","cross_lingual":true,"relevance":{"seg-a":2,"seg-b":0},"candidates":[{"id":"seg-a","text":"甲"},{"id":"seg-b","text":"乙"}]})
}
#[test]
fn load_contract_drops_zero_grades_and_fails_closed() {
    let c = parse(&[row()]).unwrap().remove(0);
    assert_eq!(c.language, "zh");
    assert!(c.cross);
    assert_eq!(
        c.candidates,
        vec![("seg-a".into(), "甲".into()), ("seg-b".into(), "乙".into())]
    );
    assert_eq!(c.relevance, BTreeMap::from([("seg-a".into(), 2)]));
    for (key, v) in [
        ("case_id", json!("")),
        ("candidates", json!([])),
        ("relevance", json!({})),
        ("relevance", Value::Null),
    ] {
        let mut r = row();
        r[key] = v;
        assert!(parse(&[r]).is_err(), "{key}");
    }
    assert!(parse(&[row(), row()]).is_err());
    assert!(parse(&[]).is_err());
}
#[test]
fn hand_computed_metrics_and_ties() {
    let c = Case {
        id: "m1".into(),
        query: "q".into(),
        language: "en".into(),
        cross: false,
        candidates: vec![("A".into(), "a".into()), ("B".into(), "b".into())],
        relevance: BTreeMap::from([("B".into(), 1)]),
    };
    for (values, k, expected) in [
        (vec![5., 1.], 1, 0.),
        (vec![5., 1.], 2, 1. / 3f64.log2()),
        (vec![1., 5.], 1, 1.),
        (vec![0., 0.], 2, 1. / 3f64.log2()),
    ] {
        let m = metrics(&[&c], &BTreeMap::from([("m1".into(), values)]), k);
        assert!((m["ndcg_at_k"].as_f64().unwrap() - expected).abs() < 1e-6);
    }
}
#[test]
fn gate_requires_zh_strict_win_en_no_regression_and_eligible_challenger() {
    let cases = load(&fixture()).unwrap();
    let base = result(&cases, &scores(&cases, false), "identity");
    let oracle = result(&cases, &scores(&cases, true), "oracle");
    let mut tie = base.clone();
    tie["adapter"] = json!("tie");
    let d = gate(&[base.clone(), tie]);
    assert_eq!(d["promotable"], false);
    assert_eq!(d["zh_net_win"], false);
    let mut regress = oracle.clone();
    regress["slices"]["language:en"]["ndcg_at_k"] = json!(0.);
    let d = gate(&[base.clone(), regress]);
    assert_eq!(d["promotable"], false);
    assert_eq!(d["zh_net_win"], true);
    assert_eq!(d["en_no_regression"], false);
    let mut errored = oracle.clone();
    errored["eligible"] = json!(false);
    let d = gate(&[base.clone(), errored]);
    assert_eq!(d["promotable"], false);
    assert!(d["winner"].is_null());
    assert!(
        d["reason"]
            .as_str()
            .unwrap()
            .contains("no eligible challenger")
    );
    assert!(
        gate(&[oracle.clone()])["reason"]
            .as_str()
            .unwrap()
            .contains("identity baseline missing")
    );
    let mut no_zh = base;
    no_zh["slices"]
        .as_object_mut()
        .unwrap()
        .remove("language:zh");
    assert!(
        gate(&[no_zh, oracle])["reason"]
            .as_str()
            .unwrap()
            .contains("no language:zh cases")
    );
}

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "bakeoff-parity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn cli_args(t: &Temp, adapters: &str) -> Vec<String> {
    vec![
        "--cases".into(),
        fixture().to_str().unwrap().into(),
        "--adapters".into(),
        adapters.into(),
        "--out".into(),
        t.0.join("report").to_str().unwrap().into(),
    ]
}
#[tokio::test]
async fn identity_only_cli_writes_offline_reports() {
    let t = Temp::new();
    run(&cli_args(&t, "identity")).await.unwrap();
    let r: Value =
        serde_json::from_slice(&std::fs::read(t.0.join("report.json")).unwrap()).unwrap();
    assert_eq!(r["schema"], "rerank-bakeoff-v1");
    assert_eq!(r["params"], json!({"k":10,"total_cases":12}));
    assert_eq!(r["adapters"].as_array().unwrap().len(), 1);
    assert_eq!(r["adapters"][0]["adapter"], "identity");
    assert_eq!(r["adapters"][0]["eligible"], true);
    assert_eq!(r["gate"]["promotable"], false);
    let md = std::fs::read_to_string(t.0.join("report.md")).unwrap();
    assert!(md.contains("# Reranker bake-off report"));
    assert!(md.contains("| identity | true | language:zh"));
}
#[tokio::test]
async fn cli_requires_identity_and_explicit_live_permission() {
    let t = Temp::new();
    let e = run(&cli_args(&t, "bge")).await.unwrap_err();
    assert!(e.to_string().contains("identity baseline"));
    let e = run(&cli_args(&t, "identity,nope:model")).await.unwrap_err();
    assert!(e.to_string().contains("unknown adapter provider"));
    for provider in ["cohere", "dashscope:qwen3-rerank", "jina"] {
        let e = run(&cli_args(&t, &format!("identity,{provider}")))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("--allow-live"));
    }
    assert!(!t.0.join("report.json").exists());
}
