//! DashScope generic image descriptions, distinct from the OCR extraction API.
use super::{
    IngestError, Result,
    ocr::{DEFAULT_PROMPT, TABLE_PROMPT},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImageDescription {
    pub description: String,
    pub model: String,
    pub tokens_used: i64,
    pub metadata: Value,
}
#[derive(Clone)]
pub struct VlmService {
    client: reqwest::Client,
    api_key: Arc<String>,
    pub model: String,
    pub endpoint: String,
    pub max_retries: usize,
    pub timeout: Duration,
}
impl VlmService {
    pub fn new(
        api_key: String,
        model: String,
        base_url: Option<&str>,
        timeout: Duration,
        max_retries: usize,
    ) -> Result<Self> {
        if api_key.is_empty() {
            return Err(IngestError::Validation(
                "DashScope API key is required for VLM service".into(),
            ));
        }
        let mut endpoint = base_url
            .unwrap_or("https://dashscope.aliyuncs.com/api/v1")
            .trim_end_matches('/')
            .to_owned();
        if !endpoint.ends_with("/services/aigc/multimodal-generation/generation") {
            endpoint.push_str("/services/aigc/multimodal-generation/generation")
        }
        let url = reqwest::Url::parse(&endpoint)
            .map_err(|_| IngestError::Validation("invalid VLM endpoint".into()))?;
        if !["http", "https"].contains(&url.scheme())
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(IngestError::Validation("invalid VLM endpoint".into()));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(timeout)
            .build()
            .map_err(|_| IngestError::Index("VLM client construction failed".into()))?;
        Ok(Self {
            client,
            api_key: Arc::new(api_key),
            model,
            endpoint,
            max_retries,
            timeout,
        })
    }
    pub fn detect_media_type(bytes: &[u8]) -> &'static str {
        if bytes.len() < 8 {
            "image/png"
        } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            "image/png"
        } else if bytes.starts_with(b"\xff\xd8") {
            "image/jpeg"
        } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
            "image/gif"
        } else if bytes.starts_with(b"BM") {
            "image/bmp"
        } else if bytes.len() > 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
            "image/webp"
        } else {
            "image/png"
        }
    }
    pub async fn describe_image(
        &self,
        bytes: &[u8],
        prompt: Option<&str>,
        image_type: &str,
        context: Option<&str>,
        max_tokens: usize,
    ) -> Result<ImageDescription> {
        let chosen = prompt.filter(|s| !s.is_empty()).unwrap_or(
            if ["table", "chart"].contains(&image_type) {
                TABLE_PROMPT
            } else {
                DEFAULT_PROMPT
            },
        );
        let prompt = if let Some(context) = context.filter(|s| !s.is_empty()) {
            format!("图片来源上下文：{context}\n\n{chosen}")
        } else {
            chosen.into()
        };
        let mime = Self::detect_media_type(bytes);
        let uri = format!("data:{mime};base64,{}", STANDARD.encode(bytes));
        let payload = json!({"model":self.model,"input":{"messages":[{"role":"user","content":[{"image":uri},{"text":prompt}]}]},"parameters":{"max_tokens":max_tokens}});
        let mut last = "no attempts".to_owned();
        for attempt in 0..self.max_retries {
            // Canonical provider/response VLMError is terminal; only transport failures retry.
            let response = self
                .client
                .post(&self.endpoint)
                .bearer_auth(self.api_key.as_str())
                .json(&payload)
                .send()
                .await;
            let mut response = match response {
                Ok(r) => r,
                Err(_) => {
                    last = "transport error".into();
                    if attempt + 1 < self.max_retries {
                        tokio::time::sleep(Duration::from_secs(((attempt + 1) * 2) as u64)).await;
                    }
                    continue;
                }
            };
            let status = response.status().as_u16();
            let mut body = Vec::new();
            let mut transport_error = false;
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        if body.len() + chunk.len() > 8 * 1024 * 1024 {
                            return Err(IngestError::Index(
                                "VLM response exceeds byte limit".into(),
                            ));
                        }
                        body.extend_from_slice(&chunk)
                    }
                    Ok(None) => break,
                    Err(_) => {
                        transport_error = true;
                        break;
                    }
                }
            }
            if transport_error {
                last = "response transport error".into();
                if attempt + 1 < self.max_retries {
                    tokio::time::sleep(Duration::from_secs(((attempt + 1) * 2) as u64)).await
                }
                continue;
            }
            let value: Value = serde_json::from_slice(&body)
                .map_err(|_| IngestError::Index("VLM response is not JSON".into()))?;
            if status >= 400 || value["status_code"].as_u64().is_some_and(|n| n >= 400) {
                return Err(IngestError::Index(format!(
                    "VLM API error: {status} {} {}",
                    value["code"].as_str().unwrap_or(""),
                    value["message"].as_str().unwrap_or("")
                )));
            }
            let output = value
                .get("output")
                .filter(|v| !v.is_null())
                .ok_or_else(|| IngestError::Index("VLM response missing output".into()))?;
            let choices = output["choices"]
                .as_array()
                .filter(|v| !v.is_empty())
                .ok_or_else(|| IngestError::Index("VLM response has no choices".into()))?;
            let items = choices[0]["message"]["content"]
                .as_array()
                .ok_or_else(|| IngestError::Index("VLM response has no text content".into()))?;
            let text = items
                .iter()
                .filter_map(|v| v["text"].as_str().or(v.as_str()))
                .collect::<String>();
            if text.is_empty() {
                return Err(IngestError::Index(
                    "VLM response has no text content".into(),
                ));
            }
            return Ok(ImageDescription {
                description: text.trim().into(),
                model: self.model.clone(),
                tokens_used: value["usage"]["total_tokens"].as_i64().unwrap_or(0),
                metadata: json!({"image_type":image_type,"media_type":mime,"image_size":bytes.len()}),
            });
        }
        Err(IngestError::Index(format!(
            "VLM API call failed after {} attempts: {last}",
            self.max_retries
        )))
    }
    pub async fn describe_images_batch(
        &self,
        images: &[Vec<u8>],
        prompts: Option<&[String]>,
        image_types: Option<&[String]>,
        contexts: Option<&[String]>,
        max_concurrent: usize,
    ) -> Result<Vec<ImageDescription>> {
        if images.is_empty() {
            return Ok(vec![]);
        }
        for values in [prompts, image_types, contexts].into_iter().flatten() {
            if !values.is_empty() && values.len() < images.len() {
                return Err(IngestError::Validation(
                    "VLM batch option list is shorter than images".into(),
                ));
            }
        }
        if max_concurrent == 0 {
            return Err(IngestError::Validation(
                "VLM batch concurrency must be positive".into(),
            ));
        }
        let semaphore = Arc::new(tokio::sync::Semaphore::new(max_concurrent));
        let mut tasks = tokio::task::JoinSet::new();
        for (index, bytes) in images.iter().enumerate() {
            let service = self.clone();
            let bytes = bytes.clone();
            let semaphore = semaphore.clone();
            let prompt = prompts.and_then(|p| p.get(index)).cloned();
            let kind = image_types
                .and_then(|p| p.get(index))
                .cloned()
                .unwrap_or("general".into());
            let context = contexts.and_then(|p| p.get(index)).cloned();
            tasks.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| IngestError::Cancelled)?;
                let result = service
                    .describe_image(&bytes, prompt.as_deref(), &kind, context.as_deref(), 1500)
                    .await;
                Ok::<_, IngestError>((index, result))
            });
        }
        let mut ordered = std::collections::BTreeMap::new();
        while let Some(result) = tasks.join_next().await {
            let (index, result) =
                result.map_err(|_| IngestError::Index("VLM batch task failed".into()))??;
            let row = result.unwrap_or_else(|error| ImageDescription {
                description: format!("[图片描述生成失败: {error}]"),
                model: self.model.clone(),
                tokens_used: 0,
                metadata: json!({"error":error.to_string()}),
            });
            ordered.insert(index, row);
        }
        Ok(ordered.into_values().collect())
    }
    pub async fn describe_table_image(
        &self,
        bytes: &[u8],
        context: Option<&str>,
    ) -> Result<ImageDescription> {
        self.describe_image(bytes, None, "table", context, 2000)
            .await
    }
}
