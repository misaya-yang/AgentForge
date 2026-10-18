use super::*;
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateConnection {
    pub request_id: uuid::Uuid,
    pub name: String,
    pub domain: String,
    pub email: String,
    pub api_token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceCredentials {
    pub expected_revision: String,
    pub expected_status: String,
    pub email: String,
    pub api_token: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControlConnection {
    pub expected_revision: String,
    pub expected_status: String,
    pub status: String,
}
fn valid_secret(email: &str, token: &str) -> bool {
    ![email, token]
        .iter()
        .any(|s| s.is_empty() || s.chars().count() > 4096 || s.chars().any(|c| (c as u32) < 32))
        && email.chars().count() <= 320
        && !email.contains(':')
        && email.contains('@')
}
fn valid_revision(rev: &str, status: &str) -> bool {
    !rev.is_empty() && rev.chars().count() <= 80 && ["active", "disabled"].contains(&status)
}
pub fn receipt(row: &Value) -> Value {
    pick(
        row,
        &["connection_id", "name", "domain", "status", "sync_mode"],
    )
}
fn revision(row: &Value) -> String {
    let value = text(row, "updated_at");
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|dt| {
            dt.to_rfc3339_opts(
                if dt.timestamp_subsec_micros() == 0 {
                    chrono::SecondsFormat::Secs
                } else {
                    chrono::SecondsFormat::Micros
                },
                false,
            )
        })
        .unwrap_or_else(|_| value.into())
}
fn control_receipt(row: &Value) -> Value {
    let mut v = receipt(row);
    v["revision"] = json!(revision(row));
    v
}
fn metadata_columns() -> &'static str {
    "connection_id,tenant_id,owner_id,created_by,name,domain,status,sync_mode,updated_at"
}
impl Sources {
    async fn connection_metadata(&self, id: &str) -> Result<Value> {
        let sql = format!(
            "SELECT to_jsonb(c) FROM(SELECT {} FROM confluence_connections WHERE connection_id=$1)c",
            metadata_columns()
        );
        sqlx::query_scalar(&sql)
            .bind(id)
            .fetch_optional(ai_platform_knowledge_domain::database::executor(&self.pool))
            .await?
            .ok_or_else(|| Error::missing("Connection receipt is unavailable"))
    }
    pub async fn connection_status(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        id: &str,
    ) -> Result<Value> {
        let actor = self.actor(user, dataset, false, false).await?;
        let row = self.connection_metadata(id).await?;
        if !owned(&row, &actor) {
            return Err(Error::missing("Connection receipt is unavailable"));
        }
        Ok(receipt(&row))
    }
    fn existing_connection(
        &self,
        row: &Value,
        actor: &query::types::UserContext,
        input: &CreateConnection,
    ) -> Result<Value> {
        if !owned(row, actor)
            || text(row, "name") != input.name
            || text(row, "domain") != input.domain
            || row["status"] != "active"
            || row["sync_mode"] != "manual"
            || row["email"] != credentials::MARKER
        {
            return Err(Error::conflict(
                "Connection request identity is already in use",
            ));
        }
        let creds = credentials::open(row, &self.encryption_key)?;
        // Constant-time compare prevents revealing a matching secret prefix.
        fn equals(a: &str, b: &str) -> bool {
            let mut diff = a.len() ^ b.len();
            for (i, x) in a.bytes().enumerate() {
                diff |= (x ^ b.as_bytes().get(i).copied().unwrap_or(0)) as usize;
            }
            diff == 0
        }
        if !equals(&creds.email, &input.email) || !equals(&creds.token, &input.api_token) {
            return Err(Error::conflict(
                "Connection request identity is already in use",
            ));
        }
        Ok(receipt(row))
    }
    pub async fn create_connection(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        input: CreateConnection,
    ) -> Result<Value> {
        if input.name.trim().is_empty()
            || input.name.chars().count() > 255
            || input.name.chars().any(|c| (c as u32) < 32)
            || !client::cloud_domain(&input.domain)
            || !valid_secret(&input.email, &input.api_token)
        {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid connection fields; use an Atlassian Cloud hostname, account email and API token",
            ));
        }
        let actor = self.actor(user, dataset, true, true).await?;
        if self.encryption_key.trim().is_empty() {
            return Err(Error::unavailable(
                "Configure GATEWAY_ENCRYPTION_KEY for Knowledge API and workers before saving connections",
            ));
        }
        let id = input.request_id.to_string();
        if let Some(row) = self.connection(&id).await? {
            return self.existing_connection(&row, &actor, &input);
        }
        let row = json!({"connection_id":id,"tenant_id":actor.tenant_id,"owner_id":actor.user_id,"created_by":actor.user_id,"domain":input.domain});
        let encrypted =
            credentials::seal(&row, &input.email, &input.api_token, &self.encryption_key)?;
        self.verify(&input.domain, &input.email, &input.api_token)
            .await?;
        let actor = self.actor(user, dataset, true, true).await?;
        let mut tx = self.pool.begin().await?;
        ai_platform_knowledge_domain::database::bind_current(&mut tx).await?;
        locked_dataset(&mut tx, &actor, dataset, true).await?;
        let saved:Option<Value>=sqlx::query_scalar("WITH saved AS(INSERT INTO confluence_connections(connection_id,tenant_id,name,domain,email,api_token,sync_mode,status,created_by,owner_id) VALUES($1,$2,$3,$4,$5,$6,'manual','active',$7,$7) ON CONFLICT DO NOTHING RETURNING connection_id,name,domain,status,sync_mode) SELECT to_jsonb(saved) FROM saved").bind(&id).bind(&actor.tenant_id).bind(&input.name).bind(&input.domain).bind(credentials::MARKER).bind(encrypted).bind(&actor.user_id).fetch_optional(&mut *tx).await?;
        let result = match saved {
            Some(row) => receipt(&row),
            None => {
                let prior: Value = sqlx::query_scalar(
                    "SELECT to_jsonb(c) FROM confluence_connections c WHERE connection_id=$1",
                )
                .bind(&id)
                .fetch_one(&mut *tx)
                .await?;
                self.existing_connection(&prior, &actor, &input)?
            }
        };
        tx.commit().await?;
        Ok(result)
    }
    pub async fn managed(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        offset: i64,
    ) -> Result<Value> {
        let actor = self.actor(user, dataset, true, false).await?;
        let sql = format!(
            "SELECT to_jsonb(c) FROM(SELECT {} FROM confluence_connections WHERE tenant_id=$1 AND COALESCE(NULLIF(owner_id,''),created_by)=$2 ORDER BY connection_id LIMIT 51 OFFSET $3)c",
            metadata_columns()
        );
        let rows: Vec<Value> = sqlx::query_scalar(&sql)
            .bind(&actor.tenant_id)
            .bind(&actor.user_id)
            .bind(offset)
            .fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool))
            .await?;
        Ok(
            json!({"connections":rows.iter().take(50).map(control_receipt).collect::<Vec<_>>(),"next_offset":if rows.len()>50{Some(offset+50)}else{None}}),
        )
    }
    pub async fn impact(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        id: &str,
        offset: i64,
    ) -> Result<Value> {
        let actor = self.actor(user, dataset, false, false).await?;
        let row = self.connection_metadata(id).await?;
        if !owned(&row, &actor) {
            return Err(Error::missing("Connection controls are unavailable"));
        }
        let count:i64=sqlx::query_scalar("SELECT COUNT(*) FROM confluence_space_bindings WHERE connection_id=$1 AND tenant_id=$2").bind(id).bind(&actor.tenant_id).fetch_one(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        let rows:Vec<Value>=sqlx::query_scalar("SELECT to_jsonb(b) FROM(SELECT binding_id,dataset_id,owner_id,created_by,space_name,status FROM confluence_space_bindings WHERE connection_id=$1 AND tenant_id=$2 ORDER BY binding_id LIMIT 51 OFFSET $3)b").bind(id).bind(&actor.tenant_id).bind(offset).fetch_all(ai_platform_knowledge_domain::database::executor(&self.pool)).await?;
        let mut visible = Vec::new();
        let mut hidden = 0;
        for binding in rows.iter().take(50) {
            if owner(binding) != actor.user_id {
                hidden += 1;
                continue;
            }
            match self
                .store
                .require_dataset_access(&actor, text(binding, "dataset_id"), "viewer")
                .await
            {
                Ok(ds) if ds.tenant_id == actor.tenant_id => visible.push(pick(
                    binding,
                    &["binding_id", "dataset_id", "space_name", "status"],
                )),
                _ => hidden += 1,
            }
        }
        Ok(
            json!({"connection":control_receipt(&row),"binding_count":count,"bindings":visible,"hidden_count":hidden,"next_offset":if rows.len()>50{Some(offset+50)}else{None}}),
        )
    }
    pub async fn reconnect(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        id: &str,
        input: ReplaceCredentials,
    ) -> Result<Value> {
        if !valid_secret(&input.email, &input.api_token)
            || !valid_revision(&input.expected_revision, &input.expected_status)
        {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid replacement credential fields",
            ));
        }
        let actor = self.actor(user, dataset, true, false).await?;
        let row = self.connection_metadata(id).await?;
        if !owned(&row, &actor) {
            return Err(Error::missing("Connection controls are unavailable"));
        }
        if revision(&row) != input.expected_revision
            || text(&row, "status") != input.expected_status
        {
            return Err(Error::conflict(
                "Connection changed; read current impact before deciding again",
            ));
        }
        if !client::cloud_domain(text(&row, "domain")) {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Replacement requires an Atlassian Cloud hostname",
            ));
        }
        credentials::seal(&row, &input.email, &input.api_token, &self.encryption_key)?;
        self.verify(text(&row, "domain"), &input.email, &input.api_token)
            .await?;
        self.change_connection(
            user,
            dataset,
            id,
            &input.expected_revision,
            &input.expected_status,
            None,
            Some((&input.email, &input.api_token)),
        )
        .await
    }
    pub async fn control(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        id: &str,
        input: ControlConnection,
    ) -> Result<Value> {
        if !valid_revision(&input.expected_revision, &input.expected_status)
            || !["active", "disabled"].contains(&input.status.as_str())
        {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid connection control fields",
            ));
        }
        self.change_connection(
            user,
            dataset,
            id,
            &input.expected_revision,
            &input.expected_status,
            Some(&input.status),
            None,
        )
        .await
    }
    async fn change_connection(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        id: &str,
        expected_revision: &str,
        expected_status: &str,
        status: Option<&str>,
        replacement: Option<(&str, &str)>,
    ) -> Result<Value> {
        let actor = self.actor(user, dataset, true, false).await?;
        let mut tx = self.pool.begin().await?;
        ai_platform_knowledge_domain::database::bind_current(&mut tx).await?;
        let sql = format!(
            "SELECT to_jsonb(c) FROM(SELECT {} FROM confluence_connections WHERE connection_id=$1 FOR UPDATE)c",
            metadata_columns()
        );
        let row: Value = sqlx::query_scalar(&sql)
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| Error::missing("Connection controls are unavailable"))?;
        if !owned(&row, &actor) {
            return Err(Error::missing("Connection controls are unavailable"));
        }
        locked_dataset(&mut tx, &actor, dataset, false).await?;
        if revision(&row) != expected_revision || text(&row, "status") != expected_status {
            return Err(Error::conflict(
                "Connection changed; read current impact before deciding again",
            ));
        }
        let changed: Option<Value> = if let Some((email, token)) = replacement {
            let encrypted = credentials::seal(&row, email, token, &self.encryption_key)?;
            sqlx::query_scalar("WITH c AS(UPDATE confluence_connections SET email=$2,api_token=$3,updated_at=NOW() WHERE connection_id=$1 AND transaction_timestamp()>updated_at RETURNING connection_id,name,domain,status,sync_mode,updated_at) SELECT to_jsonb(c) FROM c").bind(id).bind(credentials::MARKER).bind(encrypted).fetch_optional(&mut *tx).await?
        } else if status == Some(text(&row, "status")) {
            Some(row)
        } else {
            sqlx::query_scalar("WITH c AS(UPDATE confluence_connections SET status=$2,updated_at=NOW() WHERE connection_id=$1 AND transaction_timestamp()>updated_at RETURNING connection_id,name,domain,status,sync_mode,updated_at) SELECT to_jsonb(c) FROM c").bind(id).bind(status).fetch_optional(&mut *tx).await?
        };
        let row = changed.ok_or_else(|| {
            Error::conflict(
                "Connection revision cannot advance; read current status before retrying",
            )
        })?;
        tx.commit().await?;
        Ok(control_receipt(&row))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn revision_matches_source_datetime_isoformat() {
        assert_eq!(
            revision(&json!({"updated_at":"2026-08-02T12:00:00Z"})),
            "2026-08-02T12:00:00+00:00"
        );
        assert_eq!(
            revision(&json!({"updated_at":"2026-08-02T12:00:00.123Z"})),
            "2026-08-02T12:00:00.123000+00:00"
        );
    }
    #[test]
    fn metadata_receipt_never_contains_credentials() {
        let row = json!({"connection_id":"c","name":"name","domain":"test.atlassian.net","status":"active","sync_mode":"manual","email":"private","api_token":"secret","owner_id":"u"});
        let value = receipt(&row);
        assert_eq!(value.as_object().unwrap().len(), 5);
        assert!(value.get("email").is_none());
        assert!(value.get("api_token").is_none());
    }
}

#[cfg(test)]
#[path = "knowledge_migration_parity.rs"]
mod migration_parity;
