//! Pre-execution orchestration from `src/core/gateway/dispatcher.py`.
//!
//! Service lookup always precedes the actual shared validator and legacy limiter.
//! Invoke and submit validate before charging; only invoke records rate denials.
//! Stream mirrors `asyncio.gather`: its synchronous validator runs first, but the
//! limiter still runs even when validation fails. A pending limiter/event write
//! continues after that validation error, as Python's sibling coroutine does.
//! Cancellation while awaiting an otherwise-valid preflight cancels its pending
//! limiter/event future. Runtime shutdown can interrupt detached sibling work.
//!
//! Identity is replaced from explicit resolved authority, matching
//! `UnifiedRequestSchema.to_domain(default_user_id, default_tenant_id)`. This is
//! not credential verification or an HTTP router. Request-body identity never
//! supplies validation, rate-limit keys, or event attribution. The caller owns
//! current credential resolution and any API-key/service-resource restrictions.
//!
//! A successful result is the boundary before session persistence, adapter
//! construction, capacity admission, usage, or task creation. No such side effects
//! occur in this module. Only the original external security-recorder call is an
//! injected seam; lookup, validation, configuration projection, and rate charging
//! compose the actual migrated modules.
use crate::{
    dispatcher_rate_limiter::{RateLimitExceeded, RateLimiter},
    protocol::UnifiedRequest,
    request_validation::{RequestValidationError, RequestValidator},
    service_ownership::ServiceHandle,
    service_registry::{RegistryError, ServiceRegistry},
};
use ai_platform_auth::{admission::RequestPrincipal, rbac::Rbac};
use std::{fmt, future::Future, future::poll_fn, pin::Pin, sync::Arc, task::Poll};

/// Resolved `UserContext` input, never deserialized from a request body.
pub struct PreflightAuthority<'a> {
    user_id: &'a str,
    tenant_id: &'a str,
    roles: &'a [String],
    task_actor: Option<crate::task_actor::TaskActor>,
}
impl<'a> PreflightAuthority<'a> {
    pub fn from_principal(principal: &'a RequestPrincipal) -> Self {
        let mut authority = Self::from_resolved_context(
            principal.user_id(), principal.tenant_id(), principal.roles(),
        );
        authority.task_actor = principal.jwt_principal().map(crate::task_actor::TaskActor::from_session);
        authority
    }

    pub fn from_session_principal(principal: &'a ai_platform_auth::service::Principal) -> Self {
        let mut authority = Self::from_resolved_context(
            &principal.account.user_id, &principal.account.tenant_id, &principal.authority.roles,
        );
        authority.task_actor = Some(crate::task_actor::TaskActor::from_session(principal));
        authority
    }
    pub fn task_actor(&self) -> Option<crate::task_actor::TaskActor> { self.task_actor.clone() }

    pub fn user_id(&self) -> &str {
        self.user_id
    }

    pub fn tenant_id(&self) -> &str {
        self.tenant_id
    }

    pub fn roles(&self) -> &[String] {
        self.roles
    }

    /// Adapter for already-resolved Python-compatible user contexts, including
    /// guest/static-key modes not represented by `RequestPrincipal`. This does
    /// not authenticate strings. Only the authoritative user resolver may call
    /// it; never pass user/tenant/roles from body data or unverified claims.
    pub fn from_resolved_context(
        user_id: &'a str,
        tenant_id: &'a str,
        roles: &'a [String],
    ) -> Self {
        Self {
            user_id,
            tenant_id,
            roles,
            task_actor: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreflightMode {
    Invoke,
    Stream,
    Submit,
}

/// Domain request now carries the same authority used for every preflight check.
/// Continue session/adapter/task orchestration only after receiving this result.
pub struct PreflightReady {
    pub request: UnifiedRequest,
    pub service: ServiceHandle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreflightError {
    Registry(RegistryError),
    ServiceNotFound(String),
    Validation(RequestValidationError),
    RateConfiguration(&'static str),
    RateLimited(RateLimitExceeded),
}
impl fmt::Display for PreflightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registry(error) => error.fmt(f),
            Self::ServiceNotFound(service_id) => write!(f, "Service not found: {service_id}"),
            Self::Validation(error) => error.fmt(f),
            Self::RateConfiguration(message) => f.write_str(message),
            Self::RateLimited(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for PreflightError {}

/// Exact `_record_rate_limit_event` arguments. No request body or credential is
/// retained, and empty resolved tenant/user values use the source fallbacks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RateLimitSecurityEvent {
    pub tenant_id: String,
    pub user_id: Option<String>,
    pub service_id: String,
}
impl RateLimitSecurityEvent {
    pub fn event_type(&self) -> &'static str {
        "rate_limited"
    }
}

/// Recorder failures are best-effort and must not replace the rate-limit error.
/// No provider/database error strings are carried across this seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimitEventUnavailable;
impl fmt::Display for RateLimitEventUnavailable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("rate_limit_security_recorder_unavailable")
    }
}
impl std::error::Error for RateLimitEventUnavailable {}
pub type RateLimitEventFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), RateLimitEventUnavailable>> + Send + 'a>>;

/// External `SecurityEventRecorder.record_event` boundary. Implementations must
/// support cancellation and should not log credentials or private error details.
pub trait RateLimitEventRecorder: Send + Sync {
    fn record_event(&self, event: RateLimitSecurityEvent) -> RateLimitEventFuture<'_>;
}

/// Explicit source-compatible recorder when no database recorder is configured.
/// This does not provide durable recording or claim that an event was persisted.
pub struct UnconfiguredRateLimitEventRecorder;
impl RateLimitEventRecorder for UnconfiguredRateLimitEventRecorder {
    fn record_event(&self, _event: RateLimitSecurityEvent) -> RateLimitEventFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}

pub struct DispatchPreflight {
    registry: Arc<ServiceRegistry>,
    rbac: Arc<Rbac>,
    rate_limiter: Arc<RateLimiter>,
    events: Arc<dyn RateLimitEventRecorder>,
}
impl DispatchPreflight {
    pub fn new(
        registry: Arc<ServiceRegistry>,
        rbac: Arc<Rbac>,
        rate_limiter: Arc<RateLimiter>,
        events: Arc<dyn RateLimitEventRecorder>,
    ) -> Self {
        Self {
            registry,
            rbac,
            rate_limiter,
            events,
        }
    }

    /// Downstream orchestration must use the same registry that selected the
    /// validated service, never a separately configured registry.
    pub fn registry(&self) -> &Arc<ServiceRegistry> {
        &self.registry
    }

    pub async fn prepare(
        &self,
        mut request: UnifiedRequest,
        authority: &PreflightAuthority<'_>,
        client_ip: Option<&str>,
        mode: PreflightMode,
    ) -> Result<PreflightReady, PreflightError> {
        request.user_id = authority.user_id.to_owned();
        request.tenant_id = authority.tenant_id.to_owned();
        let service = self
            .registry
            .get(&request.service_id)
            .await
            .map_err(PreflightError::Registry)?
            .ok_or_else(|| PreflightError::ServiceNotFound(request.service_id.clone()))?;
        let validation = RequestValidator::new(&self.rbac)
            .validate_with_auth(
                &request.validation_view(),
                &service.snapshot().validation_view(),
                authority.roles,
                || service.materialize_config().snapshot().auth,
            )
            .map_err(PreflightError::Validation);

        if mode != PreflightMode::Stream
            && let Err(error) = validation
        {
            return Err(error);
        }
        let mut rate = Box::pin(enforce_rate_limit(
            self.rate_limiter.clone(),
            self.events.clone(),
            request.tenant_id.clone(),
            request.user_id.clone(),
            service.clone(),
            client_ip.map(str::to_owned),
            mode != PreflightMode::Submit,
        ));
        if mode == PreflightMode::Stream {
            // Python's validator has no suspension point, so its task completes
            // first. gather still starts the limiter, and does not cancel that
            // sibling when validation raises. Poll once before deciding whether
            // pending rate work must outlive this rejected preflight.
            let first_poll = poll_fn(|cx| Poll::Ready(rate.as_mut().poll(cx))).await;
            if let Err(error) = validation {
                if first_poll.is_pending() {
                    tokio::spawn(rate);
                }
                return Err(error);
            }
            match first_poll {
                Poll::Ready(result) => result?,
                Poll::Pending => rate.await?,
            }
        } else {
            rate.await?;
        }
        Ok(PreflightReady { request, service })
    }
}

async fn enforce_rate_limit(
    limiter: Arc<RateLimiter>,
    events: Arc<dyn RateLimitEventRecorder>,
    tenant_id: String,
    user_id: String,
    service: ServiceHandle,
    client_ip: Option<String>,
    record_denial: bool,
) -> Result<(), PreflightError> {
    // Construct the rate view here, after sequential validation or inside the
    // stream sibling. Invalid legacy config must not overtake validation errors.
    service.materialize_config();
    let limits = service
        .snapshot()
        .rate_limits_view()
        .map_err(PreflightError::RateConfiguration)?;
    if let Err(error) = limiter
        .enforce(&tenant_id, &user_id, &limits, client_ip.as_deref())
        .await
    {
        if record_denial {
            let _ = events
                .record_event(RateLimitSecurityEvent {
                    tenant_id: if tenant_id.is_empty() {
                        "public".into()
                    } else {
                        tenant_id
                    },
                    user_id: (!user_id.is_empty()).then_some(user_id),
                    service_id: service.service_id(),
                })
                .await;
        }
        return Err(PreflightError::RateLimited(error));
    }
    Ok(())
}
