//! Central mount of the existing source-backed gateway foundation.
//! Endpoint collaborators remain explicit, current-account authorities. These
//! factories do not connect to a database, start jobs, or manufacture identity.
use crate::*;
use ai_platform_auth::service::AuthService;
use axum::{Json, Router, http::StatusCode};
use serde_json::json;
use sqlx::PgPool;
use std::{collections::BTreeMap, sync::Arc};

pub struct FoundationCancellation {
    pub service: Arc<dyn service_config_api::ServiceConfigCancellation>,
    pub balancer: Arc<dyn balancer_config_api::BalancerConfigCancellation>,
    pub rate: Arc<dyn rate_config_api::RateConfigCancellation>,
    pub model: Arc<dyn model_admin_api::ModelAdminCancellation>,
    pub usage: Arc<dyn usage_admin_api::UsageAdminCancellation>,
    pub provider: Arc<dyn provider_admin_api::ProviderAdminCancellation>,
    pub quota: Arc<dyn quota_admin_api::QuotaAdminCancellation>,
    pub metrics: Arc<dyn metrics_admin_api::MetricsAdminCancellation>,
}
pub struct FoundationApis {
    pub openai_model_catalog: Arc<dyn openai_model_catalog::OpenAiModelCatalog>,
    pub introspection: Arc<dyn ai_platform_auth::http::CredentialIntrospectionResolver>,
    pub auth_config: Arc<auth_config_api::AuthConfigApi>,
    pub config_api_keys: Arc<config_api_keys_api::ConfigApiKeysApi>,
    pub connector_admin: Arc<connector_admin_api::ConnectorAdminApi>,
    pub connector_user: Arc<connector_user_api::ConnectorUserApi>,
    pub mcp_registry: Arc<mcp_registry_api::McpRegistryApi>,
    pub presign: Arc<presign_api::PresignApi>,
    pub roles: Arc<roles_api::RolesApi>,
    pub self_service_api_keys: Arc<self_service_api_keys_api::SelfServiceKeysApi>,
    pub service_config: Arc<service_config_api::ServiceConfigApi>,
    pub services: Arc<services_api::ServicesApi>,
    pub tenant_policies: Arc<tenant_policies_api::TenantPoliciesApi>,
    pub tool_inventory: Arc<tool_inventory_api::ToolInventoryApi>,
    pub users: Arc<users_api::UsersApi>,
    pub balancer_config: Arc<balancer_config_api::BalancerConfigApi>,
    pub rate_config: Arc<rate_config_api::RateConfigApi>,
    pub model_admin: Arc<model_admin_api::ModelAdminApi>,
    pub usage_admin: Arc<usage_admin_api::UsageAdminApi>,
    pub system_status: Arc<system_status_api::SystemStatusApi>,
    pub provider_admin: Arc<provider_admin_api::ProviderAdminApi>,
    pub quota_admin: Arc<quota_admin_api::QuotaAdminApi>,
    pub dashboard: Arc<dashboard_api::DashboardApi>,
    pub metrics_admin: Arc<metrics_admin_api::MetricsAdminApi>,
    pub provider_identity: Arc<dyn provider_admin_api::ProviderAdminIdentityResolver>,
    pub quota_identity: Arc<dyn quota_admin_api::QuotaAdminIdentityResolver>,
    pub metrics_identity: Arc<dyn metrics_admin_api::MetricsAdminIdentityResolver>,
    pub memory: assistant_memory_api::AssistantMemoryApi,
    pub memory_identity: Arc<dyn assistant_memory_api::AssistantMemoryIdentityResolver>,
    pub artifact_shares: artifact_shares_api::ArtifactSharesApi,
    pub artifact_share_identity: Arc<dyn artifact_shares_api::ArtifactSharesIdentityResolver>,
    pub local_nodes: local_nodes_api::LocalNodesApi,
    pub local_node_identity: Arc<dyn local_nodes_api::LocalNodesIdentityResolver>,
    pub sessions: sessions_api::SessionsApi,
    pub health: health_api::HealthHttpApi,
    pub setup: Arc<setup_api::SetupApi>,
    pub architecture: Arc<architecture_status_api::ArchitectureStatusApi>,
    pub invocation_identity: Arc<dyn invocation_http::InvocationIdentityResolver>,
    pub tasks: Arc<task_manager::TaskManager>,
    pub dispatcher: Arc<gateway_dispatcher::GatewayDispatcher>,
}
impl FoundationApis {
    pub fn into_router(
        self,
        auth: Arc<AuthService>,
        pool: PgPool,
        options: GatewayOptions,
        configured_roles: BTreeMap<String, Vec<String>>,
        cancellation: FoundationCancellation,
        content_type: balancer_config_api::BalancerConfigContentTypePolicy,
    ) -> Router {
        // Per-slice 404 fallback ownership is replaced by a single canonical
        // fallback here; merging two custom Axum fallbacks otherwise panics.
        let mut router = crate::router_without_knowledge_proxy(auth.clone(), pool, options)
            .reset_fallback()
            .merge(
                ai_platform_auth::http::router_with_introspection(
                    auth.clone(),
                    configured_roles,
                    self.introspection,
                )
                .reset_fallback(),
            );
        router = router.merge(
            auth_config_api::router(self.auth_config, cancellation.service.clone())
                .reset_fallback(),
        );
        router = router.merge(
            config_api_keys_api::router(self.config_api_keys, cancellation.service.clone())
                .reset_fallback(),
        );
        router = router.merge(
            connector_admin_api::router(self.connector_admin, cancellation.service.clone())
                .reset_fallback(),
        );
        router = router.merge(
            connector_user_api::router(self.connector_user, cancellation.service.clone())
                .reset_fallback(),
        );
        router = router.merge(
            mcp_registry_api::router(self.mcp_registry, cancellation.service.clone())
                .reset_fallback(),
        );
        router = router.merge(
            presign_api::router(self.presign, cancellation.service.clone()).reset_fallback(),
        );
        router = router
            .merge(roles_api::router(self.roles, cancellation.service.clone()).reset_fallback());
        router = router.merge(
            self_service_api_keys_api::router(
                self.self_service_api_keys,
                cancellation.service.clone(),
            )
            .reset_fallback(),
        );
        router = router.merge(
            service_config_api::router(self.service_config, cancellation.service.clone())
                .reset_fallback(),
        );
        router = router.merge(
            services_api::router(self.services, cancellation.service.clone()).reset_fallback(),
        );
        router = router.merge(
            tenant_policies_api::router(self.tenant_policies, cancellation.service.clone())
                .reset_fallback(),
        );
        router = router.merge(
            tool_inventory_api::router(self.tool_inventory, cancellation.service.clone())
                .reset_fallback(),
        );
        router = router
            .merge(users_api::router(self.users, cancellation.service.clone()).reset_fallback());
        router = router
            .merge(
                balancer_config_api::router(
                    self.balancer_config,
                    cancellation.balancer.clone(),
                    content_type,
                )
                .reset_fallback(),
            )
            .merge(
                system_status_api::router(self.system_status, cancellation.balancer)
                    .reset_fallback(),
            )
            .merge(rate_config_api::router(self.rate_config, cancellation.rate).reset_fallback())
            .merge(model_admin_api::router(self.model_admin, cancellation.model).reset_fallback())
            .merge(usage_admin_api::router(self.usage_admin, cancellation.usage).reset_fallback())
            .merge(
                provider_admin_api::router(
                    self.provider_identity,
                    self.provider_admin,
                    cancellation.provider,
                )
                .reset_fallback(),
            )
            .merge(
                quota_admin_api::router(self.quota_identity, self.quota_admin, cancellation.quota)
                    .reset_fallback(),
            )
            .merge(
                dashboard_api::router(
                    self.metrics_identity.clone(),
                    self.dashboard,
                    cancellation.metrics.clone(),
                )
                .reset_fallback(),
            )
            .merge(
                metrics_admin_api::router(
                    self.metrics_identity,
                    self.metrics_admin,
                    cancellation.metrics,
                )
                .reset_fallback(),
            )
            .merge(
                assistant_memory_api::assistant_memory_router(self.memory, self.memory_identity)
                    .reset_fallback(),
            )
            .merge(
                artifact_shares_api::artifact_shares_router(
                    self.artifact_shares,
                    self.artifact_share_identity,
                )
                .reset_fallback(),
            )
            .merge(
                local_nodes_api::local_nodes_router(self.local_nodes, self.local_node_identity)
                    .reset_fallback(),
            )
            .merge(self.sessions.router().reset_fallback())
            .merge(self.health.router().reset_fallback())
            .nest(
                "/api/v1",
                setup_api::setup_router(self.setup).merge(
                    architecture_status_api::architecture_status_router(self.architecture),
                ),
            )
            .merge(
                task_http::router_with_identity(
                    auth.clone(),
                    self.tasks,
                    Some(self.invocation_identity.clone()),
                )
                .reset_fallback(),
            )
            .merge(
                invocation_http::router_with_identity_and_model_catalog(
                    auth,
                    self.dispatcher,
                    Some(self.invocation_identity),
                    Some(self.openai_model_catalog),
                )
                .reset_fallback(),
            );
        router.fallback(|| async { (StatusCode::NOT_FOUND, Json(json!({"detail":"Not Found"}))) })
    }
}

#[cfg(test)]
mod setup_tests {
    use super::*;
    use crate::health_api::{HealthApiFuture, HealthCancellationFuture, HealthModelMetadata};
    use crate::setup_api::{SetupApi, SetupIdentity, SetupIdentityResolver};
    use axum::{http::HeaderMap, response::IntoResponse};
    use serde_json::Value;
    use std::sync::Mutex;

    struct Identity;
    impl SetupIdentityResolver for Identity {
        fn resolve<'a>(
            &'a self,
            _: &'a HeaderMap,
            _: &'a str,
        ) -> HealthApiFuture<'a, SetupIdentity> {
            Box::pin(async { Ok(identity(true, true)) })
        }
    }
    struct Metadata(Mutex<Vec<String>>);
    impl HealthModelMetadata for Metadata {
        fn configured<'a>(&'a self, tenant: &'a str) -> HealthApiFuture<'a, Vec<String>> {
            Box::pin(async move {
                self.0.lock().unwrap().push(tenant.into());
                Ok(vec!["openai".into()])
            })
        }
        fn model_counts<'a>(
            &'a self,
            _: &'a str,
        ) -> HealthApiFuture<'a, serde_json::Map<String, Value>> {
            Box::pin(async { Ok(Default::default()) })
        }
    }
    struct Cancellation;
    impl health_api::HealthHttpCancellation for Cancellation {
        fn cancelled(&self) -> HealthCancellationFuture<'_> {
            Box::pin(async { StatusCode::SERVICE_UNAVAILABLE.into_response() })
        }
    }
    fn identity(authenticated: bool, permission: bool) -> SetupIdentity {
        SetupIdentity {
            user_id: "user".into(),
            tenant_id: "tenant".into(),
            roles: vec![],
            permissions: if permission {
                vec!["console:services:view".into()]
            } else {
                vec![]
            },
            is_authenticated: authenticated,
        }
    }
    fn api(metadata: Arc<Metadata>) -> SetupApi {
        SetupApi {
            identity: Arc::new(Identity),
            rbac: Default::default(),
            metadata: Some(metadata),
            mode: "environment".into(),
            default_model: "configured-model".into(),
            cancellation: Arc::new(Cancellation),
        }
    }
    #[tokio::test]
    async fn setup_projects_current_tenant_configuration() {
        let metadata = Arc::new(Metadata(Mutex::new(vec![])));
        let result = api(metadata.clone())
            .state(&identity(true, true), "trace")
            .await
            .unwrap();
        assert_eq!(result["configured"], true);
        assert_eq!(result["mode"], "environment");
        assert_eq!(result["default_model"], "configured-model");
        assert!(
            !result["missing"]
                .as_array()
                .unwrap()
                .contains(&json!("openai"))
        );
        assert_eq!(*metadata.0.lock().unwrap(), vec!["tenant"]);
    }
    #[tokio::test]
    async fn setup_denies_before_reading_provider_state() {
        let metadata = Arc::new(Metadata(Mutex::new(vec![])));
        let api = api(metadata.clone());
        assert_eq!(
            api.state(&identity(false, true), "trace")
                .await
                .unwrap_err()
                .status(),
            Some(401)
        );
        assert_eq!(
            api.state(&identity(true, false), "trace")
                .await
                .unwrap_err()
                .status(),
            Some(403)
        );
        assert!(metadata.0.lock().unwrap().is_empty());
    }
}
