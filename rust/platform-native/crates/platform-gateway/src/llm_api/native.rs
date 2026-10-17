//! Production adapters over the existing registered authorities. No new tokens,
//! price selection algorithm, budget store, or provider credential store.
use super::*;
use crate::migration_host::routing_policy_v1 as routing;
use crate::{
    dispatch_preflight::{PreflightAuthority, PreflightMode},
    dispatcher::{DispatchContext, DispatchSettlement},
    protocol::{ContentItem, UnifiedRequest},
    usage_recorder::{AccountingStage, StageStatus, UsagePersistence},
};
use ai_platform_gateway_foundation::{
    price_book::service::PgPriceBookService,
    quota_store::reservation_receipts::CheckpointSettlingPersistence,
};
use std::collections::{BTreeMap, BTreeSet};
pub(super) struct NativeBackend {
    auth: Arc<AdmissionService>,
    pool: sqlx::PgPool,
    preflight: Arc<DispatchPreflight>,
    admission: Arc<dyn DispatchAdmission>,
    capacity: Arc<CapacityResolver>,
    providers: Arc<dyn crate::migration_agent::model::RuntimeProviderCredentials>,
    bindings: LlmBindings,
    routing: LlmRoutingBindings,
    persistence: Arc<dyn UsagePersistence>,
}
fn unavailable() -> LlmError {
    LlmError::new(503, "llm_authority_unavailable")
}
fn preflight_error(error: crate::dispatch_preflight::PreflightError) -> LlmError {
    use crate::dispatch_preflight::PreflightError;
    match error {
        PreflightError::RateLimited(_) => LlmError::new(429, "llm_rate_limited"),
        PreflightError::Validation(_) => LlmError::new(403, "llm_preflight_denied"),
        _ => unavailable(),
    }
}
fn admission_error(error: crate::dispatcher::DispatchError) -> LlmError {
    match error {
        crate::dispatcher::DispatchError::Admission(
            crate::capacity_admission::AdmissionError::Rejected(rejected),
        ) => LlmError::new(rejected.status_code, "llm_dispatch_admission_denied"),
        _ => unavailable(),
    }
}
impl NativeBackend {
    pub fn new(
        auth: Arc<AdmissionService>,
        pool: sqlx::PgPool,
        preflight: Arc<DispatchPreflight>,
        admission: Arc<dyn DispatchAdmission>,
        capacity: Arc<CapacityResolver>,
        providers: Arc<dyn crate::migration_agent::model::RuntimeProviderCredentials>,
        bindings: LlmBindings,
        routing: LlmRoutingBindings,
    ) -> Self {
        let persistence = Arc::new(CheckpointSettlingPersistence::new(
            Arc::new(crate::usage_recorder::PgUsagePersistence::new(pool.clone())),
            pool.clone(),
        ));
        Self {
            auth,
            pool,
            preflight,
            admission,
            capacity,
            providers,
            bindings,
            routing,
            persistence,
        }
    }
    async fn route(
        &self,
        actor: &Actor,
        invocation: &Invocation,
    ) -> LlmResult<Option<(routing::Decision, routing::QuoteWorkload)>> {
        let requested = invocation.payload["model"].as_str().unwrap_or("");
        let Some((binding, authority)) = self.routing.for_actor(actor, requested)? else {
            return Ok(None);
        };
        let rows = self.rows(actor).await?;
        // An alias never shadows an actual catalog model or provider/model.
        if requested.contains('/') || rows.iter().any(|r| r["model_id"] == requested) {
            return Err(LlmError::new(409, "llm_route_alias_collision"));
        }
        let service = routing::RoutingPolicyService::new(self.pool.clone());
        let policy = service
            .active(
                &authority,
                &binding.policy_id,
                binding.policy_pin.as_deref(),
            )
            .await
            .map_err(route_error)?;
        let mut providers: Vec<Value> = Vec::new();
        let mut workloads = BTreeMap::new();
        for rule in &policy.candidates {
            let exact = format!("{}/{}", rule.target.provider, rule.target.model);
            let Ok(model) = codec::select_model(actor, &exact, rows.clone()) else {
                continue;
            };
            let provider = if let Some(p) = providers
                .iter()
                .find(|p| p["provider_id"] == rule.target.provider)
            {
                (*p).clone()
            } else {
                let p = self
                    .providers
                    .runtime_provider(&actor.tenant, &rule.target.provider)
                    .await
                    .map_err(|_| unavailable())?;
                providers.push(p.clone());
                p
            };
            // Existing codec checks exact protocol, modalities, tools and each
            // model's real output ceiling before it can enter the candidate set.
            let Ok((body, output, _)) = codec::bind(invocation, &model, &provider) else {
                continue;
            };
            let request = UnifiedRequest::new(
                &invocation.request_id,
                self.bindings.service(invocation.surface),
                vec![ContentItem::text(
                    std::str::from_utf8(&body).map_err(|_| unavailable())?,
                )],
            );
            let input = crate::usage_parser::estimate_tokens(&crate::usage_parser::inputs_to_text(
                &request,
            ));
            workloads.insert(
                rule.target.clone(),
                routing::QuoteWorkload {
                    input_tokens: u64::try_from(input).map_err(|_| unavailable())?,
                    output_tokens: output,
                },
            );
        }
        let at_ms = chrono::Utc::now().timestamp_millis();
        let candidates = service
            .candidates_for_workloads(&authority, &policy, &providers, &workloads, at_ms)
            .await
            .map_err(route_error)?;
        let required = BTreeSet::from([format!("protocol:{}", invocation.surface.wire())]);
        use sha2::{Digest, Sha256};
        // Stable request content, not the random request/decision ID, salts the
        // operator seed so evaluation reruns select identically.
        let seed = format!("{}:{:x}", binding.seed, Sha256::digest(&invocation.body));
        let decision = service
            .decide(
                &authority,
                &policy,
                &seed,
                &invocation.request_id,
                at_ms,
                required,
                candidates,
            )
            .await
            .map_err(route_error)?;
        let workload = workloads
            .remove(&decision.selected)
            .ok_or_else(unavailable)?;
        Ok(Some((decision, workload)))
    }
    async fn rows(&self, actor: &Actor) -> LlmResult<Vec<Value>> {
        sqlx::query_scalar("SELECT to_jsonb(m)||jsonb_build_object('_provider_enabled',COALESCE(p.is_enabled,FALSE)) FROM gateway.llm_models m LEFT JOIN gateway.llm_providers p ON p.tenant_id=m.tenant_id AND p.provider_id=m.provider_id WHERE m.tenant_id=$1 ORDER BY m.provider_id,m.model_id").bind(&actor.tenant).fetch_all(&self.pool).await.map_err(|_|unavailable())
    }
}
impl Backend for NativeBackend {
    fn authenticate<'a>(&'a self, headers: &'a HeaderMap, path: &'a str) -> LlmFuture<'a, Actor> {
        Box::pin(async move {
            let headers = codec::credentials(headers, &self.bindings.api_key_header)?;
            let request = self.auth.request(&headers, path);
            let principal = request
                .resolve()
                .await
                .map_err(|e| LlmError::new(e.status(), "llm_authentication_required"))?;
            if principal.tenant_id().is_empty() || principal.user_id().is_empty() {
                return Err(LlmError::new(401, "llm_bound_identity_required"));
            }
            let current:Value=sqlx::query_scalar("SELECT jsonb_build_object('tenant',to_jsonb(t),'user',to_jsonb(u)) FROM gateway.users u JOIN gateway.tenants t ON t.tenant_id=u.tenant_id WHERE u.tenant_id=$1 AND u.user_id=$2 AND u.status='active' AND t.status='active'")
            .bind(principal.tenant_id()).bind(principal.user_id()).fetch_optional(&self.pool).await.map_err(|_|unavailable())?.ok_or(LlmError::new(403,"llm_actor_forbidden"))?;
            let key_services = principal
                .api_key()
                .map(|key| key.allowed_services.clone())
                .unwrap_or_default();
            Ok(Actor {
                routing: if self.routing.0.is_empty() {
                    None
                } else {
                    Some(Arc::new(
                        routing::RoutingAuthority::from_principal(principal)
                            .map_err(route_error)?,
                    ))
                },
                tenant: principal.tenant_id().into(),
                user: principal.user_id().into(),
                roles: principal.roles().to_vec(),
                tier: principal.tier().into(),
                allowed_models: principal
                    .api_key()
                    .map(|key| key.allowed_models.clone())
                    .unwrap_or_default(),
                constraints: crate::service_access_policy::ServiceAccessConstraints {
                    allowed_sources: vec![
                        ("api_key".into(), key_services),
                        (
                            "tenant".into(),
                            crate::service_access_policy::normalize_service_scope(
                                &current["tenant"]["allowed_services"],
                            ),
                        ),
                    ],
                    user_policy: crate::service_access_policy::service_access_policy_from_metadata(
                        &current["user"]["metadata"],
                    ),
                },
            })
        })
    }
    fn models<'a>(&'a self, actor: &'a Actor) -> LlmFuture<'a, Value> {
        Box::pin(async move {
            let mut data = vec![];
            for row in self.rows(actor).await? {
                if row["is_enabled"] != true
                    || row["_provider_enabled"] != true
                    || !codec::model_access(actor, &row)
                {
                    continue;
                }
                let Some(model) = row["model_id"].as_str() else {
                    continue;
                };
                let Some(provider) = row["provider_id"].as_str() else {
                    continue;
                };
                if !codec::allowed_model(actor, model, provider) {
                    continue;
                }
                let mut visible = false;
                for surface in [Surface::Chat, Surface::Embeddings, Surface::Messages] {
                    if !codec::profile(&row)["wire_protocols"]["supported"]
                        .as_array()
                        .is_some_and(|v| v.contains(&json!(surface.wire())))
                    {
                        continue;
                    }
                    let Some(service) = self
                        .preflight
                        .registry()
                        .get(self.bindings.service(surface))
                        .await
                        .map_err(|_| unavailable())?
                    else {
                        continue;
                    };
                    if crate::service_access_policy::registered_service_visible(
                        &service.snapshot(),
                        &actor.constraints,
                        &actor.roles,
                    ) {
                        visible = true;
                    }
                }
                if !visible {
                    continue;
                }
                let created = row["created_at"]
                    .as_str()
                    .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
                    .map(|t| t.timestamp())
                    .unwrap_or(0);
                data.push(json!({"id":format!("{provider}/{model}"),"object":"model","created":created,"owned_by":provider}));
            }
            Ok(json!({"object":"list","data":data}))
        })
    }
    fn prepare<'a>(
        &'a self,
        actor: &'a Actor,
        invocation: Invocation,
        incoming: &'a HeaderMap,
    ) -> LlmFuture<'a, Prepared> {
        Box::pin(async move {
            let guardrails = guardrails::GuardrailService::new(self.pool.clone())
                .inspect_input(
                    &actor.tenant,
                    &actor.user,
                    &invocation.request_id,
                    &invocation.body,
                )
                .await?;
            let route = self.route(actor, &invocation).await?;
            let selected = route
                .as_ref()
                .map(|(d, _)| format!("{}/{}", d.selected.provider, d.selected.model));
            let requested = selected
                .as_deref()
                .unwrap_or_else(|| invocation.payload["model"].as_str().unwrap_or(""));
            let model = codec::select_model(actor, requested, self.rows(actor).await?)?;
            let provider_id = model["provider_id"].as_str().ok_or_else(unavailable)?;
            let provider = self
                .providers
                .runtime_provider(&actor.tenant, provider_id)
                .await
                .map_err(|_| unavailable())?;
            let catalog = crate::provider_routing::RoutingSnapshot::new(
                std::slice::from_ref(&provider),
                std::slice::from_ref(&model),
            )
            .typed_catalog();
            catalog
                .select(
                    &actor.tenant,
                    &crate::provider_routing::ModelRef {
                        provider_id: provider_id.into(),
                        model_id: model["model_id"].as_str().ok_or_else(unavailable)?.into(),
                    },
                )
                .map_err(|_| LlmError::new(403, "llm_registered_route_denied"))?;
            let (body, output, profile) = codec::bind(&invocation, &model, &provider)?;
            let service_id = self.bindings.service(invocation.surface);
            let selected = self
                .preflight
                .registry()
                .get(service_id)
                .await
                .map_err(|_| unavailable())?
                .ok_or(LlmError::new(503, "llm_registered_service_missing"))?;
            if !crate::service_access_policy::registered_service_visible(
                &selected.snapshot(),
                &actor.constraints,
                &actor.roles,
            ) {
                return Err(LlmError::new(403, "llm_service_forbidden"));
            }
            let mut request = UnifiedRequest::new(
                &invocation.request_id,
                service_id,
                vec![ContentItem::text(
                    std::str::from_utf8(&body)
                        .map_err(|_| LlmError::new(400, "llm_json_invalid"))?,
                )],
            );
            request.parameters = Some(serde_json::Map::from_iter([(
                "max_output_tokens".into(),
                json!(output),
            )]));
            let ready = self
                .preflight
                .prepare(
                    request,
                    &PreflightAuthority::from_resolved_context(
                        &actor.user,
                        &actor.tenant,
                        &actor.roles,
                    ),
                    None,
                    if invocation.stream {
                        PreflightMode::Stream
                    } else {
                        PreflightMode::Invoke
                    },
                )
                .await
                .map_err(preflight_error)?;
            let mut service = ready
                .service
                .snapshot()
                .dispatch_view()
                .map_err(|_| unavailable())?;
            service.policy =
                crate::dispatcher::DispatchPolicy::new("POST", 1, service.policy.timeout);
            let timeout = service.policy.timeout;
            let budgets = self
                .capacity
                .resolve(&crate::capacity::CapacityRequest {
                    tenant_id: actor.tenant.clone(),
                    service_id: service_id.into(),
                    request_class: if invocation.stream { "stream" } else { "sync" }.into(),
                    resource_kind: "provider".into(),
                    provider_id: json!(provider_id),
                    upstream_group: Value::Null,
                    is_admin_read: false,
                    service_config: service.capacity_config.clone(),
                })
                .map_err(|_| unavailable())?;
            if let Some((_, w)) = &route {
                let measured = crate::usage_parser::estimate_tokens(
                    &crate::usage_parser::inputs_to_text(&ready.request),
                );
                if u64::try_from(measured).ok() != Some(w.input_tokens) || output != w.output_tokens
                {
                    return Err(LlmError::new(409, "llm_route_workload_changed"));
                }
            }
            let mut record = UsageRecord::new(
                &actor.tenant,
                &actor.user,
                model["model_id"].as_str().unwrap_or(""),
                chrono::Utc::now(),
            );
            if let Some((d, _)) = &route {
                record.timestamp = chrono::DateTime::from_timestamp_millis(d.context.at_ms)
                    .ok_or_else(unavailable)?;
                record
                    .metadata
                    .insert("route_decision_id".into(), json!(d.id));
                record
                    .metadata
                    .insert("routing_policy_pin".into(), json!(d.context.policy_pin));
            }
            if let Some(g) = &guardrails {
                record.metadata.insert(
                    "guardrails_input".into(),
                    serde_json::to_value(&g.input).map_err(|_| unavailable())?,
                );
            }
            record.provider = provider_id.into();
            record.request_id = invocation.request_id.clone();
            record.service_id = service_id.into();
            record.request_type = if invocation.stream {
                "stream"
            } else {
                "invoke"
            }
            .into();
            let pricing = PgPriceBookService::new(self.pool.clone())
                .freeze_usage(&record)
                .await
                .map_err(|_| unavailable())?;
            if ai_platform_gateway_foundation::request_meter::is_snapshot(&pricing) {
                return Err(LlmError::new(409, "llm_token_meter_required"));
            }
            let route_decision = if let Some((d, w)) = &route {
                routing::pricing_matches_decision(d, &pricing, w).map_err(route_error)?;
                Some(routing::decision_header(d).map_err(route_error)?)
            } else {
                None
            };
            let (url, headers) =
                endpoint_headers(invocation.surface, &provider, &profile, incoming)?;
            let context = DispatchContext {
                request_id: invocation.request_id.clone(),
                tenant_id: actor.tenant.clone(),
                user_id: actor.user.clone(),
            };
            let lease = self
                .admission
                .acquire_for_request(
                    &context,
                    &service,
                    if invocation.stream { "stream" } else { "sync" },
                    &budgets,
                    Some(&ready.request),
                    &uuid::Uuid::new_v4().to_string(),
                )
                .await
                .map_err(admission_error)?;
            // Public APIs must never silently use the legacy capacity-only fallback.
            if lease.quota_admission().is_none() {
                let _ = lease
                    .mark_completion(DispatchSettlement::NotDispatched)
                    .await;
                let _ = lease.release().await;
                return Err(unavailable());
            }
            Ok(Prepared {
                invocation,
                url,
                headers,
                body,
                timeout,
                record,
                pricing,
                route_decision,
                guardrails,
                lease,
            })
        })
    }
    fn settle<'a>(
        &'a self,
        prepared: &'a Prepared,
        observed: meter::Observation,
        status: u16,
    ) -> LlmFuture<'a, ()> {
        Box::pin(async move {
            let mut record = prepared.record.clone();
            if let Some(report) = prepared.guardrails.as_ref().and_then(|g| g.output_report()) {
                record.metadata.insert(
                    "guardrails_output".into(),
                    serde_json::to_value(report).map_err(|_| unavailable())?,
                );
            }

            record.input_tokens = observed.input;
            record.output_tokens = observed.output;
            record.status = if (200..300).contains(&status) {
                "success"
            } else {
                "error"
            }
            .into();
            record.error_type = if status >= 400 {
                Some(format!("provider_http_{status}"))
            } else {
                None
            };
            record.metadata.insert(
                "provider_cache_usage".into(),
                serde_json::to_value(observed.cache).map_err(|_| unavailable())?,
            );
            record.metadata.insert(
                "llm_surface".into(),
                json!(prepared.invocation.surface.wire()),
            );
            crate::usage_recorder::prepare_with_snapshot(&mut record, &prepared.pricing)
                .map_err(|_| unavailable())?;
            // Cumulative checkpoints bind frozen terms and suppress equal replays.
            let mut accounting = record.clone();
            accounting
                .metadata
                .insert("_price_book_v2_cumulative".into(), json!(record));
            self.persistence
                .upsert_raw(&[accounting.clone()])
                .await
                .map_err(|_| unavailable())?;
            for stage in [
                AccountingStage::QuotaCounters,
                AccountingStage::DailyAggregates,
                AccountingStage::HourlyAggregates,
            ] {
                if self
                    .persistence
                    .post_write(stage, &[accounting.clone()])
                    .await
                    .map_err(|_| unavailable())?
                    != StageStatus::Applied
                {
                    return Err(unavailable());
                }
            }
            Ok(())
        })
    }
}
fn endpoint_headers(
    surface: Surface,
    provider: &Value,
    profile: &Value,
    incoming: &HeaderMap,
) -> LlmResult<(reqwest::Url, HeaderMap)> {
    let base = provider["runtime_base_url"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(unavailable)?;
    let mut url = reqwest::Url::parse(base).map_err(|_| unavailable())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(unavailable());
    }
    let suffix = surface.path().trim_start_matches("/v1/");
    let path = url.path().trim_end_matches('/');
    let path = if path.ends_with(suffix) {
        path.to_owned()
    } else {
        format!("{}/{suffix}", if path.is_empty() { "/v1" } else { path })
    };
    url.set_path(&path);
    let key = provider["api_key"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(unavailable)?;
    let mut headers = HeaderMap::new();
    for (name, value) in incoming {
        if matches!(
            name.as_str(),
            "anthropic-version" | "anthropic-beta" | "openai-beta" | "idempotency-key"
        ) {
            headers.append(name.clone(), value.clone());
        }
    }
    headers.insert("content-type", "application/json".parse().unwrap());
    headers.insert("accept-encoding", "identity".parse().unwrap());
    if surface == Surface::Messages {
        headers.insert("x-api-key", key.parse().map_err(|_| unavailable())?);
        if !headers.contains_key("anthropic-version") {
            headers.insert("anthropic-version", "2023-06-01".parse().unwrap());
        }
    } else {
        for (name, value) in
            crate::provider_routing::provider_headers(profile, key).into_internal_headers()
        {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).map_err(|_| unavailable())?,
                value.parse().map_err(|_| unavailable())?,
            );
        }
    }
    Ok((url, headers))
}

fn route_error(e: routing::RoutingError) -> LlmError {
    let status = match e {
        routing::RoutingError::Forbidden => 403,
        routing::RoutingError::NoCandidate
        | routing::RoutingError::Conflict
        | routing::RoutingError::PinMismatch => 409,
        routing::RoutingError::NotFound => 503,
        routing::RoutingError::InvalidPolicy
        | routing::RoutingError::InvalidSnapshot
        | routing::RoutingError::Storage
        | routing::RoutingError::UncertainCommit => 503,
    };
    LlmError::new(status, "llm_route_policy_denied")
}
