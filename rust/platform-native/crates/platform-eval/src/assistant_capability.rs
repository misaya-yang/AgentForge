//! Result-level capability evidence; proxy-only observations never prove success.
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
const TIERS: &[&str] = &["offline", "mock", "local_live", "real_provider", "browser"];
fn need(b: bool, s: &str) -> Result<()> {
    if b { Ok(()) } else { Err(s.to_string().into()) }
}
fn bounded(s: &str, n: usize) -> Result<String> {
    let v = s.split_whitespace().collect::<Vec<_>>().join(" ");
    need(
        !v.is_empty() && v.chars().count() <= n,
        "invalid bounded capability text",
    )?;
    Ok(v)
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCheck {
    pub name: String,
    pub kind: String,
    pub passed: bool,
    pub evidence: String,
}
impl CapabilityCheck {
    fn validate(&mut self) -> Result<()> {
        self.name = bounded(&self.name, 96)?;
        self.evidence = bounded(&self.evidence, 240)?;
        need(
            ["outcome", "safety", "protocol", "proxy"].contains(&self.kind.as_str()),
            "unsupported capability check kind",
        )
    }
}
fn three() -> usize {
    3
}
fn two() -> usize {
    2
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityCasePolicy {
    pub case_id: String,
    pub evidence_tier: String,
    #[serde(default = "three")]
    pub repetitions: usize,
    #[serde(default = "two")]
    pub minimum_passes: usize,
    #[serde(default)]
    pub critical: bool,
}
impl CapabilityCasePolicy {
    pub fn from_value(v: Value) -> Result<Self> {
        let mut s: Self = serde_json::from_value(v)?;
        s.case_id = bounded(&s.case_id, 128)?;
        need(
            TIERS.contains(&s.evidence_tier.as_str()),
            "unsupported evidence tier",
        )?;
        need(
            (1..=10).contains(&s.repetitions),
            "repetitions must be between1and10",
        )?;
        need(
            (1..=s.repetitions).contains(&s.minimum_passes),
            "invalid minimum passes",
        )?;
        need(
            !s.critical || s.minimum_passes == s.repetitions,
            "critical cases must pass every repetition",
        )?;
        Ok(s)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilityTrialReceipt {
    pub case_id: String,
    pub evidence_tier: String,
    pub trial: usize,
    pub checks: Vec<CapabilityCheck>,
}
impl CapabilityTrialReceipt {
    pub fn from_value(v: Value) -> Result<Self> {
        let mut s: Self = serde_json::from_value(v)?;
        s.case_id = bounded(&s.case_id, 128)?;
        need(
            TIERS.contains(&s.evidence_tier.as_str()),
            "unsupported evidence tier",
        )?;
        need(
            s.trial >= 1 && !s.checks.is_empty(),
            "positive trial and nonempty checks required",
        )?;
        for c in &mut s.checks {
            c.validate()?;
        }
        Ok(s)
    }
    pub fn substantive(&self) -> bool {
        self.checks.iter().any(|c| {
            ["outcome", "safety"].contains(&c.kind.as_str())
                && !["non_empty_output", "no_run_error"].contains(&c.name.as_str())
        })
    }
    pub fn passed(&self) -> bool {
        self.substantive() && self.checks.iter().all(|c| c.passed)
    }
}
pub fn evaluate(
    policies: &[CapabilityCasePolicy],
    receipts: &[CapabilityTrialReceipt],
    minimum: f64,
) -> Result<Value> {
    need(
        !policies.is_empty(),
        "capability suite requires case policy",
    )?;
    need(
        minimum.is_finite() && (0.0..=1.0).contains(&minimum),
        "minimum case pass rate must be in[0,1]",
    )?;
    let mut by = BTreeMap::new();
    for p in policies {
        need(
            by.insert(p.case_id.as_str(), p).is_none(),
            "duplicate capability policies",
        )?;
    }
    let unknown = receipts
        .iter()
        .map(|r| r.case_id.as_str())
        .filter(|id| !by.contains_key(id))
        .collect::<BTreeSet<_>>();
    let mut failures = vec![];
    if !unknown.is_empty() {
        failures.push(format!(
            "receipts without policies: {}",
            unknown.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
    let mut results = vec![];
    let mut passing = 0;
    let mut critical = vec![];
    let mut tiers = BTreeMap::<String, (usize, usize, usize)>::new();
    for p in policies {
        let mut rs = receipts
            .iter()
            .filter(|r| r.case_id == p.case_id)
            .collect::<Vec<_>>();
        rs.sort_by_key(|r| r.trial);
        let mut errors = vec![];
        if rs.len() != p.repetitions {
            errors.push(format!(
                "expected {} receipts, observed {}",
                p.repetitions,
                rs.len()
            ));
        }
        let expected = (1..=p.repetitions).collect::<Vec<_>>();
        if rs.iter().map(|r| r.trial).collect::<Vec<_>>() != expected {
            errors.push(format!("trial numbers must be exactly {expected:?}"));
        }
        if rs.iter().any(|r| r.evidence_tier != p.evidence_tier) {
            errors.push("receipt evidence tier does not match policy".into());
        }
        let proxy = rs
            .iter()
            .filter(|r| !r.substantive())
            .map(|r| r.trial.to_string())
            .collect::<Vec<_>>();
        if !proxy.is_empty() {
            errors.push(format!(
                "proxy-only trials cannot prove capability: {}",
                proxy.join(", ")
            ));
        }
        let passes = rs.iter().filter(|r| r.passed()).count();
        let passed = errors.is_empty() && passes >= p.minimum_passes;
        if passed {
            passing += 1
        } else if p.critical {
            critical.push(p.case_id.clone());
        }
        let tier = tiers.entry(p.evidence_tier.clone()).or_default();
        tier.0 += 1;
        tier.1 += usize::from(passed);
        tier.2 += rs.len();
        results.push(json!({"case_id":p.case_id,"evidence_tier":p.evidence_tier,"critical":p.critical,"repetitions":p.repetitions,"minimum_passes":p.minimum_passes,"observed_passes":passes,"passed":passed,"validation_errors":errors,"trials":rs.iter().map(|r|json!({"trial":r.trial,"passed":r.passed(),"checks":r.checks})).collect::<Vec<_>>()}));
    }
    let rate = passing as f64 / policies.len() as f64;
    if !critical.is_empty() {
        failures.push(format!(
            "critical capability failures: {}",
            critical.join(", ")
        ));
    }
    if rate < minimum {
        failures.push(format!("case pass rate {rate:.4} < {minimum:.4}"));
    }
    for r in &results {
        let e = r["validation_errors"].as_array().unwrap();
        if !e.is_empty() {
            failures.push(format!(
                "{}: {}",
                r["case_id"].as_str().unwrap(),
                e.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("; ")
            ));
        }
    }
    let tiers = tiers
        .into_iter()
        .map(|(k, (cases, passed, trials))| {
            (k, json!({"cases":cases,"passed":passed,"trials":trials}))
        })
        .collect::<serde_json::Map<_, _>>();
    Ok(
        json!({"schema_version":"assistant-capability-gate/v1","status":if failures.is_empty(){"pass"}else{"fail"},"passed":failures.is_empty(),"case_count":policies.len(),"passing_cases":passing,"case_pass_rate":(rate*10000.).round()/10000.,"minimum_case_pass_rate":minimum,"critical_failures":critical,"failures":failures,"evidence_tiers":tiers,"cases":results}),
    )
}
pub fn main() -> Result<i32> {
    let a: Vec<String> = std::env::args().skip(1).collect();
    need(
        a.len() >= 2,
        "usage: assistant_capability_live manifest receipts [--minimum-case-pass-rate NUMBER] [--output FILE]",
    )?;
    let mut minimum = 0.9;
    let mut output = None;
    let mut i = 2;
    while i < a.len() {
        let v = a.get(i + 1).ok_or("missing argument")?;
        match a[i].as_str() {
            "--minimum-case-pass-rate" => minimum = v.parse()?,
            "--output" => output = Some(v),
            _ => return Err("unknown argument".into()),
        }
        i += 2;
    }
    let policies = crate::snapshot(&a[0])?
        .0
        .into_iter()
        .map(CapabilityCasePolicy::from_value)
        .collect::<Result<Vec<_>>>()?;
    let receipts = crate::snapshot(&a[1])?
        .0
        .into_iter()
        .map(CapabilityTrialReceipt::from_value)
        .collect::<Result<Vec<_>>>()?;
    let r = evaluate(&policies, &receipts, minimum)?;
    let rendered = format!("{}\n", serde_json::to_string_pretty(&r)?);
    if let Some(p) = output {
        std::fs::write(p, rendered)?;
    } else {
        print!("{rendered}");
    }
    Ok(if r["passed"] == true { 0 } else { 1 })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn proxy_only_never_passes() {
        let p = CapabilityCasePolicy::from_value(
            json!({"case_id":"x","evidence_tier":"mock","repetitions":1,"minimum_passes":1}),
        )
        .unwrap();
        let r=CapabilityTrialReceipt::from_value(json!({"case_id":"x","evidence_tier":"mock","trial":1,"checks":[{"name":"non_empty_output","kind":"outcome","passed":true,"evidence":"nonempty"}]})).unwrap();
        assert_eq!(evaluate(&[p], &[r], 0.).unwrap()["passed"], false);
    }
    #[test]
    fn bool_is_not_trial_count() {
        assert!(
            CapabilityCasePolicy::from_value(
                json!({"case_id":"x","evidence_tier":"mock","repetitions":true})
            )
            .is_err()
        );
    }
    #[test]
    fn substantive_exact_receipt_passes() {
        let p=CapabilityCasePolicy::from_value(json!({"case_id":"x","evidence_tier":"local_live","repetitions":1,"minimum_passes":1,"critical":true})).unwrap();
        let r=CapabilityTrialReceipt::from_value(json!({"case_id":"x","evidence_tier":"local_live","trial":1,"checks":[{"name":"answer_matches_reference","kind":"outcome","passed":true,"evidence":"exact expected result"}]})).unwrap();
        assert_eq!(evaluate(&[p], &[r], 1.).unwrap()["passed"], true);
    }
}
