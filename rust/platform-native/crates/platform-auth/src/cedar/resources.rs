//! Independent object PDP. Loaded object attributes and raw ACL grants are input;
//! the old authorization boolean is compared/audited, never a Cedar attribute.
use super::{
    DecisionLog, DecisionPoint, Mode, PolicyError, configuration_error, validated_policies,
};
pub use ai_platform_service_primitives::object_authorization::*;
use cedar_policy::{
    Authorizer, Context, Decision, Entities, EntityUid, PolicySet, Request, Schema,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap},
    future::Future,
    sync::Arc,
};
const POLICY: &str = include_str!("resource_policy.cedar");
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ResourceConfiguration {
    pub enforce: BTreeSet<ResourceKind>,
    pub expected_policy_sha256: Option<String>,
    pub expected_matrix_sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResourceAudit {
    pub kind: ResourceKind,
    pub action: ObjectAction,
    #[serde(default)]
    pub enforced: bool,
    pub principal_id_sha256: String,
    pub tenant_id_sha256: String,
    pub resource_id_sha256: String,
    pub facts_sha256: String,
    pub request_id: String,
    pub trace_id: String,
}
#[derive(Clone, Default)]
pub struct PolicyScope {
    pub point: Option<Arc<DecisionPoint>>,
    pub request_id: String,
    pub trace_id: String,
    pub subject: Arc<std::sync::Mutex<Option<ObjectSubject>>>,
}
tokio::task_local! {static SCOPE:PolicyScope;}
pub fn capture() -> PolicyScope {
    SCOPE.try_with(Clone::clone).unwrap_or_default()
}
pub fn remember_subject(subject: ObjectSubject) {
    let scope = capture();
    if let Ok(mut current) = scope.subject.lock() {
        if current
            .as_ref()
            .is_none_or(|old| old.user_id == subject.user_id && old.tenant_id == subject.tenant_id)
        {
            *current = Some(subject);
        }
    };
}
pub fn current_subject() -> Option<ObjectSubject> {
    let scope = capture();
    let result = scope.subject.lock().ok().and_then(|s| s.clone());
    result
}

pub async fn scoped<T>(scope: PolicyScope, future: impl Future<Output = T>) -> T {
    SCOPE.scope(scope, future).await
}
/// Explicit scope wrappers retain the policy handle for detached work; identity
/// and current object facts still come from their normal authorities each time.
pub fn retained<T>(future: impl Future<Output = T>) -> impl Future<Output = T> {
    let scope = capture();
    async move { scoped(scope, future).await }
}
pub fn compare_current(
    subject: &ObjectSubject,
    resource: &ObjectResource,
    action: ObjectAction,
    legacy: bool,
) -> bool {
    let scope = capture();
    scope
        .point
        .as_ref()
        .map(|p| p.decide_resource(subject, resource, action, legacy, &scope))
        .unwrap_or(legacy)
}
fn sha(value: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(value.as_ref()))
}
pub(super) struct ObjectPolicy {
    schema: Schema,
    policies: PolicySet,
    pub policy_sha256: String,
    pub matrix_sha256: String,
    roles: HashMap<String, Vec<String>>,
}
fn profile(profile: ObjectProfile) -> &'static str {
    match profile {
        ObjectProfile::Standard => "standard",
        ObjectProfile::DatasetQuery => "dataset_query",
        ObjectProfile::AgentListing => "agent_listing",
        ObjectProfile::AgentCreation => "agent_creation",
        ObjectProfile::ToolVisibility => "tool_visibility",
        ObjectProfile::ModelAdministration => "model_administration",
        ObjectProfile::UserActor => "user_actor",
        ObjectProfile::UserMutation => "user_mutation",
    }
}
fn model_admin_permissions(subject: &ObjectSubject) -> BTreeSet<String> {
    let mut values = subject
        .roles
        .iter()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .collect::<BTreeSet<_>>();
    if values.contains("admin") {
        values.insert("admin:*".into());
    }
    values
}
fn candidates(permission: &str) -> BTreeSet<String> {
    if permission.is_empty() {
        return BTreeSet::new();
    }
    let mut values = BTreeSet::from([permission.into(), "admin:*".into()]);
    let mut end = permission.len();
    while let Some(index) = permission[..end].rfind(':') {
        values.insert(format!("{}:*", &permission[..index]));
        end = index;
    }
    values
}
fn attributes() -> Value {
    let mut attrs = serde_json::Map::new();
    for key in [
        "tenant",
        "tenant_trim",
        "id",
        "owner",
        "visibility",
        "access_level",
        "profile",
    ] {
        attrs.insert(key.into(), json!({"type":"String"}));
    }
    for key in [
        "present",
        "archived",
        "allowlist_mode",
        "identity_current",
        "enabled",
        "protected",
        "current_binding",
    ] {
        attrs.insert(key.into(), json!({"type":"Boolean"}));
    }
    // Cedar Strict forbids both higher-order exists/lambda syntax and empty
    // set literals. Supply structural counts derived from the raw collections;
    // grant ranks, aliases, tenant checks and all authorization stay in Cedar.
    for key in ["nonempty_grant_count", "alias_count"] {
        attrs.insert(key.into(), json!({"type":"Long"}));
    }
    for key in [
        "grants",
        "aliases",
        "required_scopes",
        "satisfied_scopes",
        "denied_aliases",
        "allowed_aliases",
    ] {
        attrs.insert(
            key.into(),
            json!({"type":"Set","element":{"type":"String"}}),
        );
    }
    json!({"type":"Record","attributes":attrs})
}
impl ObjectPolicy {
    pub fn new(roles: HashMap<String, Vec<String>>) -> Result<Self, PolicyError> {
        let mut types = serde_json::Map::new();
        types.insert("Subject".into(),json!({"memberOfTypes":[],"shape":{"type":"Record","attributes":{"tenant":{"type":"String"},"tenant_trim":{"type":"String"},"user":{"type":"String"},"user_trim":{"type":"String"},"tier":{"type":"String"},"tier_lower":{"type":"String"},"anonymous":{"type":"Boolean"},"roles":{"type":"Set","element":{"type":"String"}},"folded_roles":{"type":"Set","element":{"type":"String"}},"normalized_roles":{"type":"Set","element":{"type":"String"}},"permissions":{"type":"Set","element":{"type":"String"}},"capability_permissions":{"type":"Set","element":{"type":"String"}},"model_admin_permissions":{"type":"Set","element":{"type":"String"}}}}}));
        for kind in ResourceKind::ALL {
            types.insert(
                kind.entity().into(),
                json!({"memberOfTypes":[],"shape":attributes()}),
            );
        }
        let actions=ObjectAction::ALL.iter().map(|a|(a.name().into(),json!({"appliesTo":{"principalTypes":["Subject"],"resourceTypes":ResourceKind::ALL.map(|k|k.entity()),"context":{"type":"Record","attributes":{"operation":{"type":"String"},"rank":{"type":"Long"},"permission_candidates":{"type":"Set","element":{"type":"String"}}}}}}))).collect::<serde_json::Map<_,_>>();
        let raw = json!({"Objects":{"entityTypes":types,"actions":actions}});
        let schema = Schema::from_json_value(raw.clone())
            .map_err(|error| configuration_error("resource schema", error))?;
        let policies = validated_policies(&schema, POLICY, "resource policy")?;
        let mut point = Self {
            schema,
            policies,
            policy_sha256: sha(format!("{POLICY}\n{raw}")),
            matrix_sha256: String::new(),
            roles,
        };
        point.matrix_sha256 = point.matrix()?;
        Ok(point)
    }
    fn fixtures(&self) -> Vec<(ObjectSubject, ObjectResource, ObjectAction)> {
        let mut cases = fixtures();
        let baseline = cases.clone();
        for role in self.roles.keys().collect::<BTreeSet<_>>() {
            for (mut subject, resource, action) in baseline
                .iter()
                .filter(|(s, _, _)| {
                    s.roles.is_empty()
                        && !s.anonymous
                        && s.tenant_id == "tenant"
                        && s.user_id == "owner"
                        && s.tier == "normal"
                })
                .cloned()
            {
                subject.roles = vec![role.clone()];
                cases.push((subject, resource, action));
            }
        }
        cases
    }
    fn permissions(&self, subject: &ObjectSubject) -> BTreeSet<String> {
        let mut permissions = subject
            .roles
            .iter()
            .chain(&subject.permissions)
            .cloned()
            .collect::<BTreeSet<_>>();
        for role in &subject.roles {
            if let Some(values) = self.roles.get(role) {
                permissions.extend(values.iter().cloned());
            }
        }
        permissions
    }
    pub fn evaluate(
        &self,
        subject: &ObjectSubject,
        resource: &ObjectResource,
        action: ObjectAction,
    ) -> Result<bool, PolicyError> {
        if resource.id.len() > 1024
            || resource.tenant_id.chars().count() > 255
            || subject.tenant_id.chars().count() > 255
            || subject.user_id.chars().count() > 255
            || resource.grants.len() > 10000
        {
            return Err(PolicyError::Evaluation);
        }
        if matches!(
            resource.profile,
            ObjectProfile::AgentListing | ObjectProfile::AgentCreation
        ) && resource.kind != ResourceKind::Agent
            || resource.profile == ObjectProfile::DatasetQuery
                && resource.kind != ResourceKind::Dataset
            || resource.profile == ObjectProfile::ToolVisibility
                && resource.kind != ResourceKind::Tool
            || resource.profile == ObjectProfile::ModelAdministration
                && resource.kind != ResourceKind::Model
            || matches!(
                resource.profile,
                ObjectProfile::UserActor | ObjectProfile::UserMutation
            ) && resource.kind != ResourceKind::User
        {
            return Err(PolicyError::Evaluation);
        }
        let p: EntityUid = "Objects::Subject::\"current\""
            .parse()
            .map_err(|_| PolicyError::Evaluation)?;
        let r: EntityUid = format!("Objects::{}::\"current\"", resource.kind.entity())
            .parse()
            .map_err(|_| PolicyError::Evaluation)?;
        let a: EntityUid = format!("Objects::Action::\"{}\"", action.name())
            .parse()
            .map_err(|_| PolicyError::Evaluation)?;
        let entities=Entities::from_json_value(json!([
 {"uid":{"type":"Objects::Subject","id":"current"},"attrs":{"tenant":subject.tenant_id,"tenant_trim":subject.tenant_id.trim(),"user":subject.user_id,"user_trim":subject.user_id.trim(),"tier":subject.tier,"tier_lower":subject.tier.to_lowercase(),"anonymous":subject.anonymous,"roles":subject.roles.iter().collect::<BTreeSet<_>>(),"folded_roles":subject.roles.iter().map(|s|s.to_lowercase()).collect::<BTreeSet<_>>(),"normalized_roles":subject.roles.iter().map(|s|s.trim().to_lowercase()).collect::<BTreeSet<_>>(),"permissions":self.permissions(subject),"model_admin_permissions":model_admin_permissions(subject),"capability_permissions":super::subjects(&crate::rbac::Rbac::new(self.roles.clone()),&subject.roles,&subject.permissions)},"parents":[]},
 {"uid":{"type":format!("Objects::{}",resource.kind.entity()),"id":"current"},"attrs":{"tenant":resource.tenant_id,"tenant_trim":resource.tenant_id.trim(),"id":resource.id,"owner":resource.owner_id,"visibility":resource.visibility.to_lowercase(),"access_level":resource.access_level,"profile":profile(resource.profile),"present":resource.present,"enabled":resource.enabled,"protected":resource.protected,"current_binding":resource.current_binding,"archived":resource.archived,"identity_current":resource.identity_current,"grants":resource.grants.iter().collect::<BTreeSet<_>>(),"nonempty_grant_count":resource.grants.iter().filter(|grant|!grant.is_empty()).count(),"aliases":resource.aliases,"alias_count":resource.aliases.len(),"required_scopes":resource.required_scopes,"satisfied_scopes":resource.satisfied_scopes,"denied_aliases":resource.denied_aliases,"allowed_aliases":resource.allowed_aliases,"allowlist_mode":resource.allowlist_mode},"parents":[]}
 ]),Some(&self.schema)).map_err(|_|PolicyError::Evaluation)?;
        let context=Context::from_json_value(json!({"operation":action.name(),"rank":action.rank(),"permission_candidates":std::iter::once(&resource.required_permission).chain(&resource.required_permissions).flat_map(|v|candidates(v)).collect::<BTreeSet<_>>()}),Some((&self.schema,&a))).map_err(|_|PolicyError::Evaluation)?;
        let request = Request::new(p, a, r, context, Some(&self.schema))
            .map_err(|_| PolicyError::Evaluation)?;
        let response = Authorizer::new().is_authorized(&request, &self.policies, &entities);
        if response.diagnostics().errors().next().is_some() {
            return Err(PolicyError::Evaluation);
        }
        Ok(response.decision() == Decision::Allow)
    }
    /// A finite, deterministic authorization basis, checked against independent
    /// Rust reference semantics and pinned to the actual owning ACL source bytes.
    fn matrix(&self) -> Result<String, PolicyError> {
        let sources = [
            include_bytes!("resources.rs").as_slice(),
            include_bytes!("../../../platform-service-primitives/src/object_authorization.rs")
                .as_slice(),
            include_bytes!(
                "../../../platform-gateway/src/migration_host/control_plane/a2a_tools.rs"
            )
            .as_slice(),
            include_bytes!("../../../platform-gateway/src/migration_agent/studio.rs").as_slice(),
            include_bytes!("../../../platform-gateway/src/migration_agent/entry.rs").as_slice(),
            include_bytes!("../../../platform-gateway-foundation/src/users_api.rs").as_slice(),
            include_bytes!("../../../platform-gateway-foundation/src/users_store/guard.rs")
                .as_slice(),
            include_bytes!("../../../platform-gateway-foundation/src/model_admin_api.rs")
                .as_slice(),
            include_bytes!(
                "../../../platform-knowledge-domain/src/migration_knowledge_query/store.rs"
            )
            .as_slice(),
            include_bytes!("../../../platform-gateway-foundation/src/tenant_policies_api.rs")
                .as_slice(),
            include_bytes!("../../../platform-gateway-foundation/src/service_access_policy.rs")
                .as_slice(),
            include_bytes!("../../../platform-gateway-foundation/src/mcp_registry_api.rs")
                .as_slice(),
            include_bytes!(
                "../../../platform-knowledge-domain/src/migration_knowledge_ingest/store.rs"
            )
            .as_slice(),
        ];
        let configured = self
            .roles
            .iter()
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut proof = Sha256::new();
        proof.update(&self.policy_sha256);
        proof.update(serde_json::to_vec(&configured).map_err(|_| PolicyError::Configuration)?);
        for source in sources {
            proof.update(Sha256::digest(source));
        }
        let proof = format!("{:x}", proof.finalize());
        static CACHE: std::sync::OnceLock<std::sync::Mutex<HashMap<String, String>>> =
            std::sync::OnceLock::new();
        let cache = CACHE.get_or_init(Default::default);
        if let Some(matrix) = cache
            .lock()
            .map_err(|_| PolicyError::Configuration)?
            .get(&proof)
            .cloned()
        {
            return Ok(matrix);
        }
        let mut results = Vec::new();
        for (subject, resource, action) in self.fixtures() {
            let legacy = reference(&self.roles, &subject, &resource, action);
            let cedar = self.evaluate(&subject, &resource, action)?;
            if legacy != cedar {
                return Err(PolicyError::MatrixMismatch);
            }
            results.push(json!([subject, resource, action, legacy, cedar]));
        }
        let mut digest = Sha256::new();
        digest.update(&proof);
        digest.update(serde_json::to_vec(&results).map_err(|_| PolicyError::Configuration)?);
        let matrix = format!("{:x}", digest.finalize());
        cache
            .lock()
            .map_err(|_| PolicyError::Configuration)?
            .insert(proof, matrix.clone());
        Ok(matrix)
    }
}
impl DecisionPoint {
    pub fn resource_policy_sha256(&self) -> &str {
        &self.objects.policy_sha256
    }
    pub fn resource_matrix_sha256(&self) -> &str {
        &self.objects.matrix_sha256
    }
    pub fn resource_enforced(&self, kind: ResourceKind) -> bool {
        self.config.mode == Mode::Enforce && self.config.resources.enforce.contains(&kind)
    }
    pub fn evaluate_resource(
        &self,
        subject: &ObjectSubject,
        resource: &ObjectResource,
        action: ObjectAction,
    ) -> Result<bool, PolicyError> {
        self.objects.evaluate(subject, resource, action)
    }
    pub fn decide_resource(
        &self,
        subject: &ObjectSubject,
        resource: &ObjectResource,
        action: ObjectAction,
        legacy: bool,
        scope: &PolicyScope,
    ) -> bool {
        let evaluated = self.objects.evaluate(subject, resource, action);
        let cedar = matches!(evaluated, Ok(true));
        let enforced = self.resource_enforced(resource.kind);
        let mut allowed = if enforced {
            legacy && cedar && self.audit.ready()
        } else {
            legacy
        };
        let resource_audit = ResourceAudit {
            kind: resource.kind,
            action,
            enforced,
            principal_id_sha256: sha(&subject.user_id),
            tenant_id_sha256: sha(&subject.tenant_id),
            resource_id_sha256: sha(&resource.id),
            facts_sha256: sha(serde_json::to_vec(&(subject, resource)).unwrap_or_default()),
            request_id: scope.request_id.chars().take(128).collect(),
            trace_id: scope.trace_id.chars().take(64).collect(),
        };
        let log = DecisionLog {
            decision_id: uuid::Uuid::new_v4().to_string(),
            capability: format!("object:{}:{}", resource.kind.name(), action.name()),
            policy_sha256: self.resource_policy_sha256().into(),
            matrix_sha256: self.resource_matrix_sha256().into(),
            principal_fingerprint: sha(serde_json::to_vec(subject).unwrap_or_default()),
            mode: self.config.mode,
            legacy_allowed: legacy,
            cedar_allowed: cedar,
            enforced_allowed: allowed,
            evaluation_error: evaluated.is_err(),
            resource: Some(resource_audit),
        };
        if self.audit.record(log).is_err() && enforced {
            allowed = false;
        }
        allowed
    }
}

/// Independent reference to the owning ACL contracts. Runtime decisions always
/// compare against the actual caller's legacy result as well as this startup basis.
pub fn reference(
    roles: &HashMap<String, Vec<String>>,
    s: &ObjectSubject,
    r: &ObjectResource,
    a: ObjectAction,
) -> bool {
    if !r.present {
        return false;
    }
    let folded = s
        .roles
        .iter()
        .map(|v| v.to_lowercase())
        .collect::<BTreeSet<_>>();
    let normalized = s
        .roles
        .iter()
        .map(|v| v.trim().to_lowercase())
        .collect::<BTreeSet<_>>();
    let admin = s.tier.eq_ignore_ascii_case("admin")
        || folded.contains("admin")
        || folded.contains("tenant_admin");
    let same = s.tenant_id == r.tenant_id;
    let permission = |trim: bool| {
        let mut values = BTreeSet::new();
        for raw in s.roles.iter().chain(&s.permissions) {
            let role = if trim { raw.trim() } else { raw.as_str() };
            values.insert(role.to_owned());
            if let Some(p) = roles.get(role) {
                values.extend(p.iter().cloned());
            }
        }
        let expected = std::iter::once(&r.required_permission)
            .chain(&r.required_permissions)
            .flat_map(|v| candidates(v))
            .collect::<BTreeSet<_>>();
        !values.is_disjoint(&expected)
    };
    let rank = |p: &str| match p {
        "viewer" => 1,
        "editor" => 2,
        "owner" => 3,
        _ => 0,
    };
    match r.kind {
        ResourceKind::Tenant => {
            normalized
                .iter()
                .any(|v| ["platform_admin", "superadmin", "super_admin"].contains(&v.as_str()))
                || same
                    && !s.tenant_id.is_empty()
                    && (s.roles.iter().any(|r| r == "admin")
                        || s.tier.eq_ignore_ascii_case("admin"))
        }
        ResourceKind::User => {
            same && !s.user_id.trim().is_empty()
                && !s.tenant_id.trim().is_empty()
                && match r.profile {
                    ObjectProfile::UserActor => {
                        s.user_id == r.id
                            && (a == ObjectAction::Profile
                                || r.current_binding
                                    && s.roles
                                        .iter()
                                        .any(|v| v == "admin" || v == "platform_admin"))
                    }
                    ObjectProfile::UserMutation => {
                        if a == ObjectAction::Profile {
                            s.user_id == r.id
                        } else {
                            s.user_id != r.id
                                && !r.protected
                                && s.roles
                                    .iter()
                                    .any(|v| v == "admin" || v == "platform_admin")
                        }
                    }
                    _ => {
                        if a == ObjectAction::Profile {
                            s.user_id == r.id
                        } else {
                            permission(false)
                                && (a == ObjectAction::Read
                                    || s.roles
                                        .iter()
                                        .any(|v| v == "admin" || v == "platform_admin"))
                        }
                    }
                }
        }
        ResourceKind::Dataset => {
            if r.archived || !r.identity_current {
                return false;
            }
            let visibility = r.visibility.to_lowercase();
            let granted = if s.anonymous {
                if visibility == "public" { 1 } else { 0 }
            } else if !s.user_id.is_empty() && s.user_id == r.owner_id
                || !s.tenant_id.trim().is_empty()
                    && s.tenant_id.trim() == r.tenant_id.trim()
                    && (s.tier == "admin" || s.roles.iter().any(|v| v == "admin"))
            {
                3
            } else if r.grants.iter().any(|v| {
                if r.profile == ObjectProfile::DatasetQuery {
                    rank(v) > 0
                } else {
                    !v.is_empty()
                }
            }) {
                r.grants.iter().map(|v| rank(v)).max().unwrap_or(0)
            } else if visibility == "public"
                || visibility == "tenant"
                    && !s.tenant_id.trim().is_empty()
                    && s.tenant_id.trim() == r.tenant_id.trim()
            {
                1
            } else {
                0
            };
            granted >= a.rank()
        }
        ResourceKind::Agent => {
            same && if r.profile == ObjectProfile::AgentCreation {
                r.current_binding && s.user_id == r.owner_id
            } else {
                admin
                    || r.profile == ObjectProfile::AgentListing && r.current_binding
                    || r.grants.iter().map(|v| rank(v)).max().unwrap_or(0) >= a.rank()
            }
        }
        ResourceKind::Tool => {
            same && if r.profile == ObjectProfile::ToolVisibility {
                !r.aliases.is_empty()
                    && r.required_scopes.is_subset(&r.satisfied_scopes)
                    && (s.roles.iter().any(|v| v == "admin")
                        || r.denied_aliases.is_disjoint(&r.aliases)
                            && (!r.allowlist_mode || !r.allowed_aliases.is_disjoint(&r.aliases)))
            } else {
                !s.anonymous
                    && !s.user_id.is_empty()
                    && !s.tenant_id.is_empty()
                    && s.tenant_id != "public"
                    && permission(true)
                    && (a == ObjectAction::Read
                        || folded.iter().any(|v| {
                            ["admin", "tenant_admin", "superadmin", "super_admin"]
                                .contains(&v.as_str())
                        }))
            }
        }
        ResourceKind::Model => {
            same && r.enabled
                && if r.profile == ObjectProfile::ModelAdministration {
                    {
                        let mut values = s
                            .roles
                            .iter()
                            .map(|v| v.trim().to_owned())
                            .filter(|v| !v.is_empty())
                            .collect::<BTreeSet<_>>();
                        if values.contains("admin") {
                            values.insert("admin:*".into());
                        }
                        let expected = std::iter::once(&r.required_permission)
                            .chain(&r.required_permissions)
                            .flat_map(|v| candidates(v))
                            .collect::<BTreeSet<_>>();
                        !values.is_disjoint(&expected)
                    }
                } else {
                    match r.access_level.as_str() {
                        "public" => true,
                        "premium" => {
                            admin
                                || ["premium", "enterprise"]
                                    .contains(&s.tier.to_lowercase().as_str())
                        }
                        "admin" => admin,
                        _ => false,
                    }
                }
        }
    }
}
pub fn fixtures() -> Vec<(ObjectSubject, ObjectResource, ObjectAction)> {
    let base = ObjectSubject {
        tenant_id: "tenant".into(),
        user_id: "owner".into(),
        tier: "normal".into(),
        ..Default::default()
    };
    let mut subjects = vec![base.clone()];
    for role in [
        "admin",
        "Admin",
        "tenant_admin",
        "platform_admin",
        " PLATFORM_ADMIN ",
        "superadmin",
        "unknown",
        "admin:*",
        "console:models:edit",
        " admin ",
    ] {
        let mut s = base.clone();
        s.roles = vec![role.into()];
        subjects.push(s);
    }
    for tier in ["premium", "enterprise", "admin", "ADMIN"] {
        let mut s = base.clone();
        s.tier = tier.into();
        subjects.push(s);
    }
    for tenant in ["other", ""] {
        let mut s = base.clone();
        s.tenant_id = tenant.into();
        subjects.push(s);
    }
    let mut s = base.clone();
    s.anonymous = true;
    subjects.push(s);
    let mut s = base.clone();
    s.user_id = String::new();
    subjects.push(s);
    let mut s = base.clone();
    s.permissions = vec!["admin:*".into()];
    subjects.push(s);
    let mut cases = vec![];
    for kind in ResourceKind::ALL {
        let mut resources = vec![];
        let mut base = ObjectResource::new(kind, "owner", "tenant");
        base.required_permission = match kind {
            ResourceKind::User => "user:list",
            ResourceKind::Tool => "console:mcp:view",
            ResourceKind::Model => "console:models:edit",
            _ => "",
        }
        .into();
        base.access_level = "public".into();
        resources.push(base.clone());
        let mut r = base.clone();
        r.present = false;
        resources.push(r);
        let mut r = base.clone();
        r.tenant_id = "other".into();
        resources.push(r);
        for grant in ["viewer", "editor", "owner", "invalid", ""] {
            let mut r = base.clone();
            r.grants = vec![grant.into()];
            resources.push(r);
        }
        for visibility in ["public", "tenant", "private"] {
            let mut r = base.clone();
            r.visibility = visibility.into();
            resources.push(r);
        }
        let mut r = base.clone();
        r.owner_id = "owner".into();
        resources.push(r);
        let mut r = base.clone();
        r.archived = true;
        r.visibility = "public".into();
        resources.push(r);
        if kind == ResourceKind::Dataset {
            let mut r = base.clone();
            r.profile = ObjectProfile::DatasetQuery;
            r.visibility = "public".into();
            r.grants = vec!["invalid".into()];
            resources.push(r);
            let mut r = base.clone();
            r.identity_current = false;
            r.visibility = "public".into();
            resources.push(r);
        }
        if kind == ResourceKind::Agent {
            for profile in [ObjectProfile::AgentListing, ObjectProfile::AgentCreation] {
                for current in [false, true] {
                    let mut r = base.clone();
                    r.profile = profile;
                    r.current_binding = current;
                    r.owner_id = "owner".into();
                    resources.push(r);
                }
            }
        }
        if kind == ResourceKind::User {
            for profile in [ObjectProfile::UserActor, ObjectProfile::UserMutation] {
                for protected in [false, true] {
                    let mut r = base.clone();
                    r.profile = profile;
                    r.protected = protected;
                    resources.push(r.clone());
                    r.current_binding = false;
                    resources.push(r);
                }
            }
        }
        if kind == ResourceKind::Model {
            let mut r = base.clone();
            r.enabled = false;
            resources.push(r);
            for access in ["premium", "admin", "unknown"] {
                let mut r = base.clone();
                r.access_level = access.into();
                resources.push(r);
            }
            let mut r = base.clone();
            r.profile = ObjectProfile::ModelAdministration;
            resources.push(r);
        }
        if kind == ResourceKind::Tool {
            for blocked in [false, true] {
                let mut r = base.clone();
                r.profile = ObjectProfile::ToolVisibility;
                r.aliases.insert("tool".into());
                r.required_scopes.insert("key".into());
                if !blocked {
                    r.satisfied_scopes.insert("key".into());
                }
                r.allowlist_mode = true;
                r.allowed_aliases.insert("tool".into());
                resources.push(r.clone());
                r.denied_aliases.insert("tool".into());
                resources.push(r);
            }
        }
        for s in &subjects {
            for r in &resources {
                for a in ObjectAction::ALL {
                    cases.push((s.clone(), r.clone(), a));
                }
            }
        }
    }
    cases
}
#[cfg(test)]
mod tests;
