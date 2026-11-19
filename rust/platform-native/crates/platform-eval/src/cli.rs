use crate::{Result, rag};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub struct Args {
    pub positional: Vec<String>,
    pub options: BTreeMap<String, String>,
    pub repeated: BTreeMap<String, Vec<String>>,
}
impl Args {
    pub fn parse(raw: impl Iterator<Item = String>, flags: &[&str]) -> Result<Self> {
        let mut args = raw.peekable();
        let mut result = Self {
            positional: vec![],
            options: BTreeMap::new(),
            repeated: BTreeMap::new(),
        };
        while let Some(arg) = args.next() {
            if let Some(name) = arg.strip_prefix("--") {
                let (name, value) = if let Some((k, v)) = name.split_once('=') {
                    (k.to_string(), v.to_string())
                } else if flags.contains(&name) {
                    (name.to_string(), "true".into())
                } else {
                    (
                        name.to_string(),
                        args.next()
                            .ok_or_else(|| format!("--{name} requires a value"))?,
                    )
                };
                if name == "evaluator-id" {
                    result
                        .repeated
                        .entry(name.clone())
                        .or_default()
                        .push(value.clone());
                }
                if result.options.insert(name.clone(), value).is_some() && name != "evaluator-id" {
                    return Err(format!("duplicate option --{name}").into());
                }
            } else {
                result.positional.push(arg)
            }
        }
        Ok(result)
    }
    pub fn get<'a>(&'a self, key: &str, default: &'a str) -> &'a str {
        self.options.get(key).map(String::as_str).unwrap_or(default)
    }
    pub fn number<T: std::str::FromStr>(&self, key: &str, default: T) -> Result<T> {
        match self.options.get(key) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("invalid --{key}").into()),
        }
    }
    pub fn required(&self, key: &str) -> Result<&str> {
        self.options
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| format!("--{key} is required").into())
    }
    pub fn reject_unknown(&self, allowed: &[&str]) -> Result<()> {
        if let Some(k) = self.options.keys().find(|k| !allowed.contains(&k.as_str())) {
            return Err(format!("unrecognized argument --{k}").into());
        }
        Ok(())
    }
}
pub fn rag_cli(args: Args) -> Result<(Value, i32)> {
    let command = args.positional.first().map(String::as_str).unwrap_or("");
    if !["validate", "gate"].contains(&command) || args.positional.len() != 2 {
        return Err("usage: eval_rag {validate,gate} expectations [--observations FILE]".into());
    }
    args.reject_unknown(&[
        "observations",
        "track",
        "output",
        "k-values",
        "min-total-samples",
        "min-track-samples",
        "bootstrap-samples",
        "min-recall",
        "min-mrr",
        "min-ndcg",
        "judge-observations",
        "judge-policy-manifest",
        "enable-external-judge-gate",
        "min-answer-samples",
        "min-faithfulness",
        "min-response-relevancy",
    ])?;
    let path = &args.positional[1];
    let (mut cases, expectations_hash) = crate::snapshot(path)?;
    let track = args.options.get("track");
    if let Some(track) = track {
        if !rag::TRACKS.contains(&track.as_str()) {
            return Err("invalid track".into());
        }
        cases.retain(|c| c["track"] == *track);
        if cases.is_empty() {
            return Err(format!("expectations contain no '{track}'-track cases").into());
        }
    }
    let (mut observations, observation_hash) = if let Some(path) = args.options.get("observations")
    {
        let (rows, hash) = crate::snapshot(path)?;
        (crate::observations(&rows)?, Some(hash))
    } else {
        if command == "gate" {
            return Err("--observations is required".into());
        }
        (BTreeMap::new(), None)
    };
    if track.is_some() {
        observations.retain(|id, _| cases.iter().any(|c| c["case_id"] == *id));
    }
    let cv = rag::validate_cases(&cases);
    let ov = if observation_hash.is_some() {
        rag::validate_observations(&cases, &observations)
    } else {
        json!({"valid":true,"skipped":"no --observations given"})
    };
    let validation =
        json!({"valid":cv["valid"]==true&&ov["valid"]==true,"expectations":cv,"observations":ov});
    if command == "validate" || validation["valid"] != true {
        let code = if validation["valid"] == true { 0 } else { 1 };
        return Ok((validation, code));
    }
    let mut o = rag::GateOptions::default();
    o.ks = args
        .get("k-values", "1,3,5")
        .split(',')
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().parse())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    o.min_total = args.number("min-total-samples", 8)?;
    o.min_track = args.number("min-track-samples", 4)?;
    o.min_answer = args.number("min-answer-samples", 4)?;
    o.bootstrap_samples = args.number("bootstrap-samples", 2000)?;
    if let Some(t) = track {
        o.required_tracks = vec![t.clone()];
    }
    for (flag, metric) in [
        ("min-recall", "recall_at_k"),
        ("min-mrr", "mrr"),
        ("min-ndcg", "ndcg_at_k"),
    ] {
        o.retrieval_thresholds
            .insert(metric.into(), args.number(flag, 0.8)?);
    }
    let judge_enabled = args.options.contains_key("enable-external-judge-gate");
    let judge_path = args.options.get("judge-observations");
    if judge_path.is_some() && !judge_enabled {
        return Err("judge observations require --enable-external-judge-gate".into());
    }
    if judge_enabled && judge_path.is_none() {
        return Err("external judge gate requires judge observations".into());
    }
    if args.options.contains_key("judge-policy-manifest") && !judge_enabled {
        return Err("judge policy requires --enable-external-judge-gate".into());
    }
    let mut report = rag::evaluate(&cases, &observations, &o)?;
    let mut judge_provenance = Value::Null;
    if judge_enabled {
        let policy_path = args
            .options
            .get("judge-policy-manifest")
            .ok_or("external judge gate requires a trusted resolved judge policy")?;
        let policy = crate::judges::resolve_policy(policy_path)?;
        let (rows, hash) = crate::snapshot(judge_path.unwrap())?;
        crate::judges::apply(
            &mut report,
            &cases,
            &observations,
            &rows,
            &policy,
            &o,
            args.number("min-faithfulness", 0.7)?,
            args.number("min-response-relevancy", 0.7)?,
        )?;
        judge_provenance = json!({"source":judge_path,"raw_source_sha256":hash,"artifact_binding":report["answer_quality"]["artifact_binding"]});
    }
    report["provenance"] = json!({"expectations":{"source":path,"sha256":expectations_hash},"observations":{"source":args.options.get("observations"),"sha256":observation_hash},"judge_observations":judge_provenance,"judge_policy":report["answer_quality"].get("policy"),"bootstrap":{"seed":42,"samples":o.bootstrap_samples},"provider_execution":"not_performed_by_this_command"});
    crate::write_report(args.get("output", "tmp/eval-e1/rag-latest.json"), &report)?;
    let code = if report["gate"]["status"] == "pass" {
        0
    } else {
        1
    };
    Ok((report, code))
}
