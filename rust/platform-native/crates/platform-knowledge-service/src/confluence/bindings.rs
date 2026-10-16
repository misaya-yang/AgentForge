use super::client::scalar;
use super::*;
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBinding {
    pub request_id: uuid::Uuid,
    pub connection_id: String,
    pub space_key: String,
    pub space_id: String,
    pub root_page_id: String,
}
pub fn valid_space(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 255
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_~-".contains(&b))
}
pub fn binding_receipt(row: &Value) -> Value {
    let mut v = pick(
        row,
        &[
            "binding_id",
            "dataset_id",
            "connection_id",
            "space_key",
            "space_id",
            "space_name",
            "root_page_id",
            "root_page_title",
        ],
    );
    v["sync_mode"] = row.get("sync_mode").cloned().unwrap_or(json!("manual"));
    v
}
pub fn connection_scope(row: &Value) -> Value {
    pick(
        row,
        &[
            "connection_id",
            "tenant_id",
            "owner_id",
            "created_by",
            "status",
            "updated_at",
        ],
    )
}
impl Sources {
    pub async fn active_connection(
        &self,
        actor: &query::types::UserContext,
        id: &str,
    ) -> Result<Value> {
        let row = self
            .connection(id)
            .await?
            .ok_or_else(|| Error::missing("Saved connection is unavailable to this owner"))?;
        if !owned(&row, actor) || row["status"] != "active" {
            return Err(Error::missing(
                "Saved connection is unavailable to this owner",
            ));
        }
        Ok(row)
    }
    pub async fn connections(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        offset: i64,
    ) -> Result<Value> {
        let actor = self.actor(user, dataset, true, true).await?;
        let rows:Vec<Value>=sqlx::query_scalar("SELECT jsonb_build_object('connection_id',connection_id,'name',name) FROM confluence_connections WHERE tenant_id=$1 AND COALESCE(NULLIF(owner_id,''),created_by)=$2 AND status='active' ORDER BY connection_id LIMIT 51 OFFSET $3").bind(&actor.tenant_id).bind(&actor.user_id).bind(offset).fetch_all(&self.pool).await?;
        Ok(
            json!({"connections":rows.iter().take(50).collect::<Vec<_>>(),"next_offset":if rows.len()>50{Some(offset+50)}else{None}}),
        )
    }
    pub async fn space(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        id: &str,
        key: &str,
    ) -> Result<Value> {
        if !valid_space(key) {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid space key",
            ));
        }
        let actor = self.actor(user, dataset, true, true).await?;
        let connection = self.active_connection(&actor, id).await?;
        let creds = credentials::open(&connection, &self.encryption_key)?;
        let response = self.provider(&creds, &["spaces"], &[("keys", key)]).await?;
        let space = response["results"]
            .as_array()
            .and_then(|v| v.first())
            .ok_or_else(|| {
                Error::new(
                    StatusCode::BAD_GATEWAY,
                    "Space could not be verified; check the saved connection and space key",
                )
            })?;
        let homepage = scalar(&space["homepageId"]);
        if homepage.is_empty() {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Space has no homepage to use as a source root",
            ));
        }
        let root = self.page(&creds, &homepage).await?;
        let actor = self.actor(user, dataset, true, true).await?;
        let current = self.active_connection(&actor, id).await?;
        if connection_scope(&current) != connection_scope(&connection) {
            return Err(Error::conflict(
                "Saved connection changed; verify the space again",
            ));
        }
        let sid = scalar(&space["id"]);
        if text(space, "key") != key
            || sid.is_empty()
            || space.get("status").is_some_and(|v| v != "current")
            || text(space, "name").is_empty()
            || scalar(&root["spaceId"]) != sid
            || scalar(&root["id"]) != homepage
        {
            return Err(Error::new(
                StatusCode::BAD_GATEWAY,
                "Provider did not return the exact current space",
            ));
        }
        Ok(
            json!({"connection_id":id,"space_key":key,"space_id":sid,"space_name":space["name"],"root_page_id":homepage,"root_page_title":root["title"]}),
        )
    }
    pub async fn binding_status(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        id: &str,
    ) -> Result<Value> {
        let actor = self.actor(user, dataset, false, false).await?;
        let row = self
            .binding(id)
            .await?
            .ok_or_else(|| Error::missing("Binding receipt is unavailable"))?;
        if !owned(&row, &actor)
            || text(&row, "dataset_id") != dataset
            || text(&row, "created_by") != actor.user_id
        {
            return Err(Error::missing("Binding receipt is unavailable"));
        }
        Ok(binding_receipt(&row))
    }
    pub async fn create_binding(
        &self,
        user: &query::types::UserContext,
        dataset: &str,
        input: CreateBinding,
    ) -> Result<Value> {
        if !valid_space(&input.space_key)
            || [&input.connection_id, &input.space_id, &input.root_page_id]
                .iter()
                .any(|s| s.is_empty() || s.chars().count() > 255)
        {
            return Err(Error::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Invalid binding fields",
            ));
        }
        let actor = self.actor(user, dataset, true, true).await?;
        let connection = self.active_connection(&actor, &input.connection_id).await?;
        let id = input.request_id.to_string();
        let same = |row: &Value| {
            owned(row, &actor)
                && text(row, "created_by") == actor.user_id
                && text(row, "dataset_id") == dataset
                && text(row, "connection_id") == input.connection_id
                && text(row, "space_key") == input.space_key
                && text(row, "space_id") == input.space_id
                && text(row, "root_page_id") == input.root_page_id
        };
        if let Some(prior) = self.binding(&id).await? {
            if !same(&prior) {
                return Err(Error::conflict("Binding request identity already used"));
            }
            return Ok(binding_receipt(&prior));
        }
        let observed = self
            .space(user, dataset, &input.connection_id, &input.space_key)
            .await?;
        if text(&observed, "space_id") != input.space_id
            || text(&observed, "root_page_id") != input.root_page_id
        {
            return Err(Error::conflict(
                "Space identity changed; verify the space again",
            ));
        }
        let current_actor = self.actor(user, dataset, true, true).await?;
        let mut tx = self.pool.begin().await?;
        let locked: Option<Value> = sqlx::query_scalar(
            "SELECT to_jsonb(c) FROM confluence_connections c WHERE connection_id=$1 FOR UPDATE",
        )
        .bind(&input.connection_id)
        .fetch_optional(&mut *tx)
        .await?;
        if locked.as_ref().map(connection_scope) != Some(connection_scope(&connection)) {
            return Err(Error::conflict("Saved connection changed before binding"));
        }
        locked_dataset(&mut tx, &current_actor, dataset, true).await?;
        let row:Option<Value>=sqlx::query_scalar("WITH inserted AS(INSERT INTO confluence_space_bindings(binding_id,connection_id,tenant_id,dataset_id,space_key,space_id,space_name,owner_id,created_by,status,sync_mode,sync_enabled,sync_images,include_attachments,include_comments,root_page_id,root_page_title) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$8,'pending','manual',false,false,false,false,$9,$10) ON CONFLICT DO NOTHING RETURNING *) SELECT to_jsonb(inserted) FROM inserted").bind(&id).bind(&input.connection_id).bind(&actor.tenant_id).bind(dataset).bind(&input.space_key).bind(&input.space_id).bind(text(&observed,"space_name")).bind(&actor.user_id).bind(&input.root_page_id).bind(text(&observed,"root_page_title")).fetch_optional(&mut *tx).await?;
        let row = match row {
            Some(row) => row,
            None => {
                let prior: Option<Value> = sqlx::query_scalar(
                    "SELECT to_jsonb(b) FROM confluence_space_bindings b WHERE binding_id=$1",
                )
                .bind(&id)
                .fetch_optional(&mut *tx)
                .await?;
                match prior {
                    Some(row) if same(&row) => row,
                    _ => {
                        return Err(Error::conflict(
                            "Binding already exists or request identity is in use; refresh sources",
                        ));
                    }
                }
            }
        };
        tx.commit().await?;
        Ok(binding_receipt(&row))
    }
}
