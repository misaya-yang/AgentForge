//! Private authorization facts. These are constructed by the current object
//! authority, never deserialized from an unverified public identity header.
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
pub const DATASET_DECISION_PATH: &str = "/internal/v1/authorization/dataset";
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Tenant,
    User,
    Dataset,
    Agent,
    Tool,
    Model,
}
impl ResourceKind {
    pub const ALL: [Self; 6] = [
        Self::Tenant,
        Self::User,
        Self::Dataset,
        Self::Agent,
        Self::Tool,
        Self::Model,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Tenant => "tenant",
            Self::User => "user",
            Self::Dataset => "dataset",
            Self::Agent => "agent",
            Self::Tool => "tool",
            Self::Model => "model",
        }
    }
    pub fn entity(self) -> &'static str {
        match self {
            Self::Tenant => "Tenant",
            Self::User => "User",
            Self::Dataset => "Dataset",
            Self::Agent => "Agent",
            Self::Tool => "Tool",
            Self::Model => "Model",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ObjectAction {
    Read,
    Write,
    Own,
    Profile,
    Admin,
    Invoke,
}
impl ObjectAction {
    pub const ALL: [Self; 6] = [
        Self::Read,
        Self::Write,
        Self::Own,
        Self::Profile,
        Self::Admin,
        Self::Invoke,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Own => "own",
            Self::Profile => "profile",
            Self::Admin => "admin",
            Self::Invoke => "invoke",
        }
    }
    pub fn rank(self) -> i64 {
        match self {
            Self::Read | Self::Invoke => 1,
            Self::Write | Self::Profile => 2,
            Self::Own | Self::Admin => 3,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ObjectSubject {
    pub tenant_id: String,
    pub user_id: String,
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub tier: String,
    pub anonymous: bool,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObjectProfile {
    #[default]
    Standard,
    DatasetQuery,
    AgentListing,
    AgentCreation,
    ToolVisibility,
    ModelAdministration,
    UserActor,
    UserMutation,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectResource {
    pub kind: ResourceKind,
    pub id: String,
    pub tenant_id: String,
    #[serde(default = "yes")]
    pub present: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
    #[serde(default)]
    pub protected: bool,
    #[serde(default = "yes")]
    pub current_binding: bool,
    #[serde(default = "yes")]
    pub identity_current: bool,
    #[serde(default)]
    pub owner_id: String,
    #[serde(default)]
    pub visibility: String,
    /// Raw grants matching the verified user/tenant roles; not an allowed/rank bit.
    #[serde(default)]
    pub grants: Vec<String>,
    #[serde(default)]
    pub access_level: String,
    #[serde(default)]
    pub profile: ObjectProfile,
    #[serde(default)]
    pub required_permission: String,
    #[serde(default)]
    pub required_permissions: Vec<String>,
    #[serde(default)]
    pub aliases: BTreeSet<String>,
    /// Primitive alias relationships produced by the existing wildcard matcher.
    /// Cedar independently combines scopes, deny precedence, allowlist and admin.
    #[serde(default)]
    pub required_scopes: BTreeSet<String>,
    #[serde(default)]
    pub satisfied_scopes: BTreeSet<String>,
    #[serde(default)]
    pub denied_aliases: BTreeSet<String>,
    #[serde(default)]
    pub allowed_aliases: BTreeSet<String>,
    #[serde(default)]
    pub allowlist_mode: bool,
}
fn yes() -> bool {
    true
}
impl ObjectResource {
    pub fn new(kind: ResourceKind, id: impl Into<String>, tenant: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            tenant_id: tenant.into(),
            present: true,
            archived: false,
            enabled: true,
            protected: false,
            current_binding: true,
            identity_current: true,
            owner_id: String::new(),
            visibility: String::new(),
            grants: vec![],
            access_level: String::new(),
            profile: ObjectProfile::Standard,
            required_permission: String::new(),
            required_permissions: vec![],
            aliases: Default::default(),
            required_scopes: Default::default(),
            satisfied_scopes: Default::default(),
            denied_aliases: Default::default(),
            allowed_aliases: Default::default(),
            allowlist_mode: false,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetDecisionRequest {
    pub subject: ObjectSubject,
    pub resource: ObjectResource,
    pub action: ObjectAction,
    pub legacy_allowed: bool,
    pub request_id: String,
    pub trace_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObjectDecisionReceipt {
    pub kind: ResourceKind,
    pub allowed: bool,
    pub legacy_allowed: bool,
    pub cedar_allowed: bool,
    pub enforced: bool,
    pub policy_sha256: String,
    pub matrix_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRankCheck {
    pub resource: ObjectResource,
    pub legacy_rank: i32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRankBatch {
    pub subject: ObjectSubject,
    pub checks: Vec<DatasetRankCheck>,
    pub request_id: String,
    pub trace_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetRankReceipt {
    pub resource_id: String,
    pub allowed_rank: i32,
    pub legacy_rank: i32,
    pub enforced: bool,
    pub policy_sha256: String,
    pub matrix_sha256: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn facts_cannot_embed_an_allowed_bit_or_an_unknown_resource_kind() {
        let r = ObjectResource::new(ResourceKind::Dataset, "d", "t");
        let mut value = serde_json::to_value(&r).unwrap();
        assert!(serde_json::from_value::<ObjectResource>(value.clone()).is_ok());
        value["allowed"] = true.into();
        assert!(serde_json::from_value::<ObjectResource>(value).is_err());
        let mut value = serde_json::to_value(r).unwrap();
        value["kind"] = "all".into();
        assert!(serde_json::from_value::<ObjectResource>(value).is_err());
    }
}
