//! Chat-domain assertions from tests/config/test_endpoints_resolver.py.
//! Non-chat resolution is deliberately not claimed by this target.
use ai_platform_gateway_foundation::{
    provider_service::ProviderEnvironment,
    provider_startup_seeder::{ChatEndpointResolver, StartupEndpointResolver},
};
use std::collections::BTreeMap;
struct Env(BTreeMap<String, String>);
impl ProviderEnvironment for Env {
    fn get(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
}
fn env(values: &[(&str, &str)]) -> Env {
    Env(values
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect())
}
#[test]
fn dashscope_chat_defaults_precedence_and_endpoint_shapes() {
    let empty = env(&[]);
    let got = ChatEndpointResolver::new(&empty).resolve_dashscope_chat();
    assert_eq!(got.api_key, "");
    assert_eq!(
        got.base_url,
        "https://dashscope.aliyuncs.com/compatible-mode"
    );
    for suffix in [
        "",
        "/api/v1",
        "/compatible-mode",
        "/compatible-mode/v1",
        "/compatible-mode/v1/chat/completions",
    ] {
        let url = format!("https://dashscope-intl.aliyuncs.com{suffix}");
        let values = env(&[
            ("DASHSCOPE_API_KEY", "general"),
            ("DASHSCOPE_BASE_URL", &url),
        ]);
        let got = ChatEndpointResolver::new(&values).resolve_dashscope_chat();
        assert_eq!(got.api_key, "general");
        assert_eq!(
            got.base_url,
            "https://dashscope-intl.aliyuncs.com/compatible-mode"
        );
    }
    let values = env(&[
        ("DASHSCOPE_API_KEY", "general"),
        ("DASHSCOPE_CHAT_API_KEY", "chat"),
        ("DASHSCOPE_CHAT_BASE_URL", "https://chat.invalid"),
        ("DASHSCOPE_BASE_URL", "https://general.invalid"),
        ("DASHSCOPE_IMAGE_API_KEY", "image"),
    ]);
    let got = ChatEndpointResolver::new(&values).resolve_dashscope_chat();
    assert_eq!(got.api_key, "chat");
    assert_eq!(got.base_url, "https://chat.invalid/compatible-mode");
}
#[test]
fn google_chat_defaults_key_precedence_backend_and_shared_fallback() {
    for (pairs, key, backend, url) in [
        (
            vec![],
            "",
            "ai_studio",
            "https://generativelanguage.googleapis.com",
        ),
        (
            vec![("GEMINI_API_KEY", "new"), ("GOOGLE_API_KEY", "legacy")],
            "new",
            "ai_studio",
            "https://generativelanguage.googleapis.com",
        ),
        (
            vec![("GOOGLE_API_KEY", "legacy")],
            "legacy",
            "ai_studio",
            "https://generativelanguage.googleapis.com",
        ),
        (
            vec![
                ("GOOGLE_API_BACKEND", "vertex"),
                ("VERTEX_API_KEY", "global"),
            ],
            "global",
            "vertex",
            "https://aiplatform.googleapis.com",
        ),
        (
            vec![
                ("GOOGLE_CHAT_BACKEND", "vertex"),
                ("VERTEX_API_KEY", "global"),
                ("VERTEX_CHAT_API_KEY", "chat"),
            ],
            "chat",
            "vertex",
            "https://aiplatform.googleapis.com",
        ),
        (
            vec![
                ("GOOGLE_CHAT_BACKEND", "vertex"),
                ("GEMINI_API_KEY", "shared"),
            ],
            "shared",
            "vertex",
            "https://aiplatform.googleapis.com",
        ),
        (
            vec![("GOOGLE_CHAT_BACKEND", "unknown")],
            "",
            "ai_studio",
            "https://generativelanguage.googleapis.com",
        ),
    ] {
        let values = env(&pairs);
        let got = ChatEndpointResolver::new(&values).resolve_google_chat();
        assert_eq!(
            (
                got.api_key.as_str(),
                got.backend.as_str(),
                got.base_url.as_str()
            ),
            (key, backend, url)
        );
    }
}

#[test]
fn dashscope_all_domains_are_independent_and_normalize_every_supported_url_shape() {
    use ai_platform_gateway_foundation::provider_startup_seeder::EndpointDomain::*;
    let domains = [
        (Chat, "CHAT"),
        (Image, "IMAGE"),
        (Embedding, "EMBEDDING"),
        (Ocr, "OCR"),
    ];
    for (domain, upper) in domains {
        let suffix = if domain == Chat {
            "/compatible-mode"
        } else {
            "/api/v1"
        };
        let empty = env(&[]);
        let got = ChatEndpointResolver::new(&empty).resolve_dashscope(domain);
        assert_eq!(got.api_key, "");
        assert_eq!(
            got.base_url,
            format!("https://dashscope.aliyuncs.com{suffix}")
        );
        for configured_suffix in [
            "",
            "/api/v1",
            "/compatible-mode",
            "/compatible-mode/v1",
            "/compatible-mode/v1/chat/completions",
        ] {
            let configured = format!("https://dashscope-intl.aliyuncs.com{configured_suffix}");
            let shared = env(&[
                ("DASHSCOPE_API_KEY", "general"),
                ("DASHSCOPE_BASE_URL", &configured),
            ]);
            let got = ChatEndpointResolver::new(&shared).resolve_dashscope(domain);
            assert_eq!(got.api_key, "general");
            assert_eq!(
                got.base_url,
                format!("https://dashscope-intl.aliyuncs.com{suffix}")
            );
        }
        let key_name = format!("DASHSCOPE_{upper}_API_KEY");
        let url_name = format!("DASHSCOPE_{upper}_BASE_URL");
        let values = env(&[
            ("DASHSCOPE_API_KEY", "general"),
            (&key_name, "domain-only"),
            (
                &url_name,
                "https://workspace.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
            ),
        ]);
        let resolver = ChatEndpointResolver::new(&values);
        let got = resolver.resolve_dashscope(domain);
        assert_eq!(got.api_key, "domain-only");
        assert_eq!(
            got.base_url,
            format!("https://workspace.ap-southeast-1.maas.aliyuncs.com{suffix}")
        );
        for (other, _) in domains {
            if other != domain {
                let got = resolver.resolve_dashscope(other);
                assert_eq!(got.api_key, "general");
                assert!(got.base_url.starts_with("https://dashscope.aliyuncs.com/"));
            }
        }
    }
}

#[test]
fn google_all_domains_global_switch_overrides_unknown_backend_and_shared_key_fallback() {
    use ai_platform_gateway_foundation::provider_startup_seeder::EndpointDomain::*;
    let domains = [
        (Chat, "CHAT"),
        (Image, "IMAGE"),
        (Embedding, "EMBEDDING"),
        (Ocr, "OCR"),
    ];
    for (domain, upper) in domains {
        let backend_name = format!("GOOGLE_{upper}_BACKEND");
        let key_name = format!("VERTEX_{upper}_API_KEY");
        let shared = env(&[
            ("GOOGLE_API_BACKEND", "vertex"),
            ("VERTEX_API_KEY", "global"),
        ]);
        let got = ChatEndpointResolver::new(&shared).resolve_google(domain);
        assert_eq!(
            (
                got.api_key.as_str(),
                got.backend.as_str(),
                got.base_url.as_str()
            ),
            ("global", "vertex", "https://aiplatform.googleapis.com")
        );
        let overridden = env(&[
            ("GEMINI_API_KEY", "studio"),
            (&backend_name, "vertex"),
            (&key_name, "domain-key"),
            ("VERTEX_API_KEY", "global"),
        ]);
        let resolver = ChatEndpointResolver::new(&overridden);
        let got = resolver.resolve_google(domain);
        assert_eq!(
            (
                got.api_key.as_str(),
                got.backend.as_str(),
                got.base_url.as_str()
            ),
            ("domain-key", "vertex", "https://aiplatform.googleapis.com")
        );
        for (other, _) in domains {
            if other != domain {
                let got = resolver.resolve_google(other);
                assert_eq!(
                    (
                        got.api_key.as_str(),
                        got.backend.as_str(),
                        got.base_url.as_str()
                    ),
                    (
                        "studio",
                        "ai_studio",
                        "https://generativelanguage.googleapis.com"
                    )
                );
            }
        }
        let values = env(&[(&backend_name, "vertex"), ("GEMINI_API_KEY", "shared")]);
        let got = ChatEndpointResolver::new(&values).resolve_google(domain);
        assert_eq!(
            (got.api_key.as_str(), got.backend.as_str()),
            ("shared", "vertex")
        );
        let values = env(&[(&backend_name, "OpenAI")]);
        let got = ChatEndpointResolver::new(&values).resolve_google(domain);
        assert_eq!(
            (got.backend.as_str(), got.base_url.as_str()),
            ("ai_studio", "https://generativelanguage.googleapis.com")
        );
    }
}
