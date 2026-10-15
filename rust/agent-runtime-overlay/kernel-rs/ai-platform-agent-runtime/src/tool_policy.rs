//! Platform-authenticated startup tool ceiling. This is an internal host
//! contract; model capabilities and mutable turn payloads never grant authority.

use std::collections::HashMap;

use codex_extension_api::ToolName;
use codex_extension_api::ToolPolicy;
use codex_protocol::ThreadId;
use codex_thread_store::ThreadStoreError;
use codex_thread_store::ThreadStoreResult;
use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;

use crate::PostgresThreadStore;
use crate::postgres_store::ThreadProjection;
use crate::postgres_store::projection::json_error;
use crate::postgres_store::projection::store_error;
use crate::postgres_store::projection::thread_uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PlatformToolPolicy {
    pub(crate) allowed_tools: Vec<ToolName>,
}

#[derive(Clone, Debug)]
pub(crate) struct PlatformStartupData {
    pub(crate) policy: PlatformToolPolicy,
    dynamic_tools: Vec<codex_protocol::dynamic_tools::DynamicToolSpec>,
}

impl PlatformToolPolicy {
    pub(crate) fn normalized(mut self) -> ThreadStoreResult<Self> {
        if self.allowed_tools.len() > 1024 {
            return Err(invalid_policy());
        }
        for tool in &mut self.allowed_tools {
            if tool.name.is_empty()
                || tool.name.len() > 255
                || tool.name.chars().any(char::is_control)
                || tool.namespace.as_ref().is_some_and(|namespace| {
                    namespace.len() > 255 || namespace.chars().any(char::is_control)
                })
            {
                return Err(invalid_policy());
            }
            if tool.is_default_namespace() {
                tool.namespace = None;
            }
        }
        self.allowed_tools.sort();
        self.allowed_tools.dedup();
        Ok(self)
    }

    pub(crate) fn kernel_policy(&self) -> ToolPolicy {
        ToolPolicy {
            allowed_tools: Some(self.allowed_tools.clone()),
            // Existing capabilities run in the Worker; native commands keep
            // their existing approval/sandbox checks. Policy never grants a
            // permission and never enables extra-permission arguments.
            require_managed_sandbox: false,
            require_unified_exec: false,
            expose_additional_permissions: false,
        }
    }

    pub(crate) fn allows_native_search(&self) -> bool {
        self.kernel_policy().allows(&ToolName::plain("web_search"))
    }

    fn contains(&self, selection: &Self) -> bool {
        let ceiling = self.kernel_policy();
        selection
            .allowed_tools
            .iter()
            .all(|tool| ceiling.allows(tool))
    }
}

fn invalid_policy() -> ThreadStoreError {
    ThreadStoreError::InvalidRequest {
        message: "invalid platform tool policy".to_string(),
    }
}

fn resolve_policy(
    stored: Option<PlatformToolPolicy>,
    requested: Option<PlatformToolPolicy>,
) -> ThreadStoreResult<PlatformToolPolicy> {
    let requested = requested.map(PlatformToolPolicy::normalized).transpose()?;
    let Some(stored) = stored else {
        return requested.ok_or_else(|| ThreadStoreError::Conflict {
            message: "legacy thread requires an authenticated tool policy before resume"
                .to_string(),
        });
    };
    let stored = stored.normalized()?;
    if requested
        .as_ref()
        .is_some_and(|requested| !stored.contains(requested))
    {
        return Err(ThreadStoreError::Conflict {
            message: "thread tool ceiling cannot be widened; create a new session".to_string(),
        });
    }
    Ok(stored)
}

impl PostgresThreadStore {
    pub(crate) async fn stage_host_startup(
        &self,
        thread_id: ThreadId,
        policy: PlatformToolPolicy,
        dynamic_tools: Vec<codex_protocol::dynamic_tools::DynamicToolSpec>,
    ) -> ThreadStoreResult<()> {
        self.root_scope(thread_uuid(thread_id)?).await?;
        self.staged_startup.lock().await.insert(
            thread_id,
            PlatformStartupData {
                policy,
                dynamic_tools,
            },
        );
        Ok(())
    }

    pub(crate) async fn clear_host_startup(&self, thread_id: ThreadId) {
        self.staged_startup.lock().await.remove(&thread_id);
    }

    pub(crate) async fn resolve_host_startup(
        &self,
        thread_id: Option<ThreadId>,
        parent_thread_id: Option<ThreadId>,
        forked_from_thread_id: Option<ThreadId>,
    ) -> ThreadStoreResult<codex_extension_api::HostThreadStartupData> {
        if let Some(thread_id) = thread_id {
            let staged = self.staged_startup.lock().await.get(&thread_id).cloned();
            if let Some(staged) = staged {
                self.root_scope(thread_uuid(thread_id)?).await?;
                return Ok(codex_extension_api::HostThreadStartupData {
                    tool_policy: staged.policy.kernel_policy(),
                    dynamic_tools: staged.dynamic_tools,
                });
            }
        }
        let source = parent_thread_id
            .or(forked_from_thread_id)
            .or(thread_id)
            .ok_or_else(invalid_policy)?;
        let scope = self.member_scope(thread_uuid(source)?).await?;
        let root = ThreadId::from_u128(scope.root_thread_id.as_u128());
        let policy = self.resolve_tool_policy(root, None).await?;
        let source_projection = self.load_projection(source, true).await?.0;
        let created = source_projection.created.ok_or_else(invalid_policy)?;
        Ok(codex_extension_api::HostThreadStartupData {
            tool_policy: policy.kernel_policy(),
            dynamic_tools: created.dynamic_tools,
        })
    }

    /// Persist the first authenticated ceiling, or verify a turn selection is a
    /// subset of it. Omission only reuses an existing ceiling; it never means all.
    pub(crate) async fn resolve_tool_policy(
        &self,
        thread_id: ThreadId,
        requested: Option<PlatformToolPolicy>,
    ) -> ThreadStoreResult<PlatformToolPolicy> {
        self.check_write_health(thread_id)?;
        let kernel_id = thread_uuid(thread_id)?;
        let scope = self.member_scope(kernel_id).await?;
        // The persisted ceiling is immutable. Reusing it is a read, not a
        // write by the previous turn's (possibly expired) execution owner.
        if let Some(stored) = self.load_projection(thread_id, true).await?.0.tool_policy {
            return resolve_policy(Some(stored), requested);
        }
        let mut transaction = self.pool.begin().await.map_err(store_error)?;
        self.lock_root(&mut transaction, scope.root_thread_id)
            .await?;
        let row = sqlx::query(
            "SELECT projection FROM assistant_runtime_thread_projections WHERE kernel_thread_id=$1 AND deleted_at IS NULL FOR UPDATE",
        )
        .bind(kernel_id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(store_error)?
        .ok_or(ThreadStoreError::ThreadNotFound { thread_id })?;
        let mut projection: ThreadProjection =
            serde_json::from_value(row.try_get::<Value, _>("projection").map_err(store_error)?)
                .map_err(json_error)?;
        let policy = resolve_policy(projection.tool_policy.clone(), requested)?;
        if projection.tool_policy.is_none() {
            projection.tool_policy = Some(policy.clone());
            sqlx::query("UPDATE assistant_runtime_thread_projections SET projection=$2 WHERE kernel_thread_id=$1")
                .bind(kernel_id)
                .bind(serde_json::to_value(projection).map_err(json_error)?)
                .execute(&mut *transaction)
                .await
                .map_err(store_error)?;
        }
        transaction.commit().await.map_err(store_error)?;
        Ok(policy)
    }
}

/// Native features with no platform authorization/product adapter remain off.
/// Existing direct dynamic tools, Worker dispatch and Codex collaboration stay on.
pub(crate) fn apply_managed_feature_profile(config: &mut HashMap<String, Value>) {
    let features = config
        .entry("features".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if let Some(features) = features.as_object_mut() {
        for key in [
            "reasoning_effort_override",
            "code_mode",
            "code_mode_host",
            "code_mode_only",
            "guardianv2",
            "guardian_approval",
            "guardian_reuse_parent_compaction",
            "hooks",
            "apps",
            "plugins",
            "realtime_conversation",
            "daemon_auto_start",
            "worktrees",
            "system_proxy_fallback",
            "unified_exec_tty",
        ] {
            features.insert(key.to_string(), Value::Bool(false));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(names: &[&str]) -> PlatformToolPolicy {
        PlatformToolPolicy {
            allowed_tools: names.iter().map(|name| ToolName::plain(*name)).collect(),
        }
    }

    #[test]
    fn absent_legacy_policy_is_never_unrestricted() {
        assert!(matches!(
            resolve_policy(None, None),
            Err(ThreadStoreError::Conflict { .. })
        ));
        let denied = resolve_policy(None, Some(policy(&[]))).unwrap();
        assert!(
            !denied
                .kernel_policy()
                .allows(&ToolName::plain("exec_command"))
        );
        assert!(!denied.allows_native_search());
    }

    #[test]
    fn resume_can_narrow_but_cannot_widen_original_ceiling() {
        let original = policy(&["search_web", "web_search"]);
        assert_eq!(
            resolve_policy(Some(original.clone()), Some(policy(&[]))).unwrap(),
            original
        );
        assert!(resolve_policy(Some(policy(&[])), Some(policy(&["web_search"]))).is_err());
        assert!(
            resolve_policy(
                Some(policy(&["search_web"])),
                Some(policy(&["exec_command"]))
            )
            .is_err()
        );
    }

    #[test]
    fn native_search_needs_authority_not_a_model_capability_flag() {
        assert!(!policy(&["search_web"]).allows_native_search());
        assert!(policy(&["web_search"]).allows_native_search());
    }

    #[test]
    fn namespace_identity_survives_policy_normalization() {
        let original = PlatformToolPolicy {
            allowed_tools: vec![
                ToolName::namespaced("alpha", "lookup"),
                ToolName::namespaced("beta", "lookup"),
                ToolName::namespaced("functions", "wait_agent"),
                ToolName::plain("wait_agent"),
            ],
        }
        .normalized()
        .unwrap();
        assert_eq!(original.allowed_tools.len(), 3);
        assert!(!original.kernel_policy().allows(&ToolName::plain("lookup")));
        assert!(
            original
                .kernel_policy()
                .allows(&ToolName::namespaced("alpha", "lookup"))
        );
    }
}
