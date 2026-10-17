//! Provider-namespaced cache observations. Absence is not a reported zero.
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "tokens", rename_all = "snake_case")]
pub enum Count {
    Reported(i64),
    NotReported,
    Invalid,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CacheUsage {
    pub provider: String,
    pub input_includes_cache: Option<bool>,
    pub read: Count,
    pub write: Count,
    pub raw: Value,
}
fn count(v: Option<&Value>) -> Count {
    match v {
        None => Count::NotReported,
        Some(v) => v
            .as_i64()
            .filter(|n| *n >= 0)
            .map(Count::Reported)
            .unwrap_or(Count::Invalid),
    }
}
impl CacheUsage {
    pub fn observe(provider: &str, payload: &Value) -> Self {
        let usage = payload
            .get("usage")
            .or_else(|| payload.get("usageMetadata"))
            .or_else(|| payload.get("response").and_then(|r| r.get("usage")))
            .unwrap_or(payload);
        // Select by wire vocabulary, never by model-name prefixes. Preserve the
        // configured provider identity even for OpenAI-compatible providers.
        let (includes, read, write) = if usage.get("cache_read_input_tokens").is_some()
            || usage.get("cache_creation_input_tokens").is_some()
        {
            (
                Some(false),
                count(usage.get("cache_read_input_tokens")),
                count(usage.get("cache_creation_input_tokens")),
            )
        } else if usage.get("promptTokenCount").is_some() {
            (
                Some(true),
                count(usage.get("cachedContentTokenCount")),
                Count::NotReported,
            )
        } else if usage.get("prompt_tokens").is_some() || usage.get("input_tokens").is_some() {
            (
                Some(true),
                count(
                    usage
                        .get("prompt_tokens_details")
                        .or_else(|| usage.get("input_tokens_details"))
                        .and_then(|v| v.get("cached_tokens")),
                ),
                Count::NotReported,
            )
        } else {
            (None, Count::NotReported, Count::NotReported)
        };
        Self {
            provider: provider.into(),
            input_includes_cache: includes,
            read,
            write,
            raw: usage.clone(),
        }
    }
    /// Disjoint components; an absent field remains explicit evidence, not an
    /// invented hit. Only providers reporting cache counts select cache pricing.
    pub fn partition(&self, provider: &str, input: i64) -> Result<(i64, i64, i64), &'static str> {
        if self.provider != provider || input < 0 {
            return Err("cache identity/count invalid");
        }
        let n = |c: &Count| match c {
            Count::Reported(n) if *n >= 0 => Ok(*n),
            Count::NotReported => Ok(0),
            _ => Err("invalid cache count"),
        };
        let read = n(&self.read)?;
        let write = n(&self.write)?;
        let cached = read.checked_add(write).ok_or("cache overflow")?;
        let plain = match self.input_includes_cache {
            Some(true) => input
                .checked_sub(cached)
                .filter(|n| *n >= 0)
                .ok_or("cache exceeds input")?,
            Some(false) => input,
            None if cached == 0 => input,
            _ => return Err("unknown cache semantics"),
        };
        Ok((plain, read, write))
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn openai_subset_and_anthropic_disjoint() {
        let a = CacheUsage::observe(
            "vendor-a",
            &json!({"usage":{"prompt_tokens":100,"prompt_tokens_details":{"cached_tokens":80,"vendor_extra":7}}}),
        );
        assert_eq!(a.partition("vendor-a", 100), Ok((20, 80, 0)));
        assert!(a.partition("vendor-b", 100).is_err());
        assert_eq!(a.raw["prompt_tokens_details"]["vendor_extra"], 7);
        let b = CacheUsage::observe(
            "anthropic",
            &json!({"usage":{"input_tokens":20,"cache_read_input_tokens":80,"cache_creation_input_tokens":10}}),
        );
        assert_eq!(b.partition("anthropic", 20), Ok((20, 80, 10)));
    }
    #[test]
    fn unknown_and_invalid_are_not_reported_zero() {
        assert_eq!(
            CacheUsage::observe("x", &json!({})).read,
            Count::NotReported
        );
        let bad = CacheUsage::observe(
            "x",
            &json!({"input_tokens":1,"input_tokens_details":{"cached_tokens":2}}),
        );
        assert!(bad.partition("x", 1).is_err());
        assert!(
            CacheUsage::observe(
                "x",
                &json!({"input_tokens":1,"input_tokens_details":{"cached_tokens":-1}})
            )
            .partition("x", 1)
            .is_err()
        );
    }
}
