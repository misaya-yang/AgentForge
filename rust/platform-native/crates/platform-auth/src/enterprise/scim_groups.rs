//! SCIM groups project only operator-approved roles into existing user_roles.
// SPDX-License-Identifier: Apache-2.0
use super::*;
impl EnterpriseService {
    pub(in crate::enterprise) async fn scim_groups(
        &self,
        a: &ScimActor,
        start: u64,
        count: u64,
    ) -> Result<Value> {
        a.require("scim.groups.read")?;
        let start = start.max(1);
        if start > i64::MAX as u64 {
            return Err(EnterpriseError::Invalid);
        }
        let total: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM gateway.enterprise_scim_groups WHERE tenant_id=$1",
        )
        .bind(&a.tenant)
        .fetch_one(&self.pool)
        .await?;
        let ids:Vec<String>=sqlx::query_scalar("SELECT group_id::text FROM gateway.enterprise_scim_groups WHERE tenant_id=$1 ORDER BY group_id OFFSET $2 LIMIT $3").bind(&a.tenant).bind(start as i64-1).bind(count.min(100) as i64).fetch_all(&self.pool).await?;
        let mut resources = Vec::new();
        for id in ids {
            resources.push(self.scim_group(a, &id).await?);
        }
        Ok(
            json!({"schemas":["urn:ietf:params:scim:api:messages:2.0:ListResponse"],"totalResults":total,"startIndex":start,"itemsPerPage":resources.len(),"Resources":resources}),
        )
    }
    pub(in crate::enterprise) async fn scim_group(&self, a: &ScimActor, id: &str) -> Result<Value> {
        if !a
            .scopes
            .iter()
            .any(|s| s == "scim.groups.read" || s == "scim.groups.write")
        {
            return Err(EnterpriseError::Forbidden);
        }
        uuid::Uuid::parse_str(id).map_err(|_| EnterpriseError::NotFound)?;
        let row=sqlx::query("SELECT external_id,display_name,version FROM gateway.enterprise_scim_groups WHERE tenant_id=$1 AND group_id=$2::text::uuid").bind(&a.tenant).bind(id).fetch_optional(&self.pool).await?.ok_or(EnterpriseError::NotFound)?;
        let members:Vec<String>=sqlx::query_scalar("SELECT user_id FROM gateway.enterprise_scim_members WHERE tenant_id=$1 AND group_id=$2::text::uuid ORDER BY user_id").bind(&a.tenant).bind(id).fetch_all(&self.pool).await?;
        Ok(
            json!({"schemas":[GROUP_SCHEMA],"id":id,"externalId":row.try_get::<String,_>("external_id")?,"displayName":row.try_get::<String,_>("display_name")?,"members":members.into_iter().map(|value|json!({"value":value})).collect::<Vec<_>>(),"meta":{"resourceType":"Group","version":format!("W/\"{}\"",row.try_get::<i64,_>("version")?)}}),
        )
    }
    pub(in crate::enterprise) async fn scim_write_group(
        &self,
        a: &ScimActor,
        id: Option<&str>,
        input: GroupInput,
        expected: Option<&str>,
    ) -> Result<Value> {
        a.require("scim.groups.write")?;
        if input.schemas != [GROUP_SCHEMA] || input.members.len() > 10000 {
            return Err(EnterpriseError::Invalid);
        }
        bounded(&input.external_id, 255)?;
        bounded(&input.display_name, 255)?;
        let id = id
            .map(str::to_owned)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        uuid::Uuid::parse_str(&id).map_err(|_| EnterpriseError::Invalid)?;
        let mut tx = self.pool.begin().await?;
        // Serialize every managed grant recomputation per tenant to preserve the
        // union when two provisioning groups map onto the same existing role.
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("enterprise-scim:{}", a.tenant))
            .execute(&mut *tx)
            .await?;
        let current:Option<i64>=sqlx::query_scalar("SELECT version FROM gateway.enterprise_scim_groups WHERE tenant_id=$1 AND group_id=$2::text::uuid FOR UPDATE").bind(&a.tenant).bind(&id).fetch_optional(&mut *tx).await?;
        if let Some(expected) = expected {
            if current != Some(parse_version(expected)?) {
                return Err(EnterpriseError::Precondition);
            }
        }
        let old:Vec<String>=sqlx::query_scalar("SELECT user_id FROM gateway.enterprise_scim_members WHERE tenant_id=$1 AND group_id=$2::text::uuid").bind(&a.tenant).bind(&id).fetch_all(&mut *tx).await?;
        sqlx::query("INSERT INTO gateway.enterprise_scim_groups(tenant_id,group_id,external_id,display_name) VALUES($1,$2::text::uuid,$3,$4) ON CONFLICT(tenant_id,group_id) DO UPDATE SET external_id=EXCLUDED.external_id,display_name=EXCLUDED.display_name,version=enterprise_scim_groups.version+1").bind(&a.tenant).bind(&id).bind(&input.external_id).bind(input.display_name).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM gateway.enterprise_scim_members WHERE tenant_id=$1 AND group_id=$2::text::uuid").bind(&a.tenant).bind(&id).execute(&mut *tx).await?;
        let mut affected = std::collections::BTreeSet::from_iter(old);
        for member in input.members {
            bounded(&member.value, 255)?;
            if sqlx::query("INSERT INTO gateway.enterprise_scim_members(tenant_id,group_id,user_id) SELECT $1,$2::text::uuid,$3 WHERE EXISTS(SELECT 1 FROM gateway.enterprise_scim_users WHERE tenant_id=$1 AND user_id=$3 AND NOT deleted) ON CONFLICT DO NOTHING").bind(&a.tenant).bind(&id).bind(&member.value).execute(&mut *tx).await?.rows_affected()!=1{return Err(EnterpriseError::Invalid);}
            affected.insert(member.value);
        }
        for user in affected {
            sync_roles(&mut tx, &a.tenant, &user).await?;
        }
        tx.commit().await?;
        self.scim_group(a, &id).await
    }
    pub(in crate::enterprise) async fn scim_delete_group(
        &self,
        a: &ScimActor,
        id: &str,
        expected: Option<&str>,
    ) -> Result<()> {
        a.require("scim.groups.write")?;
        uuid::Uuid::parse_str(id).map_err(|_| EnterpriseError::NotFound)?;
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("enterprise-scim:{}", a.tenant))
            .execute(&mut *tx)
            .await?;
        let version:Option<i64>=sqlx::query_scalar("SELECT version FROM gateway.enterprise_scim_groups WHERE tenant_id=$1 AND group_id=$2::text::uuid FOR UPDATE").bind(&a.tenant).bind(id).fetch_optional(&mut *tx).await?;
        let version = version.ok_or(EnterpriseError::NotFound)?;
        if expected
            .map(parse_version)
            .transpose()?
            .is_some_and(|v| v != version)
        {
            return Err(EnterpriseError::Precondition);
        }
        let members:Vec<String>=sqlx::query_scalar("SELECT user_id FROM gateway.enterprise_scim_members WHERE tenant_id=$1 AND group_id=$2::text::uuid").bind(&a.tenant).bind(id).fetch_all(&mut *tx).await?;
        sqlx::query("DELETE FROM gateway.enterprise_scim_groups WHERE tenant_id=$1 AND group_id=$2::text::uuid").bind(&a.tenant).bind(id).execute(&mut *tx).await?;
        for user in members {
            sync_roles(&mut tx, &a.tenant, &user).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
async fn sync_roles(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant: &str,
    user: &str,
) -> Result<()> {
    let tag = format!("enterprise-scim:{tenant}");
    let desired:Vec<String>=sqlx::query_scalar("SELECT DISTINCT r.role_name FROM gateway.enterprise_scim_members m JOIN gateway.enterprise_scim_groups g USING(tenant_id,group_id) JOIN gateway.enterprise_scim_role_map r ON r.tenant_id=g.tenant_id AND r.external_group_id=g.external_id WHERE m.tenant_id=$1 AND m.user_id=$2").bind(tenant).bind(user).fetch_all(&mut **tx).await?;
    sqlx::query("DELETE FROM gateway.user_roles WHERE user_id=$1 AND granted_by=$2 AND NOT(role_name=ANY($3))").bind(user).bind(&tag).bind(&desired).execute(&mut **tx).await?;
    for role in desired {
        sqlx::query("INSERT INTO gateway.user_roles(user_id,role_name,granted_by) VALUES($1,$2,$3) ON CONFLICT(user_id,role_name) DO NOTHING").bind(user).bind(role).bind(&tag).execute(&mut **tx).await?;
    }
    sqlx::query(
        "UPDATE gateway.users SET auth_epoch=auth_epoch+1 WHERE tenant_id=$1 AND user_id=$2",
    )
    .bind(tenant)
    .bind(user)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
