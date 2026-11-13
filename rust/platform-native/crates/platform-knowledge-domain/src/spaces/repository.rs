//! SQL for spaces. Every statement is parameterized and tenant-scoped.
use super::authorize::{self, Action, MemberRole};
use super::model::*;
use crate::database::{self, bind_current};
use crate::migration_knowledge_query::UserContext;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct Spaces {
    pool: PgPool,
}

struct SpaceRecord {
    space_id: String,
    tenant_id: String,
    name: String,
    icon: String,
    kind: String,
    visibility: String,
    dataset_id: String,
    created_by: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    role: Option<String>,
}

struct ItemRecord {
    item_id: String,
    tenant_id: String,
    space_id: String,
    space_name: String,
    space_icon: String,
    space_visibility: String,
    parent_id: Option<String>,
    kind: String,
    title: String,
    icon: String,
    position: String,
    source: String,
    created_by: String,
    updated_by: String,
    content_format: String,
    current_revision: i64,
    last_activity_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    deleted_at: Option<DateTime<Utc>>,
    starred: bool,
    shared: bool,
    space_role: Option<String>,
    share_role: Option<String>,
}

const SPACE_RETURNING: &str = "space_id, tenant_id, name, icon, kind, visibility, dataset_id, created_by, created_at, updated_at";

impl Spaces {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn list_spaces(&self, user: &UserContext) -> SpaceResult<Vec<Space>> {
        self.ensure_actor(user)?;
        let rows = sqlx::query(&format!(
            "SELECT {SPACE_RETURNING}, (SELECT m.role FROM knowledge.space_members m WHERE m.space_id = s.space_id AND m.principal_type = 'user' AND m.principal_id = $2 AND m.revoked_at IS NULL) AS role FROM knowledge.spaces s WHERE s.tenant_id = $1 AND s.archived_at IS NULL ORDER BY s.updated_at DESC, s.space_id DESC LIMIT 200"
        ))
        .bind(&user.tenant_id)
        .bind(&user.user_id)
        .fetch_all(database::executor(&self.pool))
        .await
        .map_err(db)?;
        Ok(rows
            .iter()
            .filter_map(|row| {
                let record = space_record(row);
                allow_record(user, &record, Action::View).then(|| space_json(&record))
            })
            .collect())
    }

    pub async fn create_space(&self, user: &UserContext, input: NewSpace) -> SpaceResult<Space> {
        if !authorize::allow_create(user) {
            return Err(SpaceError::NotFound);
        }
        let name = bounded_name(&input.name)?;
        let icon = bounded_icon(&input.icon)?;
        let kind = space_kind(&input.kind)?;
        let visibility = visibility(&input.visibility)?;
        let space_id = Uuid::new_v4().to_string();
        let dataset_id = Uuid::new_v4().to_string();
        let collection = format!("space-{dataset_id}");
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        sqlx::query(
            "INSERT INTO knowledge.datasets (dataset_id, name, description, tenant_id, visibility, embedding_provider, embedding_model, embedding_dimension, embedding_config, index_config, collection_name, is_deleted, deleted_at, deleted_by, delete_reason, created_by) VALUES ($1, $2, 'hidden space dataset', $3, 'private', 'gemini', 'gemini-embedding-001', 1024, '{}'::jsonb, '{\"hidden_space\":true}'::jsonb, $4, false, NULL, NULL, NULL, $5)",
        )
        .bind(&dataset_id)
        .bind(&name)
        .bind(&user.tenant_id)
        .bind(&collection)
        .bind(&user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        let row = sqlx::query(&format!(
            "INSERT INTO knowledge.spaces (space_id, tenant_id, name, icon, kind, visibility, dataset_id, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {SPACE_RETURNING}, NULL::text AS role"
        ))
        .bind(&space_id)
        .bind(&user.tenant_id)
        .bind(&name)
        .bind(&icon)
        .bind(&kind)
        .bind(&visibility)
        .bind(&dataset_id)
        .bind(&user.user_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        sqlx::query(
            "INSERT INTO knowledge.space_members (space_id, principal_type, principal_id, role) VALUES ($1, 'user', $2, 'owner')",
        )
        .bind(&space_id)
        .bind(&user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        tx.commit().await.map_err(db)?;
        Ok(space_json(&space_record(&row)))
    }

    pub async fn get_space(&self, user: &UserContext, space_id: &str) -> SpaceResult<Space> {
        let record = self.space(user, space_id).await?;
        if !allow_record(user, &record, Action::View) {
            return Err(SpaceError::NotFound);
        }
        Ok(space_json(&record))
    }

    pub async fn patch_space(&self, user: &UserContext, space_id: &str, patch: SpacePatch) -> SpaceResult<Space> {
        let current = self.space(user, space_id).await?;
        if !allow_record(user, &current, Action::Admin) {
            return Err(SpaceError::NotFound);
        }
        let name = match patch.name {
            Some(value) => bounded_name(&value)?,
            None => current.name.clone(),
        };
        let icon = match patch.icon {
            Some(value) => bounded_icon(&value)?,
            None => current.icon.clone(),
        };
        let kind = match patch.kind {
            Some(value) => space_kind(&value)?,
            None => current.kind.clone(),
        };
        let visibility = match patch.visibility {
            Some(value) => visibility(&value)?,
            None => current.visibility.clone(),
        };
        let row = sqlx::query(&format!(
            "UPDATE knowledge.spaces SET name = $3, icon = $4, kind = $5, visibility = $6, updated_at = now() WHERE space_id = $1 AND tenant_id = $2 AND archived_at IS NULL RETURNING {SPACE_RETURNING}, $7::text AS role"
        ))
        .bind(space_id)
        .bind(&user.tenant_id)
        .bind(name)
        .bind(icon)
        .bind(kind)
        .bind(visibility)
        .bind(current.role.clone())
        .fetch_optional(database::executor(&self.pool))
        .await
        .map_err(db)?
        .ok_or(SpaceError::NotFound)?;
        Ok(space_json(&space_record(&row)))
    }

    pub async fn archive_space(&self, user: &UserContext, space_id: &str) -> SpaceResult<()> {
        let current = self.space(user, space_id).await?;
        if !allow_record(user, &current, Action::Admin) {
            return Err(SpaceError::NotFound);
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        let archived = sqlx::query(
            "UPDATE knowledge.spaces SET archived_at = now(), updated_at = now() WHERE space_id = $1 AND tenant_id = $2 AND archived_at IS NULL RETURNING dataset_id",
        )
        .bind(space_id)
        .bind(&user.tenant_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(db)?;
        let Some(row) = archived else {
            return Err(SpaceError::NotFound);
        };
        let dataset_id: String = row.get("dataset_id");
        let hidden = sqlx::query(
            "UPDATE knowledge.datasets SET is_deleted = true, deleted_at = now(), deleted_by = $2, delete_reason = 'space_archived' WHERE dataset_id = $1 AND is_deleted = false",
        )
        .bind(&dataset_id)
        .bind(&user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        if hidden.rows_affected() != 1 {
            return Err(SpaceError::Unavailable);
        }
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    pub async fn list_members(&self, user: &UserContext, space_id: &str) -> SpaceResult<Vec<Member>> {
        let current = self.space(user, space_id).await?;
        if !allow_record(user, &current, Action::View) {
            return Err(SpaceError::NotFound);
        }
        let rows = sqlx::query(
            "SELECT principal_type, principal_id, role FROM knowledge.space_members WHERE space_id = $1 AND revoked_at IS NULL ORDER BY principal_type, principal_id",
        )
        .bind(space_id)
        .fetch_all(database::executor(&self.pool))
        .await
        .map_err(db)?;
        Ok(rows
            .iter()
            .map(|row| Member {
                principal_type: row.get("principal_type"),
                principal_id: row.get("principal_id"),
                role: row.get("role"),
            })
            .collect())
    }

    pub async fn put_members(&self, user: &UserContext, space_id: &str, input: MemberPut) -> SpaceResult<Vec<Member>> {
        let current = self.space(user, space_id).await?;
        if !allow_record(user, &current, Action::Admin) {
            return Err(SpaceError::NotFound);
        }
        let members = normalize_members(&input.members)?;
        if !members.iter().any(|member| member.2 == "owner") {
            return Err(SpaceError::Invalid("a space needs an owner"));
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        sqlx::query("UPDATE knowledge.space_members SET revoked_at = now() WHERE space_id = $1 AND revoked_at IS NULL")
            .bind(space_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        for (kind, id, role) in &members {
            sqlx::query(
                "INSERT INTO knowledge.space_members (space_id, principal_type, principal_id, role, revoked_at) VALUES ($1, $2, $3, $4, NULL) ON CONFLICT (space_id, principal_type, principal_id) DO UPDATE SET role = EXCLUDED.role, revoked_at = NULL",
            )
            .bind(space_id)
            .bind(kind)
            .bind(id)
            .bind(role)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        tx.commit().await.map_err(db)?;
        self.list_members(user, space_id).await
    }

    pub async fn list_items(&self, user: &UserContext, filter: ItemFilter) -> SpaceResult<ItemPage> {
        self.ensure_actor(user)?;
        let kind = filter.kind.as_deref().map(item_kind).transpose()?;
        let pattern = filter.query.as_deref().map(like_pattern).transpose()?;
        let (cursor_at, cursor_id) = match &filter.cursor {
            Some((at, id)) => (Some(parse_time(at)?), Some(id.clone())),
            None => (None, None),
        };
        self.list_items_query(user, filter, kind, pattern, cursor_at, cursor_id).await
    }

    async fn list_items_query(
        &self,
        user: &UserContext,
        filter: ItemFilter,
        kind: Option<String>,
        pattern: Option<String>,
        cursor_at: Option<DateTime<Utc>>,
        cursor_id: Option<String>,
    ) -> SpaceResult<ItemPage> {
        let view_sql = match filter.view {
            ItemView::All | ItemView::Trash => "TRUE",
            ItemView::Recent => "EXISTS (SELECT 1 FROM knowledge.space_item_activity a WHERE a.item_id = i.item_id AND a.user_id = $2)",
            ItemView::Starred => "EXISTS (SELECT 1 FROM knowledge.space_item_stars st WHERE st.item_id = i.item_id AND st.user_id = $2 AND st.revoked_at IS NULL)",
            ItemView::Mine => "i.created_by = $2",
            ItemView::Shared => "EXISTS (SELECT 1 FROM knowledge.space_item_shares sh WHERE sh.item_id = i.item_id AND sh.principal_type = 'user' AND sh.principal_id = $2 AND sh.revoked_at IS NULL) AND i.created_by <> $2",
        };
        let deleted_sql = if filter.view == ItemView::Trash {
            "i.deleted_at IS NOT NULL AND ($3::bool OR i.created_by = $2 OR COALESCE((SELECT m.role FROM knowledge.space_members m WHERE m.space_id = s.space_id AND m.principal_type = 'user' AND m.principal_id = $2 AND m.revoked_at IS NULL), '') IN ('owner', 'editor'))"
        } else {
            "i.deleted_at IS NULL"
        };
        let sql = format!(
            "SELECT i.item_id, i.tenant_id, i.space_id, s.name AS space_name, s.icon AS space_icon, s.visibility AS space_visibility,
                    i.parent_id, i.kind, i.title, i.icon, i.position, i.source, i.created_by, i.updated_by, i.content_format,
                    i.current_revision, i.last_activity_at, i.updated_at, i.deleted_at,
                    EXISTS (SELECT 1 FROM knowledge.space_item_stars st WHERE st.item_id = i.item_id AND st.user_id = $2 AND st.revoked_at IS NULL) AS starred,
                    EXISTS (SELECT 1 FROM knowledge.space_item_shares sh WHERE sh.item_id = i.item_id AND sh.principal_type = 'user' AND sh.principal_id = $2 AND sh.revoked_at IS NULL) AS shared,
                    (SELECT m.role FROM knowledge.space_members m WHERE m.space_id = s.space_id AND m.principal_type = 'user' AND m.principal_id = $2 AND m.revoked_at IS NULL) AS space_role,
                    (SELECT sh.role FROM knowledge.space_item_shares sh WHERE sh.item_id = i.item_id AND sh.principal_type = 'user' AND sh.principal_id = $2 AND sh.revoked_at IS NULL) AS share_role
             FROM knowledge.space_items i
             JOIN knowledge.spaces s ON s.space_id = i.space_id AND s.tenant_id = i.tenant_id
             WHERE i.tenant_id = $1 AND s.archived_at IS NULL AND {deleted_sql}
               AND ($3::bool OR s.visibility = 'tenant' OR i.created_by = $2
                    OR EXISTS (SELECT 1 FROM knowledge.space_members m WHERE m.space_id = s.space_id AND m.principal_type = 'user' AND m.principal_id = $2 AND m.revoked_at IS NULL)
                    OR EXISTS (SELECT 1 FROM knowledge.space_item_shares sh WHERE sh.item_id = i.item_id AND sh.principal_type = 'user' AND sh.principal_id = $2 AND sh.revoked_at IS NULL))
               AND ($4::text IS NULL OR i.kind = $4)
               AND ($5::text IS NULL OR i.title ILIKE $5 ESCAPE '\\' OR i.title_tsv @@ plainto_tsquery('simple', $6))
               AND ($7::text IS NULL OR i.space_id = $7)
               AND ($8::timestamptz IS NULL OR (i.last_activity_at, i.item_id) < ($8, $9))
               AND {view_sql}
             ORDER BY i.last_activity_at DESC, i.item_id DESC
             LIMIT $10"
        );
        let rows = sqlx::query(&sql)
            .bind(&user.tenant_id)
            .bind(&user.user_id)
            .bind(user.tenant_admin())
            .bind(kind.as_deref())
            .bind(pattern.as_deref())
            .bind(filter.query.as_deref())
            .bind(filter.space_id.as_deref())
            .bind(cursor_at)
            .bind(cursor_id.as_deref())
            .bind(filter.limit + 1)
            .fetch_all(database::executor(&self.pool))
            .await
            .map_err(db)?;
        let mut items = Vec::new();
        for row in &rows {
            let record = item_record(row);
            if allow_item_record(user, &record, Action::View) {
                items.push(item_json(&record));
            }
        }
        let next_cursor = if items.len() > filter.limit as usize {
            items.pop();
            items.last().map(|item| encode_cursor(&item.last_activity_at, &item.item_id))
        } else {
            None
        };
        Ok(ItemPage { items, next_cursor })
    }

    pub async fn create_item(&self, user: &UserContext, space_id: &str, input: NewItem) -> SpaceResult<Item> {
        let space = self.space(user, space_id).await?;
        if !allow_record(user, &space, Action::Edit) {
            return Err(SpaceError::NotFound);
        }
        let title = bounded_title(&input.title)?;
        let kind = item_kind(&input.kind)?;
        let icon = bounded_icon(&input.icon)?;
        let format = content_format(&input.content_format)?;
        let source = source_label(&input.source)?;
        let position = position(input.position.as_deref().unwrap_or("a0"))?;
        let item_id = Uuid::new_v4().to_string();
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        if let Some(parent) = input.parent_id.as_deref() {
            ensure_parent(&mut tx, user, space_id, &item_id, parent).await?;
        }
        sqlx::query(
            "INSERT INTO knowledge.space_items (item_id, tenant_id, space_id, parent_id, kind, title, icon, position, source, content_format, created_by, updated_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $11)",
        )
        .bind(&item_id)
        .bind(&user.tenant_id)
        .bind(space_id)
        .bind(input.parent_id.as_deref())
        .bind(&kind)
        .bind(&title)
        .bind(&icon)
        .bind(&position)
        .bind(&source)
        .bind(&format)
        .bind(&user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        activity(&mut tx, &item_id, &user.user_id, "create").await?;
        tx.commit().await.map_err(db)?;
        self.get_item(user, &item_id).await
    }

    pub async fn get_item(&self, user: &UserContext, item_id: &str) -> SpaceResult<Item> {
        let record = self.item(user, item_id, true).await?;
        if !allow_item_record(user, &record, Action::View) {
            return Err(SpaceError::NotFound);
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        activity(&mut tx, item_id, &user.user_id, "open").await?;
        sqlx::query("UPDATE knowledge.space_items SET last_activity_at = now() WHERE item_id = $1 AND tenant_id = $2")
            .bind(item_id)
            .bind(&user.tenant_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        tx.commit().await.map_err(db)?;
        let mut item = item_json(&record);
        item.last_activity_at = Utc::now().to_rfc3339();
        Ok(item)
    }

    pub async fn patch_item(&self, user: &UserContext, item_id: &str, patch: ItemPatch) -> SpaceResult<Item> {
        let current = self.item(user, item_id, true).await?;
        if !allow_item_record(user, &current, Action::Edit) {
            return Err(SpaceError::NotFound);
        }
        let title = match &patch.title {
            Some(value) => bounded_title(value)?,
            None => current.title.clone(),
        };
        let icon = match &patch.icon {
            Some(value) => bounded_icon(value)?,
            None => current.icon.clone(),
        };
        let position = match &patch.position {
            Some(value) => position(value)?,
            None => current.position.clone(),
        };
        let space_id = patch.space_id.unwrap_or_else(|| current.space_id.clone());
        if space_id != current.space_id {
            let destination = self.space(user, &space_id).await?;
            if !allow_record(user, &destination, Action::Edit) {
                return Err(SpaceError::NotFound);
            }
        }
        let parent = if space_id != current.space_id || patch.clear_parent == Some(true) {
            None
        } else {
            patch.parent_id.clone().or(current.parent_id.clone())
        };
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        if let Some(parent) = parent.as_deref() {
            ensure_parent(&mut tx, user, &space_id, item_id, parent).await?;
        }
        let action = if title != current.title {
            "rename"
        } else if space_id != current.space_id || parent != current.parent_id || position != current.position {
            "move"
        } else {
            "edit"
        };
        sqlx::query(
            "UPDATE knowledge.space_items SET title = $3, icon = $4, position = $5, space_id = $6, parent_id = $7, updated_by = $8, updated_at = now(), last_activity_at = now() WHERE item_id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(item_id)
        .bind(&user.tenant_id)
        .bind(&title)
        .bind(&icon)
        .bind(&position)
        .bind(&space_id)
        .bind(parent.as_deref())
        .bind(&user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        activity(&mut tx, item_id, &user.user_id, action).await?;
        tx.commit().await.map_err(db)?;
        self.item(user, item_id, false).await.map(|row| item_json(&row))
    }

    pub async fn delete_item(&self, user: &UserContext, item_id: &str) -> SpaceResult<()> {
        let current = self.item(user, item_id, false).await?;
        if !allow_item_record(user, &current, Action::Edit) {
            return Err(SpaceError::NotFound);
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        let children: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM knowledge.space_items WHERE parent_id = $1 AND tenant_id = $2 AND deleted_at IS NULL)",
        )
        .bind(item_id)
        .bind(&user.tenant_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if children {
            return Err(SpaceError::Invalid("move or delete child items first"));
        }
        let changed = sqlx::query(
            "UPDATE knowledge.space_items SET deleted_at = now(), updated_by = $3, updated_at = now(), last_activity_at = now() WHERE item_id = $1 AND tenant_id = $2 AND deleted_at IS NULL",
        )
        .bind(item_id)
        .bind(&user.tenant_id)
        .bind(&user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        if changed.rows_affected() == 0 {
            return Err(SpaceError::NotFound);
        }
        activity(&mut tx, item_id, &user.user_id, "delete").await?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    pub async fn restore_item(&self, user: &UserContext, item_id: &str) -> SpaceResult<Item> {
        let current = self.item(user, item_id, true).await?;
        if current.deleted_at.is_none() || !allow_item_record(user, &current, Action::Edit) {
            return Err(SpaceError::NotFound);
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        sqlx::query(
            "UPDATE knowledge.space_items SET deleted_at = NULL, updated_by = $3, updated_at = now(), last_activity_at = now() WHERE item_id = $1 AND tenant_id = $2 AND deleted_at IS NOT NULL",
        )
        .bind(item_id)
        .bind(&user.tenant_id)
        .bind(&user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        activity(&mut tx, item_id, &user.user_id, "restore").await?;
        tx.commit().await.map_err(db)?;
        self.item(user, item_id, false).await.map(|row| item_json(&row))
    }

    pub async fn star(&self, user: &UserContext, item_id: &str, on: bool) -> SpaceResult<()> {
        let current = self.item(user, item_id, false).await?;
        if !allow_item_record(user, &current, Action::Star) {
            return Err(SpaceError::NotFound);
        }
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        if on {
            sqlx::query(
                "INSERT INTO knowledge.space_item_stars (user_id, item_id, revoked_at) VALUES ($1, $2, NULL) ON CONFLICT (user_id, item_id) DO UPDATE SET revoked_at = NULL",
            )
            .bind(&user.user_id)
            .bind(item_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        } else {
            sqlx::query("UPDATE knowledge.space_item_stars SET revoked_at = now() WHERE user_id = $1 AND item_id = $2 AND revoked_at IS NULL")
                .bind(&user.user_id)
                .bind(item_id)
                .execute(&mut *tx)
                .await
                .map_err(db)?;
        }
        activity(&mut tx, item_id, &user.user_id, if on { "star" } else { "unstar" }).await?;
        tx.commit().await.map_err(db)?;
        Ok(())
    }

    pub async fn put_shares(&self, user: &UserContext, item_id: &str, input: SharePut) -> SpaceResult<Vec<Member>> {
        let current = self.item(user, item_id, false).await?;
        if !allow_item_record(user, &current, Action::Share) {
            return Err(SpaceError::NotFound);
        }
        let shares = normalize_members(&input.shares)?;
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        sqlx::query("UPDATE knowledge.space_item_shares SET revoked_at = now() WHERE item_id = $1 AND revoked_at IS NULL")
            .bind(item_id)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        for (kind, id, role) in &shares {
            sqlx::query(
                "INSERT INTO knowledge.space_item_shares (item_id, principal_type, principal_id, role, revoked_at) VALUES ($1, $2, $3, $4, NULL) ON CONFLICT (item_id, principal_type, principal_id) DO UPDATE SET role = EXCLUDED.role, revoked_at = NULL",
            )
            .bind(item_id)
            .bind(kind)
            .bind(id)
            .bind(role)
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        }
        activity(&mut tx, item_id, &user.user_id, "share").await?;
        tx.commit().await.map_err(db)?;
        Ok(shares
            .into_iter()
            .map(|(principal_type, principal_id, role)| Member { principal_type, principal_id, role })
            .collect())
    }

    pub async fn get_content(&self, user: &UserContext, item_id: &str) -> SpaceResult<ItemContent> {
        let current = self.item(user, item_id, false).await?;
        if !allow_item_record(user, &current, Action::View) {
            return Err(SpaceError::NotFound);
        }
        let row = sqlx::query(
            "SELECT revision, blocks, markdown, content_sha256 FROM knowledge.space_item_revisions WHERE item_id = $1 AND revision = $2",
        )
        .bind(item_id)
        .bind(current.current_revision)
        .fetch_optional(database::executor(&self.pool))
        .await
        .map_err(db)?;
        let (blocks, markdown, sha) = match row {
            Some(row) => (
                row.get::<Option<Value>, _>("blocks").unwrap_or(Value::Null),
                row.get::<Option<String>, _>("markdown").unwrap_or_default(),
                row.get::<String, _>("content_sha256"),
            ),
            None => (Value::Null, String::new(), content_digest(&current.content_format, &Value::Null, "")?),
        };
        Ok(ItemContent {
            item_id: item_id.to_owned(),
            revision: current.current_revision,
            content_format: current.content_format,
            blocks,
            markdown,
            content_sha256: sha,
        })
    }

    pub async fn put_content(
        &self,
        user: &UserContext,
        item_id: &str,
        expected: Option<i64>,
        input: ContentPut,
    ) -> SpaceResult<ItemContent> {
        let current = self.item(user, item_id, false).await?;
        if !allow_item_record(user, &current, Action::Edit) {
            return Err(SpaceError::NotFound);
        }
        match expected {
            None if current.current_revision > 0 => return Err(SpaceError::Precondition),
            Some(value) if value != current.current_revision => return Err(SpaceError::Conflict),
            _ => {}
        }
        let format = content_format(&input.content_format)?;
        let markdown = input.markdown;
        let blocks = input.blocks;
        let digest = content_digest(&format, &blocks, &markdown)?;
        let next = current.current_revision + 1;
        let mut tx = self.pool.begin().await.map_err(db)?;
        bind_current(&mut tx).await.map_err(db)?;
        let updated = sqlx::query(
            "UPDATE knowledge.space_items SET content_format = $4, current_revision = $5, updated_by = $6, updated_at = now(), last_activity_at = now() WHERE item_id = $1 AND tenant_id = $2 AND current_revision = $3 AND deleted_at IS NULL",
        )
        .bind(item_id)
        .bind(&user.tenant_id)
        .bind(current.current_revision)
        .bind(&format)
        .bind(next)
        .bind(&user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        if updated.rows_affected() == 0 {
            return Err(SpaceError::Conflict);
        }
        sqlx::query(
            "INSERT INTO knowledge.space_item_revisions (item_id, revision, blocks, markdown, content_sha256, author) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(item_id)
        .bind(next)
        .bind(&blocks)
        .bind(&markdown)
        .bind(&digest)
        .bind(&user.user_id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
        activity(&mut tx, item_id, &user.user_id, "edit").await?;
        tx.commit().await.map_err(db)?;
        Ok(ItemContent {
            item_id: item_id.to_owned(),
            revision: next,
            content_format: format,
            blocks,
            markdown,
            content_sha256: digest,
        })
    }

    fn ensure_actor(&self, user: &UserContext) -> SpaceResult<()> {
        if authorize::allow_create(user) {
            Ok(())
        } else {
            Err(SpaceError::NotFound)
        }
    }

    async fn space(&self, user: &UserContext, space_id: &str) -> SpaceResult<SpaceRecord> {
        self.ensure_actor(user)?;
        let row = sqlx::query(&format!(
            "SELECT {SPACE_RETURNING}, (SELECT m.role FROM knowledge.space_members m WHERE m.space_id = s.space_id AND m.principal_type = 'user' AND m.principal_id = $3 AND m.revoked_at IS NULL) AS role FROM knowledge.spaces s WHERE s.space_id = $1 AND s.tenant_id = $2 AND s.archived_at IS NULL"
        ))
        .bind(space_id)
        .bind(&user.tenant_id)
        .bind(&user.user_id)
        .fetch_optional(database::executor(&self.pool))
        .await
        .map_err(db)?
        .ok_or(SpaceError::NotFound)?;
        Ok(space_record(&row))
    }

    async fn item(&self, user: &UserContext, item_id: &str, include_deleted: bool) -> SpaceResult<ItemRecord> {
        self.ensure_actor(user)?;
        let deleted = if include_deleted { "TRUE" } else { "i.deleted_at IS NULL" };
        let sql = format!(
            "SELECT i.item_id, i.tenant_id, i.space_id, s.name AS space_name, s.icon AS space_icon, s.visibility AS space_visibility,
                    i.parent_id, i.kind, i.title, i.icon, i.position, i.source, i.created_by, i.updated_by, i.content_format,
                    i.current_revision, i.last_activity_at, i.updated_at, i.deleted_at,
                    EXISTS (SELECT 1 FROM knowledge.space_item_stars st WHERE st.item_id = i.item_id AND st.user_id = $3 AND st.revoked_at IS NULL) AS starred,
                    EXISTS (SELECT 1 FROM knowledge.space_item_shares sh WHERE sh.item_id = i.item_id AND sh.principal_type = 'user' AND sh.principal_id = $3 AND sh.revoked_at IS NULL) AS shared,
                    (SELECT m.role FROM knowledge.space_members m WHERE m.space_id = s.space_id AND m.principal_type = 'user' AND m.principal_id = $3 AND m.revoked_at IS NULL) AS space_role,
                    (SELECT sh.role FROM knowledge.space_item_shares sh WHERE sh.item_id = i.item_id AND sh.principal_type = 'user' AND sh.principal_id = $3 AND sh.revoked_at IS NULL) AS share_role
             FROM knowledge.space_items i
             JOIN knowledge.spaces s ON s.space_id = i.space_id AND s.tenant_id = i.tenant_id
             WHERE i.item_id = $1 AND i.tenant_id = $2 AND s.archived_at IS NULL AND {deleted}"
        );
        let row = sqlx::query(&sql)
            .bind(item_id)
            .bind(&user.tenant_id)
            .bind(&user.user_id)
            .fetch_optional(database::executor(&self.pool))
            .await
            .map_err(db)?
            .ok_or(SpaceError::NotFound)?;
        Ok(item_record(&row))
    }
}

fn allow_record(user: &UserContext, record: &SpaceRecord, action: Action) -> bool {
    authorize::allow_space(user, &record.tenant_id, record.role.as_deref().and_then(MemberRole::parse), &record.visibility, action)
}

fn allow_item_record(user: &UserContext, record: &ItemRecord, action: Action) -> bool {
    authorize::allow_item(
        user,
        &record.tenant_id,
        record.space_role.as_deref().and_then(MemberRole::parse),
        &record.space_visibility,
        record.share_role.as_deref().and_then(MemberRole::parse),
        &record.created_by,
        record.deleted_at.is_some(),
        action,
    )
}

fn space_record(row: &sqlx::postgres::PgRow) -> SpaceRecord {
    SpaceRecord {
        space_id: row.get("space_id"),
        tenant_id: row.get("tenant_id"),
        name: row.get("name"),
        icon: row.get("icon"),
        kind: row.get("kind"),
        visibility: row.get("visibility"),
        dataset_id: row.get("dataset_id"),
        created_by: row.get("created_by"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        role: row.get("role"),
    }
}

fn space_json(record: &SpaceRecord) -> Space {
    Space {
        space_id: record.space_id.clone(),
        name: record.name.clone(),
        icon: record.icon.clone(),
        kind: record.kind.clone(),
        visibility: record.visibility.clone(),
        dataset_id: record.dataset_id.clone(),
        created_by: record.created_by.clone(),
        created_at: record.created_at.to_rfc3339(),
        updated_at: record.updated_at.to_rfc3339(),
    }
}

fn item_record(row: &sqlx::postgres::PgRow) -> ItemRecord {
    ItemRecord {
        item_id: row.get("item_id"),
        tenant_id: row.get("tenant_id"),
        space_id: row.get("space_id"),
        space_name: row.get("space_name"),
        space_icon: row.get("space_icon"),
        space_visibility: row.get("space_visibility"),
        parent_id: row.get("parent_id"),
        kind: row.get("kind"),
        title: row.get("title"),
        icon: row.get("icon"),
        position: row.get("position"),
        source: row.get("source"),
        created_by: row.get("created_by"),
        updated_by: row.get("updated_by"),
        content_format: row.get("content_format"),
        current_revision: row.get("current_revision"),
        last_activity_at: row.get("last_activity_at"),
        updated_at: row.get("updated_at"),
        deleted_at: row.get("deleted_at"),
        starred: row.get("starred"),
        shared: row.get("shared"),
        space_role: row.get("space_role"),
        share_role: row.get("share_role"),
    }
}

fn item_json(record: &ItemRecord) -> Item {
    Item {
        item_id: record.item_id.clone(),
        space_id: record.space_id.clone(),
        space_name: record.space_name.clone(),
        space_icon: record.space_icon.clone(),
        parent_id: record.parent_id.clone(),
        kind: record.kind.clone(),
        title: record.title.clone(),
        icon: record.icon.clone(),
        position: record.position.clone(),
        source: record.source.clone(),
        created_by: record.created_by.clone(),
        updated_by: record.updated_by.clone(),
        content_format: record.content_format.clone(),
        current_revision: record.current_revision,
        starred: record.starred,
        shared: record.shared,
        last_activity_at: record.last_activity_at.to_rfc3339(),
        updated_at: record.updated_at.to_rfc3339(),
        deleted_at: record.deleted_at.map(|value| value.to_rfc3339()),
    }
}

fn normalize_members(input: &[MemberInput]) -> SpaceResult<Vec<(String, String, String)>> {
    if input.len() > 200 {
        return Err(SpaceError::Invalid("too many principals"));
    }
    let mut out = Vec::new();
    for member in input {
        let (kind, id) = principal(&member.principal_type, &member.principal_id)?;
        let role = member_role(&member.role)?;
        if out.iter().any(|(seen_kind, seen_id, _)| seen_kind == &kind && seen_id == &id) {
            return Err(SpaceError::Invalid("duplicate principal"));
        }
        out.push((kind, id, role));
    }
    Ok(out)
}

async fn ensure_parent(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user: &UserContext,
    space_id: &str,
    item_id: &str,
    parent_id: &str,
) -> SpaceResult<()> {
    if parent_id == item_id {
        return Err(SpaceError::Invalid("an item cannot parent itself"));
    }
    let mut cursor = parent_id.to_owned();
    for _ in 0..32 {
        let row = sqlx::query(
            "SELECT parent_id FROM knowledge.space_items WHERE item_id = $1 AND tenant_id = $2 AND space_id = $3 AND deleted_at IS NULL",
        )
        .bind(&cursor)
        .bind(&user.tenant_id)
        .bind(space_id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(db)?
        .ok_or(SpaceError::Invalid("parent item is not in this space"))?;
        let parent: Option<String> = row.get("parent_id");
        match parent {
            Some(next) if next == item_id => return Err(SpaceError::Invalid("parent cycle")),
            Some(next) => cursor = next,
            None => return Ok(()),
        }
    }
    Err(SpaceError::Invalid("parent chain is too deep"))
}

async fn activity(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    item_id: &str,
    user_id: &str,
    action: &str,
) -> SpaceResult<()> {
    sqlx::query("INSERT INTO knowledge.space_item_activity (item_id, user_id, action) VALUES ($1, $2, $3)")
        .bind(item_id)
        .bind(user_id)
        .bind(action)
        .execute(&mut **tx)
        .await
        .map_err(db)?;
    Ok(())
}

fn parse_time(value: &str) -> SpaceResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| SpaceError::Invalid("invalid cursor"))
}

fn encode_cursor(at: &str, id: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{at}\n{id}"))
}

pub fn decode_cursor(value: &str) -> SpaceResult<(String, String)> {
    if value.len() > 512 {
        return Err(SpaceError::Invalid("invalid cursor"));
    }
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| SpaceError::Invalid("invalid cursor"))?;
    let text = String::from_utf8(bytes).map_err(|_| SpaceError::Invalid("invalid cursor"))?;
    let (at, id) = text.split_once('\n').ok_or(SpaceError::Invalid("invalid cursor"))?;
    let _ = parse_time(at)?;
    if id.trim().is_empty() || id.len() > 255 {
        return Err(SpaceError::Invalid("invalid cursor"));
    }
    Ok((at.to_owned(), id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::decode_cursor;
    use base64::Engine;

    #[test]
    fn cursor_round_trip_rejects_garbage_and_accepts_a_timestamp() {
        assert!(decode_cursor("@@@").is_err());
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode("2026-11-13T00:00:00+00:00\nitem-1");
        let (at, id) = decode_cursor(&encoded).unwrap();
        assert_eq!(at, "2026-11-13T00:00:00+00:00");
        assert_eq!(id, "item-1");
    }
}

fn db(error: sqlx::Error) -> SpaceError {
    match &error {
        sqlx::Error::Database(database) if database.code().as_deref() == Some("42P01") => {
            SpaceError::SchemaUnavailable
        }
        other => {
            let sqlstate = match other {
                sqlx::Error::Database(database) => database.code().map(|code| code.to_string()),
                _ => None,
            };
            tracing::warn!(
                sqlstate = sqlstate.as_deref().unwrap_or("none"),
                "knowledge space database request failed"
            );
            SpaceError::Unavailable
        }
    }
}
