//! Read capability executor: dispatches tenant-bound read-only tool calls.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::Arc;

use ai_platform_capability_contract::canonical_json_hash;
use reqwest::Url;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::fs::{glob_files, grep_files, read_file};
use super::proof::{CapabilityProofInput, sign_capability_proof};
use super::web::{is_public_ip, strip_html, validated_public_url};
use super::working_memory::{render_working_memory, todo_read_empty};
use super::{
    MAX_MCP_RESPONSE_BYTES, MAX_REDIRECTS, ReadCapabilityConfig, ReadCapabilityContext,
    ReadCapabilityError, ReadHttpAdapter, SessionMemoryReadAdapter, internal_endpoint,
    working_memory_key,
};

#[derive(Clone)]
pub struct ReadCapabilityExecutor {
    config: ReadCapabilityConfig,
    http: Arc<dyn ReadHttpAdapter>,
    session_memory: Option<Arc<dyn SessionMemoryReadAdapter>>,
}

impl ReadCapabilityExecutor {
    pub fn new(
        config: ReadCapabilityConfig,
        http: Arc<dyn ReadHttpAdapter>,
    ) -> Result<Self, ReadCapabilityError> {
        config.validate()?;
        Ok(Self {
            config,
            http,
            session_memory: None,
        })
    }

    pub fn with_session_memory(
        mut self,
        session_memory: Arc<dyn SessionMemoryReadAdapter>,
    ) -> Self {
        self.session_memory = Some(session_memory);
        self
    }

    pub async fn execute(
        &self,
        capability_id: &str,
        context: &ReadCapabilityContext,
        arguments: Value,
    ) -> Result<Value, ReadCapabilityError> {
        context.validate()?;
        if !arguments.is_object() {
            return Err(ReadCapabilityError::Arguments);
        }
        if context
            .connector_binding
            .as_ref()
            .is_some_and(|binding| binding.provider == "mcp" && binding.tool_name == capability_id)
        {
            return self.mcp_read(context, capability_id, &arguments).await;
        }
        match capability_id {
            "search_knowledge_base" => self.search_knowledge_base(context, &arguments).await,
            "web_fetch" => self.web_fetch(&arguments).await,
            "search_web" => self.web_search(context, &arguments).await,
            "fs_read" => self.fs_read(context, &arguments).await,
            "fs_glob" => self.fs_glob(context, &arguments).await,
            "fs_grep" => self.fs_grep(context, &arguments).await,
            "read_tool_artifact" => self.read_tool_artifact(context, &arguments).await,
            "confluence_read" => self.confluence_read(context, &arguments).await,
            "todo_read" => self.todo_read(context).await,
            _ => Err(ReadCapabilityError::NotFound),
        }
    }

    async fn todo_read(
        &self,
        context: &ReadCapabilityContext,
    ) -> Result<Value, ReadCapabilityError> {
        let memory = self
            .session_memory
            .as_ref()
            .ok_or(ReadCapabilityError::WorkerFailed)?;
        let key = working_memory_key(&context.tenant_id, &context.user_id, &context.session_id);
        let envelope = memory
            .get_session_memory(&context.tenant_id, &context.session_id, &key)
            .await?;
        let Some(envelope) = envelope else {
            return Ok(todo_read_empty());
        };
        render_working_memory(&envelope, context)
    }

    async fn search_knowledge_base(
        &self,
        context: &ReadCapabilityContext,
        arguments: &Value,
    ) -> Result<Value, ReadCapabilityError> {
        let query = string_argument(arguments, "query", 1, 4_096)?;
        let intent = arguments
            .get("intent")
            .and_then(Value::as_str)
            .unwrap_or("general");
        if !matches!(intent, "general" | "find_document") {
            return Err(ReadCapabilityError::Arguments);
        }
        let dataset_ids = match arguments.get("dataset_ids") {
            Some(_) => string_array_argument(arguments, "dataset_ids", 8, 128)?,
            None => context.bound_dataset_ids.iter().cloned().collect(),
        };
        if dataset_ids.is_empty()
            || dataset_ids.iter().any(|dataset_id| {
                !valid_dataset_id(dataset_id) || !context.bound_dataset_ids.contains(dataset_id)
            })
        {
            return Err(ReadCapabilityError::Scope);
        }
        let top_k = integer_argument(arguments, "top_k", 5, 1, 20)?;
        let score_threshold = number_argument(arguments, "score_threshold", 0.0, 0.0, 1.0)?;
        let mut results = Vec::new();
        let mut metadata = BTreeMap::new();
        for dataset_id in &dataset_ids {
            let url = internal_endpoint(
                &self.config.knowledge_base_url,
                &format!("internal/v2/capabilities/knowledge/{dataset_id}/retrieve"),
            )?;
            let body = json!({
                "query": query,
                "top_k": top_k,
                "threshold": score_threshold
            });
            let headers = self.internal_headers(context, &url, &body)?;
            let value = self.http.post_internal_json(url, &headers, body).await?;
            results.extend(
                value
                    .get("results")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            metadata.insert(
                dataset_id.clone(),
                value.get("metadata").cloned().unwrap_or_else(|| json!({})),
            );
        }
        Ok(json!({
            "query": query,
            "intent": intent,
            "dataset_ids": dataset_ids,
            "results": results,
            "metadata": metadata
        }))
    }

    async fn web_fetch(&self, arguments: &Value) -> Result<Value, ReadCapabilityError> {
        let raw_url = string_argument(arguments, "url", 1, 4_096)?;
        let max_chars = integer_argument(arguments, "max_chars", 8_000, 1, 32_000)? as usize;
        let extract = arguments
            .get("extract")
            .and_then(Value::as_str)
            .unwrap_or("markdown");
        if !matches!(extract, "markdown" | "text" | "raw") {
            return Err(ReadCapabilityError::Arguments);
        }
        let mut current = validated_public_url(raw_url).await?;
        for redirect in 0..=MAX_REDIRECTS {
            let response = self.http.get_public(current.clone()).await?;
            match response.remote_ip {
                Some(remote_ip) if is_public_ip(remote_ip) => {}
                _ => return Err(ReadCapabilityError::SsrfBlocked),
            }
            if (300..400).contains(&response.status) {
                if redirect == MAX_REDIRECTS {
                    return Err(ReadCapabilityError::DownstreamRejected);
                }
                let location = response
                    .location
                    .ok_or(ReadCapabilityError::DownstreamRejected)?;
                current = validated_public_url(
                    current
                        .join(&location)
                        .map_err(|_| ReadCapabilityError::SsrfBlocked)?
                        .as_str(),
                )
                .await?;
                continue;
            }
            if !(200..300).contains(&response.status) {
                return Err(ReadCapabilityError::DownstreamRejected);
            }
            let raw = String::from_utf8_lossy(&response.bytes);
            let extracted = if extract == "raw" {
                raw.into_owned()
            } else {
                strip_html(&raw)
            };
            let content: String = extracted.chars().take(max_chars).collect();
            return Ok(json!({
                "url": current.as_str(),
                "status": response.status,
                "content_type": response.content_type,
                "content": content,
                "truncated": extracted.chars().count() > content.chars().count()
            }));
        }
        Err(ReadCapabilityError::DownstreamRejected)
    }

    async fn web_search(
        &self,
        context: &ReadCapabilityContext,
        arguments: &Value,
    ) -> Result<Value, ReadCapabilityError> {
        let queries = string_array_argument(arguments, "queries", 5, 500)?;
        if queries.is_empty() {
            return Err(ReadCapabilityError::Arguments);
        }
        let max_results = integer_argument(arguments, "max_results", 5, 1, 10)?;
        let url = internal_endpoint(
            &self.config.gateway_url,
            "internal/v2/agent-capabilities/web-search",
        )?;
        let body = json!({"queries": queries, "max_results": max_results});
        let headers = self.internal_headers(context, &url, &body)?;
        self.http.post_internal_json(url, &headers, body).await
    }

    async fn fs_read(
        &self,
        context: &ReadCapabilityContext,
        arguments: &Value,
    ) -> Result<Value, ReadCapabilityError> {
        let root = self.scoped_workspace_root(context);
        let path = string_argument(arguments, "path", 1, 1_024)?.to_string();
        let offset = integer_argument(arguments, "offset", 0, 0, 1_000_000)? as usize;
        let limit = integer_argument(arguments, "limit", 2_000, 1, 20_000)? as usize;
        tokio::task::spawn_blocking(move || read_file(&root, &path, offset, limit))
            .await
            .map_err(|_| ReadCapabilityError::WorkerFailed)?
    }

    async fn fs_glob(
        &self,
        context: &ReadCapabilityContext,
        arguments: &Value,
    ) -> Result<Value, ReadCapabilityError> {
        let root = self.scoped_workspace_root(context);
        let pattern = string_argument(arguments, "pattern", 1, 1_024)?.to_string();
        tokio::task::spawn_blocking(move || glob_files(&root, &pattern))
            .await
            .map_err(|_| ReadCapabilityError::WorkerFailed)?
    }

    async fn fs_grep(
        &self,
        context: &ReadCapabilityContext,
        arguments: &Value,
    ) -> Result<Value, ReadCapabilityError> {
        let root = self.scoped_workspace_root(context);
        let pattern = string_argument(arguments, "pattern", 1, 1_024)?.to_string();
        let glob = arguments
            .get("glob")
            .and_then(Value::as_str)
            .unwrap_or("**/*")
            .to_string();
        let case_sensitive = arguments
            .get("case_sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        tokio::task::spawn_blocking(move || grep_files(&root, &pattern, &glob, case_sensitive))
            .await
            .map_err(|_| ReadCapabilityError::WorkerFailed)?
    }

    async fn read_tool_artifact(
        &self,
        context: &ReadCapabilityContext,
        arguments: &Value,
    ) -> Result<Value, ReadCapabilityError> {
        let artifact_id = string_argument(arguments, "artifact_id", 12, 68)?;
        if !artifact_id.starts_with("art_")
            || !artifact_id[4..]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
        {
            return Err(ReadCapabilityError::Arguments);
        }
        let offset = integer_argument(arguments, "offset", 0, 0, 2_000_000)?;
        let limit = integer_argument(arguments, "limit", 4_000, 1, 4_000)?;
        let url = internal_endpoint(
            &self.config.gateway_url,
            &format!("internal/v2/agent-capabilities/artifacts/{artifact_id}/read"),
        )?;
        let body = json!({"offset": offset, "limit": limit});
        let headers = self.internal_headers(context, &url, &body)?;
        self.http.post_internal_json(url, &headers, body).await
    }

    async fn confluence_read(
        &self,
        context: &ReadCapabilityContext,
        arguments: &Value,
    ) -> Result<Value, ReadCapabilityError> {
        let action = arguments
            .get("action")
            .and_then(Value::as_str)
            .ok_or(ReadCapabilityError::Arguments)?;
        if !matches!(
            action,
            "search" | "read_page" | "list_spaces" | "get_space" | "list_children"
        ) {
            return Err(ReadCapabilityError::Arguments);
        }
        let binding = context
            .connector_binding
            .as_ref()
            .filter(|binding| {
                binding.provider == "confluence"
                    && binding.tool_name == "confluence_read"
                    && !binding.channel.is_empty()
            })
            .ok_or(ReadCapabilityError::Scope)?;
        let url = internal_endpoint(
            &self.config.gateway_url,
            "internal/v2/agent-capabilities/confluence/read",
        )?;
        let envelope = json!({
            "arguments": arguments,
            "binding": binding,
        });
        let headers = self.internal_headers(context, &url, &envelope)?;
        self.http.post_internal_json(url, &headers, envelope).await
    }

    async fn mcp_read(
        &self,
        context: &ReadCapabilityContext,
        capability_id: &str,
        arguments: &Value,
    ) -> Result<Value, ReadCapabilityError> {
        let binding = context
            .connector_binding
            .as_ref()
            .filter(|binding| binding.provider == "mcp" && binding.tool_name == capability_id)
            .ok_or(ReadCapabilityError::Scope)?;
        let connection_id = binding
            .connection_id
            .as_deref()
            .filter(|value| !value.is_empty())
            .ok_or(ReadCapabilityError::Scope)?;
        let schema_hash = binding
            .schema_hash
            .as_deref()
            .filter(|value| value.starts_with("sha256:"))
            .ok_or(ReadCapabilityError::Scope)?;
        let principal_type = binding
            .principal_type
            .as_deref()
            .filter(|value| matches!(*value, "service_account" | "user_delegated"))
            .ok_or(ReadCapabilityError::Scope)?;
        let url = internal_endpoint(
            &self.config.gateway_url,
            "internal/v2/agent-capabilities/mcp/read",
        )?;
        let envelope = json!({
            "connection_id": connection_id,
            "principal_type": principal_type,
            "channel": binding.channel,
            "runtime_name": capability_id,
            "schema_hash": schema_hash,
            "risk_level": binding.risk_level.as_deref().unwrap_or("medium"),
            "arguments": arguments,
            "arguments_hash": canonical_json_hash(arguments)
                .map_err(|_| ReadCapabilityError::Arguments)?,
        });
        let headers = self.internal_headers(context, &url, &envelope)?;
        let result = self
            .http
            .post_internal_json(url, &headers, envelope)
            .await?;
        if serde_json::to_vec(&result)
            .map_err(|_| ReadCapabilityError::DownstreamRejected)?
            .len()
            > MAX_MCP_RESPONSE_BYTES
        {
            return Err(ReadCapabilityError::ResponseTooLarge);
        }
        Ok(result)
    }

    fn internal_headers(
        &self,
        context: &ReadCapabilityContext,
        url: &Url,
        body: &Value,
    ) -> Result<Vec<(String, String)>, ReadCapabilityError> {
        let proof = sign_capability_proof(
            &self.config.proof_secret,
            CapabilityProofInput {
                method: "POST",
                path: url.path(),
                body,
                context,
                now: current_epoch_seconds()?,
                nonce: Uuid::now_v7().simple().to_string(),
            },
        )?;
        let mut headers = vec![
            (
                "x-ai-platform-internal-token".to_string(),
                self.config.internal_token.clone(),
            ),
            ("x-ai-tenant-id".to_string(), context.tenant_id.clone()),
            ("x-ai-user-id".to_string(), context.user_id.clone()),
            ("x-ai-session-id".to_string(), context.session_id.clone()),
            ("x-ai-capability-proof".to_string(), proof),
            (
                "x-ai-tool-call-id".to_string(),
                context.tool_call_id.clone(),
            ),
        ];
        headers.extend(crate::trace_context::header_pairs(
            &context.run_id,
            &context.run_id,
            &context.execution_id,
        ));
        Ok(headers)
    }

    fn scoped_workspace_root(&self, context: &ReadCapabilityContext) -> PathBuf {
        let mut digest = Sha256::new();
        digest.update(context.tenant_id.as_bytes());
        digest.update(b"\0");
        digest.update(context.user_id.as_bytes());
        digest.update(b"\0");
        digest.update(context.session_id.as_bytes());
        self.config
            .workspace_root
            .join(format!("{:x}", digest.finalize()))
    }
}

fn current_epoch_seconds() -> Result<u64, ReadCapabilityError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| ReadCapabilityError::Configuration)
}

fn string_argument<'a>(
    value: &'a Value,
    name: &str,
    minimum: usize,
    maximum: usize,
) -> Result<&'a str, ReadCapabilityError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|item| {
            (minimum..=maximum).contains(&item.len())
                && !item.bytes().any(|byte| byte.is_ascii_control())
        })
        .ok_or(ReadCapabilityError::Arguments)
}

fn string_array_argument(
    value: &Value,
    name: &str,
    maximum_items: usize,
    maximum_length: usize,
) -> Result<Vec<String>, ReadCapabilityError> {
    let values = value
        .get(name)
        .and_then(Value::as_array)
        .ok_or(ReadCapabilityError::Arguments)?;
    if values.len() > maximum_items {
        return Err(ReadCapabilityError::Arguments);
    }
    let output: Vec<_> = values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.is_empty() && value.len() <= maximum_length)
                .map(str::to_string)
                .ok_or(ReadCapabilityError::Arguments)
        })
        .collect::<Result<_, _>>()?;
    if output.iter().collect::<BTreeSet<_>>().len() != output.len() {
        return Err(ReadCapabilityError::Arguments);
    }
    Ok(output)
}

fn valid_dataset_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn integer_argument(
    value: &Value,
    name: &str,
    default: i64,
    minimum: i64,
    maximum: i64,
) -> Result<i64, ReadCapabilityError> {
    let value = match value.get(name) {
        None => default,
        Some(value) => value.as_i64().ok_or(ReadCapabilityError::Arguments)?,
    };
    if !(minimum..=maximum).contains(&value) {
        return Err(ReadCapabilityError::Arguments);
    }
    Ok(value)
}

fn number_argument(
    value: &Value,
    name: &str,
    default: f64,
    minimum: f64,
    maximum: f64,
) -> Result<f64, ReadCapabilityError> {
    let value = match value.get(name) {
        None => default,
        Some(value) => value.as_f64().ok_or(ReadCapabilityError::Arguments)?,
    };
    if !value.is_finite() || value < minimum || value > maximum {
        return Err(ReadCapabilityError::Arguments);
    }
    Ok(value)
}
