//! Membership and tenant checks for one space or item. Missing and forbidden
//! both surface as not found so callers cannot probe another tenant.
use crate::migration_knowledge_query::UserContext;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemberRole {
    Owner,
    Editor,
    Commenter,
    Viewer,
}

impl MemberRole {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "owner" => Self::Owner,
            "editor" => Self::Editor,
            "commenter" => Self::Commenter,
            "viewer" => Self::Viewer,
            _ => return None,
        })
    }

    fn rank(self) -> u8 {
        match self {
            Self::Owner => 4,
            Self::Editor => 3,
            Self::Commenter => 2,
            Self::Viewer => 1,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    View,
    Edit,
    Share,
    Admin,
    Star,
}

pub fn allow_create(user: &UserContext) -> bool {
    active(user)
}

pub fn allow_space(
    user: &UserContext,
    resource_tenant: &str,
    role: Option<MemberRole>,
    visibility: &str,
    action: Action,
) -> bool {
    if !active(user) || user.tenant_id != resource_tenant {
        return false;
    }
    if user.tenant_admin() || granted(user, "knowledge:space:admin") {
        return true;
    }
    let rank = role.map(MemberRole::rank).unwrap_or(0);
    match action {
        Action::View | Action::Star => visibility == "tenant" || rank >= MemberRole::Viewer.rank(),
        Action::Edit | Action::Share => rank >= MemberRole::Editor.rank(),
        Action::Admin => rank >= MemberRole::Owner.rank(),
    }
}

pub fn allow_item(
    user: &UserContext,
    resource_tenant: &str,
    space_role: Option<MemberRole>,
    visibility: &str,
    share: Option<MemberRole>,
    created_by: &str,
    deleted: bool,
    action: Action,
) -> bool {
    if !active(user) || user.tenant_id != resource_tenant {
        return false;
    }
    if deleted && !matches!(action, Action::View | Action::Edit | Action::Admin) {
        return false;
    }
    if user.tenant_admin() || granted(user, "knowledge:space:admin") {
        return true;
    }
    let space_rank = space_role.map(MemberRole::rank).unwrap_or(0);
    let share_rank = share.map(MemberRole::rank).unwrap_or(0);
    let rank = space_rank.max(share_rank);
    if deleted {
        return rank >= MemberRole::Editor.rank() || created_by == user.user_id;
    }
    match action {
        Action::View | Action::Star => {
            visibility == "tenant" || rank >= MemberRole::Viewer.rank() || created_by == user.user_id
        }
        Action::Edit | Action::Share => {
            space_rank >= MemberRole::Editor.rank() || share_rank >= MemberRole::Editor.rank()
        }
        Action::Admin => space_rank >= MemberRole::Owner.rank(),
    }
}

fn active(user: &UserContext) -> bool {
    !user.anonymous() && !user.tenant_id.trim().is_empty() && !user.user_id.trim().is_empty()
}

fn granted(user: &UserContext, permission: &str) -> bool {
    user.roles.iter().any(|role| {
        role == permission || role == "admin:*" || role == "knowledge:*" || role == "knowledge:space:*"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(tenant: &str, id: &str, roles: &[&str]) -> UserContext {
        UserContext {
            user_id: id.into(),
            tenant_id: tenant.into(),
            user_tier: "normal".into(),
            user_type: "user".into(),
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            is_authenticated: true,
        }
    }

    #[test]
    fn private_space_is_hidden_from_other_members_and_tenants() {
        let owner = user("t", "owner", &[]);
        let stranger = user("t", "stranger", &[]);
        let other = user("other", "owner", &[]);
        assert!(allow_space(&owner, "t", Some(MemberRole::Owner), "private", Action::Admin));
        assert!(!allow_space(&stranger, "t", None, "private", Action::View));
        assert!(!allow_space(&other, "t", Some(MemberRole::Owner), "private", Action::View));
        assert!(allow_space(&stranger, "t", None, "tenant", Action::View));
        assert!(!allow_space(&stranger, "t", None, "tenant", Action::Edit));
    }

    #[test]
    fn shared_item_is_visible_without_space_membership_and_deleted_rows_stay_with_editors() {
        let reader = user("t", "reader", &[]);
        assert!(allow_item(
            &reader,
            "t",
            None,
            "private",
            Some(MemberRole::Viewer),
            "author",
            false,
            Action::View
        ));
        assert!(!allow_item(
            &reader,
            "t",
            None,
            "private",
            Some(MemberRole::Viewer),
            "author",
            false,
            Action::Edit
        ));
        assert!(allow_item(
            &reader,
            "t",
            Some(MemberRole::Editor),
            "private",
            None,
            "author",
            true,
            Action::Edit
        ));
        assert!(!allow_item(
            &reader,
            "t",
            Some(MemberRole::Viewer),
            "private",
            None,
            "author",
            true,
            Action::View
        ));
        assert!(allow_item(&reader, "t", None, "private", None, "reader", true, Action::View));
    }

    #[test]
    fn anonymous_callers_cannot_create_or_administer() {
        let mut anonymous = user("t", "anonymous", &[]);
        anonymous.is_authenticated = false;
        assert!(!allow_create(&anonymous));
        assert!(!allow_space(&anonymous, "t", Some(MemberRole::Owner), "tenant", Action::Admin));
        let admin = user("t", "admin", &["admin"]);
        assert!(allow_space(&admin, "t", None, "private", Action::Admin));
    }
}
