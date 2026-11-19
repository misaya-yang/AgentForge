//! Canonical VLM OCR transports and Tesseract/VLM/hybrid dispatch.

pub const OCR_PROMPT: &str = r###"You are a highly accurate OCR engine. Extract ALL text from this scanned document image.
Rules:
1. Preserve the original language (Arabic and/or English) exactly as written.
2. Maintain right-to-left (RTL) reading order for Arabic text.
3. Preserve paragraph breaks and logical structure.
4. If there are tables, reproduce them in a readable text format.
5. If there are headers or titles, mark them clearly.
6. Do NOT translate, summarize, or interpret — only transcribe.
7. If text is partially illegible, transcribe what you can and mark unclear parts with [?].
Output the extracted text only, with no additional commentary."###;

pub const DEFAULT_PROMPT: &str = r###"请详细描述这张图片的内容，包括：
1. 图片的主要内容和主题
2. 图片中包含的文字（如果有）
3. 数据、表格或图表的具体信息（如果有）
4. 其他重要的视觉元素

请用清晰、结构化的方式描述，方便后续搜索和理解。"###;

pub const TABLE_PROMPT: &str = r###"这是一张包含表格或图表的图片。请详细提取其中的信息：
1. 表格/图表的标题或主题
2. 列名和行名
3. 具体的数据值
4. 单位和注释
5. 关键发现或趋势

请以结构化的方式输出，保留原始数据的完整性。"###;
use super::{
    IngestError, Result,
    parser::{DocumentParser, require_text_budget},
    structured::{BackendRegistry, PageClient},
};
use base64::Engine;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;
pub fn media_type(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\xff\xd8") {
        "image/jpeg"
    } else {
        "image/png"
    }
}
#[derive(Clone)]
pub struct OcrConfig {
    pub provider: String,
    pub model: String,
    pub endpoint: String,
    pub api_keys: Vec<String>,
    pub concurrency: usize,
    pub timeout: Duration,
    pub max_retries: usize,
    pub task: String,
    pub min_pixels: usize,
    pub max_pixels: usize,
    pub max_tokens: usize,
    pub enable_rotate: bool,
}
impl OcrConfig {
    pub fn dashscope(endpoint: String, api_keys: Vec<String>) -> Self {
        Self {
            provider: "dashscope".into(),
            model: "qwen-vl-ocr".into(),
            endpoint,
            api_keys,
            concurrency: 4,
            timeout: Duration::from_secs(30),
            max_retries: 2,
            task: "document_parsing".into(),
            min_pixels: 3072,
            max_pixels: 8_388_608,
            max_tokens: 8192,
            enable_rotate: true,
        }
    }
}
#[derive(Clone)]
pub struct VlmOcrService {
    pub config: OcrConfig,
    semaphore: Arc<Semaphore>,
}
impl VlmOcrService {
    pub fn new(mut config: OcrConfig) -> Result<Self> {
        if config.model.is_empty() {
            config.model = "qwen-vl-ocr".into()
        }
        if config.provider == "auto" {
            config.provider = if config.model.starts_with("gemini-") {
                "gemini"
            } else if config.model.to_lowercase().contains("deepseek") {
                "siliconflow"
            } else {
                "dashscope"
            }
            .into()
        }
        if !["gemini", "dashscope", "siliconflow"].contains(&config.provider.as_str()) {
            return Err(IngestError::Validation("unknown OCR provider".into()));
        }
        // Credentials and vendor endpoints belong only to the Gateway registry.
        config.api_keys.clear();
        config.endpoint.clear();
        if config.provider == "dashscope"
            && (![
                "text_recognition",
                "advanced_recognition",
                "key_information_extraction",
                "table_parsing",
                "document_parsing",
                "formula_recognition",
                "multi_lan",
            ]
            .contains(&config.task.as_str())
                || config.min_pixels == 0
                || config.max_pixels < config.min_pixels
                || !(1..=8192).contains(&config.max_tokens))
        {
            return Err(IngestError::Validation(
                "DashScope OCR task or pixel/token bounds are invalid".into(),
            ));
        }
        config.concurrency = config.concurrency.max(1);
        config.max_retries = config.max_retries.max(1);
        Ok(Self {
            semaphore: Arc::new(Semaphore::new(config.concurrency)),
            config,
        })
    }
    async fn post(&self, payload: &Value, gemini: bool) -> Result<(u16, Value)> {
        use crate::migration_knowledge_query::model_plane::{self, ModelPurpose, ModelWire};
        let wire = if gemini {
            ModelWire::GeminiGenerateContent
        } else if self.config.provider == "dashscope" {
            ModelWire::DashscopeVision
        } else {
            ModelWire::ChatCompletions
        };
        Ok(model_plane::execute(
            &self.config.provider,
            &self.config.model,
            "",
            ModelPurpose::Ocr,
            wire,
            payload.clone(),
            self.config.timeout,
        )
        .await?)
    }
    async fn call(&self, bytes: &[u8]) -> Result<String> {
        let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        let uri = format!("data:{};base64,{b64}", media_type(bytes));
        match self.config.provider.as_str() {
            "dashscope" => {
                if bytes.is_empty() {
                    return Ok(String::new());
                }
                let payload = json!({"model":self.config.model,"input":{"messages":[{"role":"user","content":[{"image":uri,"min_pixels":self.config.min_pixels,"max_pixels":self.config.max_pixels,"enable_rotate":self.config.enable_rotate}]}]},"parameters":{"max_tokens":self.config.max_tokens,"ocr_options":{"task":self.config.task}}});
                let (status, result) = self.post(&payload, false).await?;
                if status >= 400 {
                    return Err(IngestError::Index(format!(
                        "DashScope OCR API error: HTTP {status}"
                    )));
                }
                Ok(dashscope_text(&result))
            }
            "gemini" => {
                let payload = gemini_ocr_payload(bytes, &b64, self.config.max_tokens);
                let (status, result) = self.post(&payload, true).await?;
                if status >= 400 {
                    return Err(IngestError::Index(format!(
                        "Gemini OCR API error: HTTP {status}"
                    )));
                }
                Ok(result["candidates"][0]["content"]["parts"]
                    .as_array()
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(|v| v["text"].as_str())
                            .collect::<String>()
                    })
                    .unwrap_or_default()
                    .trim()
                    .into())
            }
            _ => {
                let payload = json!({"model":self.config.model,"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":uri}},{"type":"text","text":OCR_PROMPT}]}],"max_tokens":4096});
                for attempt in 0..self.config.max_retries {
                    match self.post(&payload, false).await {
                        Ok((429, _)) => {
                            tokio::time::sleep(Duration::from_secs(
                                if self.config.api_keys.len() > 1 {
                                    1
                                } else {
                                    2u64.saturating_pow((attempt + 1) as u32)
                                },
                            ))
                            .await
                        }
                        Ok((status, result)) if status < 400 => {
                            return Ok(result["choices"][0]["message"]["content"]
                                .as_str()
                                .unwrap_or("")
                                .trim()
                                .into());
                        }
                        Err(error) => return Err(error),
                        _ => {
                            if attempt + 1 < self.config.max_retries {
                                tokio::time::sleep(Duration::from_secs(
                                    2u64.saturating_pow(attempt as u32),
                                ))
                                .await
                            }
                        }
                    }
                }
                Ok(String::new())
            }
        }
    }
    pub async fn ocr_image(&self, bytes: &[u8]) -> String {
        let Ok(_permit) = self.semaphore.acquire().await else {
            return String::new();
        };
        for attempt in 0..self.config.max_retries {
            match self.call(bytes).await {
                Ok(text) => return text,
                Err(_) => {
                    if attempt + 1 < self.config.max_retries {
                        tokio::time::sleep(Duration::from_secs((attempt as u64 + 1) * 2)).await
                    }
                }
            }
        }
        String::new()
    }
    pub async fn ocr_pdf_pages(&self, images: &[Vec<u8>]) -> Vec<String> {
        let mut tasks = tokio::task::JoinSet::new();
        for (i, bytes) in images.iter().cloned().enumerate() {
            let service = self.clone();
            tasks.spawn(crate::migration_knowledge_query::model_plane::inherit(
                async move { (i, service.ocr_image(&bytes).await) },
            ));
        }
        let mut values = vec![String::new(); images.len()];
        while let Some(Ok((i, text))) = tasks.join_next().await {
            values[i] = text
        }
        values
    }
    pub fn register(&self, registry: &mut BackendRegistry) {
        let service = self.clone();
        let client: PageClient = Arc::new(move |job| {
            let service = service.clone();
            Box::pin(async move {
                let bytes = job
                    .image_bytes
                    .ok_or_else(|| IngestError::Parser("OCR needs image bytes".into()))?;
                let text = service.ocr_image(&bytes).await;
                require_text_budget(&text)?;
                Ok(json!(text))
            })
        });
        registry.register("legacy_ocr_vlm", "1", client);
    }
}
pub fn dashscope_text(value: &Value) -> String {
    let content = &value["output"]["choices"][0]["message"]["content"];
    if let Some(s) = content.as_str() {
        return s.trim().into();
    }
    content
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    if let Some(text) = item.get("text") {
                        Some(python_string(text))
                    } else if let Some(result) = item.get("ocr_result") {
                        Some(python_string(result))
                    } else {
                        item.as_str().map(str::to_owned)
                    }
                })
                .collect::<String>()
        })
        .unwrap_or_default()
        .trim()
        .into()
}
fn python_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        _ => v.to_string(),
    }
}
pub async fn ocr_image_auto(
    bytes: &[u8],
    vlm: Option<&VlmOcrService>,
    parser: &DocumentParser,
    strategy: &str,
) -> Result<String> {
    let strategy = strategy.trim().to_lowercase();
    if strategy != "tesseract" {
        if let Some(service) = vlm {
            let text = service.ocr_image(bytes).await;
            if !text.is_empty() || strategy == "vlm" {
                return Ok(text);
            }
        }
    }
    let bytes = bytes.to_vec();
    let parser = parser.clone();
    tokio::task::spawn_blocking(move || parser.ocr_image_bytes(&bytes, true))
        .await
        .map_err(|_| IngestError::Parser("OCR task failed".into()))?
}

fn gemini_ocr_payload(bytes: &[u8], b64: &str, max_tokens: usize) -> Value {
    json!({"contents":[{"role":"user","parts":[{"inlineData":{"mimeType":media_type(bytes),"data":b64}},{"text":OCR_PROMPT}]}],"generationConfig":{"maxOutputTokens":max_tokens}})
}
#[cfg(test)]
mod gemini_payload_tests {
    use super::*;
    #[test]
    fn actual_gemini_ocr_payload_carries_configured_native_output_cap() {
        for cap in [1, 257, 8192] {
            let payload = gemini_ocr_payload(b"image", "aW1hZ2U=", cap);
            assert_eq!(payload["generationConfig"]["maxOutputTokens"], json!(cap));
            assert_eq!(
                payload["contents"][0]["parts"][0]["inlineData"]["data"],
                "aW1hZ2U="
            );
            assert_eq!(payload["contents"][0]["parts"][1]["text"], OCR_PROMPT);
            assert!(payload.get("max_tokens").is_none());
        }
    }
}
