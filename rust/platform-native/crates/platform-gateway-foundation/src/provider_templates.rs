//! Source-faithful guided-onboarding catalog from
//! `src/services/llm/provider_templates.py`.
//!
//! This is a source snapshot, not a model recommendation or a live provider list.
//! Display order, defaults, prices, metadata and matching precedence are retained.
//! Runtime services still select persisted provider/model rows. No provider,
//! credential, storage or network access is performed here.
//!
//! Model kwargs retain Decimal values; only response projection converts prices
//! to binary64, as Python `float(Decimal)` does. Decimal uses the existing native
//! 96-bit coefficient / 28-place envelope; every canonical catalog price is zero.
//! Capability metadata is resolved by the existing bundled capability module.

use crate::capabilities;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{Map, Value, json};
use std::{collections::BTreeSet, sync::LazyLock};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CredentialField {
    pub name: String,
    pub label: String,
    pub field_type: String,
    pub required: bool,
    pub placeholder: Option<String>,
}
impl CredentialField {
    pub fn new(name: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            label: label.into(),
            field_type: "password".into(),
            required: true,
            placeholder: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogModel {
    pub model_id: String,
    pub display_name: String,
    pub context_window: i64,
    pub max_output_tokens: i64,
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub input_price_per_1k: Decimal,
    pub output_price_per_1k: Decimal,
    pub access_level: String,
    pub sort_order: i64,
}

/// Typed equivalent of Python's kwargs dictionary: prices remain exact Decimal.
/// `None` means the capabilities key is absent, not a JSON null or empty object.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogModelKwargs {
    pub model_id: String,
    pub display_name: String,
    pub context_window: i64,
    pub max_output_tokens: i64,
    pub supports_vision: bool,
    pub supports_tools: bool,
    pub input_price_per_1k: Decimal,
    pub output_price_per_1k: Decimal,
    pub access_level: String,
    pub sort_order: i64,
    pub catalog_capabilities: Option<Value>,
}

impl CatalogModel {
    pub fn new(model_id: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            model_id: model_id.into(),
            display_name: display_name.into(),
            context_window: 128000,
            max_output_tokens: 4096,
            supports_vision: false,
            supports_tools: true,
            input_price_per_1k: Decimal::ZERO,
            output_price_per_1k: Decimal::ZERO,
            access_level: "public".into(),
            sort_order: 0,
        }
    }

    pub fn to_model_kwargs(&self, provider_id: Option<&str>) -> CatalogModelKwargs {
        CatalogModelKwargs {
            model_id: self.model_id.clone(),
            display_name: self.display_name.clone(),
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            supports_vision: self.supports_vision,
            supports_tools: self.supports_tools,
            input_price_per_1k: self.input_price_per_1k,
            output_price_per_1k: self.output_price_per_1k,
            access_level: self.access_level.clone(),
            sort_order: self.sort_order,
            // Python checks truthiness before normalization: " " adds the key.
            catalog_capabilities: provider_id.filter(|id| !id.is_empty()).map(|id| {
                capabilities::builtin(&normalize(id), &normalize(&self.model_id))
                    .unwrap_or_else(|| json!({}))
            }),
        }
    }

    pub fn to_response(&self, provider_id: Option<&str>) -> Value {
        let kwargs = self.to_model_kwargs(provider_id);
        let mut response = Map::new();
        response.insert("model_id".into(), json!(kwargs.model_id));
        response.insert("display_name".into(), json!(kwargs.display_name));
        response.insert("context_window".into(), json!(kwargs.context_window));
        response.insert("max_output_tokens".into(), json!(kwargs.max_output_tokens));
        response.insert("supports_vision".into(), json!(kwargs.supports_vision));
        response.insert("supports_tools".into(), json!(kwargs.supports_tools));
        // Parsing the exact decimal text uses correctly rounded binary64 rather
        // than coefficient/divisor arithmetic that may introduce double rounding.
        response.insert(
            "input_price_per_1k".into(),
            json!(decimal_float(kwargs.input_price_per_1k)),
        );
        response.insert(
            "output_price_per_1k".into(),
            json!(decimal_float(kwargs.output_price_per_1k)),
        );
        response.insert("access_level".into(), json!(kwargs.access_level));
        response.insert("sort_order".into(), json!(kwargs.sort_order));
        if let Some(profile) = kwargs.catalog_capabilities {
            response.insert("catalog_capabilities".into(), profile);
        }
        Value::Object(response)
    }
}

fn decimal_float(value: Decimal) -> f64 {
    value
        .to_string()
        .parse()
        .expect("finite native decimal is representable as f64")
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderTemplate {
    pub template_id: String,
    pub display_name: String,
    pub default_provider_id: String,
    pub api_type: String,
    pub default_base_url: String,
    pub credential_fields: Vec<CredentialField>,
    pub discovery_strategy: String,
    pub default_models: Vec<CatalogModel>,
    pub default_metadata: Vec<(String, Value)>,
    pub description: String,
    pub advanced: bool,
}
impl ProviderTemplate {
    pub fn to_response(&self) -> Value {
        // Collecting in source order mirrors dict(tuple): duplicate keys use
        // the last value, while arrays retain their original display order.
        let metadata: Map<String, Value> = self.default_metadata.iter().cloned().collect();
        let mut response = Map::new();
        response.insert("template_id".into(), json!(self.template_id));
        response.insert("display_name".into(), json!(self.display_name));
        response.insert("description".into(), json!(self.description));
        response.insert(
            "default_provider_id".into(),
            json!(self.default_provider_id),
        );
        response.insert("api_type".into(), json!(self.api_type));
        response.insert("default_base_url".into(), json!(self.default_base_url));
        response.insert("credential_fields".into(), json!(self.credential_fields));
        response.insert("discovery_strategy".into(), json!(self.discovery_strategy));
        response.insert(
            "default_models".into(),
            json!(
                self.default_models
                    .iter()
                    .map(|model| model.to_response(Some(&self.default_provider_id)))
                    .collect::<Vec<_>>()
            ),
        );
        response.insert("default_metadata".into(), Value::Object(metadata));
        response.insert("advanced".into(), json!(self.advanced));
        Value::Object(response)
    }
}

// Python str.strip also accepts the four ASCII information separators, unlike
// Rust str::trim. Keeping them matters even for otherwise ASCII catalog keys.
fn normalize(value: &str) -> String {
    value
        .trim_matches(|c: char| c.is_whitespace() || ('\u{001c}'..='\u{001f}').contains(&c))
        .to_lowercase()
}

/// Canonical Python `DASHSCOPE_MODELS` declaration, in source order.
pub static DASHSCOPE_MODELS: LazyLock<Vec<CatalogModel>> = LazyLock::new(|| {
    vec![
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 131072,
            supports_tools: true,
            sort_order: 140,
            ..CatalogModel::new("qwen3.8-flash", "Qwen 3.8 Flash")
        },
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 65536,
            supports_tools: true,
            sort_order: 130,
            ..CatalogModel::new("qwen3.7-plus", "Qwen 3.7 Plus")
        },
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 8192,
            supports_tools: true,
            sort_order: 120,
            ..CatalogModel::new("qwen3.7-max", "Qwen 3.7 Max")
        },
        CatalogModel {
            context_window: 128000,
            max_output_tokens: 8192,
            supports_tools: true,
            sort_order: 100,
            ..CatalogModel::new("qwen3.6-plus", "Qwen 3.6 Plus")
        },
        CatalogModel {
            context_window: 128000,
            max_output_tokens: 8192,
            supports_tools: true,
            sort_order: 90,
            ..CatalogModel::new("qwen-plus", "Qwen Plus")
        },
        CatalogModel {
            context_window: 32768,
            max_output_tokens: 8192,
            supports_tools: true,
            sort_order: 80,
            ..CatalogModel::new("qwen-max", "Qwen Max")
        },
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 8192,
            supports_tools: true,
            sort_order: 70,
            ..CatalogModel::new("qwen-turbo", "Qwen Turbo")
        },
    ]
});

/// Canonical Python `GOOGLE_GEMINI_MODELS` declaration, in source order.
pub static GOOGLE_GEMINI_MODELS: LazyLock<Vec<CatalogModel>> = LazyLock::new(|| {
    vec![
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 8192,
            supports_vision: true,
            supports_tools: true,
            sort_order: 120,
            ..CatalogModel::new("gemini-3.5-flash", "Gemini3.5-flash")
        },
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 8192,
            supports_vision: true,
            supports_tools: true,
            sort_order: 110,
            ..CatalogModel::new("gemini-3.1-pro-preview", "Gemini 3.1 Pro")
        },
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 8192,
            supports_vision: true,
            supports_tools: true,
            sort_order: 100,
            ..CatalogModel::new("gemini-3-flash-preview", "Gemini 3 Flash")
        },
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 8192,
            supports_vision: true,
            supports_tools: true,
            sort_order: 90,
            ..CatalogModel::new("gemini-2.5-flash", "Gemini 2.5 Flash")
        },
        CatalogModel {
            context_window: 2000000,
            max_output_tokens: 8192,
            supports_vision: true,
            supports_tools: true,
            sort_order: 80,
            ..CatalogModel::new("gemini-2.5-pro", "Gemini 2.5 Pro")
        },
    ]
});

/// Canonical Python `VERTEX_GEMINI_MODELS` declaration, in source order.
pub static VERTEX_GEMINI_MODELS: LazyLock<Vec<CatalogModel>> = LazyLock::new(|| {
    vec![
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 8192,
            supports_vision: true,
            supports_tools: true,
            sort_order: 120,
            ..CatalogModel::new("gemini-3.5-flash", "Gemini3.5-flash")
        },
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 8192,
            supports_vision: true,
            supports_tools: true,
            sort_order: 110,
            ..CatalogModel::new("gemini-3.1-pro-preview", "Gemini 3.1 Pro")
        },
        CatalogModel {
            context_window: 1000000,
            max_output_tokens: 8192,
            supports_vision: true,
            supports_tools: true,
            sort_order: 100,
            ..CatalogModel::new("gemini-3-flash-preview", "Gemini 3 Flash")
        },
    ]
});

/// Canonical Python `PROVIDER_TEMPLATES` declaration, in source order.
pub static PROVIDER_TEMPLATES: LazyLock<Vec<ProviderTemplate>> = LazyLock::new(|| {
    vec![
        ProviderTemplate {
            template_id: "dashscope-cn".into(),
            display_name: "Qwen/DashScope China".into(),
            description: "Alibaba Cloud Model Studio China endpoint.".into(),
            default_provider_id: "dashscope-cn".into(),
            api_type: "openai".into(),
            default_base_url: "https://dashscope.aliyuncs.com/compatible-mode".into(),
            credential_fields: vec![CredentialField {
                placeholder: Some("sk-...".into()),
                ..CredentialField::new("api_key", "API Key")
            }],
            discovery_strategy: "catalog".into(),
            default_models: DASHSCOPE_MODELS.clone(),
            default_metadata: vec![("wire_protocol".into(), json!("responses_v1"))],
            advanced: false,
        },
        ProviderTemplate {
            template_id: "dashscope-intl".into(),
            display_name: "Qwen/DashScope Intl".into(),
            description: "Alibaba Cloud Model Studio international endpoint.".into(),
            default_provider_id: "dashscope-intl".into(),
            api_type: "openai".into(),
            default_base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode".into(),
            credential_fields: vec![CredentialField {
                placeholder: Some("sk-...".into()),
                ..CredentialField::new("api_key", "API Key")
            }],
            discovery_strategy: "catalog".into(),
            default_models: DASHSCOPE_MODELS.clone(),
            default_metadata: vec![("wire_protocol".into(), json!("responses_v1"))],
            advanced: false,
        },
        ProviderTemplate {
            template_id: "google-ai-studio".into(),
            display_name: "Google Gemini".into(),
            description: "Google AI Studio Gemini API.".into(),
            default_provider_id: "google".into(),
            api_type: "google".into(),
            default_base_url: "https://generativelanguage.googleapis.com".into(),
            credential_fields: vec![CredentialField {
                placeholder: Some("AIzaSy...".into()),
                ..CredentialField::new("api_key", "API Key")
            }],
            discovery_strategy: "google_ai_studio_models_list".into(),
            default_models: GOOGLE_GEMINI_MODELS.clone(),
            default_metadata: vec![],
            advanced: false,
        },
        ProviderTemplate {
            template_id: "google-vertex".into(),
            display_name: "Google Vertex AI".into(),
            description: "Google Vertex AI Gemini via official Google auth.".into(),
            default_provider_id: "google-vertex".into(),
            api_type: "google-vertex".into(),
            default_base_url: "https://aiplatform.googleapis.com".into(),
            credential_fields: vec![
                CredentialField {
                    field_type: "text".into(),
                    placeholder: Some("my-gcp-project".into()),
                    ..CredentialField::new("project", "Project ID")
                },
                CredentialField {
                    field_type: "text".into(),
                    placeholder: Some("us-central1".into()),
                    ..CredentialField::new("location", "Location")
                },
                CredentialField {
                    required: false,
                    placeholder: Some("Leave blank to use server ADC".into()),
                    ..CredentialField::new("api_key", "Service account JSON")
                },
            ],
            discovery_strategy: "vertex_best_effort".into(),
            default_models: VERTEX_GEMINI_MODELS.clone(),
            default_metadata: vec![],
            advanced: false,
        },
        ProviderTemplate {
            template_id: "custom-openai-compatible".into(),
            display_name: "Custom OpenAI-Compatible".into(),
            description: "Advanced custom provider path.".into(),
            default_provider_id: "".into(),
            api_type: "openai".into(),
            default_base_url: "".into(),
            credential_fields: vec![
                CredentialField {
                    field_type: "text".into(),
                    ..CredentialField::new("provider_id", "Provider ID")
                },
                CredentialField {
                    field_type: "text".into(),
                    ..CredentialField::new("display_name", "Display Name")
                },
                CredentialField {
                    field_type: "text".into(),
                    ..CredentialField::new("base_url", "Base URL")
                },
                CredentialField {
                    placeholder: Some("sk-...".into()),
                    ..CredentialField::new("api_key", "API Key")
                },
            ],
            discovery_strategy: "openai_compatible_best_effort".into(),
            default_models: vec![],
            advanced: true,
            default_metadata: vec![],
        },
    ]
});

/// Return a new list in UI display order; the catalog records remain immutable.
pub fn list_provider_templates() -> Vec<&'static ProviderTemplate> {
    PROVIDER_TEMPLATES.iter().collect()
}

pub fn get_provider_template(template_id: &str) -> Option<&'static ProviderTemplate> {
    let normalized = normalize(template_id);
    PROVIDER_TEMPLATES
        .iter()
        .find(|template| template.template_id == normalized)
}

/// Preserve source precedence exactly, including empty-ID custom exact matches
/// and substring URL matching. This is inference, not URL/host validation.
pub fn find_provider_template_for_config(
    provider_id: &str,
    api_type: Option<&str>,
    base_url: Option<&str>,
) -> Option<&'static ProviderTemplate> {
    let provider = normalize(provider_id);
    let api_type = normalize(api_type.unwrap_or_default());
    let base_url = normalize(base_url.unwrap_or_default());
    if let Some(exact) = PROVIDER_TEMPLATES
        .iter()
        .find(|template| template.default_provider_id == provider)
    {
        return Some(exact);
    }
    if base_url.contains("dashscope-intl.aliyuncs.com") {
        return get_provider_template("dashscope-intl");
    }
    if base_url.contains("dashscope.aliyuncs.com") || provider.starts_with("dashscope") {
        return get_provider_template("dashscope-cn");
    }
    if matches!(api_type.as_str(), "google" | "google-ai-studio") {
        return get_provider_template("google-ai-studio");
    }
    if matches!(api_type.as_str(), "google-vertex" | "vertex") {
        return get_provider_template("google-vertex");
    }
    None
}

pub fn known_catalog_model_provider_ids(model_id: &str) -> BTreeSet<String> {
    let model_id = normalize(model_id);
    PROVIDER_TEMPLATES
        .iter()
        .filter(|template| {
            template
                .default_models
                .iter()
                .any(|model| model.model_id.to_lowercase() == model_id)
        })
        .map(|template| template.default_provider_id.clone())
        .collect()
}
