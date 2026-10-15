//! Source-backed platform glue for callback delivery and rate-event persistence.
use crate::{
    connector_oauth_http::{
        ConnectorDnsResolver, NativeConnectorDnsResolver, oauth_form_safe_address,
    },
    dispatch_preflight::{
        RateLimitEventFuture, RateLimitEventRecorder, RateLimitEventUnavailable,
        RateLimitSecurityEvent,
    },
    task_manager::{
        CallbackFailure, CallbackFuture, CallbackGuard, CallbackPayload, SafeCallbackTransport,
    },
};
use sqlx::PgPool;
use std::{net::SocketAddr, time::Duration};

/// safe_fetch.safe_callback_post: explicit allowlist, all-answer DNS validation,
/// first approved address pin, retained Host/SNI, no redirect and a 10s timeout.
pub struct NativeCallbackTransport {
    pub allowed_hosts: Vec<String>,
}
impl SafeCallbackTransport for NativeCallbackTransport {
    fn post<'a>(&'a self, raw: &'a str, payload: &'a CallbackPayload) -> CallbackFuture<'a> {
        Box::pin(async move {
            let denied = || CallbackFailure::Guard(CallbackGuard::UnsafeDestination);
            let url = reqwest::Url::parse(raw).map_err(|_| denied())?;
            if !matches!(url.scheme(), "http" | "https") {
                return Err(denied());
            }
            let host = url.host_str().ok_or_else(denied)?.trim_matches(['[', ']']);
            let port = url.port_or_known_default().ok_or_else(denied)?;
            let normalized = host.to_lowercase();
            let normalized = normalized.trim_end_matches('.');
            let allowed = self.allowed_hosts.iter().any(|suffix| {
                let suffix = suffix.to_lowercase();
                let suffix = suffix.trim_start_matches('.');
                normalized == suffix || normalized.ends_with(&format!(".{suffix}"))
            });
            let mut client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .timeout(Duration::from_secs(10));
            if !allowed {
                let addresses = NativeConnectorDnsResolver
                    .resolve(host, port)
                    .await
                    .map_err(|_| denied())?;
                if addresses.is_empty() || addresses.iter().any(|ip| !oauth_form_safe_address(*ip))
                {
                    return Err(denied());
                }
                client = client.resolve(host, SocketAddr::new(addresses[0], port));
            }
            // Source guards the URL before asking the raw task result for JSON.
            let body = payload.to_json()?;
            let client = client.build().map_err(|_| CallbackFailure::Other)?;
            let response = client.post(url).json(&body).send().await.map_err(|error| {
                if error.is_timeout() {
                    CallbackFailure::Timeout
                } else {
                    CallbackFailure::Other
                }
            })?;
            Ok(response.status().as_u16())
        })
    }
}
/// database.record_security_event uses UTC day and the exact expression conflict
/// key. The caller treats an error as best-effort without retrying uncertain writes.
pub struct PgRateLimitEventRecorder {
    pub pool: PgPool,
}
impl RateLimitEventRecorder for PgRateLimitEventRecorder {
    fn record_event(&self, event: RateLimitSecurityEvent) -> RateLimitEventFuture<'_> {
        Box::pin(async move {
            sqlx::query("INSERT INTO security_event_daily_aggregates (tenant_id,user_id,service_id,event_type,date,event_count) VALUES ($1,$2,$3,$4,$5,1) ON CONFLICT (tenant_id,COALESCE(user_id,''),COALESCE(service_id,''),event_type,date) DO UPDATE SET event_count=security_event_daily_aggregates.event_count+1,updated_at=CURRENT_TIMESTAMP")
                .bind(event.tenant_id).bind(event.user_id.filter(|v|!v.is_empty())).bind((!event.service_id.is_empty()).then_some(event.service_id)).bind("rate_limited").bind(chrono::Utc::now().date_naive()).execute(&self.pool).await.map_err(|_|RateLimitEventUnavailable)?;
            Ok(())
        })
    }
}
impl super::core::CoreBackends {
    pub fn native(
        pools: &super::DatabasePools,
        langgraph_prepare: std::sync::Arc<
            dyn crate::migration_adapters::LangGraphPayloadPreparation,
        >,
        callback_allowlist: Vec<String>,
    ) -> Self {
        Self {
            callbacks: std::sync::Arc::new(NativeCallbackTransport {
                allowed_hosts: callback_allowlist.clone(),
            }),
            rate_events: std::sync::Arc::new(PgRateLimitEventRecorder {
                pool: pools.data.clone(),
            }),
            langgraph_prepare,
            callback_allowlist,
        }
    }
}

/// Bind public health projection to the existing real dependency probe.
impl crate::health_api::HealthDependencyState for crate::migration_host::readiness::ReadinessState {
    fn probe(&self) -> Option<crate::health_api::HealthApiFuture<'_, serde_json::Value>> {
        Some(Box::pin(async move { Ok(self.snapshot().await) }))
    }
    fn cached_snapshot(&self) -> Option<serde_json::Value> {
        let snapshot = self.last_snapshot.read().unwrap_or_else(|e| e.into_inner());
        snapshot.is_object().then(|| snapshot.clone())
    }
    fn fallback_state(
        &self,
    ) -> Result<crate::health_api::HealthFallbackState, crate::health_api::HealthApiError> {
        let config = self.config.read().unwrap_or_else(|e| e.into_inner());
        Ok(crate::health_api::HealthFallbackState {
            runtime_present: config.runtime_url.as_ref().is_some_and(|u| !u.is_empty()),
            model_plane_present: config.model_plane_ready,
            worker_running: config.image_worker_status.as_deref() == Some("healthy"),
        })
    }
}
