//! Inert endpoint resolution over explicit environment values only.
use std::fmt;
fn python_whitespace(character: char) -> bool { character.is_whitespace() || matches!(character, '\u{1c}'..='\u{1f}') }
pub trait ProviderEnvironment: Send + Sync {fn get(&self, name: &str) -> Option<String>;}
pub struct DashscopeChatEndpoint {
    pub api_key: String,
    pub base_url: String,
}
pub struct GoogleChatEndpoint {
    pub api_key: String,
    pub base_url: String,
    pub backend: String,
}
impl fmt::Debug for DashscopeChatEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DashscopeChatEndpoint(<internal>)")
    }
}
impl fmt::Debug for GoogleChatEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("GoogleChatEndpoint(<internal>)")
    }
}
pub trait StartupEndpointResolver: Send + Sync {
    fn resolve_dashscope_chat(&self) -> DashscopeChatEndpoint;
    fn resolve_google_chat(&self) -> GoogleChatEndpoint;
}

/// Inert resolver over an explicitly supplied environment; never `std::env`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointDomain {
    Chat,
    Image,
    Embedding,
    Ocr,
}
impl EndpointDomain {
    fn environment_name(self) -> &'static str {
        match self {
            Self::Chat => "CHAT",
            Self::Image => "IMAGE",
            Self::Embedding => "EMBEDDING",
            Self::Ocr => "OCR",
        }
    }
}
pub struct ChatEndpointResolver<'a> {
    environment: &'a dyn ProviderEnvironment,
}
impl ProviderEnvironment for std::collections::BTreeMap<String, String> {
    fn get(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
}
impl<'a> ChatEndpointResolver<'a> {
    pub fn new(environment: &'a dyn ProviderEnvironment) -> Self {
        Self { environment }
    }
    fn first(&self, names: &[&str]) -> String {
        // Python evaluates all os.environ.get arguments before calling _first.
        let values: Vec<_> = names
            .iter()
            .map(|name| self.environment.get(name).unwrap_or_default())
            .collect();
        values
            .iter()
            .map(|value| value.trim_matches(python_whitespace))
            .find(|value| !value.is_empty())
            .unwrap_or("")
            .to_owned()
    }
}
impl ChatEndpointResolver<'_> {
    pub fn resolve_dashscope(&self, domain: EndpointDomain) -> DashscopeChatEndpoint {
        let up = domain.environment_name();
        let api_key = self.first(&[&format!("DASHSCOPE_{up}_API_KEY"), "DASHSCOPE_API_KEY"]);
        let raw_url = self.first(&[&format!("DASHSCOPE_{up}_BASE_URL"), "DASHSCOPE_BASE_URL"]);
        let suffix = if domain == EndpointDomain::Chat {
            "/compatible-mode"
        } else {
            "/api/v1"
        };
        let base_url = if raw_url.is_empty() {
            format!("https://dashscope.aliyuncs.com{suffix}")
        } else {
            let mut cleaned = raw_url.trim_end_matches('/');
            for suffix in [
                "/compatible-mode/v1/chat/completions",
                "/compatible-mode/v1",
                "/compatible-mode",
                "/api/v1",
            ] {
                if let Some(base) = cleaned.strip_suffix(suffix) {
                    cleaned = base;
                    break;
                }
            }
            format!("{cleaned}{suffix}")
        };
        DashscopeChatEndpoint { api_key, base_url }
    }
    pub fn resolve_google(&self, domain: EndpointDomain) -> GoogleChatEndpoint {
        let up = domain.environment_name();
        let mut backend = self
            .first(&[&format!("GOOGLE_{up}_BACKEND"), "GOOGLE_API_BACKEND"])
            .to_lowercase();
        if backend != "vertex" && backend != "ai_studio" {
            backend = "ai_studio".into();
        }
        let studio_key = self.first(&["GEMINI_API_KEY", "GOOGLE_API_KEY"]);
        let (api_key, base_url) = if backend == "vertex" {
            let vertex_key = self.first(&[&format!("VERTEX_{up}_API_KEY"), "VERTEX_API_KEY"]);
            (
                if vertex_key.is_empty() {
                    studio_key
                } else {
                    vertex_key
                },
                "https://aiplatform.googleapis.com",
            )
        } else {
            (studio_key, "https://generativelanguage.googleapis.com")
        };
        GoogleChatEndpoint {
            api_key,
            base_url: base_url.into(),
            backend,
        }
    }
}

impl StartupEndpointResolver for ChatEndpointResolver<'_> {
    fn resolve_dashscope_chat(&self) -> DashscopeChatEndpoint {
        self.resolve_dashscope(EndpointDomain::Chat)
    }
    fn resolve_google_chat(&self) -> GoogleChatEndpoint {
        self.resolve_google(EndpointDomain::Chat)
    }
}
