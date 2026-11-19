//! Commit-scoped, payload-free wake hints. PostgreSQL delivers NOTIFY only on commit.
use super::{PostgresThreadStore, store_error};
use codex_protocol::ThreadId;
use codex_thread_store::ThreadStoreResult;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub(crate) const EVENT_CHANNEL: &str = "agent_runtime_events_v2";
pub(crate) const CONTROL_CHANNEL: &str = "agent_runtime_control_v1";
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct NotificationHint {
    root_thread_id: Uuid,
    sequence: i64,
}
impl NotificationHint {
    pub(crate) fn parse(payload: &str) -> Option<Self> {
        if payload.len() > 256 {
            return None;
        }
        let hint: Self = serde_json::from_str(payload).ok()?;
        (hint.sequence > 0).then_some(hint)
    }
    pub(crate) fn thread_id(&self) -> ThreadId {
        ThreadId::from_u128(self.root_thread_id.as_u128())
    }
}
impl PostgresThreadStore {
    pub(crate) async fn notify_v1_event_in_transaction(
        transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        root_thread_id: Uuid,
        sequence: i64,
    ) -> ThreadStoreResult<()> {
        let payload = serde_json::to_string(&NotificationHint {
            root_thread_id,
            sequence,
        })
        .map_err(super::projection::json_error)?;
        sqlx::query("SELECT pg_catalog.pg_notify($1,$2)")
            .bind(EVENT_CHANNEL)
            .bind(payload)
            .execute(&mut **transaction)
            .await
            .map_err(store_error)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_hint_rejects_unbounded_or_noncanonical_data() {
        let root = Uuid::from_u128(1);
        let hint = serde_json::json!({"root_thread_id":root,"sequence":7});
        assert_eq!(
            NotificationHint::parse(&hint.to_string())
                .unwrap()
                .thread_id(),
            ThreadId::from_u128(1)
        );
        for invalid in [
            "{}".to_owned(),
            "x".repeat(257),
            serde_json::json!({"root_thread_id":root,"sequence":0}).to_string(),
            serde_json::json!({"root_thread_id":"other","sequence":1}).to_string(),
            serde_json::json!({"root_thread_id":root,"sequence":1,"payload":"must not travel"})
                .to_string(),
        ] {
            assert!(NotificationHint::parse(&invalid).is_none());
        }
    }

    async fn receive_for(
        listener: &mut sqlx::postgres::PgListener,
        root: Uuid,
    ) -> NotificationHint {
        loop {
            let notification = listener.recv().await.unwrap();
            if let Some(hint) = NotificationHint::parse(notification.payload()) {
                if hint.root_thread_id == root {
                    return hint;
                }
            }
        }
    }

    #[tokio::test]
    #[ignore = "explicit real Runtime-role PostgreSQL URL required; notification helper evidence only"]
    async fn real_postgres_notification_is_commit_scoped_and_rollback_suppressed() {
        let url = std::env::var("AI_PLATFORM_RUNTIME_NOTIFY_TEST_DATABASE_URL")
            .expect("set the scoped Runtime-role test DSN privately");
        let connected = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect(&url)
            .await;
        assert!(
            connected.is_ok(),
            "scoped notification database must connect"
        );
        let pool = connected.unwrap();
        let actual_role: bool = sqlx::query_scalar("SELECT current_user=session_user AND rolcanlogin AND current_user ~ '_runtime$' AND NOT (rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls) FROM pg_catalog.pg_roles WHERE rolname=current_user")
            .fetch_one(&pool).await.unwrap();
        assert!(
            actual_role,
            "notification evidence requires the actual unprivileged Runtime login"
        );
        let mut listener = sqlx::postgres::PgListener::connect_with(&pool)
            .await
            .unwrap();
        listener.listen(EVENT_CHANNEL).await.unwrap();
        let root = Uuid::new_v4();
        let mut tx = pool.begin().await.unwrap();
        PostgresThreadStore::notify_v1_event_in_transaction(&mut tx, root, 41)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(150),
                receive_for(&mut listener, root)
            )
            .await
            .is_err()
        );
        tx.rollback().await.unwrap();
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(150),
                receive_for(&mut listener, root)
            )
            .await
            .is_err()
        );
        let mut tx = pool.begin().await.unwrap();
        PostgresThreadStore::notify_v1_event_in_transaction(&mut tx, root, 42)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let hint = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            receive_for(&mut listener, root),
        )
        .await
        .unwrap();
        assert_eq!(hint.root_thread_id, root);
        assert_eq!(hint.sequence, 42);
        drop(listener);
        pool.close().await;
    }
}
