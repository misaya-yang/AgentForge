//! Approval-first Skill proposals and audit records; never activates a Skill.
use super::{ContentDatabase, ContentError, skills::validate_manifest};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillProposalResult {
    pub proposal_id: String,
    pub status: String,
    pub reason: String,
}
#[derive(Default, Clone)]
pub struct SkillBuilder {
    pub database: Option<ContentDatabase>,
}
impl SkillBuilder {
    pub fn new(database: Option<ContentDatabase>) -> Self {
        Self { database }
    }
    pub fn validate_manifest(&self, manifest: &Value) -> Vec<String> {
        validate_manifest(manifest)
    }
    pub async fn propose_skill(
        &self,
        tenant: &str,
        user: &str,
        manifest: &Value,
        proposed_by: &str,
    ) -> Result<SkillProposalResult, ContentError> {
        let errors = self.validate_manifest(manifest);
        let proposal_id = uuid::Uuid::new_v4().to_string();
        if !errors.is_empty() {
            return Ok(SkillProposalResult {
                proposal_id,
                status: "rejected".into(),
                reason: errors.join("; "),
            });
        }
        if let Some(db) = &self.database {
            let (tenant, user, actor, resource, payload) = (
                tenant.to_owned(),
                user.to_owned(),
                proposed_by.to_owned(),
                proposal_id.clone(),
                manifest.clone(),
            );
            db.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO assistant_audit_events (event_id,tenant_id,user_id,event_type,actor_id,resource_type,resource_id,payload,created_at) VALUES($1,$2,$3,'skill_proposed',$4,'skill',$5,$6,NOW())").bind(uuid::Uuid::new_v4().to_string()).bind(tenant).bind(user).bind(actor).bind(resource).bind(payload).execute(c).await?;Ok(())})).await?;
        }
        Ok(SkillProposalResult {
            proposal_id,
            status: "pending_approval".into(),
            reason: "Skill proposal submitted for approval".into(),
        })
    }
    pub async fn mark_approved(
        &self,
        tenant: &str,
        user: &str,
        proposal_id: &str,
        approved_by: &str,
        reason: Option<&str>,
    ) -> Result<(), ContentError> {
        let Some(db) = &self.database else {
            return Ok(());
        };
        let (tenant, user, actor, resource) = (
            tenant.to_owned(),
            user.to_owned(),
            approved_by.to_owned(),
            proposal_id.to_owned(),
        );
        let payload = json!({"reason":reason,"approved_at":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros,false)});
        db.run(move|c|Box::pin(async move{sqlx::query("INSERT INTO assistant_audit_events(event_id,tenant_id,user_id,event_type,actor_id,resource_type,resource_id,payload,created_at) VALUES($1,$2,$3,'skill_approved',$4,'skill',$5,$6,NOW())").bind(uuid::Uuid::new_v4().to_string()).bind(tenant).bind(user).bind(actor).bind(resource).bind(payload).execute(c).await?;Ok(())})).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn proposals_never_imply_activation() {
        let b = SkillBuilder::default();
        let manifest =
            json!({"name":"demo","entrypoint":"md://demo","description":"Demo","permissions":[]});
        let r = b.propose_skill("t", "u", &manifest, "u").await.unwrap();
        assert_eq!(r.status, "pending_approval");
        assert!(uuid::Uuid::parse_str(&r.proposal_id).is_ok());
        let mut dangerous = manifest;
        dangerous["permissions"] = json!(["exec:command"]);
        assert_eq!(
            b.propose_skill("t", "u", &dangerous, "u")
                .await
                .unwrap()
                .status,
            "rejected"
        );
        b.mark_approved("t", "u", "p", "reviewer", None)
            .await
            .unwrap();
    }
}
