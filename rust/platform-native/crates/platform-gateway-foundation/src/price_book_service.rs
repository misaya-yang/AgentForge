//! Persistent v2 management and first-receipt authority. Append-only publications
//! are immutable snapshots of terms, not a view of today's mutable catalog.
//! Every data query binds authenticated tenant and exact provider/model keys.
use super::*;
use crate::{
    pricing_receipt::validate_pricing_snapshot,
    usage_recorder::{UsageFuture, UsageRecord, UsageRecorderError, UsageSnapshotResolver},
};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Row};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publication {
    pub tenant: String,
    pub book_version: String,
    pub provider: String,
    pub model: String,
    pub model_version: String,
    pub currency: String,
    pub entries: Vec<PriceEntry>,
    pub discounts: Vec<TenantDiscount>,
}
impl Publication {
    pub fn validate(&self, tenant: &str, tenant_only: bool) -> Result<PriceBook> {
        if self.tenant != tenant
            || !identity(tenant)
            || self.entries.is_empty()
            || self.entries.len() > 1024
            || self.discounts.len() > 1024
        {
            return Err(PriceBookError::Forbidden);
        }
        let matches = |key: &PriceKey| {
            key.book_version == self.book_version
                && key.provider == self.provider
                && key.model == self.model
                && key.model_version == self.model_version
                && key.currency == self.currency
        };
        if self.entries.iter().any(|entry| {
            !matches(&entry.key)
                || entry.tenant.as_deref().is_some_and(|t| t != tenant)
                || (tenant_only && entry.tenant.as_deref() != Some(tenant))
        }) || self
            .discounts
            .iter()
            .any(|d| !matches(&d.key) || d.tenant != tenant)
        {
            return Err(PriceBookError::Forbidden);
        }
        PriceBook::new(self.entries.clone(), self.discounts.clone())
    }
    pub fn reference(&self) -> Result<String> {
        self.validate(&self.tenant, false)?;
        digest("publication", self)
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Activation {
    pub provider: String,
    pub model: String,
    pub model_version: String,
    pub book_version: String,
    pub currency: String,
    /// None creates the first binding. Some performs an exact compare-and-swap.
    pub expected_revision: Option<i64>,
}
#[derive(Clone)]
pub struct PgPriceBookService {
    pool: PgPool,
}
impl PgPriceBookService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
    /// Deduplicated raw records, scoped by authenticated tenant and bounded time.
    /// Unknown observations are counted separately and excluded from hit ratios.
    pub async fn cache_metrics(
        &self,
        tenant: &str,
        from_ms: i64,
        to_ms: i64,
    ) -> Result<Vec<Value>> {
        if !identity(tenant)
            || from_ms >= to_ms
            || to_ms
                .checked_sub(from_ms)
                .is_none_or(|n| n > 31 * 86_400_000)
        {
            return Err(PriceBookError::InvalidTerms);
        }
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| PriceBookError::Storage)?;
        let sql=ai_platform_service_primitives::data_connection::append_store::pruning_sql(&mut connection,"gateway.usage_records","SELECT provider,model,input_tokens,metadata->'provider_cache_usage' AS cache FROM usage_records WHERE COALESCE(metadata->'measured_units'->>'meter','tokens')<>'requests' AND tenant_id=$1 AND created_at>=to_timestamp($2::double precision/1000) AND created_at<to_timestamp($3::double precision/1000)").await.map_err(|_|PriceBookError::Storage)?;
        let rows = sqlx::query(&sql)
            .bind(tenant)
            .bind(from_ms)
            .bind(to_ms)
            .fetch_all(&mut *connection)
            .await
            .map_err(|_| PriceBookError::Storage)?;
        let mut groups =
            std::collections::BTreeMap::<(String, String), (i64, i64, i64, i64)>::new();
        for row in rows {
            let provider: String = row
                .try_get("provider")
                .map_err(|_| PriceBookError::Decode)?;
            let model: String = row.try_get("model").map_err(|_| PriceBookError::Decode)?;
            let group = groups.entry((provider.clone(), model)).or_default();
            let raw: Option<Value> = row.try_get("cache").map_err(|_| PriceBookError::Decode)?;
            let observation = raw.and_then(|v| {
                serde_json::from_value::<crate::usage_parser::cache::CacheUsage>(v).ok()
            });
            let input: i64 = row
                .try_get("input_tokens")
                .map_err(|_| PriceBookError::Decode)?;
            let known = observation
                .as_ref()
                .filter(|v| matches!(v.read, crate::usage_parser::cache::Count::Reported(_)))
                .and_then(|v| v.partition(&provider, input).ok());
            if let Some((plain, read, write)) = known {
                for (target, value) in [
                    (&mut group.0, read),
                    (
                        &mut group.1,
                        plain
                            .checked_add(read)
                            .and_then(|n| n.checked_add(write))
                            .ok_or(PriceBookError::Arithmetic)?,
                    ),
                    (&mut group.2, 1),
                ] {
                    *target = target
                        .checked_add(value)
                        .ok_or(PriceBookError::Arithmetic)?;
                }
            } else {
                group.3 = group.3.checked_add(1).ok_or(PriceBookError::Arithmetic)?;
            }
        }
        Ok(groups.into_iter().map(|((provider,model),(read,total,reported,unknown))|serde_json::json!({"provider":provider,"model":model,"cached_read_tokens":read,"reported_input_tokens":total,"reported_requests":reported,"unknown_requests":unknown,"token_hit_ratio":if total>0{Some(read as f64/total as f64)}else{None}})).collect())
    }
    pub async fn list_publications(&self, tenant: &str) -> Result<Vec<Value>> {
        if !identity(tenant) {
            return Err(PriceBookError::InvalidTerms);
        }
        let rows=sqlx::query("SELECT reference,payload FROM gateway.price_book_v2_publications WHERE tenant_id=$1 ORDER BY provider_id,model_id,model_version,book_version,currency").bind(tenant).fetch_all(&self.pool).await.map_err(|_|PriceBookError::Storage)?;
        rows.into_iter()
            .map(|row| {
                let payload: Value = row.try_get("payload").map_err(|_| PriceBookError::Decode)?;
                let reference: String = row
                    .try_get("reference")
                    .map_err(|_| PriceBookError::Decode)?;
                let publication: Publication =
                    serde_json::from_value(payload.clone()).map_err(|_| PriceBookError::Decode)?;
                publication.validate(tenant, false)?;
                if publication.reference()? != reference {
                    return Err(PriceBookError::InvalidReference);
                }
                Ok(serde_json::json!({"reference":reference,"publication":payload}))
            })
            .collect()
    }
    pub async fn publish(
        &self,
        tenant: &str,
        actor: &str,
        publication: Publication,
    ) -> Result<String> {
        self.persist_publication(tenant, actor, publication, true)
            .await
    }
    /// Trusted operator-only promotion of already audited legacy base terms.
    /// This entrypoint is deliberately not exposed by the tenant HTTP router.
    pub async fn publish_imported(
        &self,
        tenant: &str,
        actor: &str,
        publication: Publication,
    ) -> Result<String> {
        publication.validate(tenant, false)?;
        for entry in publication.entries.iter().filter(|e| e.tenant.is_none()) {
            let audited:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM gateway.price_book_v2_import_audit WHERE disposition='attributed' AND (input_reference=$1 OR output_reference=$1))").bind(entry.reference()?).fetch_one(&self.pool).await.map_err(|_|PriceBookError::Storage)?;
            if !audited {
                return Err(PriceBookError::Forbidden);
            }
        }
        self.persist_publication(tenant, actor, publication, false)
            .await
    }
    async fn persist_publication(
        &self,
        tenant: &str,
        actor: &str,
        publication: Publication,
        tenant_only: bool,
    ) -> Result<String> {
        publication.validate(tenant, tenant_only)?;
        if !identity(actor) {
            return Err(PriceBookError::Forbidden);
        }
        let reference = publication.reference()?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| PriceBookError::Storage)?;
        // Same-name models from another provider/tenant never authorize this write.
        let known:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM llm_models WHERE tenant_id=$1 AND provider_id=$2 AND model_id=$3)")
            .bind(tenant).bind(&publication.provider).bind(&publication.model).fetch_one(&mut *tx).await.map_err(|_|PriceBookError::Storage)?;
        if !known {
            return Err(PriceBookError::UnknownModel);
        }
        for entry in &publication.entries {
            let payload = serde_json::to_value(entry).map_err(|_| PriceBookError::Decode)?;
            let stored:Option<Value>=sqlx::query_scalar("INSERT INTO gateway.price_book_v2_terms(reference,kind,payload) VALUES($1,'entry',$2) ON CONFLICT(reference) DO NOTHING RETURNING payload")
                .bind(entry.reference()?).bind(&payload).fetch_optional(&mut *tx).await.map_err(|_|PriceBookError::Storage)?;
            if stored.is_none() {
                let existing: Value = sqlx::query_scalar(
                    "SELECT payload FROM gateway.price_book_v2_terms WHERE reference=$1",
                )
                .bind(entry.reference()?)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| PriceBookError::Storage)?;
                if existing != payload {
                    return Err(PriceBookError::InvalidReference);
                }
            }
        }
        for discount in &publication.discounts {
            let payload = serde_json::to_value(discount).map_err(|_| PriceBookError::Decode)?;
            let stored:Option<Value>=sqlx::query_scalar("INSERT INTO gateway.price_book_v2_terms(reference,kind,payload) VALUES($1,'discount',$2) ON CONFLICT(reference) DO NOTHING RETURNING payload")
                .bind(discount.reference()?).bind(&payload).fetch_optional(&mut *tx).await.map_err(|_|PriceBookError::Storage)?;
            if stored.is_none() {
                let existing: Value = sqlx::query_scalar(
                    "SELECT payload FROM gateway.price_book_v2_terms WHERE reference=$1",
                )
                .bind(discount.reference()?)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| PriceBookError::Storage)?;
                if existing != payload {
                    return Err(PriceBookError::InvalidReference);
                }
            }
        }
        let payload = serde_json::to_value(&publication).map_err(|_| PriceBookError::Decode)?;
        let inserted:Option<Value>=sqlx::query_scalar("INSERT INTO gateway.price_book_v2_publications(tenant_id,provider_id,model_id,model_version,book_version,currency,reference,payload,created_by) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT(tenant_id,provider_id,model_id,model_version,book_version,currency) DO NOTHING RETURNING payload")
            .bind(tenant).bind(&publication.provider).bind(&publication.model).bind(&publication.model_version).bind(&publication.book_version).bind(&publication.currency).bind(&reference).bind(&payload).bind(actor).fetch_optional(&mut *tx).await.map_err(|_|PriceBookError::Storage)?;
        if inserted.is_none() {
            let existing:Value=sqlx::query_scalar("SELECT payload FROM gateway.price_book_v2_publications WHERE tenant_id=$1 AND provider_id=$2 AND model_id=$3 AND model_version=$4 AND book_version=$5 AND currency=$6").bind(tenant).bind(&publication.provider).bind(&publication.model).bind(&publication.model_version).bind(&publication.book_version).bind(&publication.currency).fetch_one(&mut *tx).await.map_err(|_|PriceBookError::Storage)?;
            if existing != payload {
                return Err(PriceBookError::RevisionConflict);
            }
        }
        tx.commit()
            .await
            .map_err(|_| PriceBookError::CommitUncertain)?;
        Ok(reference)
    }
    pub async fn activate(
        &self,
        tenant: &str,
        actor: &str,
        activation: &Activation,
        at_ms: i64,
    ) -> Result<i64> {
        if !identity(tenant)
            || !identity(actor)
            || activation.expected_revision.is_some_and(|r| r < 1)
        {
            return Err(PriceBookError::InvalidTerms);
        }
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| PriceBookError::Storage)?;
        let payload:Value=sqlx::query_scalar("SELECT payload FROM gateway.price_book_v2_publications WHERE tenant_id=$1 AND provider_id=$2 AND model_id=$3 AND model_version=$4 AND book_version=$5 AND currency=$6")
            .bind(tenant).bind(&activation.provider).bind(&activation.model).bind(&activation.model_version).bind(&activation.book_version).bind(&activation.currency).fetch_optional(&mut *tx).await.map_err(|_|PriceBookError::Storage)?.ok_or(PriceBookError::MissingPrice)?;
        let publication: Publication =
            serde_json::from_value(payload).map_err(|_| PriceBookError::Decode)?;
        let book = publication.validate(tenant, false)?;
        // Activation for legacy USD token billing requires both complete terms now.
        if activation.currency == "USD"
            && publication
                .entries
                .iter()
                .any(|e| matches!(e.key.component, Component::Input | Component::Output))
        {
            token_pair(&book, tenant, activation, at_ms)?;
        }
        if publication
            .entries
            .iter()
            .any(|e| e.key.component == Component::Request)
        {
            crate::request_meter::snapshot(&book.resolve(
                tenant,
                &PriceKey {
                    book_version: activation.book_version.clone(),
                    provider: activation.provider.clone(),
                    model: activation.model.clone(),
                    model_version: activation.model_version.clone(),
                    component: Component::Request,
                    currency: activation.currency.clone(),
                },
                at_ms,
            )?)?;
        }
        let revision: Option<i64> = if let Some(expected) = activation.expected_revision {
            sqlx::query_scalar("UPDATE gateway.price_book_v2_routes SET model_version=$4,book_version=$5,revision=revision+1,updated_by=$6,updated_at=CURRENT_TIMESTAMP WHERE tenant_id=$1 AND provider_id=$2 AND model_id=$3 AND currency=$7 AND revision=$8 RETURNING revision")
                .bind(tenant).bind(&activation.provider).bind(&activation.model).bind(&activation.model_version).bind(&activation.book_version).bind(actor).bind(&activation.currency).bind(expected).fetch_optional(&mut *tx).await.map_err(|_|PriceBookError::Storage)?
        } else {
            sqlx::query_scalar("INSERT INTO gateway.price_book_v2_routes(tenant_id,provider_id,model_id,model_version,book_version,currency,revision,updated_by) VALUES($1,$2,$3,$4,$5,$6,1,$7) ON CONFLICT(tenant_id,provider_id,model_id,currency) DO NOTHING RETURNING revision")
                .bind(tenant).bind(&activation.provider).bind(&activation.model).bind(&activation.model_version).bind(&activation.book_version).bind(&activation.currency).bind(actor).fetch_optional(&mut *tx).await.map_err(|_|PriceBookError::Storage)?
        };
        let revision = revision.ok_or(PriceBookError::RevisionConflict)?;
        sqlx::query("INSERT INTO gateway.price_book_v2_activation_audit(tenant_id,provider_id,model_id,currency,revision,publication_reference,actor_id) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(tenant).bind(&activation.provider).bind(&activation.model).bind(&activation.currency).bind(revision).bind(publication.reference()?).bind(actor).execute(&mut *tx).await.map_err(|_|PriceBookError::Storage)?;
        tx.commit()
            .await
            .map_err(|_| PriceBookError::CommitUncertain)?;
        Ok(revision)
    }
    async fn current(
        &self,
        tenant: &str,
        provider: &str,
        model: &str,
        currency: &str,
    ) -> Result<Publication> {
        let mut connection = self
            .pool
            .acquire()
            .await
            .map_err(|_| PriceBookError::Storage)?;
        Self::current_on(&mut connection, tenant, provider, model, currency).await
    }
    async fn current_on(
        connection: &mut PgConnection,
        tenant: &str,
        provider: &str,
        model: &str,
        currency: &str,
    ) -> Result<Publication> {
        if [tenant, provider, model].iter().any(|v| !identity(v)) {
            return Err(PriceBookError::InvalidTerms);
        }
        let row=sqlx::query("SELECT p.reference,p.payload FROM gateway.price_book_v2_routes r JOIN gateway.price_book_v2_publications p USING(tenant_id,provider_id,model_id,model_version,book_version,currency) WHERE r.tenant_id=$1 AND r.provider_id=$2 AND r.model_id=$3 AND r.currency=$4")
            .bind(tenant).bind(provider).bind(model).bind(currency).fetch_optional(&mut *connection).await.map_err(|_|PriceBookError::Storage)?.ok_or(PriceBookError::NotActivated)?;
        let publication: Publication =
            serde_json::from_value(row.try_get("payload").map_err(|_| PriceBookError::Decode)?)
                .map_err(|_| PriceBookError::Decode)?;
        let reference: String = row
            .try_get("reference")
            .map_err(|_| PriceBookError::Decode)?;
        publication.validate(tenant, false)?;
        if publication.provider != provider
            || publication.model != model
            || publication.currency != currency
        {
            return Err(PriceBookError::IdentityMismatch);
        }
        if reference != publication.reference()? {
            return Err(PriceBookError::InvalidReference);
        }
        Ok(publication)
    }
    pub async fn quote_component(
        &self,
        tenant: &str,
        provider: &str,
        model: &str,
        currency: &str,
        component: Component,
        units: u64,
        at_ms: i64,
    ) -> Result<Value> {
        let publication = self.current(tenant, provider, model, currency).await?;
        let book = publication.validate(tenant, false)?;
        let key = PriceKey {
            book_version: publication.book_version,
            provider: provider.into(),
            model: model.into(),
            model_version: publication.model_version,
            component,
            currency: currency.into(),
        };
        let receipt = book.resolve(tenant, &key, at_ms)?;
        let total = receipt.quote_nanos(units)?;
        Ok(
            serde_json::json!({"currency":currency,"cost_nanos":total,"units":units,"receipt":receipt}),
        )
    }
    pub async fn token_quote(
        &self,
        tenant: &str,
        provider: &str,
        model: &str,
        at_ms: i64,
    ) -> Result<Value> {
        let publication = self.current(tenant, provider, model, "USD").await?;
        let book = publication.validate(tenant, false)?;
        token_pair(
            &book,
            tenant,
            &Activation {
                provider: provider.into(),
                model: model.into(),
                model_version: publication.model_version,
                book_version: publication.book_version,
                currency: "USD".into(),
                expected_revision: None,
            },
            at_ms,
        )
    }
    /// Claim the first authoritative receipt before enqueue, across processes and
    /// restarts. Retain a pre-existing v1 raw receipt; never reprice old usage.
    pub async fn freeze_usage(&self, record: &UsageRecord) -> Result<Value> {
        if [
            &record.tenant_id,
            &record.provider,
            &record.model,
            &record.request_id,
        ]
        .iter()
        .any(|v| !identity(v))
        {
            return Err(PriceBookError::InvalidTerms);
        }
        let identity_key =
            serde_json::to_string(&record.identity().ok_or(PriceBookError::InvalidTerms)?)
                .map_err(|_| PriceBookError::Decode)?;
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| PriceBookError::Storage)?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(identity_key)
            .execute(&mut *tx)
            .await
            .map_err(|_| PriceBookError::Storage)?;
        let existing:Option<Value>=sqlx::query_scalar("SELECT snapshot FROM gateway.price_book_v2_usage_receipts WHERE tenant_id=$1 AND request_id=$2 AND service_id=$3 AND request_type=$4")
            .bind(&record.tenant_id).bind(&record.request_id).bind(&record.service_id).bind(&record.request_type).fetch_optional(&mut *tx).await.map_err(|_|PriceBookError::Storage)?;
        if let Some(snapshot) = existing {
            validate_bound_snapshot(record, &snapshot)?;
            tx.commit()
                .await
                .map_err(|_| PriceBookError::CommitUncertain)?;
            return Ok(snapshot);
        }
        let old=sqlx::query("SELECT provider,model,metadata->'pricing_snapshot' AS snapshot FROM usage_records WHERE tenant_id=$1 AND request_id=$2 AND COALESCE(service_id,'')=$3 AND COALESCE(request_type,'')=$4")
            .bind(&record.tenant_id).bind(&record.request_id).bind(&record.service_id).bind(&record.request_type).fetch_optional(&mut *tx).await.map_err(|_|PriceBookError::Storage)?;
        let snapshot = if let Some(old) = old {
            let provider: String = old
                .try_get("provider")
                .map_err(|_| PriceBookError::IdentityMismatch)?;
            let model: String = old
                .try_get("model")
                .map_err(|_| PriceBookError::IdentityMismatch)?;
            if provider != record.provider || model != record.model {
                return Err(PriceBookError::IdentityMismatch);
            }
            let value: Option<Value> = old
                .try_get("snapshot")
                .map_err(|_| PriceBookError::Decode)?;
            value
                .filter(|v| !v.is_null())
                .ok_or(PriceBookError::InvalidReference)?
        } else {
            let publication = Self::current_on(
                &mut tx,
                &record.tenant_id,
                &record.provider,
                &record.model,
                "USD",
            )
            .await?;
            let book = publication.validate(&record.tenant_id, false)?;
            if crate::request_meter::request_count(record)
                .map_err(|_| PriceBookError::InvalidTerms)?
                .is_some()
            {
                crate::request_meter::snapshot(&book.resolve(
                    &record.tenant_id,
                    &PriceKey {
                        book_version: publication.book_version,
                        provider: record.provider.clone(),
                        model: record.model.clone(),
                        model_version: publication.model_version,
                        component: Component::Request,
                        currency: "USD".into(),
                    },
                    record.timestamp.timestamp_millis(),
                )?)?
            } else {
                token_pair(
                    &book,
                    &record.tenant_id,
                    &Activation {
                        provider: record.provider.clone(),
                        model: record.model.clone(),
                        model_version: publication.model_version,
                        book_version: publication.book_version,
                        currency: "USD".into(),
                        expected_revision: None,
                    },
                    record.timestamp.timestamp_millis(),
                )?
            }
        };
        validate_bound_snapshot(record, &snapshot)?;
        sqlx::query("INSERT INTO gateway.price_book_v2_usage_receipts(tenant_id,request_id,service_id,request_type,provider_id,model_id,snapshot) VALUES($1,$2,$3,$4,$5,$6,$7)")
            .bind(&record.tenant_id).bind(&record.request_id).bind(&record.service_id).bind(&record.request_type).bind(&record.provider).bind(&record.model).bind(&snapshot).execute(&mut *tx).await.map_err(|_|PriceBookError::Storage)?;
        tx.commit()
            .await
            .map_err(|_| PriceBookError::CommitUncertain)?;
        Ok(snapshot)
    }
    /// Operator-only migration entrypoint, deliberately absent from tenant HTTP.
    /// Source rows are read directly; ambiguous/null provider mappings quarantine.
    pub async fn import_legacy(
        &self,
        import_id: &str,
        attributions: &[LegacyAttribution],
    ) -> Result<LegacyImport> {
        let rows=sqlx::query("SELECT id::text,model,provider,input_price_per_1k::text AS input,output_price_per_1k::text AS output,is_active,(extract(epoch from effective_from)*1000)::bigint AS from_ms,(extract(epoch from effective_to)*1000)::bigint AS to_ms,((extract(epoch from effective_from)*1000)=trunc(extract(epoch from effective_from)*1000) AND (effective_to IS NULL OR (extract(epoch from effective_to)*1000)=trunc(extract(epoch from effective_to)*1000))) AS exact_range FROM model_pricing").fetch_all(&self.pool).await.map_err(|_|PriceBookError::Storage)?;
        let mut legacy = Vec::new();
        let mut inactive = Vec::new();
        let mut quarantine = Vec::new();
        for row in rows {
            let id: String = row.try_get("id").map_err(|_| PriceBookError::Decode)?;
            let Some(active) = row
                .try_get::<Option<bool>, _>("is_active")
                .map_err(|_| PriceBookError::Decode)?
            else {
                quarantine.push(QuarantinedPrice {
                    source_id: id,
                    reason: "missing_active_state",
                });
                continue;
            };
            if !active {
                inactive.push(id);
                continue;
            }
            if row
                .try_get::<Option<bool>, _>("exact_range")
                .map_err(|_| PriceBookError::Decode)?
                != Some(true)
            {
                quarantine.push(QuarantinedPrice {
                    source_id: id,
                    reason: "missing_or_unrepresentable_effective_range",
                });
                continue;
            }
            let Some(from_ms) = row
                .try_get::<Option<i64>, _>("from_ms")
                .map_err(|_| PriceBookError::Decode)?
            else {
                quarantine.push(QuarantinedPrice {
                    source_id: id,
                    reason: "missing_effective_range",
                });
                continue;
            };
            legacy.push(LegacyPriceRow {
                source_id: id,
                model: row.try_get("model").map_err(|_| PriceBookError::Decode)?,
                provider: row
                    .try_get("provider")
                    .map_err(|_| PriceBookError::Decode)?,
                input_per_1k: row.try_get("input").map_err(|_| PriceBookError::Decode)?,
                output_per_1k: row.try_get("output").map_err(|_| PriceBookError::Decode)?,
                effective: EffectiveRange {
                    from_ms,
                    to_ms: row.try_get("to_ms").map_err(|_| PriceBookError::Decode)?,
                },
                active: row
                    .try_get::<Option<bool>, _>("is_active")
                    .map_err(|_| PriceBookError::Decode)?
                    .unwrap_or(false),
            });
        }
        let mut report = plan_legacy_import(&legacy, attributions);
        report.inactive.extend(inactive);
        report.quarantine.extend(quarantine);
        persist_legacy_import(&self.pool, import_id, &report).await?;
        Ok(report)
    }
}
fn token_pair(
    book: &PriceBook,
    tenant: &str,
    activation: &Activation,
    at_ms: i64,
) -> Result<Value> {
    let key = |component| PriceKey {
        book_version: activation.book_version.clone(),
        provider: activation.provider.clone(),
        model: activation.model.clone(),
        model_version: activation.model_version.clone(),
        component,
        currency: "USD".into(),
    };
    let input = book.resolve(tenant, &key(Component::Input), at_ms)?;
    let mut snapshot = token_snapshot(
        &input,
        &book.resolve(tenant, &key(Component::Output), at_ms)?,
    )?;
    for (field, component) in [
        ("price_book_cached_input", Component::CachedInput),
        ("price_book_cache_write", Component::CacheWrite),
    ] {
        match book.resolve(tenant, &key(component.clone()), at_ms) {
            Ok(receipt) => {
                validate_cache_term(&input, &receipt, component)?;
                snapshot[field] =
                    serde_json::to_value(receipt).map_err(|_| PriceBookError::Decode)?;
            }
            Err(PriceBookError::MissingPrice) => {}
            Err(error) => return Err(error),
        }
    }
    Ok(snapshot)
}
pub fn validate_bound_snapshot(record: &UsageRecord, snapshot: &Value) -> Result<()> {
    if crate::request_meter::is_snapshot(snapshot) {
        crate::request_meter::validate_snapshot(
            snapshot,
            &record.tenant_id,
            &record.provider,
            &record.model,
        )?;
        if crate::request_meter::request_count(record)
            .map_err(|_| PriceBookError::InvalidTerms)?
            .is_none()
        {
            return Err(PriceBookError::InvalidTerms);
        }
        return Ok(());
    }
    validate_pricing_snapshot(snapshot, &record.tenant_id, &record.provider, &record.model)
        .map_err(|_| PriceBookError::IdentityMismatch)?;
    if snapshot.get("price_book_schema").is_some() {
        validate_token_snapshot(snapshot)?;
    } else if snapshot.get("pricing_status").and_then(Value::as_str) == Some("price_book_v2") {
        return Err(PriceBookError::InvalidReference);
    }
    Ok(())
}
impl UsageSnapshotResolver for PgPriceBookService {
    fn resolve<'a>(&'a self, record: &'a UsageRecord) -> UsageFuture<'a, Value> {
        Box::pin(async move {
            self.freeze_usage(record)
                .await
                .map_err(UsageRecorderError::from)
        })
    }
}

#[cfg(test)]
#[path = "price_book_service_tests.rs"]
mod tests;
