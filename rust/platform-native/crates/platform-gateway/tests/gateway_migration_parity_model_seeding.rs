//! Original startup seeder cases, using actual orchestration and injected provider boundaries.
use ai_platform_gateway_foundation::{
    provider_service::{
        CreateProvider, ProviderEnvironment, ServiceError, ServiceFuture, UpdateProvider,
    },
    provider_startup_seeder::*,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Mutex};
#[derive(Default)]
struct Environment(BTreeMap<String, String>);
impl ProviderEnvironment for Environment {
    fn get(&self, k: &str) -> Option<String> {
        self.0.get(k).cloned()
    }
}
#[derive(Default)]
struct Log(Mutex<Vec<String>>);
impl StartupLogger for Log {
    fn log(&self, _: StartupLogLevel, message: &str) {
        self.0.lock().unwrap().push(message.into());
    }
}
#[derive(Default)]
struct Providers {
    existing: BTreeMap<String, Value>,
    runtime_key: String,
    listed: Vec<Value>,
    failures: bool,
    gets: Mutex<Vec<String>>,
    creates: Mutex<Vec<CreateProvider>>,
    updates: Mutex<Vec<(String, UpdateProvider)>>,
    lists: Mutex<Vec<(String, bool)>>,
}
impl StartupProviderService for Providers {
    fn get_provider<'a>(&'a self, t: &'a str, p: &'a str) -> ServiceFuture<'a, Option<Value>> {
        Box::pin(async move {
            assert_eq!(t, "default");
            self.gets.lock().unwrap().push(p.into());
            if self.failures && p == "openai" {
                return Err(ServiceError::Database);
            }
            Ok(self.existing.get(p).cloned())
        })
    }
    fn create_provider<'a>(&'a self, t: &'a str, p: CreateProvider) -> ServiceFuture<'a, Value> {
        Box::pin(async move {
            assert_eq!(t, "default");
            if self.failures && p.provider_id == "anthropic" {
                return Err(ServiceError::Database);
            }
            self.creates.lock().unwrap().push(p);
            Ok(json!({}))
        })
    }
    fn update_provider<'a>(
        &'a self,
        t: &'a str,
        id: &'a str,
        p: UpdateProvider,
    ) -> ServiceFuture<'a, Option<Value>> {
        Box::pin(async move {
            assert_eq!(t, "default");
            self.updates.lock().unwrap().push((id.into(), p));
            Ok(Some(json!({})))
        })
    }
    fn get_runtime_provider_config<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> ServiceFuture<'a, StartupRuntimeConfig> {
        Box::pin(async move { Ok(StartupRuntimeConfig::new(Some(self.runtime_key.clone()))) })
    }
    fn list_providers<'a>(&'a self, t: &'a str, disabled: bool) -> ServiceFuture<'a, Vec<Value>> {
        Box::pin(async move {
            self.lists.lock().unwrap().push((t.into(), disabled));
            if self.failures {
                Err(ServiceError::Database)
            } else {
                Ok(self.listed.clone())
            }
        })
    }
}
async fn seed(p: &Providers, e: &Environment, log: &Log) -> ProviderSeedResult {
    seed_startup_providers(
        Some(p),
        e,
        &ChatEndpointResolver::new(e),
        StartupSeedSettings::default(),
        log,
    )
    .await
}
#[tokio::test]
async fn seed_creates_all_default_rows_without_credentials() {
    let p = Providers::default();
    let r = seed(&p, &Environment::default(), &Log::default()).await;
    let c = p.creates.lock().unwrap();
    assert_eq!(
        c.iter().map(|c| c.provider_id.as_str()).collect::<Vec<_>>(),
        [
            "openai",
            "anthropic",
            "deepseek",
            "dashscope",
            "google",
            "google-vertex"
        ]
    );
    assert!(c.iter().all(|c| c.api_key.is_none()));
    assert_eq!(*p.lists.lock().unwrap(), vec![("default".into(), false)]);
    assert!(r.configured_providers.is_empty());
    assert!(r.runtime_configured_providers.is_empty());
}
#[tokio::test]
async fn existing_rows_update_only_when_environment_key_is_present() {
    let p = Providers {
        existing: DEFAULT_PROVIDER_CONFIGS
            .iter()
            .map(|c| (c.provider_id.into(), json!({"provider_id":c.provider_id})))
            .collect(),
        ..Default::default()
    };
    let e = Environment(BTreeMap::from([
        ("OPENAI_API_KEY".into(), "synthetic-key".into()),
        ("OPENAI_BASE_URL".into(), "https://openai.example".into()),
    ]));
    let log = Log::default();
    let r = seed(&p, &e, &log).await;
    assert!(p.creates.lock().unwrap().is_empty());
    let updates = p.updates.lock().unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].0, "openai");
    let u = &updates[0].1;
    assert_eq!(u.api_key.as_deref(), Some("synthetic-key"));
    assert_eq!(u.api_type.as_deref(), Some("openai"));
    assert_eq!(u.base_url.as_deref(), Some("https://openai.example"));
    assert_eq!(r.configured_providers, ["openai"]);
    assert_eq!(
        r.runtime_configured_providers
            .into_iter()
            .collect::<Vec<_>>(),
        ["openai"]
    );
    assert!(!log.0.lock().unwrap().join("\n").contains("synthetic-key"));
}
#[tokio::test]
async fn restart_preserves_revision_only_for_unchanged_runtime_configuration() {
    for changed in ["none", "credential", "endpoint", "api_type"] {
        let r = json!({"provider_id":"openai","has_api_key":true,"api_type":if changed=="api_type" {"anthropic"}else{"openai"},"base_url":if changed=="endpoint" {"https://previous.example"}else{"https://openai.example"},"updated_at":"original-revision"});
        let p = Providers {
            existing: BTreeMap::from([("openai".into(), r)]),
            runtime_key: if changed == "credential" {
                "previous-key"
            } else {
                "synthetic-key"
            }
            .into(),
            ..Default::default()
        };
        let e = Environment(BTreeMap::from([
            ("OPENAI_API_KEY".into(), "synthetic-key".into()),
            ("OPENAI_BASE_URL".into(), "https://openai.example".into()),
        ]));
        let log = Log::default();
        for _ in 0..2 {
            seed(&p, &e, &log).await;
        }
        assert_eq!(
            p.updates.lock().unwrap().len(),
            if changed == "none" { 0 } else { 2 }
        );
        assert_eq!(p.existing["openai"]["updated_at"], "original-revision");
        assert!(!log.0.lock().unwrap().join("\n").contains("synthetic-key"));
    }
}
struct Endpoints {
    dash: &'static str,
    vertex: bool,
}
impl StartupEndpointResolver for Endpoints {
    fn resolve_dashscope_chat(&self) -> DashscopeChatEndpoint {
        DashscopeChatEndpoint {
            api_key: self.dash.into(),
            base_url: "https://dashscope.example/compatible-mode".into(),
        }
    }
    fn resolve_google_chat(&self) -> GoogleChatEndpoint {
        GoogleChatEndpoint {
            api_key: if self.vertex { "vertex-secret" } else { "" }.into(),
            base_url: "https://vertex.example".into(),
            backend: if self.vertex { "vertex" } else { "ai_studio" }.into(),
        }
    }
}
#[tokio::test]
async fn dashscope_legacy_key_seeds_database_without_proving_runtime_readiness() {
    for key in ["dashscope-runtime-secret", ""] {
        let p = Providers::default();
        let r = seed_startup_providers(
            Some(&p),
            &Environment::default(),
            &Endpoints {
                dash: key,
                vertex: false,
            },
            StartupSeedSettings {
                legacy_dashscope_api_key: "legacy-secret",
                ..Default::default()
            },
            &Log::default(),
        )
        .await;
        let c = p.creates.lock().unwrap();
        let d = c.iter().find(|c| c.provider_id == "dashscope").unwrap();
        assert_eq!(
            d.api_key.as_deref(),
            Some(if key.is_empty() { "legacy-secret" } else { key })
        );
        assert_eq!(
            d.base_url.as_deref(),
            Some("https://dashscope.example/compatible-mode")
        );
        assert_eq!(r.configured_providers, ["dashscope"]);
        assert_eq!(
            r.runtime_configured_providers.contains("dashscope"),
            !key.is_empty()
        );
    }
}
#[tokio::test]
async fn google_vertex_resolution_seeds_none_base_url() {
    let p = Providers::default();
    let r = seed_startup_providers(
        Some(&p),
        &Environment::default(),
        &Endpoints {
            dash: "",
            vertex: true,
        },
        StartupSeedSettings::default(),
        &Log::default(),
    )
    .await;
    let c = p.creates.lock().unwrap();
    let g = c.iter().find(|c| c.provider_id == "google").unwrap();
    assert_eq!(g.api_key.as_deref(), Some("vertex-secret"));
    assert!(g.base_url.is_none());
    assert_eq!(r.configured_providers, ["google"]);
    assert_eq!(
        r.runtime_configured_providers
            .into_iter()
            .collect::<Vec<_>>(),
        ["google"]
    );
}
#[tokio::test]
async fn database_only_provider_is_configured_but_not_runtime_ready() {
    let p = Providers {
        listed: vec![
            json!({"provider_id":"deepseek","has_api_key":true}),
            json!({"provider_id":"custom-provider","has_api_key":true}),
            json!({"provider_id":"anthropic","has_api_key":false}),
        ],
        ..Default::default()
    };
    let r = seed(&p, &Environment::default(), &Log::default()).await;
    assert_eq!(r.configured_providers, ["deepseek"]);
    assert!(r.runtime_configured_providers.is_empty());
}
#[tokio::test]
async fn provider_and_list_failures_are_isolated() {
    let p = Providers {
        failures: true,
        ..Default::default()
    };
    let log = Log::default();
    let r = seed(&p, &Environment::default(), &log).await;
    assert_eq!(
        *p.gets.lock().unwrap(),
        [
            "openai",
            "anthropic",
            "deepseek",
            "dashscope",
            "google",
            "google-vertex"
        ]
    );
    assert!(r.configured_providers.is_empty());
    let logs = log.0.lock().unwrap().join("\n");
    for fragment in [
        "Failed to sync provider openai",
        "Failed to sync provider anthropic",
        "Failed to load providers from database",
    ] {
        assert!(logs.contains(fragment));
    }
}
#[derive(Default)]
struct Catalog(Mutex<Vec<(String, String, bool)>>);
impl StartupCatalogSync for Catalog {
    fn sync_provider_models<'a>(
        &'a self,
        t: &'a str,
        p: &'a str,
        d: bool,
    ) -> StartupCatalogFuture<'a> {
        Box::pin(async move {
            self.0.lock().unwrap().push((t.into(), p.into(), d));
            if p == "google" {
                Err(StartupCatalogError::SyncFailed)
            } else {
                Ok(json!({"created_models":[{"model_id":"created"}],"updated_models":[]}))
            }
        })
    }
}
#[tokio::test]
async fn catalog_sync_is_sorted_deduplicated_and_failure_isolated() {
    let c = Catalog::default();
    let log = Log::default();
    sync_startup_model_catalog(
        Some(&c),
        &[
            "openai".into(),
            "google".into(),
            "anthropic".into(),
            "openai".into(),
        ],
        "default",
        &log,
    )
    .await;
    assert_eq!(
        *c.0.lock().unwrap(),
        vec![
            ("default".into(), "anthropic".into(), false),
            ("default".into(), "google".into(), false),
            ("default".into(), "openai".into(), false)
        ]
    );
    assert!(
        log.0
            .lock()
            .unwrap()
            .join("\n")
            .contains("Failed to sync startup model catalog for provider google")
    );
}
