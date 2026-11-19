use crate::migration_local_node::broker::{BrokerAuthorization, BrokerError, BrokerScope};
use axum::http::HeaderMap;
use serde_json::Value;

/// Adapter over the shared, source-faithful capability proof verifier. The host
/// supplies secrets in memory; this module never reads process environment.
impl BrokerAuthorization for crate::migration_content::capabilities::CapabilityAuthorization {
    fn authorize(
        &self,
        headers: &HeaderMap,
        path: &str,
        body: &Value,
    ) -> std::result::Result<BrokerScope, BrokerError> {
        let convert = |error: crate::migration_content::ContentError| {
            BrokerError::new(
                error.status,
                error
                    .detail
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| error.detail.to_string()),
            )
        };
        self.peer(headers).map_err(convert)?;
        for key in [
            "x-ai-tenant-id",
            "x-ai-user-id",
            "x-ai-session-id",
            "x-ai-execution-id",
            "x-ai-run-id",
        ] {
            if !crate::migration_local_node::models::valid_identifier(crate::migration_local_node::broker::header(headers, key), 255) {
                return Err(BrokerError::new(403, "scope is invalid"));
            }
        }
        let scope = crate::migration_content::capabilities::CapabilityAuthorization::authorize(
            self,
            headers,
            path,
            body,
            chrono::Utc::now().timestamp(),
        )
        .map_err(convert)?;
        Ok(BrokerScope {
            tenant_id: scope.tenant_id,
            user_id: scope.user_id,
            session_id: scope.session_id,
            execution_id: scope.execution_id,
            run_id: scope.run_id,
        })
    }
}
