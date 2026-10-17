//! Existing catalog/ACL/capability/price-book boundary. JSON stays at this adapter.
use super::*;
use ai_platform_gateway_foundation::{
    price_book::{Component, PriceBookError, PriceReceipt, service::PgPriceBookService},
    provider_routing::{ModelRef, RoutingSnapshot},
};
use serde_json::{Value, json};
use sqlx::Row;
use std::collections::BTreeSet;
/// Workload is supplied by the server's existing tokenizer/enforced token limit,
/// never used as a reservation or replacement for admission/billing.
#[derive(Clone)]
pub struct QuoteWorkload {
    pub input_tokens: u64,
    pub output_tokens: u64,
}
pub struct CandidateSet {
    pub(super) tenant: String,
    pub(super) actor: String,
    pub(super) candidates: Vec<Candidate>,
}
impl RoutingPolicyService {
    /// Providers must be the existing RuntimeProviderCredentials server records
    /// (same source used by native LLM preparation); nothing is read from client
    /// metadata. Models and ACL are read afresh from their current authority.
    pub async fn candidates(
        &self,
        a: &RoutingAuthority,
        p: &Policy,
        providers: &[Value],
        workload: &QuoteWorkload,
        at_ms: i64,
    ) -> Result<CandidateSet> {
        let workloads = p
            .candidates
            .iter()
            .map(|r| (r.target.clone(), workload.clone()))
            .collect();
        self.candidates_for_workloads(a, p, providers, &workloads, at_ms)
            .await
    }
    pub async fn candidates_for_workloads(
        &self,
        a: &RoutingAuthority,
        p: &Policy,
        providers: &[Value],
        workloads: &std::collections::BTreeMap<Identity, QuoteWorkload>,
        at_ms: i64,
    ) -> Result<CandidateSet> {
        p.validate()?;
        if at_ms < 0
            || at_ms as u64 > MAX_EXACT
            || p.tenant != a.tenant
            || workloads.len() > 128
            || workloads
                .values()
                .any(|w| w.input_tokens > MAX_EXACT || w.output_tokens > MAX_EXACT)
        {
            return Err(RoutingError::Forbidden);
        }
        let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(m) FROM gateway.llm_models m WHERE m.tenant_id=$1 ORDER BY m.provider_id,m.model_id").bind(&a.tenant).fetch_all(&self.pool).await.map_err(storage)?;
        let catalog = RoutingSnapshot::new(providers, &rows).typed_catalog();
        let mut candidates = vec![];
        for r in &p.candidates {
            let matched = rows
                .iter()
                .filter(|m| {
                    m["tenant_id"] == a.tenant
                        && m["provider_id"] == r.target.provider
                        && m["model_id"] == r.target.model
                })
                .collect::<Vec<_>>();
            if matched.len() > 1 {
                return Err(RoutingError::InvalidSnapshot);
            }
            let row = matched.first().copied();
            let workload = workloads.get(&r.target);
            let authorized = row.is_some_and(|m| authorized(a, m));
            let catalog_available = catalog
                .select(
                    &a.tenant,
                    &ModelRef {
                        provider_id: r.target.provider.clone(),
                        model_id: r.target.model.clone(),
                    },
                )
                .is_ok();
            let capabilities = if authorized {
                row.map(profile).unwrap_or_default()
            } else {
                BTreeSet::new()
            };
            let (cost, latency) = if authorized && catalog_available && workload.is_some() {
                (
                    self.cost(
                        a,
                        &r.target,
                        workload.ok_or(RoutingError::InvalidSnapshot)?,
                        at_ms,
                    )
                    .await?,
                    self.latency(a, &r.target, at_ms, p.metric_max_age_ms)
                        .await?,
                )
            } else {
                (None, None)
            };
            candidates.push(Candidate {
                tenant: a.tenant.clone(),
                target: r.target.clone(),
                catalog_available: catalog_available && cost.is_some(),
                authorized,
                invocation_compatible: workload.is_some(),
                capabilities,
                cost,
                latency,
            });
        }
        Ok(CandidateSet {
            tenant: a.tenant.clone(),
            actor: a.actor.clone(),
            candidates,
        })
    }
    async fn cost(
        &self,
        a: &RoutingAuthority,
        target: &Identity,
        w: &QuoteWorkload,
        at: i64,
    ) -> Result<Option<Observation>> {
        let service = PgPriceBookService::new(self.pool.clone());
        let mut quotes = vec![];
        for (component, units) in [
            (Component::Input, w.input_tokens),
            (Component::Output, w.output_tokens),
        ] {
            let quote = match service
                .quote_component(
                    &a.tenant,
                    &target.provider,
                    &target.model,
                    "USD",
                    component,
                    units,
                    at,
                )
                .await
            {
                Ok(q) => q,
                Err(PriceBookError::NotActivated | PriceBookError::MissingPrice) => {
                    return Ok(None);
                }
                Err(_) => return Err(RoutingError::Storage),
            };
            let receipt: PriceReceipt = serde_json::from_value(quote["receipt"].clone())
                .map_err(|_| RoutingError::InvalidSnapshot)?;
            receipt
                .validate(&a.tenant, &receipt.entry.key)
                .map_err(|_| RoutingError::InvalidSnapshot)?;
            if receipt.entry.key.provider != target.provider
                || receipt.entry.key.model != target.model
                || receipt.entry.key.model_version != target.version
                || receipt.entry.key.currency != "USD"
                || receipt.selected_at_ms != at
            {
                return Ok(None);
            }
            quotes.push((
                quote["cost_nanos"]
                    .as_u64()
                    .ok_or(RoutingError::InvalidSnapshot)?,
                receipt,
            ));
        }
        if quotes[0].1.entry.key.book_version != quotes[1].1.entry.key.book_version {
            return Ok(None);
        }
        let value = quotes[0]
            .0
            .checked_add(quotes[1].0)
            .filter(|v| *v <= MAX_EXACT)
            .ok_or(RoutingError::InvalidSnapshot)?;
        let evidence = json!({"input":quotes[0].1.reference,"output":quotes[1].1.reference,"input_tokens":w.input_tokens,"output_tokens":w.output_tokens,"currency":"USD"});
        let bytes = serde_json::to_vec(&evidence).map_err(|_| RoutingError::InvalidSnapshot)?;
        let reference =
            ai_platform_gateway_foundation::canonical_json_v2::CanonicalDocument::from_json(&bytes)
                .and_then(|d| d.sha256_hex())
                .map_err(|_| RoutingError::InvalidSnapshot)?;
        Ok(Some(Observation {
            value,
            measured_at_ms: at,
            reference: format!("price_book_v2:{reference}"),
        }))
    }
    async fn latency(
        &self,
        a: &RoutingAuthority,
        target: &Identity,
        at: i64,
        max_age: u32,
    ) -> Result<Option<Observation>> {
        // Latest successful measured request for the exact immutable price/model
        // version. Missing legacy version/timing is unknown, never zero latency.
        let row=sqlx::query("SELECT id::text AS id,latency_ms::bigint AS latency_ms,(extract(epoch FROM created_at AT TIME ZONE 'UTC')*1000)::bigint AS measured_at_ms FROM usage_records WHERE tenant_id=$1 AND provider=$2 AND model=$3 AND status='success' AND latency_ms>0 AND metadata->'pricing_snapshot'->'price_book_input'->'entry'->'key'->>'model_version'=$4 AND created_at>=to_timestamp($5::double precision/1000) AT TIME ZONE 'UTC' AND created_at<=to_timestamp($6::double precision/1000) AT TIME ZONE 'UTC' ORDER BY created_at DESC,id DESC LIMIT 1").bind(&a.tenant).bind(&target.provider).bind(&target.model).bind(&target.version).bind(at.saturating_sub(i64::from(max_age))).bind(at).fetch_optional(&self.pool).await.map_err(storage)?;
        row.map(|r| {
            Ok(Observation {
                value: u64::try_from(r.try_get::<i64, _>("latency_ms").map_err(storage)?)
                    .map_err(|_| RoutingError::InvalidSnapshot)?,
                measured_at_ms: r.try_get("measured_at_ms").map_err(storage)?,
                reference: format!(
                    "usage_record:{}",
                    r.try_get::<String, _>("id").map_err(storage)?
                ),
            })
        })
        .transpose()
    }
}
fn authorized(a: &RoutingAuthority, m: &Value) -> bool {
    let admin = a.tier == "admin" || a.roles.iter().any(|r| r == "admin");
    let access = match m["access_level"].as_str() {
        Some("public") => true,
        Some("premium") => admin || ["premium", "enterprise"].contains(&a.tier.as_str()),
        Some("admin") => admin,
        _ => false,
    };
    access
        && (a.allowed_models.is_empty()
            || crate::service_access_policy::service_scope_matches(
                &json!(a.allowed_models),
                &[
                    m["model_id"].clone(),
                    json!(format!(
                        "{}/{}",
                        m["provider_id"].as_str().unwrap_or(""),
                        m["model_id"].as_str().unwrap_or("")
                    )),
                ],
            ))
}
fn profile(m: &Value) -> BTreeSet<String> {
    let p = ai_platform_gateway_foundation::capabilities::effective(
        m["provider_id"].as_str().unwrap_or(""),
        m["model_id"].as_str().unwrap_or(""),
        &m["catalog_capabilities"],
        &m["capability_overrides"],
        m["supports_tools"] == true,
        m["supports_vision"] == true,
    );
    let mut out = BTreeSet::new();
    if let Some(protocols) = p["wire_protocols"]["supported"].as_array() {
        for protocol in protocols {
            if let Some(s) = protocol.as_str() {
                out.insert(format!("protocol:{s}"));
            }
        }
    }
    if p["tools"]["function_calling"] == true {
        out.insert("tools".into());
    }
    if p["streaming"]["text_deltas"] == true {
        out.insert("streaming".into());
    }
    for side in ["input", "output"] {
        if let Some(modalities) = p["modalities"][side].as_array() {
            for modality in modalities {
                if let Some(s) = modality.as_str() {
                    out.insert(format!("{side}:{s}"));
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn current_acl_and_key_provider_scope_both_apply() {
        let a = RoutingAuthority {
            tenant: "t".into(),
            actor: "u".into(),
            roles: vec![],
            tier: "normal".into(),
            allowed_models: vec!["provider-a/same".into()],
            manage: false,
        };
        let mut row = json!({"tenant_id":"t","provider_id":"provider-a","model_id":"same","access_level":"public"});
        assert!(authorized(&a, &row));
        row["provider_id"] = json!("provider-b");
        assert!(!authorized(&a, &row));
        row["provider_id"] = json!("provider-a");
        row["access_level"] = json!("admin");
        assert!(!authorized(&a, &row));
        row["access_level"] = Value::Null;
        assert!(!authorized(&a, &row));
    }
    #[test]
    fn capabilities_come_from_existing_effective_profile() {
        let row = json!({"provider_id":"private-provider","model_id":"private-model","supports_tools":false,"supports_vision":false});
        let p = profile(&row);
        assert!(p.contains("protocol:chat_completions"));
        assert!(!p.contains("tools"));
        assert!(!p.contains("input:image"));
    }
}

/// Revalidate the finally frozen accounting snapshot against the exact projected
/// price evidence, closing activation changes between ranking and admission.
pub fn pricing_matches_decision(d: &Decision, snapshot: &Value, w: &QuoteWorkload) -> Result<()> {
    let input: PriceReceipt = serde_json::from_value(snapshot["price_book_input"].clone())
        .map_err(|_| RoutingError::InvalidSnapshot)?;
    let output: PriceReceipt = serde_json::from_value(snapshot["price_book_output"].clone())
        .map_err(|_| RoutingError::InvalidSnapshot)?;
    for r in [&input, &output] {
        r.validate(&d.context.tenant, &r.entry.key)
            .map_err(|_| RoutingError::InvalidSnapshot)?;
        if r.selected_at_ms != d.context.at_ms
            || r.entry.key.provider != d.selected.provider
            || r.entry.key.model != d.selected.model
            || r.entry.key.model_version != d.selected.version
        {
            return Err(RoutingError::PinMismatch);
        }
    }
    let evidence = json!({"input":input.reference,"output":output.reference,"input_tokens":w.input_tokens,"output_tokens":w.output_tokens,"currency":"USD"});
    let bytes = serde_json::to_vec(&evidence).map_err(|_| RoutingError::InvalidSnapshot)?;
    let reference =
        ai_platform_gateway_foundation::canonical_json_v2::CanonicalDocument::from_json(&bytes)
            .and_then(|d| d.sha256_hex())
            .map_err(|_| RoutingError::InvalidSnapshot)?;
    let cost = d
        .trace
        .iter()
        .find(|t| t.target == d.selected)
        .and_then(|t| t.cost.as_ref())
        .ok_or(RoutingError::PinMismatch)?;
    if cost.reference != format!("price_book_v2:{reference}") {
        return Err(RoutingError::PinMismatch);
    }
    Ok(())
}
