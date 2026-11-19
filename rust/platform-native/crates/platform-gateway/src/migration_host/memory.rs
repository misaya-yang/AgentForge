//! Concrete long-term context from ai_gateway_core.memory.service.MemoryService.
//! Preference read, access update and frequent lookup remain separate operations.
use crate::{
    assistant_memory_store::PgAssistantMemoryStore,
    assistant_memory_types::{AssistantMemoryIdentity, AssistantMemoryStore, source_value},
    migration_agent::{AgentError, AgentFuture, Scope, control::RuntimeMemory},
    postgres_operation::PgOperationStorage,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use std::sync::Arc;

#[derive(Clone)]
pub struct PgRuntimeMemory {
    preferences: Arc<PgAssistantMemoryStore>,
    operations: PgOperationStorage,
}
fn unavailable<T>(_: T) -> AgentError {
    AgentError::code(503, "AI_PLATFORM_AGENT_RUNTIME_MEMORY_UNAVAILABLE")
}
#[derive(sqlx::FromRow)]
struct FrequentMemory {
    key: String,
    value: Option<String>,
    access_count: Option<i64>,
}
impl PgRuntimeMemory {
    pub fn new(pool: PgPool, control_pool: PgPool) -> Result<Self, AgentError> {
        Ok(Self {
            preferences: Arc::new(
                PgAssistantMemoryStore::new(pool.clone(), control_pool.clone())
                    .map_err(unavailable)?,
            ),
            operations: PgOperationStorage::new(pool, control_pool).map_err(unavailable)?,
        })
    }
    pub async fn drain_cancelled(&self) -> Result<(), AgentError> {
        self.preferences
            .drain_cancelled()
            .await
            .map_err(unavailable)?;
        self.operations.drain_cancelled().await.map_err(unavailable)
    }
}
impl RuntimeMemory for PgRuntimeMemory {
    fn context<'a>(&'a self, scope: &'a Scope, limit: i64) -> AgentFuture<'a, Value> {
        Box::pin(async move {
            let owner = AssistantMemoryIdentity {
                tenant_id: scope.tenant_id.clone(),
                user_id: scope.user_id.clone(),
                is_authenticated: true,
            };
            let raw = self
                .preferences
                .read_active(&owner, "preferences")
                .await
                .map_err(unavailable)?;
            // Even JSON null or a non-object existing preference row is touched.
            if raw.is_some() {
                self.preferences
                    .increment_access(&owner, "preferences")
                    .await
                    .map_err(unavailable)?;
            }
            let mut preferences = json!({"language":"zh-CN","response_style":"professional","preferred_tools":[],"default_datasets":[]});
            if let Some(Value::Object(values)) = raw.map(source_value) {
                preferences.as_object_mut().unwrap().extend(values);
            }
            let rows: Vec<FrequentMemory> = self.operations.acquire().await.map_err(unavailable)?
                .execute(move |conn| Box::pin(async move {
                    sqlx::query_as("SELECT key, value::text AS value, access_count::bigint AS access_count FROM user_memory WHERE tenant_id=$1 AND user_id=$2 AND (expires_at IS NULL OR expires_at > NOW()) AND key <> '__assistant_memory_control__' ORDER BY access_count DESC, last_accessed_at DESC NULLS LAST, updated_at DESC LIMIT $3")
                        .bind(owner.tenant_id).bind(owner.user_id).bind(limit).fetch_all(conn).await
                })).await.map_err(unavailable)?;
            // The SQL limit precedes skipping preferences, as in the source.
            let frequent: Vec<Value> = rows.into_iter().filter(|row| row.key != "preferences").take(10)
                .map(|row| json!({"key":row.key,"value":row.value.map(|v| source_value(Value::String(v))),"access_count":row.access_count})).collect();
            Ok(json!({"preferences":preferences,"frequent_memories":frequent}))
        })
    }
}
