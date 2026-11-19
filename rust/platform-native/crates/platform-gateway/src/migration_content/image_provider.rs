//! One-provider image execution from core.media.image_generation. No routing,
//! resubmission or retry can create a second billable generation after dispatch.
use super::{
    ContentError, ContentFuture, attachments::sniff_image, capabilities::ImageProvider,
    image_adapters::PinnedImageFetch, images::ImageReferenceFetch, string,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::Arc,
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct ImageGenerationConfig {
    pub provider: String,
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub timeout: f64,
    pub dashscope_submit_path: String,
    pub dashscope_protocol: String,
    pub google_backend: String,
    pub result_host_suffixes: Vec<String>,
    pub supports_reference_images: bool,
}
impl Default for ImageGenerationConfig {
    fn default() -> Self {
        Self {
            provider: "none".into(),
            api_key: String::new(),
            base_url: String::new(),
            model: String::new(),
            timeout: 120.0,
            dashscope_submit_path: "/services/aigc/image-generation/generation".into(),
            dashscope_protocol: "wan26".into(),
            google_backend: "ai_studio".into(),
            result_host_suffixes: vec![
                "aliyuncs.com".into(),
                "alibabacloud.com".into(),
                "volces.com".into(),
            ],
            supports_reference_images: false,
        }
    }
}
pub struct HttpImageProvider {
    pub config: ImageGenerationConfig,
    client: reqwest::Client,
    fetch: Arc<dyn ImageReferenceFetch>,
}
impl HttpImageProvider {
    pub fn new(config: ImageGenerationConfig) -> Result<Self, ContentError> {
        let timeout = if config.timeout.is_finite() && (1.0..=300.0).contains(&config.timeout) {
            config.timeout
        } else {
            120.0
        };
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs_f64(timeout))
            .read_timeout(Duration::from_secs_f64(timeout))
            .build()
            .map_err(|_| ContentError::new(503, "Image provider unavailable"))?;
        let fetch = PinnedImageFetch {
            allowed_hosts: Some(
                config
                    .result_host_suffixes
                    .iter()
                    .cloned()
                    .collect::<HashSet<_>>(),
            ),
            max_redirects: 1,
            ..Default::default()
        };
        Ok(Self {
            config,
            client,
            fetch: Arc::new(fetch),
        })
    }
    pub fn with_fetch(
        config: ImageGenerationConfig,
        client: reqwest::Client,
        fetch: Arc<dyn ImageReferenceFetch>,
    ) -> Self {
        Self {
            config,
            client,
            fetch,
        }
    }
    fn failure(&self, code: &str, error: &str, blocked: bool, unknown: bool) -> Value {
        json!({"success":false,"provider":self.config.provider,"images":[],"error":error,"error_code":code,"blocked":blocked,"outcome_unknown":unknown})
    }
    fn success(&self, images: Vec<Value>) -> Value {
        json!({"success":true,"provider":self.config.provider,"images":images,"error":null,"error_code":null,"blocked":false,"outcome_unknown":false})
    }
    fn http_failure(&self, status: u16) -> Value {
        self.failure(
            "provider_http_error",
            "image provider request failed",
            false,
            status >= 500,
        )
    }
    fn valid_config(&self) -> bool {
        let c = &self.config;
        let safe = reqwest::Url::parse(&c.base_url).is_ok_and(|u| {
            u.scheme() == "https"
                && u.host_str().is_some()
                && u.username().is_empty()
                && u.password().is_none()
                && u.query().is_none()
                && u.fragment().is_none()
        });
        let model = !c.model.is_empty()
            && c.model.len() <= 255
            && c.model.as_bytes()[0].is_ascii_alphanumeric()
            && c.model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b));
        let path = &c.dashscope_submit_path;
        safe && model
            && (1.0..=300.0).contains(&c.timeout)
            && ["ai_studio", "vertex"].contains(&c.google_backend.as_str())
            && path.starts_with("/services/")
            && !path.contains("..")
            && !path.contains(['?', '#'])
            && ["wan26", "legacy"].contains(&c.dashscope_protocol.as_str())
            && !c.result_host_suffixes.is_empty()
            && c.result_host_suffixes.iter().all(|s| {
                (2..=254).contains(&s.len())
                    && s.as_bytes()[0].is_ascii_alphanumeric()
                    && s.as_bytes()[s.len() - 1].is_ascii_alphanumeric()
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".-".contains(&b))
            })
    }
    async fn post(
        &self,
        path: &str,
        payload: Value,
        google: bool,
        dashscope: bool,
    ) -> Result<(u16, Value), ContentError> {
        let mut request = self
            .client
            .post(format!(
                "{}{}",
                self.config.base_url.trim_end_matches('/'),
                path
            ))
            .header("Content-Type", "application/json")
            .json(&payload);
        if google {
            request = request.header("x-goog-api-key", &self.config.api_key)
        } else {
            request = request.bearer_auth(&self.config.api_key)
        }
        if dashscope {
            request = request.header("X-DashScope-Async", "enable")
        }
        let response = request
            .send()
            .await
            .map_err(|_| ContentError::new(502, "Image provider unavailable"))?;
        let status = response.status().as_u16();
        if status != 200 {
            return Ok((status, Value::Null));
        }
        let body = response
            .json()
            .await
            .map_err(|_| ContentError::new(502, "Image provider response invalid"))?;
        Ok((status, body))
    }
    pub async fn generate_arguments(&self, args: Value) -> Result<Value, ContentError> {
        let prompt = string(&args, "prompt");
        let n = args.get("n").and_then(Value::as_i64).unwrap_or(1);
        let size = args
            .get("size")
            .and_then(Value::as_str)
            .unwrap_or("1536*1536");
        if prompt.trim().is_empty() {
            return Ok(self.failure("invalid_request", "prompt is required", false, false));
        }
        if !(1..=4).contains(&n) {
            return Ok(self.failure("invalid_request", "n is out of range", false, false));
        }
        if !["google", "dashscope", "doubao"].contains(&self.config.provider.as_str())
            || self.config.api_key.is_empty()
        {
            return Ok(self.failure(
                "provider_unconfigured",
                "image provider is not configured",
                false,
                false,
            ));
        }
        let reference = if let Some(encoded) =
            args.get("reference_image_base64").filter(|v| !v.is_null())
        {
            if self.config.provider != "google" || !self.config.supports_reference_images {
                return Ok(self.failure(
                    "reference_unsupported",
                    "configured image provider does not support reference images",
                    false,
                    false,
                ));
            }
            let mime = string(&args, "reference_mime");
            let bytes = encoded.as_str().and_then(|s| STANDARD.decode(s).ok());
            let Some(bytes) = bytes.filter(|b| b.len() <= 20 * 1024 * 1024 && valid_image(b, mime))
            else {
                return Ok(self.failure(
                    "invalid_reference",
                    "reference image is invalid",
                    false,
                    false,
                ));
            };
            Some((bytes, mime))
        } else {
            None
        };
        if !self.valid_config() {
            return Ok(self.failure(
                "provider_endpoint_invalid",
                "image provider endpoint is invalid",
                false,
                false,
            ));
        }
        let result = match self.config.provider.as_str() {
            "google" => self.google(prompt, n, size, reference).await,
            "dashscope" => {
                self.dashscope(
                    prompt,
                    n,
                    size,
                    args.get("style")
                        .and_then(Value::as_str)
                        .unwrap_or("<auto>"),
                    string(&args, "negative_prompt"),
                )
                .await
            }
            _ => self.doubao(prompt, n, size).await,
        };
        Ok(result.unwrap_or_else(|_| {
            self.failure(
                "provider_request_failed",
                "image provider request failed",
                false,
                true,
            )
        }))
    }
    async fn google(
        &self,
        prompt: &str,
        n: i64,
        size: &str,
        reference: Option<(Vec<u8>, &str)>,
    ) -> Result<Value, ContentError> {
        let path = if self.config.google_backend == "vertex" {
            format!(
                "/v1/publishers/google/models/{}:generateContent",
                self.config.model
            )
        } else {
            format!("/v1beta/models/{}:generateContent", self.config.model)
        };
        let mut parts = vec![];
        if let Some((bytes, mime)) = reference {
            parts.push(json!({"inlineData":{"mimeType":mime,"data":STANDARD.encode(bytes)}}))
        }
        parts.push(json!({"text":prompt}));
        let(status,payload)=self.post(&path,json!({"contents":[{"role":"user","parts":parts}],"generationConfig":{"responseModalities":["TEXT","IMAGE"],"candidateCount":n,"imageConfig":{"aspectRatio":aspect_ratio(size)}}}),true,false).await?;
        if status != 200 {
            return Ok(self.http_failure(status));
        }
        if !payload["promptFeedback"]["blockReason"].is_null()
            && payload["promptFeedback"]["blockReason"] != ""
        {
            return Ok(self.failure(
                "content_blocked",
                "image generation was blocked",
                true,
                false,
            ));
        }
        let mut images = vec![];
        for candidate in payload["candidates"].as_array().into_iter().flatten() {
            for part in candidate["content"]["parts"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let inline = &part["inlineData"];
                if inline.is_object() && !string(inline, "data").is_empty() {
                    images.push(json!({"data":inline["data"],"mime_type":inline.get("mimeType").filter(|v|!v.is_null()&&*v!="").or_else(||inline.get("mime_type").filter(|v|!v.is_null()&&*v!="")).cloned().unwrap_or(json!("image/png"))}));
                }
            }
        }
        self.inline(images)
    }
    async fn doubao(&self, prompt: &str, n: i64, size: &str) -> Result<Value, ContentError> {
        let size = match size {
            "720*1280" => "1440x2560",
            "1280*720" => "2560x1440",
            _ => "2048x2048",
        };
        let(status,payload)=self.post("/images/generations",json!({"model":self.config.model,"prompt":prompt,"n":n,"size":size,"response_format":"b64_json"}),false,false).await?;
        if status != 200 {
            return Ok(self.http_failure(status));
        }
        let items = payload["data"]
            .as_array()
            .ok_or_else(|| ContentError::new(502, "Invalid provider image response"))?;
        let urls = items
            .iter()
            .filter(|v| !string(v, "url").is_empty())
            .map(|v| v["url"].clone())
            .collect::<Vec<_>>();
        if !urls.is_empty() {
            self.urls(urls).await
        } else {
            self.inline(
                items
                    .iter()
                    .filter(|v| v.is_object())
                    .map(|v| v["b64_json"].clone())
                    .collect(),
            )
        }
    }
    pub fn dashscope_payload(
        &self,
        prompt: &str,
        n: i64,
        size: &str,
        style: &str,
        negative: &str,
    ) -> Value {
        if self.config.dashscope_protocol == "wan26" {
            json!({"model":self.config.model,"input":{"messages":[{"role":"user","content":[{"text":prompt}]}]},"parameters":{"negative_prompt":negative,"size":if size=="1536*1536"{"1280*1280"}else{size},"n":n,"prompt_extend":true,"watermark":false}})
        } else {
            let mut body = json!({"model":self.config.model,"input":{"prompt":prompt},"parameters":{"size":size,"style":style,"n":n}});
            if !negative.is_empty() {
                body["input"]["negative_prompt"] = json!(negative)
            }
            body
        }
    }
    async fn dashscope(
        &self,
        prompt: &str,
        n: i64,
        size: &str,
        style: &str,
        negative: &str,
    ) -> Result<Value, ContentError> {
        let (status, payload) = self
            .post(
                &self.config.dashscope_submit_path,
                self.dashscope_payload(prompt, n, size, style, negative),
                false,
                true,
            )
            .await?;
        if status != 200 {
            return Ok(self.http_failure(status));
        }
        let output = &payload["output"];
        let immediate = results(output, false);
        if !immediate.is_empty() {
            if let Some(result) = self.results(&immediate, false).await? {
                return Ok(result);
            }
        }
        let id = string(output, "task_id");
        if id.is_empty() {
            return Ok(self.failure(
                "invalid_provider_response",
                "image provider returned no task",
                false,
                false,
            ));
        }
        let deadline = Instant::now() + Duration::from_secs_f64(self.config.timeout);
        while Instant::now() < deadline {
            let response = self
                .client
                .get(format!(
                    "{}/tasks/{id}",
                    self.config.base_url.trim_end_matches('/')
                ))
                .bearer_auth(&self.config.api_key)
                .send()
                .await
                .map_err(|_| ContentError::new(502, "Image provider request failed"))?;
            if response.status().as_u16() != 200 {
                return Ok(self.http_failure(response.status().as_u16()));
            }
            let body: Value = response
                .json()
                .await
                .map_err(|_| ContentError::new(502, "Image provider response invalid"))?;
            let output = &body["output"];
            match string(output, "task_status") {
                "SUCCEEDED" => {
                    return Ok(self
                        .results(&results(output, true), true)
                        .await?
                        .expect("completed results always return a result"));
                }
                "FAILED" => {
                    return Ok(self.failure(
                        "provider_generation_failed",
                        "image provider generation failed",
                        false,
                        false,
                    ));
                }
                "PENDING" | "RUNNING" => (),
                _ => {
                    return Ok(self.failure(
                        "invalid_provider_response",
                        "image provider returned an invalid task status",
                        false,
                        false,
                    ));
                }
            }
            tokio::time::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(2)),
            )
            .await;
        }
        Ok(self.failure("provider_timeout", "image provider timed out", false, true))
    }
    async fn results(
        &self,
        items: &[Value],
        completed: bool,
    ) -> Result<Option<Value>, ContentError> {
        let values = items
            .iter()
            .filter(|v| v.is_object())
            .map(|v| {
                v.get("b64_json")
                    .filter(|v| !v.is_null() && *v != "")
                    .or_else(|| v.get("content_base64"))
                    .cloned()
                    .unwrap_or(Value::Null)
            })
            .collect::<Vec<_>>();
        if values.iter().any(|v| !v.is_null() && *v != "") {
            return self.inline(values).map(Some);
        }
        let urls = items
            .iter()
            .filter(|v| v.is_object() && !string(v, "url").is_empty())
            .map(|v| v["url"].clone())
            .collect::<Vec<_>>();
        if completed || !urls.is_empty() {
            self.urls(urls).await.map(Some)
        } else {
            Ok(None)
        }
    }
    fn inline(&self, values: Vec<Value>) -> Result<Value, ContentError> {
        let mut images = vec![];
        for (index, value) in values.into_iter().take(4).enumerate() {
            let (mime, data) = if value.is_object() {
                (
                    value
                        .get("mime_type")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .unwrap_or("image/png")
                        .to_lowercase(),
                    value.get("data").cloned().unwrap_or(Value::Null),
                )
            } else {
                ("image/png".into(), value)
            };
            let Some(data) = data
                .as_str()
                .filter(|s| !s.is_empty() && supported_mime(&mime))
            else {
                continue;
            };
            let raw = STANDARD
                .decode(data)
                .map_err(|_| ContentError::new(502, "Invalid provider image response"))?;
            if raw.is_empty() || raw.len() > 20 * 1024 * 1024 || !valid_image(&raw, &mime) {
                return Ok(self.failure(
                    "image_too_large",
                    "provider returned an invalid image",
                    false,
                    true,
                ));
            }
            let ext = if matches!(mime.as_str(), "image/jpeg" | "image/jpg") {
                "jpg"
            } else {
                &mime[6..]
            };
            images.push(json!({"filename":format!("generated_{}.{ext}",index+1),"content_base64":data,"mime_type":mime,"size_bytes":raw.len()}));
        }
        Ok(if images.is_empty() {
            self.failure(
                "no_image",
                "provider returned no inline images",
                false,
                true,
            )
        } else {
            self.success(images)
        })
    }
    async fn urls(&self, urls: Vec<Value>) -> Result<Value, ContentError> {
        let mut images = vec![];
        for (index, url) in urls.into_iter().take(4).enumerate() {
            let Some(url) = url.as_str().filter(|s| !s.is_empty()) else {
                continue;
            };
            let fetched = match self
                .fetch
                .fetch(
                    url,
                    20 * 1024 * 1024,
                    Duration::from_secs_f64(self.config.timeout.min(60.0)),
                )
                .await
            {
                Ok(v) => v,
                Err(_) => {
                    return Ok(self.failure(
                        "url_fetch_failed",
                        "provider image download failed",
                        false,
                        true,
                    ));
                }
            };
            let mime = fetched
                .content_type
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_lowercase();
            if !supported_mime(&mime) || !valid_image(&fetched.bytes, &mime) {
                return Ok(self.failure(
                    "invalid_provider_image",
                    "provider returned an invalid image",
                    false,
                    true,
                ));
            }
            let ext = if matches!(mime.as_str(), "image/jpeg" | "image/jpg") {
                "jpg"
            } else {
                &mime[6..]
            };
            images.push(json!({"filename":format!("generated_{}.{ext}",index+1),"content_base64":STANDARD.encode(&fetched.bytes),"mime_type":mime,"size_bytes":fetched.bytes.len()}));
        }
        Ok(if images.is_empty() {
            self.failure("no_image", "provider returned no images", false, true)
        } else {
            self.success(images)
        })
    }
}
impl ImageProvider for HttpImageProvider {
    fn generate<'a>(&'a self, args: Value) -> ContentFuture<'a, Value> {
        Box::pin(self.generate_arguments(args))
    }
}
fn supported_mime(s: &str) -> bool {
    [
        "image/png",
        "image/jpeg",
        "image/jpg",
        "image/webp",
        "image/gif",
    ]
    .contains(&s)
}
fn valid_image(bytes: &[u8], mime: &str) -> bool {
    sniff_image(bytes)
        == Some(if mime == "image/jpg" {
            "image/jpeg"
        } else {
            mime
        })
}
fn results(output: &Value, completed: bool) -> Vec<Value> {
    for key in if completed {
        ["results", "images"]
    } else {
        ["images", "results"]
    } {
        if let Some(v) = output[key].as_array().filter(|a| !a.is_empty()) {
            return v.clone();
        }
    }
    let mut images = vec![];
    for choice in output["choices"].as_array().into_iter().flatten() {
        for part in choice["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(url) = part["image"].as_str().filter(|s| !s.is_empty()) {
                images.push(json!({"url":url}));
                if images.len() == 4 {
                    return images;
                }
            }
        }
    }
    images
}
fn aspect_ratio(size: &str) -> &'static str {
    let Some((w, h)) = size.split_once('*') else {
        return "1:1";
    };
    let (Ok(w), Ok(h)) = (w.parse::<f64>(), h.parse::<f64>()) else {
        return "1:1";
    };
    if h == 0.0 {
        return "1:1";
    }
    let ratio = w / h;
    let mut best = ("1:1", (1.0 - ratio).abs());
    for (label, value) in [
        ("16:9", 16.0 / 9.0),
        ("9:16", 9.0 / 16.0),
        ("4:3", 4.0 / 3.0),
        ("3:4", 3.0 / 4.0),
    ] {
        let delta = (value - ratio).abs();
        if delta < best.1 {
            best = (label, delta)
        }
    }
    best.0
}

impl ImageGenerationConfig {
    /// Pure source startup resolution. The host passes selected configuration;
    /// this function never reads process variables or secret files itself.
    pub fn from_values(values: &std::collections::BTreeMap<String, String>) -> Self {
        let get = |name: &str| values.get(name).map(String::as_str).unwrap_or("");
        let fallback =
            |name: &str, default: &str| values.get(name).cloned().unwrap_or_else(|| default.into());
        use ai_platform_gateway_foundation::provider_startup_seeder::{
            ChatEndpointResolver, EndpointDomain,
        };
        let endpoints = ChatEndpointResolver::new(values);
        let google = endpoints.resolve_google(EndpointDomain::Image);
        let dash = endpoints.resolve_dashscope(EndpointDomain::Image);
        let backend = google.backend;
        let google_key = google.api_key;
        let google_base = google.base_url;
        let dashscope_key = dash.api_key;
        let dashscope_base = dash.base_url;
        let ark_key = get("ARK_API_KEY").trim().to_owned();
        let mut provider = get("IMAGE_GENERATION_PROVIDER").trim().to_lowercase();
        if provider.is_empty() {
            provider = if !google_key.is_empty() {
                "google"
            } else if !dashscope_key.is_empty() {
                "dashscope"
            } else if !ark_key.is_empty() {
                "doubao"
            } else {
                "none"
            }
            .into()
        }
        let mut out = Self::default();
        let suffixes = get("IMAGE_GENERATION_RESULT_HOST_SUFFIXES")
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if !suffixes.is_empty() {
            out.result_host_suffixes = suffixes
        }
        match provider.as_str() {
            "google" | "gemini" | "ai_studio" | "vertex" => {
                out.provider = "google".into();
                out.api_key = google_key;
                out.base_url = google_base.into();
                out.google_backend = backend;
                out.model = fallback("GOOGLE_IMAGE_MODEL", "gemini-3.1-flash-image-preview");
                out.supports_reference_images = ["1", "true", "yes"].contains(
                    &fallback("IMAGE_GENERATION_SUPPORTS_REFERENCE_IMAGES", "true")
                        .to_lowercase()
                        .as_str(),
                );
            }
            "dashscope" | "qwen" => {
                out.provider = "dashscope".into();
                out.api_key = dashscope_key;
                out.base_url = dashscope_base;
                out.model = fallback("DASHSCOPE_IMAGE_MODEL", "wan2.6-t2i");
                out.dashscope_submit_path = fallback(
                    "DASHSCOPE_IMAGE_SUBMIT_PATH",
                    "/services/aigc/image-generation/generation",
                );
                out.dashscope_protocol = fallback("DASHSCOPE_IMAGE_PROTOCOL", "wan26")
                    .trim()
                    .to_lowercase();
            }
            "doubao" | "volcengine" | "ark" => {
                out.provider = "doubao".into();
                out.api_key = ark_key;
                out.base_url = fallback("ARK_BASE_URL", "https://ark.cn-beijing.volces.com/api/v3")
                    .trim()
                    .into();
                out.model = fallback("DOUBAO_IMAGE_MODEL", "doubao-seedream-5-0-260128");
            }
            _ => {
                out.provider = provider;
                out.result_host_suffixes = Self::default().result_host_suffixes;
            }
        }
        out
    }
}
