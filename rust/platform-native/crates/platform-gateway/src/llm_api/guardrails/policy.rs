use super::*;
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Input,
    Output,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Injection,
    SensitiveData,
    ContentSafety,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Enforce,
    Observe,
}
/// Rules are bounded conjunctions of independent signals, not an implicit
/// universal semantic classifier. The operator chooses domain/category coverage.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub id: String,
    pub all: Vec<String>,
    pub any: Vec<String>,
    pub min_any: u8,
    pub score: u16,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagePolicy {
    pub stage: Stage,
    pub mode: Mode,
    pub directions: BTreeSet<Direction>,
    pub threshold: u16,
    pub rules: Vec<Rule>,
    pub detectors: BTreeSet<Detector>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Detector {
    Email,
    UsSsn,
    PaymentCard,
    PrivateKey,
    CloudAccessKey,
    CredentialAssignment,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema: String,
    pub version: String,
    pub enabled: bool,
    pub budget_ms: u16,
    pub max_input_bytes: u32,
    pub max_output_bytes: u32,
    pub stages: Vec<StagePolicy>,
}
pub(super) struct CompiledRule {
    pub id: String,
    pub all: Vec<Regex>,
    pub any: Vec<Regex>,
    pub min_any: usize,
    pub score: u16,
}
pub(super) struct CompiledStage {
    pub policy: StagePolicy,
    pub rules: Vec<CompiledRule>,
}
pub(super) struct CompiledPolicy {
    pub policy: Policy,
    pub pin: String,
    pub stages: Vec<CompiledStage>,
}
fn regex(s: &str) -> LlmResult<Regex> {
    if s.is_empty() || s.len() > 2048 {
        return Err(invalid());
    }
    RegexBuilder::new(s)
        .size_limit(256 * 1024)
        .dfa_size_limit(256 * 1024)
        .build()
        .map_err(|_| invalid())
}
impl Policy {
    pub fn validate(&self) -> LlmResult<()> {
        self.compile().map(|_| ())
    }
    pub(super) fn compile(&self) -> LlmResult<CompiledPolicy> {
        if self.schema != "agentforge.guardrails/v1"
            || !label(&self.version)
            || !(5..=500).contains(&self.budget_ms)
            || !(1024..=2_097_152).contains(&self.max_input_bytes)
            || !(1024..=4_194_304).contains(&self.max_output_bytes)
            || self.stages.len() != 3
        {
            return Err(invalid());
        }
        let mut stages = BTreeSet::new();
        let mut compiled = vec![];
        let mut total = 0;
        for stage in &self.stages {
            if !stages.insert(stage.stage)
                || stage.directions.is_empty()
                || stage.threshold == 0
                || stage.rules.len() > 32
                || stage.detectors.len() > 6
                || (stage.stage != Stage::SensitiveData && !stage.detectors.is_empty())
                || stage.rules.is_empty() && stage.detectors.is_empty()
            {
                return Err(invalid());
            }
            let mut ids = BTreeSet::new();
            let mut rules = vec![];
            for r in &stage.rules {
                total += r.all.iter().chain(&r.any).map(String::len).sum::<usize>();
                if !label(&r.id)
                    || !ids.insert(&r.id)
                    || r.score == 0
                    || r.all.len() > 8
                    || r.any.len() > 8
                    || r.all.is_empty() && r.any.is_empty()
                    || usize::from(r.min_any) > r.any.len()
                    || (!r.any.is_empty() && r.min_any == 0)
                {
                    return Err(invalid());
                }
                rules.push(CompiledRule {
                    id: r.id.clone(),
                    all: r.all.iter().map(|s| regex(s)).collect::<LlmResult<_>>()?,
                    any: r.any.iter().map(|s| regex(s)).collect::<LlmResult<_>>()?,
                    min_any: usize::from(r.min_any),
                    score: r.score,
                });
            }
            compiled.push(CompiledStage {
                policy: stage.clone(),
                rules,
            });
        }
        if total > 32 * 1024 {
            return Err(invalid());
        }
        let raw = serde_json::to_vec(self).map_err(|_| invalid())?;
        let pin =
            ai_platform_gateway_foundation::canonical_json_v2::CanonicalDocument::from_json(&raw)
                .and_then(|d| d.sha256_hex())
                .map_err(|_| invalid())?;
        Ok(CompiledPolicy {
            policy: self.clone(),
            pin,
            stages: compiled,
        })
    }
    pub fn pin(&self) -> LlmResult<String> {
        Ok(self.compile()?.pin)
    }
}
pub(super) fn label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 96
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-.:/".contains(&b))
}
pub(super) fn invalid() -> LlmError {
    LlmError::new(503, "llm_guardrails_policy_invalid")
}
