//! Observe native protocols without changing any forwarded bytes.
use super::*;
use std::collections::BTreeSet;
#[derive(Clone, Debug)]
pub(super) struct Observation {
    pub input: i64,
    pub output: i64,
    pub cache: crate::usage_parser::cache::CacheUsage,
}
fn count(value: &Value, key: &str) -> Option<i64> {
    value.get(key)?.as_i64().filter(|n| *n >= 0)
}
pub(super) fn usage(surface: Surface, provider: &str, value: &Value) -> Option<Observation> {
    let u = value.get("usage")?;
    let (input, output) = match surface {
        Surface::Chat => (count(u, "prompt_tokens")?, count(u, "completion_tokens")?),
        Surface::Embeddings => {
            let input = count(u, "prompt_tokens")?;
            if count(u, "total_tokens")? != input {
                return None;
            }
            (input, 0)
        }
        Surface::Messages => (count(u, "input_tokens")?, count(u, "output_tokens")?),
    };
    let mut cache = crate::usage_parser::cache::CacheUsage::observe(provider, value);
    // Anthropic input_tokens excludes cached reads/writes. The canonical ledger
    // counts total input; its cache partition then separates those same units.
    let input = if surface == Surface::Messages {
        let read = match &cache.read {
            crate::usage_parser::cache::Count::Reported(n) => *n,
            crate::usage_parser::cache::Count::NotReported => 0,
            _ => return None,
        };
        let write = match &cache.write {
            crate::usage_parser::cache::Count::Reported(n) => *n,
            crate::usage_parser::cache::Count::NotReported => 0,
            _ => return None,
        };
        cache.input_includes_cache = Some(true);
        input.checked_add(read)?.checked_add(write)?
    } else {
        input
    };
    cache.partition(provider, input).ok()?;
    Some(Observation {
        input,
        output,
        cache,
    })
}
pub(super) fn non_stream(surface: Surface, provider: &str, bytes: &[u8]) -> Option<Observation> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if value.get("error").is_some() {
        return usage(surface, provider, &value);
    }
    match surface {
        Surface::Chat
            if value["choices"].as_array()?.is_empty()
                || value["choices"]
                    .as_array()?
                    .iter()
                    .any(|c| c["finish_reason"].as_str().is_none()) =>
        {
            return None;
        }
        Surface::Messages
            if value["type"] != "message" || value["stop_reason"].as_str().is_none() =>
        {
            return None;
        }
        Surface::Embeddings if !value["data"].is_array() => return None,
        _ => {}
    }
    usage(surface, provider, &value)
}
pub(super) struct StreamMeter {
    surface: Surface,
    provider: String,
    buffer: Vec<u8>,
    data: Vec<String>,
    usage: Option<Value>,
    started: bool,
    stopped: bool,
    final_usage: bool,
    finished: BTreeSet<u64>,
    choices: u64,
    invalid: bool,
}
impl StreamMeter {
    pub fn new(surface: Surface, provider: String, choices: u64) -> Self {
        Self {
            surface,
            provider,
            buffer: vec![],
            data: vec![],
            usage: None,
            started: false,
            stopped: false,
            final_usage: false,
            finished: BTreeSet::new(),
            choices,
            invalid: false,
        }
    }
    pub fn push(&mut self, chunk: &[u8]) -> LlmResult<Option<Observation>> {
        self.buffer.extend_from_slice(chunk);
        while let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
            let line = self.buffer.drain(..=end).collect::<Vec<_>>();
            let line = std::str::from_utf8(&line)
                .map_err(|_| LlmError::new(502, "llm_stream_invalid"))?
                .trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                self.event()?;
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).into());
            }
            if self.data.iter().map(String::len).sum::<usize>() > 1024 * 1024 {
                return Err(LlmError::new(502, "llm_stream_event_limit"));
            }
        }
        if self.buffer.len() > 1024 * 1024 {
            return Err(LlmError::new(502, "llm_stream_event_limit"));
        }
        if self.invalid {
            return Err(LlmError::new(502, "llm_stream_error"));
        }
        if self.stopped {
            return self
                .usage
                .as_ref()
                .and_then(|u| usage(self.surface, &self.provider, &json!({"usage":u})))
                .map(Some)
                .ok_or(LlmError::new(502, "llm_terminal_usage_unknown"));
        }
        Ok(None)
    }
    fn event(&mut self) -> LlmResult<()> {
        if self.data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.data).join("\n");
        if self.stopped {
            return Err(LlmError::new(502, "llm_stream_after_terminal"));
        }
        if self.surface == Surface::Chat && data == "[DONE]" {
            self.stopped = self.finished.len() as u64 == self.choices;
            if !self.stopped {
                self.invalid = true;
            }
            return Ok(());
        }
        let value: Value =
            serde_json::from_str(&data).map_err(|_| LlmError::new(502, "llm_stream_invalid"))?;
        if value.get("error").is_some() || value["type"] == "error" {
            self.invalid = true;
            return Ok(());
        }
        match self.surface {
            Surface::Chat => {
                if let Some(choices) = value["choices"].as_array() {
                    for choice in choices {
                        if choice["finish_reason"].as_str().is_some() {
                            let index = choice["index"]
                                .as_u64()
                                .ok_or(LlmError::new(502, "llm_stream_invalid"))?;
                            if index >= self.choices {
                                self.invalid = true;
                            }
                            self.finished.insert(index);
                        }
                    }
                }
                if value["usage"].is_object() {
                    self.usage = Some(value["usage"].clone());
                }
            }
            Surface::Messages => match value["type"].as_str() {
                Some("message_start") => {
                    if self.started {
                        self.invalid = true;
                    }
                    self.started = true;
                    self.usage = Some(value["message"]["usage"].clone());
                }
                Some("message_delta") => {
                    if !self.started {
                        self.invalid = true;
                    }
                    if value["delta"]["stop_reason"].as_str().is_some() {
                        self.finished.insert(0);
                    }
                    if count(&value["usage"], "output_tokens").is_some() {
                        self.final_usage = true;
                    } else {
                        self.invalid = true;
                    }
                    if let (Some(current), Some(delta)) = (
                        self.usage.as_mut().and_then(Value::as_object_mut),
                        value["usage"].as_object(),
                    ) {
                        for (k, v) in delta {
                            if !v.is_null() {
                                if [
                                    "input_tokens",
                                    "output_tokens",
                                    "cache_read_input_tokens",
                                    "cache_creation_input_tokens",
                                ]
                                .contains(&k.as_str())
                                    && (v.as_i64().is_none_or(|n| n < 0)
                                        || current
                                            .get(k)
                                            .and_then(Value::as_i64)
                                            .zip(v.as_i64())
                                            .is_some_and(|(old, new)| new < old))
                                {
                                    self.invalid = true;
                                }
                                current.insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
                Some("message_stop") => {
                    self.stopped = self.started && self.final_usage && self.finished.contains(&0);
                    if !self.stopped {
                        self.invalid = true;
                    }
                }
                _ => {}
            },
            Surface::Embeddings => self.invalid = true,
        }
        Ok(())
    }
}
