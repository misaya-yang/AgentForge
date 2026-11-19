//! Durable reservation/dispatch/receipt authority; unknown calls retain budgets.
use super::*;
use crate::usage_recorder::{UsageRecord, prepare_with_snapshot};
use ai_platform_gateway_foundation::price_book;
impl KnowledgeModelPlane {
    pub(super) async fn authorization_snapshot(&self, call: &ModelCall) -> PlaneResult<Value> {
        let mut row:Value=sqlx::query_scalar("SELECT jsonb_build_object('model',to_jsonb(m),'provider',to_jsonb(p)) FROM gateway.llm_models m JOIN gateway.llm_providers p ON p.tenant_id=m.tenant_id AND p.provider_id=m.provider_id WHERE m.tenant_id=$1 AND m.provider_id=$2 AND m.model_id=$3")
   .bind(&call.scope.actor.tenant_id).bind(&call.model.provider).bind(&call.model.model).fetch_optional(&self.pool).await?.ok_or(PlaneError::new(403,"knowledge_model_not_registered"))?;
        let runtime = self
            .providers
            .runtime_provider(&call.scope.actor.tenant_id, &call.model.provider)
            .await
            .map_err(|_| PlaneError::new(503, "knowledge_provider_credentials_unavailable"))?;
        let provider_revision = runtime["updated_at"].clone();
        let no_auth = ai_platform_gateway_foundation::request_meter::credentialless_local(
            &row["model"],
            &runtime,
        );
        let rows = [runtime];
        let models = [row["model"].clone()];
        let mut catalog =
            crate::provider_routing::RoutingSnapshot::new(&rows, &models).typed_catalog();
        if no_auth {
            for provider in &mut catalog.providers {
                if provider.tenant_id == call.scope.actor.tenant_id
                    && provider.provider_id == call.model.provider
                {
                    provider.credentials =
                        crate::provider_routing::CredentialAvailability::NotRequired;
                }
            }
        }
        catalog
            .select(
                &call.scope.actor.tenant_id,
                &crate::provider_routing::ModelRef {
                    provider_id: call.model.provider.clone(),
                    model_id: call.model.model.clone(),
                },
            )
            .map_err(|_| PlaneError::new(403, "knowledge_model_route_denied"))?;
        row["provider"] = json!({"tenant_id":call.scope.actor.tenant_id,"provider_id":call.model.provider,"updated_at":provider_revision});
        Ok(row)
    }
    /// New reservations/dispatch require the selected revision. Completed spend
    /// uses its durable pricing receipt independently of subsequent publications.
    pub(super) async fn model_snapshot(&self, call: &ModelCall) -> PlaneResult<Value> {
        let row = self.authorization_snapshot(call).await?;
        let route:Option<(String,String)>=sqlx::query_as("SELECT model_version,book_version FROM gateway.price_book_v2_routes WHERE tenant_id=$1 AND provider_id=$2 AND model_id=$3 AND currency='USD'").bind(&call.scope.actor.tenant_id).bind(&call.model.provider).bind(&call.model.model).fetch_optional(&self.pool).await?;
        if route.as_ref()
            != Some(&(
                call.model.revision.clone(),
                self.price_book_revision.clone(),
            ))
        {
            return Err(PlaneError::new(409, "knowledge_model_revision_changed"));
        }
        Ok(row)
    }
    pub(super) async fn pricing(&self, call: &ModelCall) -> PlaneResult<Value> {
        let mut record = base_record(call);
        let snapshot = self.model_snapshot(call).await?;
        if snapshot["model"]["metadata"]["billing_meter"] == "requests" {
            ai_platform_gateway_foundation::request_meter::set_requests(&mut record, 1);
        }

        let snapshot = price_book::service::PgPriceBookService::new(self.pool.clone())
            .freeze_usage(&record)
            .await
            .map_err(|_| PlaneError::new(503, "knowledge_price_book_unavailable"))?;
        let key = if ai_platform_gateway_foundation::request_meter::is_snapshot(&snapshot) {
            &snapshot["price_book_request"]["entry"]["key"]
        } else {
            &snapshot["price_book_input"]["entry"]["key"]
        };
        if key["book_version"] != self.price_book_revision
            || key["model_version"] != call.model.revision
        {
            return Err(PlaneError::new(
                409,
                "knowledge_model_price_revision_changed",
            ));
        }
        Ok(snapshot)
    }
    pub(super) async fn lookup(&self, call: &ModelCall) -> PlaneResult<Option<Value>> {
        Ok(sqlx::query_scalar("SELECT to_jsonb(c) FROM gateway.knowledge_model_calls c WHERE tenant_id=$1 AND user_id=$2 AND call_id=$3")
   .bind(&call.scope.actor.tenant_id).bind(&call.scope.actor.user_id).bind(&call.call_id).fetch_optional(&self.pool).await?)
    }
    pub(super) async fn reserve_row(
        &self,
        request: &ReserveRequest,
        lease: &ModelLease,
        snapshot: &Value,
        pricing: &Value,
    ) -> PlaneResult<ModelLease> {
        // UTF-8 bytes are a conservative token ceiling; no bytes/4 estimate grants
        // a quota bypass. Actual provider usage is mandatory for settlement.
        let request_meter = ai_platform_gateway_foundation::request_meter::is_snapshot(pricing);
        let input = i64::try_from(if request_meter {
            0
        } else {
            request.call.budget.input_bytes
        })
        .map_err(|_| PlaneError::new(422, "knowledge_budget_overflow"))?;
        let output = i64::try_from(if request_meter {
            0
        } else {
            request.call.budget.max_output_tokens
        })
        .map_err(|_| PlaneError::new(422, "knowledge_budget_overflow"))?;
        let record = if request_meter {
            priced_request(&request.call, pricing, 1)?
        } else {
            priced(&request.call, pricing, input, output)?
        };
        let mut max_input_cost = record.input_cost_cents;
        for field in ["price_book_cached_input", "price_book_cache_write"] {
            if let Some(value) = pricing.get(field) {
                let term: price_book::PriceReceipt = serde_json::from_value(value.clone())
                    .map_err(|_| PlaneError::new(503, "knowledge_model_price_invalid"))?;
                let nanos = term
                    .quote_nanos(input as u64)
                    .map_err(|_| PlaneError::new(422, "knowledge_budget_overflow"))?;
                let microcents = i64::try_from(nanos / 1000 + u64::from(nanos % 1000 != 0))
                    .map_err(|_| PlaneError::new(422, "knowledge_budget_overflow"))?;
                max_input_cost = max_input_cost.max(microcents);
            }
        }
        let cost = max_input_cost
            .checked_add(record.output_cost_cents)
            .ok_or(PlaneError::new(422, "knowledge_budget_overflow"))?;
        let row:Value=sqlx::query_scalar("SELECT gateway.reserve_knowledge_model_call($1,$2,$3,$4,$5::uuid,$6,$7,$8,$9,$10,$11,$12)")
   .bind(&lease.tenant_id).bind(&lease.user_id).bind(&lease.call_id).bind(&lease.request_sha256).bind(&lease.lease_id).bind(serde_json::to_value(lease).map_err(|_|PlaneError::new(503,"knowledge_lease_encode"))?).bind(serde_json::to_value(&request.call).map_err(|_|PlaneError::new(422,"knowledge_call_encode"))?).bind(snapshot).bind(pricing).bind(input).bind(output).bind(cost).fetch_one(&self.pool).await.map_err(authority_error)?;
        existing_lease(&row, &request.request_sha256)
    }
    pub(super) async fn claim(&self, lease: &ModelLease) -> PlaneResult<bool> {
        Ok(sqlx::query("UPDATE gateway.knowledge_model_calls SET state='dispatched',dispatched_at=NOW() WHERE lease_id=$1::uuid AND tenant_id=$2 AND user_id=$3 AND request_sha256=$4 AND state='reserved' AND expires_at>NOW()")
   .bind(&lease.lease_id).bind(&lease.tenant_id).bind(&lease.user_id).bind(&lease.request_sha256).execute(&self.pool).await?.rows_affected()==1)
    }
    pub(super) async fn mark_unknown(&self, lease: &ModelLease, code: &str) -> PlaneResult<()> {
        sqlx::query("UPDATE gateway.knowledge_model_calls SET state='unknown',error_code=$2 WHERE lease_id=$1::uuid AND state IN ('dispatched','observed')").bind(&lease.lease_id).bind(code).execute(&self.pool).await?;
        Ok(())
    }
    pub(super) async fn settle(
        &self,
        call: &ModelCall,
        lease: &ModelLease,
        pricing: &Value,
        response: provider::ProviderResult,
    ) -> PlaneResult<ModelExecution> {
        let revision = replay::frozen_revision(pricing)?;
        let observed = serde_json::to_value(&response)
            .map_err(|_| PlaneError::new(503, "knowledge_observation_encode"))?;
        sqlx::query("UPDATE gateway.knowledge_model_calls SET observation=COALESCE(observation,$2),state=CASE WHEN state='dispatched' THEN 'observed' ELSE state END WHERE lease_id=$1::uuid AND state IN ('dispatched','observed','unknown')").bind(&lease.lease_id).bind(observed).execute(&self.pool).await?;
        let request_priced = ai_platform_gateway_foundation::request_meter::is_snapshot(pricing);
        let (mut record, units, input, output) = if request_priced {
            let Some(requests) = response.requests else {
                self.mark_unknown(lease, "request_invocation_outcome_unknown")
                    .await?;
                let mut execution = unknown(lease, revision);
                execution.upstream_status = response.status;
                execution.output = response.output;
                execution.receipt.metering = Some(ModelMetering {
                    kind: MeterKind::Requests,
                    tokens: TokenCountState::Unknown,
                    cost_nanos: None,
                });
                return Ok(execution);
            };
            (
                priced_request(call, pricing, requests)?,
                std::collections::BTreeMap::from([("requests".into(), requests)]),
                0,
                0,
            )
        } else {
            let Some((input, output)) = response.usage else {
                self.mark_unknown(lease, "provider_usage_missing").await?;
                let mut execution = unknown(lease, revision);
                execution.upstream_status = response.status;
                execution.output = response.output;
                return Ok(execution);
            };
            let mut record = priced(call, pricing, input, output)?;
            let cache = crate::usage_parser::cache::CacheUsage::observe(
                &call.model.provider,
                &response.cache,
            );
            record.metadata.insert(
                "provider_cache_usage".into(),
                serde_json::to_value(&cache)
                    .map_err(|_| PlaneError::new(502, "knowledge_provider_usage_invalid"))?,
            );
            prepare_with_snapshot(&mut record, pricing)
                .map_err(|_| PlaneError::new(503, "knowledge_model_settlement_price_invalid"))?;
            let mut units = std::collections::BTreeMap::from([
                ("input_tokens".into(), input as u64),
                ("output_tokens".into(), output as u64),
            ]);
            for (name, count) in [
                ("cached_input_tokens", &cache.read),
                ("cache_write_tokens", &cache.write),
            ] {
                if let crate::usage_parser::cache::Count::Reported(tokens) = count {
                    units.insert(name.into(), *tokens as u64);
                }
            }
            (record, units, input, output)
        };
        if response.status >= 400 {
            record.status = "error".into();
            record.error_type = Some(format!("provider_http_{}", response.status));
        }
        let mut execution = ModelExecution {
            call_id: lease.call_id.clone(),
            lease_id: lease.lease_id.clone(),
            request_sha256: lease.request_sha256.clone(),
            state: if response.status < 400 {
                ExecutionState::Completed
            } else {
                ExecutionState::Rejected
            },
            upstream_status: response.status,
            output: response.output,
            receipt: UsageReceipt {
                receipt_id: lease.lease_id.clone(),
                call_id: lease.call_id.clone(),
                lease_id: lease.lease_id.clone(),
                tenant_id: lease.tenant_id.clone(),
                user_id: lease.user_id.clone(),
                provider_id: lease.model.provider.clone(),
                model: lease.model.clone(),
                price_book_revision: revision.into(),
                units,
                settled: true,
                metering: Some(ModelMetering {
                    kind: if request_priced {
                        MeterKind::Requests
                    } else {
                        MeterKind::Tokens
                    },
                    tokens: if request_priced {
                        TokenCountState::NotApplicable
                    } else {
                        TokenCountState::Reported
                    },
                    cost_nanos: if request_priced {
                        record
                            .metadata
                            .get("request_cost_nanos")
                            .and_then(Value::as_u64)
                    } else {
                        None
                    },
                }),
            },
        };
        let value = serde_json::to_value(&execution)
            .map_err(|_| PlaneError::new(503, "knowledge_receipt_encode"))?;
        let mut transaction = self.pool.begin().await?;
        let persisted: Value = sqlx::query_scalar(
            "SELECT gateway.settle_knowledge_model_call($1::uuid,$2,$3,$4,$5,$6,$7,$8,$9)",
        )
        .bind(&lease.lease_id)
        .bind(&lease.tenant_id)
        .bind(&lease.user_id)
        .bind(input)
        .bind(output)
        .bind(record.input_cost_cents)
        .bind(record.output_cost_cents)
        .bind(&value)
        .bind(
            serde_json::to_value(&record)
                .map_err(|_| PlaneError::new(503, "knowledge_accounting_encode"))?,
        )
        .fetch_one(&mut *transaction)
        .await
        .map_err(authority_error)?;
        let observed = ai_platform_gateway_foundation::knowledge_model_accounting::settle_observed_in_transaction(
            &mut transaction,
            &record,
        )
        .await
        .map_err(|_| PlaneError::new(503, "knowledge_accounting_unavailable"))?;
        transaction
            .commit()
            .await
            .map_err(|_| PlaneError::new(503, "knowledge_settlement_commit_unknown"))?;
        for record in &observed {
            ai_platform_gateway_foundation::usage_recorder::settled_metrics::record(record);
        }
        execution = serde_json::from_value(persisted)
            .map_err(|_| PlaneError::new(503, "knowledge_receipt_corrupt"))?;
        Ok(execution)
    }
}
fn priced(call: &ModelCall, snapshot: &Value, input: i64, output: i64) -> PlaneResult<UsageRecord> {
    if input < 0 || output < 0 {
        return Err(PlaneError::new(502, "knowledge_provider_usage_invalid"));
    }
    let mut record = base_record(call);
    record.input_tokens = input;
    record.output_tokens = output;
    prepare_with_snapshot(&mut record, snapshot)
        .map_err(|_| PlaneError::new(503, "knowledge_model_settlement_price_invalid"))?;
    Ok(record)
}
fn authority_error(error: sqlx::Error) -> PlaneError {
    let code = error.as_database_error().map(|e| e.message()).unwrap_or("");
    if code.contains("KNOWLEDGE_MODEL_BODY_CONFLICT") {
        PlaneError::new(409, "knowledge_model_idempotency_conflict")
    } else if code.contains("KNOWLEDGE_MODEL_QUOTA") {
        PlaneError::new(429, "knowledge_model_quota_exhausted")
    } else {
        PlaneError::new(503, "knowledge_model_authority_unavailable")
    }
}

fn base_record(call: &ModelCall) -> UsageRecord {
    let mut record = UsageRecord::new(
        &call.scope.actor.tenant_id,
        &call.scope.actor.user_id,
        &call.model.model,
        chrono::Utc::now(),
    );
    record.provider = call.model.provider.clone();
    record.request_id = hash(
        &serde_json::to_vec(&(
            &call.scope.actor.tenant_id,
            &call.scope.actor.user_id,
            &call.call_id,
        ))
        .expect("strings serialize"),
    );
    record.service_id = "knowledge-model-plane".into();
    record.request_type = "knowledge_model".into();
    record
}

fn priced_request(call: &ModelCall, snapshot: &Value, requests: u64) -> PlaneResult<UsageRecord> {
    let mut record = base_record(call);
    ai_platform_gateway_foundation::request_meter::set_requests(&mut record, requests);
    prepare_with_snapshot(&mut record, snapshot)
        .map_err(|_| PlaneError::new(503, "knowledge_request_price_invalid"))?;
    Ok(record)
}
