//! Shares and entry points consume the same read-only Runtime history projection.
use super::{AgentError, Scope, entry::PgSourceAccess, threads::AgentThreadStore};
use serde_json::Value;
impl AgentThreadStore {
    pub async fn history_messages(
        &self,
        scope: &Scope,
        limit: usize,
        access: &PgSourceAccess,
    ) -> Result<Option<(Vec<Value>, usize)>, AgentError> {
        crate::session_history::agent_runtime_history(&self.pool, scope, access, limit).await
    }
}
