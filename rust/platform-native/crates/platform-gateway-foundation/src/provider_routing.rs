//! Typed Gateway catalog and routing domain. Wire coercion, credential material,
//! provider-specific metadata and JSON payload construction live in `wire`.
//! Legacy public exports remain available as compatibility adapters.
#[path = "provider_routing_wire.rs"]
mod wire;
use std::collections::BTreeSet;
use std::fmt;
pub use wire::*;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ModelRef {
    pub provider_id: String,
    pub model_id: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialAvailability {
    Resolved,
    Environment,
    Missing,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogProvider {
    pub tenant_id: String,
    pub provider_id: String,
    pub enabled: bool,
    pub runtime_provider: String,
    /// Availability only; secrets never enter the catalog/routing domain.
    pub credentials: CredentialAvailability,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogModel {
    pub tenant_id: String,
    pub identity: ModelRef,
    pub enabled: bool,
}
#[derive(Clone, Debug, Default)]
pub struct CatalogSnapshot {
    pub providers: Vec<CatalogProvider>,
    pub models: Vec<CatalogModel>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoutingFailure {
    ProviderRequired,
    ModelRequired,
    ProviderNotFound,
    ProviderDisabled,
    ModelNotFound,
    ModelDisabled,
    ApiKeyMissing,
    ProviderUnsupported,
    ModelProviderRequired,
}
impl RoutingFailure {
    pub fn suffix(self) -> &'static str {
        match self {
            Self::ProviderRequired => "PROVIDER_REQUIRED",
            Self::ModelRequired => "MODEL_REQUIRED",
            Self::ProviderNotFound => "PROVIDER_NOT_FOUND",
            Self::ProviderDisabled => "PROVIDER_DISABLED",
            Self::ModelNotFound => "MODEL_NOT_FOUND",
            Self::ModelDisabled => "MODEL_DISABLED",
            Self::ApiKeyMissing => "API_KEY_MISSING",
            Self::ProviderUnsupported => "PROVIDER_UNSUPPORTED",
            Self::ModelProviderRequired => "model_provider_required",
        }
    }
    pub fn code(self, prefix: &str) -> String {
        if self == Self::ModelProviderRequired {
            self.suffix().into()
        } else {
            format!("{prefix}_{}", self.suffix())
        }
    }
}
impl fmt::Display for RoutingFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.suffix())
    }
}
impl std::error::Error for RoutingFailure {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteTarget {
    pub tenant_id: String,
    pub identity: ModelRef,
    pub runtime_provider: String,
}
impl CatalogSnapshot {
    pub fn provider(&self, tenant: &str, id: &str) -> Option<&CatalogProvider> {
        self.providers
            .iter()
            .find(|p| p.tenant_id == tenant && p.provider_id == id)
    }
    /// Disabled models still participate in provider ambiguity, preserving the
    /// existing model-service contract. Explicit provider lookup is exact.
    pub fn model(
        &self,
        tenant: &str,
        model: &str,
        provider: Option<&str>,
    ) -> Result<Option<&CatalogModel>, RoutingFailure> {
        let mut found = self.models.iter().filter(|m| {
            m.tenant_id == tenant
                && m.identity.model_id == model
                && provider.is_none_or(|p| m.identity.provider_id == p)
        });
        let first = found.next();
        if provider.is_none() && found.next().is_some() {
            return Err(RoutingFailure::ModelProviderRequired);
        }
        Ok(first)
    }
    /// Pure, ordered fail-closed selection. Tenant matching is mandatory for
    /// both records; an otherwise matching foreign record is never a fallback.
    pub fn select(&self, tenant: &str, identity: &ModelRef) -> Result<RouteTarget, RoutingFailure> {
        if identity.provider_id.is_empty() {
            return Err(RoutingFailure::ProviderRequired);
        }
        if identity.model_id.is_empty() {
            return Err(RoutingFailure::ModelRequired);
        }
        let provider = self
            .provider(tenant, &identity.provider_id)
            .ok_or(RoutingFailure::ProviderNotFound)?;
        if !provider.enabled {
            return Err(RoutingFailure::ProviderDisabled);
        }
        let model = self
            .model(tenant, &identity.model_id, Some(&identity.provider_id))?
            .ok_or(RoutingFailure::ModelNotFound)?;
        if !model.enabled {
            return Err(RoutingFailure::ModelDisabled);
        }
        if provider.credentials == CredentialAvailability::Missing {
            return Err(RoutingFailure::ApiKeyMissing);
        }
        if provider.runtime_provider.is_empty() {
            return Err(RoutingFailure::ProviderUnsupported);
        }
        Ok(RouteTarget {
            tenant_id: tenant.into(),
            identity: identity.clone(),
            runtime_provider: provider.runtime_provider.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FallbackRef {
    InvalidRecord,
    Model(ModelRef),
}
#[derive(Clone, Debug)]
pub struct RoutePolicy {
    pub primary: ModelRef,
    pub failover: Option<FailoverPolicy>,
}
#[derive(Clone, Debug)]
pub struct FailoverPolicy {
    pub max_attempts: usize,
    pub candidates: Vec<FallbackRef>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FailoverWarning {
    InvalidRecord,
    InvalidIdentity(ModelRef),
    Duplicate(ModelRef),
    Rejected {
        identity: ModelRef,
        failure: RoutingFailure,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailoverPlan {
    pub primary: RouteTarget,
    pub enabled: bool,
    /// Ordered; includes primary exactly once. This plans, never executes retry.
    pub candidates: Vec<RouteTarget>,
    pub max_attempts: usize,
    pub warnings: Vec<FailoverWarning>,
}
impl CatalogSnapshot {
    pub fn plan(&self, tenant: &str, policy: &RoutePolicy) -> Result<FailoverPlan, RoutingFailure> {
        let primary = self.select(tenant, &policy.primary)?;
        let mut plan = FailoverPlan {
            primary: primary.clone(),
            enabled: policy.failover.is_some(),
            candidates: vec![primary],
            max_attempts: 1,
            warnings: vec![],
        };
        let Some(failover) = &policy.failover else {
            return Ok(plan);
        };
        let mut seen = BTreeSet::from([policy.primary.clone()]);
        for fallback in &failover.candidates {
            let identity = match fallback {
                FallbackRef::InvalidRecord => {
                    plan.warnings.push(FailoverWarning::InvalidRecord);
                    continue;
                }
                FallbackRef::Model(identity) => identity,
            };
            if identity.provider_id.is_empty() || identity.model_id.is_empty() {
                plan.warnings
                    .push(FailoverWarning::InvalidIdentity(identity.clone()));
                continue;
            }
            if seen.contains(identity) {
                plan.warnings
                    .push(FailoverWarning::Duplicate(identity.clone()));
                continue;
            }
            match self.select(tenant, identity) {
                Ok(candidate) => {
                    plan.candidates.push(candidate);
                    seen.insert(identity.clone());
                }
                Err(failure) => plan.warnings.push(FailoverWarning::Rejected {
                    identity: identity.clone(),
                    failure,
                }),
            }
        }
        plan.max_attempts = failover
            .max_attempts
            .clamp(1, 10)
            .max(if plan.candidates.len() > 1 { 2 } else { 1 })
            .min(plan.candidates.len());
        Ok(plan)
    }
}

impl FailoverPlan {
    /// Versioned identity of the selected route, excluding credentials and wire
    /// sampling/request bodies. This is not an invocation authorization proof.
    /// Existing v1 proof verifiers never consume this new fingerprint.
    pub fn fingerprint_v2(&self) -> Result<String, crate::canonical_json_v2::CanonicalError> {
        use crate::canonical_json_v2::{CanonicalDocument, CanonicalValue as J};
        let target = |route: &RouteTarget| {
            J::object([
                ("tenant_id".into(), J::String(route.tenant_id.clone())),
                (
                    "provider_id".into(),
                    J::String(route.identity.provider_id.clone()),
                ),
                (
                    "model_id".into(),
                    J::String(route.identity.model_id.clone()),
                ),
                (
                    "runtime_provider".into(),
                    J::String(route.runtime_provider.clone()),
                ),
            ])
        };
        let targets = self
            .candidates
            .iter()
            .map(target)
            .collect::<Result<Vec<_>, _>>()?;
        CanonicalDocument::new(J::object([
            (
                "schema_version".into(),
                J::String("agentforge.route-plan/v2".into()),
            ),
            ("primary".into(), target(&self.primary)?),
            ("failover_enabled".into(), J::Bool(self.enabled)),
            ("max_attempts".into(), J::number(self.max_attempts as f64)?),
            ("candidates".into(), J::Array(targets)),
        ])?)
        .sha256_hex()
    }
}

#[cfg(test)]
mod domain_tests {
    use super::*;
    fn catalog() -> CatalogSnapshot {
        CatalogSnapshot {
            providers: ["a", "b"]
                .into_iter()
                .map(|p| CatalogProvider {
                    tenant_id: "t".into(),
                    provider_id: p.into(),
                    enabled: true,
                    runtime_provider: "openai".into(),
                    credentials: CredentialAvailability::Resolved,
                })
                .collect(),
            models: ["a", "b"]
                .into_iter()
                .map(|p| CatalogModel {
                    tenant_id: "t".into(),
                    identity: ModelRef {
                        provider_id: p.into(),
                        model_id: "shared".into(),
                    },
                    enabled: true,
                })
                .collect(),
        }
    }
    fn reference(p: &str) -> ModelRef {
        ModelRef {
            provider_id: p.into(),
            model_id: "shared".into(),
        }
    }
    #[test]
    fn exact_tenant_and_provider_catalog() {
        let c = catalog();
        assert_eq!(
            c.model("t", "shared", None),
            Err(RoutingFailure::ModelProviderRequired)
        );
        assert_eq!(
            c.select("foreign", &reference("a")),
            Err(RoutingFailure::ProviderNotFound)
        );
        assert_eq!(
            c.select("t", &reference("b")).unwrap().identity,
            reference("b")
        );
    }
    #[test]
    fn failover_order_duplicates_and_warnings() {
        let c = catalog();
        let plan = c
            .plan(
                "t",
                &RoutePolicy {
                    primary: reference("a"),
                    failover: Some(FailoverPolicy {
                        max_attempts: 1,
                        candidates: vec![
                            FallbackRef::InvalidRecord,
                            FallbackRef::Model(reference("a")),
                            FallbackRef::Model(reference("missing")),
                            FallbackRef::Model(reference("b")),
                        ],
                    }),
                },
            )
            .unwrap();
        assert_eq!(
            plan.candidates
                .iter()
                .map(|p| p.identity.provider_id.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert_eq!(plan.max_attempts, 2);
        assert_eq!(plan.warnings.len(), 3);
    }
    #[test]
    fn credentials_disabled_and_primary_fail_closed() {
        let mut c = catalog();
        c.providers[0].credentials = CredentialAvailability::Missing;
        assert_eq!(
            c.select("t", &reference("a")),
            Err(RoutingFailure::ApiKeyMissing)
        );
        c.providers[0].credentials = CredentialAvailability::Environment;
        assert!(c.select("t", &reference("a")).is_ok());
        c.models[0].enabled = false;
        assert_eq!(
            c.plan(
                "t",
                &RoutePolicy {
                    primary: reference("a"),
                    failover: Some(FailoverPolicy {
                        max_attempts: 10,
                        candidates: vec![FallbackRef::Model(reference("b"))]
                    })
                }
            ),
            Err(RoutingFailure::ModelDisabled)
        );
    }
}
