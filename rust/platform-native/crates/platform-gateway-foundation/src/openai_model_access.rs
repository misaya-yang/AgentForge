//! Scope checks for volatile model-backed services projected from the existing
//! model-management catalog. These checks complement normal ServiceAuth and
//! AgentInvoke validation; they keep synthetic service IDs tenant-scoped even
//! if a caller reaches the generic dispatcher route directly.
use crate::service_ownership::ServiceHandle;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenAiModelScopeDecision {
    Unscoped,
    Allowed,
    TenantDenied,
    ApiKeyModelDenied,
    ModelAccessDenied,
}

pub fn decide(
    service: &ServiceHandle,
    tenant_id: &str,
    tier: &str,
    roles: &[String],
    allowed_models: Option<&[String]>,
) -> OpenAiModelScopeDecision {
    let snapshot = service.snapshot();
    let scope_value = snapshot.metadata.get("openai_compat_scope");
    let openai_adapter = snapshot
        .metadata
        .get("adapter_type")
        .and_then(|v| v.as_str())
        == Some("openai");
    if scope_value.is_none() && !openai_adapter {
        return OpenAiModelScopeDecision::Unscoped;
    }
    if let Some(allowed_models) = allowed_models.filter(|allowed| !allowed.is_empty()) {
        let model = snapshot
            .metadata
            .get("openai_model_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                snapshot
                    .metadata
                    .get("model")
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or(snapshot.service_id.as_str());
        if !model_allowed(allowed_models, model) {
            return OpenAiModelScopeDecision::ApiKeyModelDenied;
        }
    }
    let Some(scope) = scope_value else {
        return if openai_adapter && allowed_models.is_some_and(|allowed| !allowed.is_empty()) {
            OpenAiModelScopeDecision::Allowed
        } else {
            OpenAiModelScopeDecision::Unscoped
        };
    };
    let Some(scope) = scope.as_object() else {
        return OpenAiModelScopeDecision::ModelAccessDenied;
    };
    if scope.get("tenant_id").and_then(serde_json::Value::as_str) != Some(tenant_id) {
        return OpenAiModelScopeDecision::TenantDenied;
    }
    let Some(required) = scope
        .get("access_level")
        .and_then(serde_json::Value::as_str)
    else {
        return OpenAiModelScopeDecision::ModelAccessDenied;
    };
    if can_access_model(tier, roles, required) {
        OpenAiModelScopeDecision::Allowed
    } else {
        OpenAiModelScopeDecision::ModelAccessDenied
    }
}

fn model_allowed(allowed_models: &[String], model: &str) -> bool {
    let model = model.trim().to_lowercase();
    if model.is_empty() {
        return false;
    }
    allowed_models.iter().any(|pattern| {
        let pattern = pattern.trim().to_lowercase();
        !pattern.is_empty()
            && pattern
                .strip_suffix('*')
                .map_or_else(|| model == pattern, |prefix| model.starts_with(prefix))
    })
}

fn can_access_model(tier: &str, roles: &[String], required: &str) -> bool {
    if !["public", "premium", "admin"].contains(&required) {
        return false;
    }
    if tier == "admin" || roles.iter().any(|role| role == "admin") {
        return true;
    }
    match required {
        "public" => true,
        "premium" => {
            ["premium", "enterprise", "admin"].contains(&tier)
                || roles
                    .iter()
                    .any(|role| ["premium", "vip", "enterprise", "admin"].contains(&role.as_str()))
        }
        "admin" => false,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ServiceDefinition;
    use serde_json::json;

    fn managed(level: &str, tenant: &str) -> ServiceHandle {
        let mut service = ServiceDefinition::new("managed-openai-test", "Model");
        service.metadata = json!({"openai_compat_scope":{"tenant_id":tenant,"access_level":level}});
        ServiceHandle::new(service)
    }

    #[test]
    fn model_scope_enforces_tenant_tier_and_admin_semantics() {
        let roles = vec!["user".to_owned()];
        assert_eq!(
            decide(&managed("public", "t1"), "t1", "basic", &roles, None),
            OpenAiModelScopeDecision::Allowed
        );
        assert_eq!(
            decide(&managed("premium", "t1"), "t1", "basic", &roles, None),
            OpenAiModelScopeDecision::ModelAccessDenied
        );
        assert_eq!(
            decide(&managed("premium", "t1"), "t1", "premium", &roles, None),
            OpenAiModelScopeDecision::Allowed
        );
        assert_eq!(
            decide(&managed("admin", "t1"), "t1", "premium", &roles, None),
            OpenAiModelScopeDecision::ModelAccessDenied
        );
        assert_eq!(
            decide(
                &managed("admin", "t1"),
                "t1",
                "basic",
                &["admin".into()],
                None
            ),
            OpenAiModelScopeDecision::Allowed
        );
        assert_eq!(
            decide(
                &managed("public", "t1"),
                "t2",
                "admin",
                &["admin".into()],
                None
            ),
            OpenAiModelScopeDecision::TenantDenied
        );
        assert_eq!(
            decide(
                &managed("unknown", "t1"),
                "t1",
                "admin",
                &["admin".into()],
                None
            ),
            OpenAiModelScopeDecision::ModelAccessDenied
        );
    }

    #[test]
    fn ordinary_service_has_no_managed_model_scope() {
        let service = ServiceHandle::new(ServiceDefinition::new("ordinary", "Ordinary"));
        assert_eq!(
            decide(&service, "t1", "public", &[], None),
            OpenAiModelScopeDecision::Unscoped
        );
    }

    #[test]
    fn api_key_model_allowlist_also_filters_openai_adapter_services_without_managed_metadata() {
        let mut definition = ServiceDefinition::new("legacy-openai-qwen-plus", "Qwen Plus");
        definition.metadata = serde_json::json!({
            "adapter_type":"openai",
            "model":"qwen-plus"
        });
        let service = ServiceHandle::new(definition);
        let allowlist = vec!["qwen3.8-flash".to_owned()];
        assert_eq!(
            decide(
                &service,
                "tenant-a",
                "admin",
                &["admin".into()],
                Some(&allowlist)
            ),
            OpenAiModelScopeDecision::ApiKeyModelDenied
        );
    }

    #[test]
    fn api_key_model_allowlist_is_independent_of_admin_tier_and_roles() {
        let allowed = vec!["qwen3.8-*".to_owned()];
        assert_eq!(
            decide(
                &managed("public", "t1"),
                "t1",
                "admin",
                &["admin".into()],
                Some(&allowed)
            ),
            OpenAiModelScopeDecision::ApiKeyModelDenied
        );

        let mut definition =
            crate::protocol::ServiceDefinition::new("managed-openai-test", "Model");
        definition.metadata = serde_json::json!({
            "openai_model_id":"qwen3.8-flash",
            "openai_compat_scope":{"tenant_id":"t1","access_level":"public"}
        });
        let service = ServiceHandle::new(definition);
        assert_eq!(
            decide(&service, "t1", "normal", &["user".into()], Some(&allowed)),
            OpenAiModelScopeDecision::Allowed
        );
    }
}
