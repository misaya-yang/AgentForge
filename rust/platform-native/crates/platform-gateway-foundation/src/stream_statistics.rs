//! Side-effect-free accumulation/finalization from `GatewayDispatcher.stream`.
//!
//! The caller owns provider polling, cancellation, lease cleanup and persistence.
//! Elapsed milliseconds are supplied at the Python-equivalent observation points:
//! first upstream chunk, normal completion before stream_end, record fallback after
//! cleanup, and session persistence after cleanup. They must not share one clock
//! sample: normal recorded duration excludes cleanup; session duration includes it.
use crate::{
    dispatcher::StreamStatus,
    protocol::{ContentData, ContentItem, StreamChunk, StreamEventType},
    usage_parser::{
        UsageParseError, annotate_token_provenance, content_data_to_text, estimate_tokens,
        observe_token_provenance, python_add_json, python_whitespace,
    },
};
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::{collections::HashMap, fmt};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CollectedToolCall {
    pub tool_call_id: String,
    pub name: String,
    pub arguments: String,
    pub result: Option<String>,
    pub status: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamStatisticsError {
    /// Python would raise TypeError when adding these upstream token fields.
    IncompatibleTokenTotal,
    /// Python's arbitrary-precision integer sum does not fit a JSON number.
    TokenTotalOverflow,
}
impl fmt::Display for StreamStatisticsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::IncompatibleTokenTotal => "upstream token fields cannot be added",
            Self::TokenTotalOverflow => "upstream token total is outside the JSON number range",
        })
    }
}
impl std::error::Error for StreamStatisticsError {}

#[derive(Clone, Debug, PartialEq)]
pub struct StreamFinalization {
    pub status: StreamStatus,
    pub output_text: String,
    pub record_stats: Map<String, Value>,
    /// None without a session, or for blank output without tools or genuine SDK identity.
    pub session_metadata: Option<Map<String, Value>>,
}

#[derive(Clone, Debug, Default)]
pub struct StreamStatistics {
    accumulated_text: String,
    // A separate index keeps Python dict insertion order while permitting updates.
    tool_calls: Vec<CollectedToolCall>,
    tool_call_indices: HashMap<String, usize>,
    usage_stats: Map<String, Value>,
    usage_provenance: Map<String, Value>,
    first_chunk_ms: Option<i64>,
    recorded_stats: Map<String, Value>,
    // Identity is independent of usage and survives interruption before provider output.
    sdk_run_metadata: Option<Map<String, Value>>,
    sdk_stream: bool,
}

impl StreamStatistics {
    /// Select before polling so cancellation before the first chunk stays unknown.
    pub fn sdk() -> Self {
        let mut value = Self { sdk_stream: true, ..Self::default() };
        value.usage_stats.insert("sdk_usage".into(), json!({"state":"unavailable","cost_state":"unavailable"}));
        value
    }

    pub fn mark_sdk_cancelled(&mut self) {
        for stats in [&mut self.usage_stats, &mut self.recorded_stats] {
            if let Some(audit) = stats.get_mut("sdk_usage").and_then(Value::as_object_mut) {
                audit.insert("local_execution_status".into(), json!("cancelled"));
                audit.insert("provider_outcome".into(), json!("unknown"));
            }
        }
    }
    pub fn accumulated_text(&self) -> &str {
        &self.accumulated_text
    }
    pub fn tool_calls(&self) -> &[CollectedToolCall] {
        &self.tool_calls
    }
    pub fn usage_stats(&self) -> &Map<String, Value> {
        &self.usage_stats
    }
    pub fn first_chunk_ms(&self) -> Option<i64> {
        self.first_chunk_ms
    }
    pub fn recorded_stats(&self) -> &Map<String, Value> {
        &self.recorded_stats
    }

    /// Generic FINAL text retains the legacy non-accumulating behavior. Official
    /// SDK FINAL text is a verified suffix-only delta, so persist it before rewriting.
    /// SDK admission-only identity is not a provider event. All other existing
    /// event types, including lifecycle events and legacy empty chunks, set timing.
    pub fn observe(&mut self, mut chunk: StreamChunk, elapsed_ms: i64) -> StreamChunk {
        let sdk_identity = self.sdk_stream
            .then(|| chunk.metadata.as_ref().and_then(sdk_run_metadata)).flatten();
        let admission_only = sdk_identity.is_some() && chunk.chunk_index == 0
            && chunk.event_type == StreamEventType::TextDelta && !chunk.is_final
            && matches!(&chunk.content.data, Some(ContentData::Text(text) | ContentData::Json(Value::String(text))) if text.is_empty())
            && chunk.metadata.as_ref().is_some_and(|meta| !meta.contains_key("usage") && !meta.contains_key("sdk_lifecycle"));
        if self.sdk_run_metadata.is_none() { self.sdk_run_metadata = sdk_identity; }
        if !admission_only { self.first_chunk_ms.get_or_insert(elapsed_ms); }
        match chunk.event_type {
            StreamEventType::TextDelta | StreamEventType::Final
                if chunk.event_type == StreamEventType::TextDelta || (self.sdk_stream && chunk.is_final) => {
                if let Some(ContentData::Text(text) | ContentData::Json(Value::String(text))) =
                    &chunk.content.data
                {
                    self.accumulated_text.push_str(text);
                }
            }
            StreamEventType::ToolCallStart
            | StreamEventType::ToolCallDelta
            | StreamEventType::ToolCallEnd => {
                if let Some(call) = chunk
                    .tool_call
                    .as_ref()
                    .filter(|c| !c.tool_call_id.is_empty())
                {
                    let index = *self
                        .tool_call_indices
                        .entry(call.tool_call_id.clone())
                        .or_insert_with(|| {
                            let index = self.tool_calls.len();
                            self.tool_calls.push(CollectedToolCall {
                                tool_call_id: call.tool_call_id.clone(),
                                name: call.name.clone(),
                                arguments: String::new(),
                                result: None,
                                status: call.status.clone(),
                            });
                            index
                        });
                    let collected = &mut self.tool_calls[index];
                    collected.status.clone_from(&call.status);
                    if !call.name.is_empty() {
                        collected.name.clone_from(&call.name);
                    }
                    // Python len(str) counts code points, not UTF-8 bytes.
                    if call.arguments.chars().count() > collected.arguments.chars().count() {
                        collected.arguments.clone_from(&call.arguments);
                    }
                }
            }
            StreamEventType::ToolResult => {
                if let Some(call) = chunk.tool_call.as_ref().filter(|call| !call.tool_call_id.is_empty()) {
                    let index = *self.tool_call_indices.entry(call.tool_call_id.clone()).or_insert_with(|| {
                        let index = self.tool_calls.len();
                        self.tool_calls.push(CollectedToolCall {
                            tool_call_id: call.tool_call_id.clone(), name: call.name.clone(),
                            arguments: call.arguments.clone(), result: None, status: call.status.clone(),
                        });
                        index
                    });
                    let collected = &mut self.tool_calls[index];
                    collected.status.clone_from(&call.status);
                    if let Some(data) = &chunk.content.data {
                        collected.result = content_data_to_text(data);
                    }
                }
            }
            _ => {}
        }
        if let Some(Value::Object(usage)) = chunk.metadata.as_ref().and_then(|m| m.get("usage")) {
            self.usage_stats.extend(usage.clone());
            // A new value invalidates the old field's label unless supplied again.
            self.usage_provenance.extend(observe_token_provenance(usage));
        }
        if chunk.event_type == StreamEventType::Final && chunk.is_final {
            chunk.is_final = false;
            chunk.event_type = StreamEventType::TextDelta;
        }
        chunk
    }

    /// Create the normal frontend marker and freeze its stats before it is yielded.
    /// Upstream totals are not clamped or normalized in this stream metadata layer.
    pub fn stream_end(
        &mut self,
        request_id: &str,
        input_text: &str,
        elapsed_ms: i64,
    ) -> Result<StreamChunk, StreamStatisticsError> {
        let mut stats = self.usage_stats.clone();
        fill_normal_tokens(&mut stats, input_text, &self.accumulated_text)?;
        self.add_timing(&mut stats, elapsed_ms);
        // Preserve the legacy nested token/identity extractor payload verbatim.
        self.recorded_stats.clone_from(&stats);
        self.annotate_provenance(&mut stats);
        Ok(StreamChunk {
            request_id: request_id.into(),
            chunk_index: 9999,
            content: ContentItem::text(""),
            is_final: true,
            event_type: StreamEventType::StreamEnd,
            tool_call: None,
            metadata: Some(Map::from_iter([("usage".into(), Value::Object(stats))])),
        })
    }

    /// Call only after owned producer/lease cleanup. None skips session metadata;
    /// Some supplies its separate elapsed sample when session persistence applies.
    /// Cancellation after stream_end changes status but retains the frozen stats.
    pub fn finalize(
        &mut self,
        status: StreamStatus,
        input_text: &str,
        record_elapsed_ms: i64,
        session_elapsed_ms: Option<i64>,
    ) -> Result<StreamFinalization, StreamStatisticsError> {
        if self.recorded_stats.is_empty() {
            let mut stats = self.usage_stats.clone();
            if stats.contains_key("sdk_usage") {
                // Keep absence distinct from zero on error/cancellation.
            } else if status == StreamStatus::Error
                && self.usage_stats.is_empty()
                && python_is_blank(&self.accumulated_text)
            {
                for key in ["input_tokens", "output_tokens", "total_tokens"] {
                    stats.entry(key).or_insert(json!(0));
                }
            } else {
                fill_missing_tokens(&mut stats, input_text, &self.accumulated_text);
                fill_missing_total(&mut stats)?;
            }
            self.add_timing(&mut stats, record_elapsed_ms);
            self.recorded_stats = stats;
        }
        let session_metadata = match session_elapsed_ms {
            Some(elapsed_ms) if !python_is_blank(&self.accumulated_text) || !self.tool_calls.is_empty() || self.sdk_run_metadata.is_some() => {
                let mut metadata = self.sdk_run_metadata.clone().unwrap_or_default();
                // Local Gateway observation, not proof the upstream stopped.
                metadata.insert("gateway_stream_status".into(), json!(match status {
                    StreamStatus::Success => "completed",
                    StreamStatus::Error => "failed",
                    StreamStatus::InProgress | StreamStatus::Unknown => "interrupted",
                }));
                if self.sdk_run_metadata.is_some() {
                    // This describes the local stream, never the durable run/provider outcome.
                    metadata.insert("sdk_stream_status".into(), json!(match status {
                        StreamStatus::Success => "completed",
                        StreamStatus::Error => "failed",
                        StreamStatus::InProgress | StreamStatus::Unknown => "interrupted",
                    }));
                }
                if !self.tool_calls.is_empty() {
                    metadata.insert("tool_calls".into(), json!(self.tool_calls));
                }
                let mut stats = self.usage_stats.clone();
                fill_normal_tokens(&mut stats, input_text, &self.accumulated_text)?;
                self.annotate_provenance(&mut stats);
                self.add_timing(&mut stats, elapsed_ms);
                metadata.insert("stats".into(), Value::Object(stats));
                Some(metadata)
            }
            _ => None,
        };
        Ok(StreamFinalization {
            status,
            output_text: self.accumulated_text.clone(),
            record_stats: self.recorded_stats.clone(),
            session_metadata,
        })
    }

    fn annotate_provenance(&self, stats: &mut Map<String, Value>) {
        let estimated: Vec<_> = ["input_tokens", "output_tokens"].into_iter()
            .filter(|key| !self.usage_stats.contains_key(*key)).collect();
        let derived_total = !self.usage_stats.contains_key("total_tokens")
            && stats.contains_key("total_tokens");
        annotate_token_provenance(stats, &self.usage_provenance, &estimated, derived_total);
    }

    fn add_timing(&self, stats: &mut Map<String, Value>, elapsed_ms: i64) {
        stats.insert("duration_ms".into(), elapsed_ms.into());
        if let Some(first_chunk_ms) = self.first_chunk_ms {
            stats.insert("first_token_ms".into(), first_chunk_ms.into());
        }
    }
}

fn sdk_run_metadata(metadata: &Map<String, Value>) -> Option<Map<String, Value>> {
    let framework = metadata.get("sdk_framework")?.as_str()?;
    if !matches!(framework, "openai_agents" | "claude_agent_sdk") { return None; }
    let run = uuid::Uuid::parse_str(metadata.get("assistant_run_id")?.as_str()?).ok()?;
    Some(Map::from_iter([
        ("sdk_framework".into(), json!(framework)),
        ("assistant_run_id".into(), json!(run.to_string())),
    ]))
}

fn python_is_blank(text: &str) -> bool {
    text.chars().all(python_whitespace)
}

fn fill_missing_tokens(stats: &mut Map<String, Value>, input_text: &str, output_text: &str) {
    stats
        .entry("input_tokens")
        .or_insert_with(|| json!(estimate_tokens(input_text)));
    stats
        .entry("output_tokens")
        .or_insert_with(|| json!(estimate_tokens(output_text)));
}

fn fill_normal_tokens(
    stats: &mut Map<String, Value>,
    input_text: &str,
    output_text: &str,
) -> Result<(), StreamStatisticsError> {
    if stats.contains_key("sdk_usage") { return Ok(()); }
    // Deliberately leave total_tokens absent when both component fields existed.
    if !stats.contains_key("input_tokens") || !stats.contains_key("output_tokens") {
        fill_missing_tokens(stats, input_text, output_text);
        fill_missing_total(stats)?;
    }
    Ok(())
}

fn fill_missing_total(stats: &mut Map<String, Value>) -> Result<(), StreamStatisticsError> {
    if !stats.contains_key("total_tokens") {
        let input = stats.get("input_tokens").unwrap_or(&Value::Null);
        let output = stats.get("output_tokens").unwrap_or(&Value::Null);
        let total = python_add_json(input, output).map_err(|error| match error {
            UsageParseError::IntegerOverflow | UsageParseError::NonFiniteStatistic => {
                StreamStatisticsError::TokenTotalOverflow
            }
            _ => StreamStatisticsError::IncompatibleTokenTotal,
        })?;
        stats.insert("total_tokens".into(), total);
    }
    Ok(())
}

#[cfg(test)]
mod sdk_usage_statistics_tests {
    use super::*;
    #[test]
    fn sdk_absent_stream_usage_stays_absent() {
        let mut stats=StreamStatistics::sdk();
        let end=stats.stream_end("request","long input text",10).unwrap();
        assert!(end.metadata.unwrap()["usage"].get("input_tokens").is_none());
        let result=stats.finalize(StreamStatus::Success,"input",10,None).unwrap();
        assert!(!result.record_stats.contains_key("input_tokens"));
    }
    #[test]
    fn sdk_cancellation_is_not_provider_cancellation() {
        let mut stats=StreamStatistics::sdk(); stats.mark_sdk_cancelled();
        let result=stats.finalize(StreamStatus::Unknown,"input",10,None).unwrap();
        assert_eq!(result.record_stats["sdk_usage"]["local_execution_status"],"cancelled");
        assert_eq!(result.record_stats["sdk_usage"]["provider_outcome"],"unknown");
        assert!(!result.record_stats.contains_key("total_tokens"));
    }
    #[test]
    fn sdk_reported_fields_survive_stream_finalization() {
        let mut stats=StreamStatistics::sdk();
        stats.observe(StreamChunk {request_id:"r".into(),chunk_index:0,content:ContentItem::text(""),is_final:true,event_type:StreamEventType::Final,tool_call:None,
            metadata:Some(Map::from_iter([("usage".into(),json!({"input_tokens":7,"output_tokens":3,"total_tokens":10,"sdk_usage":{"state":"reported","assistant_run_id":"run","provider_cost_usd":0.5}}))]))},1);
        stats.stream_end("r","input",2).unwrap();
        let result=stats.finalize(StreamStatus::Success,"input",3,None).unwrap();
        assert_eq!(result.record_stats["total_tokens"],10);
        assert_eq!(result.record_stats["sdk_usage"]["assistant_run_id"],"run");
        assert_eq!(result.record_stats["sdk_usage"]["provider_cost_usd"],0.5);
    }
}

#[cfg(test)]
mod sdk_identity_history_tests {
    use super::*;
    const RUN: &str = "00000000-0000-4000-8000-000000000001";
    fn identity() -> StreamChunk {
        StreamChunk {
            request_id: "request-is-not-a-run".into(), chunk_index: 0,
            content: ContentItem::text(""), is_final: false,
            event_type: StreamEventType::TextDelta, tool_call: None,
            metadata: Some(Map::from_iter([
                ("sdk_framework".into(), json!("openai_agents")),
                ("assistant_run_id".into(), json!(RUN)),
            ])),
        }
    }
    #[test]
    fn sdk_empty_history_preserves_identity_and_local_outcome_without_usage() {
        for (status, expected) in [(StreamStatus::Unknown, "interrupted"),
            (StreamStatus::Error, "failed"), (StreamStatus::Success, "completed")] {
            let mut stats = StreamStatistics::sdk();
            stats.observe(identity(), 10);
            if status == StreamStatus::Unknown { stats.mark_sdk_cancelled(); }
            let result = stats.finalize(status, "input", 100, Some(101)).unwrap();
            let metadata = result.session_metadata.unwrap();
            assert_eq!(metadata["assistant_run_id"], RUN);
            assert_eq!(metadata["sdk_framework"], "openai_agents");
            assert_eq!(metadata["sdk_stream_status"], expected);
            assert!(result.output_text.is_empty());
            assert_eq!(metadata["stats"]["sdk_usage"]["state"], "unavailable");
            for key in ["input_tokens", "output_tokens", "total_tokens", "first_token_ms"] {
                assert!(metadata["stats"].get(key).is_none(), "{key}");
                assert!(!result.record_stats.contains_key(key), "{key}");
            }
            assert_ne!(metadata["stats"]["sdk_usage"]["provider_outcome"], "completed");
        }
    }
    #[test]
    fn sdk_partial_history_keeps_early_identity_without_terminal_usage() {
        let mut stats = StreamStatistics::sdk();
        stats.observe(identity(), 10);
        let mut delta = identity(); delta.chunk_index = 1;
        delta.content = ContentItem::text("partial"); delta.metadata = None;
        stats.observe(delta, 80);
        let result = stats.finalize(StreamStatus::Error, "input", 100, Some(101)).unwrap();
        assert_eq!(result.output_text, "partial");
        let metadata = result.session_metadata.unwrap();
        assert_eq!(metadata["assistant_run_id"], RUN);
        assert_eq!(metadata["sdk_stream_status"], "failed");
        assert_eq!(metadata["stats"]["first_token_ms"], 80);
        assert!(metadata["stats"].get("total_tokens").is_none());
    }
    #[test]
    fn sdk_admission_does_not_replace_existing_provider_event_timing() {
        for event in [StreamEventType::TextDelta, StreamEventType::ApprovalRequired] {
            let mut stats = StreamStatistics::sdk();
            stats.observe(identity(), 10);
            assert_eq!(stats.first_chunk_ms(), None);
            let mut provider = identity(); provider.chunk_index = 1; provider.event_type = event;
            provider.content = ContentItem::text("provider event");
            stats.observe(provider, 80);
            assert_eq!(stats.first_chunk_ms(), Some(80));
        }
        let mut legacy = StreamStatistics::default();
        legacy.observe(identity(), 10);
        assert_eq!(legacy.first_chunk_ms(), Some(10));
        assert!(legacy.finalize(StreamStatus::Success, "input", 20, Some(21)).unwrap().session_metadata.is_none());
    }
    #[test]
    fn sdk_empty_history_requires_valid_identity_and_enabled_session() {
        for value in [json!("request-is-not-a-run"), Value::Null] {
            let mut stats = StreamStatistics::sdk();
            let mut chunk = identity();
            chunk.metadata.as_mut().unwrap().insert("assistant_run_id".into(), value);
            stats.observe(chunk, 10);
            assert!(stats.finalize(StreamStatus::Unknown, "input", 20, Some(21)).unwrap().session_metadata.is_none());
        }
        let mut stats = StreamStatistics::sdk(); stats.observe(identity(), 10);
        assert!(stats.finalize(StreamStatus::Unknown, "input", 20, None).unwrap().session_metadata.is_none());
    }
    #[test]
    fn sdk_empty_completion_preserves_real_zero_usage_and_independent_identity() {
        let mut stats = StreamStatistics::sdk(); stats.observe(identity(), 10);
        let mut terminal = identity(); terminal.chunk_index = 1;
        terminal.event_type = StreamEventType::Final; terminal.is_final = true;
        terminal.metadata = Some(Map::from_iter([("usage".into(), json!({
            "input_tokens":0,"output_tokens":0,"total_tokens":0,
            "sdk_usage":{"state":"reported","provider_outcome":"completed"}
        }))]));
        stats.observe(terminal, 80); stats.stream_end("request", "input", 90).unwrap();
        let metadata = stats.finalize(StreamStatus::Success, "input", 100, Some(101)).unwrap().session_metadata.unwrap();
        assert_eq!(metadata["assistant_run_id"], RUN);
        assert_eq!(metadata["sdk_stream_status"], "completed");
        assert_eq!(metadata["stats"]["total_tokens"], 0);
        assert_eq!(metadata["stats"]["sdk_usage"]["state"], "reported");
    }

    #[test]
    fn sdk_final_only_text_is_preserved_in_history() {
        let mut stats = StreamStatistics::sdk(); stats.observe(identity(), 10);
        let mut terminal = identity(); terminal.chunk_index = 1;
        terminal.event_type = StreamEventType::Final; terminal.is_final = true;
        terminal.content = ContentItem::text("answer");
        let forwarded = stats.observe(terminal, 80);
        assert_eq!(forwarded.event_type, StreamEventType::TextDelta);
        assert!(!forwarded.is_final);
        let result = stats.finalize(StreamStatus::Success, "input", 100, Some(101)).unwrap();
        assert_eq!(result.output_text, "answer");
        assert_eq!(result.session_metadata.unwrap()["assistant_run_id"], RUN);
    }
    #[test]
    fn sdk_final_suffix_extends_history_once_without_repeating_prior_deltas() {
        let mut stats = StreamStatistics::sdk(); stats.observe(identity(), 10);
        let mut delta = identity(); delta.chunk_index = 1; delta.content = ContentItem::text("hello");
        stats.observe(delta, 50);
        let mut terminal = identity(); terminal.chunk_index = 2;
        terminal.event_type = StreamEventType::Final; terminal.is_final = true;
        terminal.content = ContentItem::text(" world");
        stats.observe(terminal, 80); stats.stream_end("request", "input", 90).unwrap();
        let result = stats.finalize(StreamStatus::Success, "input", 100, Some(101)).unwrap();
        assert_eq!(result.output_text, "hello world");
        assert_eq!(result.session_metadata.unwrap()["sdk_stream_status"], "completed");
    }
    #[test]
    fn generic_final_history_is_unchanged_even_with_sdk_looking_metadata() {
        let mut stats = StreamStatistics::default();
        let mut delta = identity(); delta.content = ContentItem::text("legacy prefix");
        delta.metadata.as_mut().unwrap().insert("usage".into(), json!({"sdk_usage":{"state":"unavailable"}}));
        stats.observe(delta, 10);
        let mut terminal = identity(); terminal.chunk_index = 1;
        terminal.event_type = StreamEventType::Final; terminal.is_final = true;
        terminal.content = ContentItem::text("legacy full final answer");
        stats.observe(terminal, 80);
        let result = stats.finalize(StreamStatus::Success, "input", 100, Some(101)).unwrap();
        assert_eq!(result.output_text, "legacy prefix");
        let metadata = result.session_metadata.unwrap();
        assert!(!metadata.contains_key("assistant_run_id"));
        assert!(!metadata.contains_key("sdk_stream_status"));
    }
}

#[cfg(test)]
mod token_provenance_tests {
    use super::*;
    use crate::{protocol::{UnifiedRequest, UnifiedResponse}, usage_parser::sync_response_stats};
    fn chunk(usage: Option<Value>, text: &str) -> StreamChunk {
        StreamChunk { request_id: "fixture-request".into(), chunk_index: 0,
            content: ContentItem::text(text), is_final: false,
            event_type: StreamEventType::TextDelta, tool_call: None,
            metadata: usage.map(|value| Map::from_iter([("usage".into(), value)])) }
    }
    fn counts(stats: &Map<String, Value>) -> Value {
        Value::Object(["input_tokens", "output_tokens", "total_tokens"].into_iter()
            .filter_map(|key| stats.get(key).map(|value| (key.into(), value.clone()))).collect())
    }
    #[test]
    fn shared_stream_and_sync_provenance_cases() {
        let fixture: Value = serde_json::from_str(include_str!("../../../../../tests/fixtures/gateway/token_provenance_v1.json")).unwrap();
        let input = fixture["input_text"].as_str().unwrap();
        let output = fixture["output_text"].as_str().unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let usage = case["usage"].as_object().cloned();
            let mut statistics = StreamStatistics::default();
            statistics.observe(chunk(usage.clone().map(Value::Object), output), 2);
            let terminal = statistics.stream_end("fixture-request", input, 10).unwrap();
            let terminal = terminal.metadata.unwrap()["usage"].as_object().unwrap().clone();
            let finalized = statistics.finalize(StreamStatus::Success, input, 11, Some(12)).unwrap();
            // Presentation labels must never affect legacy recursive accounting extraction.
            assert_eq!(finalized.record_stats.get("token_provenance"), usage.as_ref().and_then(|value| value.get("token_provenance")), "{}", case["name"]);
            let mut expected_record = usage.clone().unwrap_or_default();
            expected_record.extend(case["stream_counts"].as_object().unwrap().clone());
            let recorded = json!({"usage": finalized.record_stats});
            let expected = json!({"usage": expected_record});
            assert_eq!(crate::usage_parser::extract_token_usage(&recorded), crate::usage_parser::extract_token_usage(&expected), "{}", case["name"]);
            assert_eq!(crate::usage_parser::extract_model(&recorded), crate::usage_parser::extract_model(&expected), "{}", case["name"]);
            assert_eq!(crate::usage_parser::extract_provider(&recorded), crate::usage_parser::extract_provider(&expected), "{}", case["name"]);
            assert_eq!(crate::usage_parser::extract_assistant_id(&recorded), crate::usage_parser::extract_assistant_id(&expected), "{}", case["name"]);
            let history = finalized.session_metadata.unwrap();
            assert_eq!(counts(&terminal), case["stream_counts"], "{}", case["name"]);
            assert_eq!(terminal["token_provenance"], case["stream_provenance"], "{}", case["name"]);
            assert_eq!(counts(history["stats"].as_object().unwrap()), case["stream_counts"], "{}", case["name"]);
            assert_eq!(history["stats"]["token_provenance"], case["stream_provenance"], "{}", case["name"]);
            let request = UnifiedRequest::new("fixture-request", "fixture", vec![ContentItem::text(input)]);
            let mut response = UnifiedResponse::new("fixture-request", "success", vec![ContentItem::text(output)]);
            response.usage = usage;
            let sync = sync_response_stats(&request, &response, 10).unwrap();
            assert_eq!(counts(&sync), case["sync_counts"], "{}", case["name"]);
            assert_eq!(sync["token_provenance"], case["sync_provenance"], "{}", case["name"]);
        }
    }
    #[test]
    fn later_counts_clear_stale_labels_but_label_only_updates_do_not_relabel() {
        let mut statistics = StreamStatistics::default();
        statistics.observe(chunk(Some(json!({"input_tokens":2,"output_tokens":3,"token_provenance":{"input_tokens":"estimated","output_tokens":"estimated"}})), ""), 1);
        statistics.observe(chunk(Some(json!({"input_tokens":5})), ""), 2);
        statistics.observe(chunk(Some(json!({"token_provenance":{"output_tokens":"upstream"}})), ""), 3);
        let end = statistics.stream_end("fixture-request", "input", 10).unwrap().metadata.unwrap();
        assert_eq!(end["usage"]["token_provenance"], json!({"input_tokens":"upstream","output_tokens":"estimated"}));
        assert_eq!(counts(end["usage"].as_object().unwrap()), json!({"input_tokens":5,"output_tokens":3}));
    }
    #[test]
    fn error_placeholder_zero_is_unknown_and_partial_cancellation_is_estimated() {
        let mut statistics = StreamStatistics::default();
        let failed = statistics.finalize(StreamStatus::Error, "input", 10, None).unwrap();
        assert_eq!(counts(&failed.record_stats), json!({"input_tokens":0,"output_tokens":0,"total_tokens":0}));
        assert!(!failed.record_stats.contains_key("token_provenance"));
        let mut partial = StreamStatistics::default();
        partial.observe(chunk(None, "abcdefghijkl"), 1);
        let cancelled = partial.finalize(StreamStatus::Unknown, "abcdefghijklmnop", 10, Some(11)).unwrap();
        assert_eq!(counts(&cancelled.record_stats), json!({"input_tokens":4,"output_tokens":3,"total_tokens":7}));
        assert!(!cancelled.record_stats.contains_key("token_provenance"));
        assert_eq!(cancelled.session_metadata.unwrap()["stats"]["token_provenance"], json!({"input_tokens":"estimated","output_tokens":"estimated","total_tokens":"derived"}));
    }
    #[test]
    fn invalid_arithmetic_retains_existing_errors() {
        for invalid in [Value::Null, json!([]), json!({}), json!("invalid")] {
            let mut statistics = StreamStatistics::default();
            statistics.observe(chunk(Some(json!({"input_tokens":invalid})), "output"), 1);
            assert_eq!(statistics.stream_end("fixture-request", "input", 10), Err(StreamStatisticsError::IncompatibleTokenTotal));
            let request = UnifiedRequest::new("fixture-request", "fixture", vec![ContentItem::text("input")]);
            let mut response = UnifiedResponse::new("fixture-request", "success", vec![ContentItem::text("output")]);
            response.usage = Some(Map::from_iter([("input_tokens".into(), invalid)]));
            assert_eq!(sync_response_stats(&request, &response, 10), Err(UsageParseError::InvalidTokenAddition));
        }
    }
    #[test]
    fn sdk_labels_remain_independent_and_absent_usage_stays_absent() {
        let mut statistics = StreamStatistics::sdk();
        let end = statistics.stream_end("fixture-request", "input", 10).unwrap().metadata.unwrap();
        assert!(end["usage"].get("token_provenance").is_none());
        assert!(end["usage"].get("input_tokens").is_none());
        let mut statistics = StreamStatistics::sdk();
        statistics.observe(chunk(Some(json!({"input_tokens":0,"output_tokens":0,"total_tokens":0,"sdk_usage":{"state":"reported"}})), "output"), 1);
        let end = statistics.stream_end("fixture-request", "input", 10).unwrap().metadata.unwrap();
        assert!(end["usage"].get("token_provenance").is_none());
        assert_eq!(counts(end["usage"].as_object().unwrap()), json!({"input_tokens":0,"output_tokens":0,"total_tokens":0}));
    }
}
