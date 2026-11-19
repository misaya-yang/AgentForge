use super::Page;
use crate::migration_agent::{Actor, AgentError};
use ai_platform_gateway_foundation::native_contract::responses::{self, JOIN, TERMINAL};
use serde_json::Value;
use sqlx::PgPool;

// Receipts are the notification record: stable execution IDs, no copy, no worker,
// and no gap between a terminal scheduler commit and inbox availability.
pub(super) async fn list(pool: &PgPool, actor: &Actor, page: &Page) -> Result<Value, AgentError> {
    // One statement gives the page and full-owner unread count a single snapshot.
    // The cursor uses the immutable outcome timestamp plus ID; acknowledgements
    // deliberately leave updated_at and every execution-control field untouched.
    let sql = responses::list_sql();
    Ok(sqlx::query_scalar(&sql)
        .bind(&actor.tenant_id)
        .bind(&actor.user_id)
        .bind(page.unread_only)
        .bind(page.before_at)
        .bind(&page.before_id)
        .bind((page.limit + 1) as i64)
        .fetch_one(ai_platform_gateway_foundation::tenant_database::executor(
            pool,
            &actor.tenant_id,
        ))
        .await?)
}

pub(super) async fn summary(pool: &PgPool, actor: &Actor) -> Result<Value, AgentError> {
    let sql = responses::summary_sql();
    Ok(sqlx::query_scalar(&sql)
        .bind(&actor.tenant_id)
        .bind(&actor.user_id)
        .fetch_one(ai_platform_gateway_foundation::tenant_database::executor(
            pool,
            &actor.tenant_id,
        ))
        .await?)
}

pub(super) async fn get(pool: &PgPool, actor: &Actor, id: &str) -> Result<Value, AgentError> {
    let projection = responses::notification_projection::sql();
    let sql = format!(
        "SELECT {projection} FROM {JOIN} WHERE e.execution_id=$1::uuid AND e.tenant_id=$2 AND e.user_id=$3 AND {TERMINAL}"
    );
    sqlx::query_scalar(&sql)
        .bind(id)
        .bind(&actor.tenant_id)
        .bind(&actor.user_id)
        .fetch_optional(ai_platform_gateway_foundation::tenant_database::executor(
            pool,
            &actor.tenant_id,
        ))
        .await?
        .ok_or_else(not_found)
}

fn not_found() -> AgentError {
    AgentError::code(404, "SCHEDULE_NOTIFICATION_NOT_FOUND")
}

pub(super) async fn acknowledge(
    pool: &PgPool,
    actor: &Actor,
    ids: &[String],
    read: bool,
) -> Result<Value, AgentError> {
    let mut tx =
        ai_platform_gateway_foundation::tenant_database::begin(pool, &actor.tenant_id).await?;
    let sql = format!(
        r#"
SELECT e.execution_id::text FROM gateway.scheduled_task_executions e
WHERE e.execution_id = ANY($1::text[]::uuid[]) AND e.tenant_id=$2 AND e.user_id=$3 AND {TERMINAL}
ORDER BY e.execution_id FOR UPDATE
"#
    );
    let owned: Vec<String> = sqlx::query_scalar(&sql)
        .bind(ids)
        .bind(&actor.tenant_id)
        .bind(&actor.user_id)
        .fetch_all(&mut *tx)
        .await?;
    // Unknown, nonterminal, cross-owner and cross-tenant IDs all return the same
    // error. Mixed batches are all-or-nothing and never reveal the matching subset.
    if owned.len() != ids.len() {
        return Err(not_found());
    }
    let rows: Vec<Value> = sqlx::query_scalar(&format!(
        r#"
UPDATE gateway.scheduled_task_executions
SET notification_read_at=CASE WHEN $4 THEN COALESCE(notification_read_at,NOW()) ELSE NULL END
WHERE execution_id = ANY($1::text[]::uuid[]) AND tenant_id=$2 AND user_id=$3
    AND status IN ('succeeded','failed','cancelled')
RETURNING {}
"#,
        responses::ack_projection::sql()
    ))
    .bind(ids)
    .bind(&actor.tenant_id)
    .bind(&actor.user_id)
    .bind(read)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(responses::acknowledgements::value(rows))
}
