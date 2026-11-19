//! Platform-owned tool side-effect lifecycle.
//!
//! Agent owns the model/tool loop, but the platform owns the safety ledger
//! around a tool call.  In particular, an interruption must never turn an
//! already-dispatched write into an ordinary retry.  This state machine is
//! intentionally independent from any concrete tool implementation so the
//! same rules apply to MCP, Local Node, office tools, and sandboxes.

use std::collections::HashMap;
use std::collections::HashSet;

use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    ReadOnly,
    Write,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolTerminalStatus {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    SideEffectUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "phase")]
pub enum ToolCallState {
    Published,
    AwaitingApproval,
    Dispatched,
    Terminal { status: ToolTerminalStatus },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ToolResult {
    pub call_id: String,
    pub status: ToolTerminalStatus,
    pub detail: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalBinding {
    pub approval_id: String,
    pub tenant_id: String,
    pub user_id: String,
    pub session_id: String,
    pub run_id: String,
    pub arguments_hash: String,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApprovalScope<'a> {
    pub tenant_id: &'a str,
    pub user_id: &'a str,
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub arguments_hash: &'a str,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublishDisposition {
    Published,
    AwaitingApproval { approval_id: String },
    AlreadyPublished(ToolCallState),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    InvalidCallId,
    InvalidApproval,
    IdempotencyConflict,
    ApprovalNotFound,
    ApprovalScopeMismatch,
    ApprovalExpired,
    ApprovalReplay,
    DispatchFence,
    NotDispatchable,
    Terminal,
    ResultFence,
    TurnNotClosed,
}

#[derive(Clone, Debug)]
struct ToolCallRecord {
    effect: ToolEffect,
    arguments_hash: String,
    state: ToolCallState,
    dispatch_fence: Option<String>,
    result: Option<ToolResult>,
}

/// An append-once-in-spirit ledger for one turn's published tool calls.
///
/// The record is kept after terminal completion so provider retries and
/// reconnects can replay the exact result without executing the side effect
/// twice.  Callers should persist this record alongside the runtime Item
/// Store; this type deliberately has no external I/O.
#[derive(Default)]
pub struct ToolLifecycleLedger {
    calls: HashMap<String, ToolCallRecord>,
    approvals: HashMap<String, String>,
    approval_bindings: HashMap<String, ApprovalBinding>,
    consumed_approvals: HashSet<String>,
}

impl ToolLifecycleLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish one model tool-use item. Writes and unknown effects are held
    /// until their exact, scope-bound approval is consumed.
    pub fn publish(
        &mut self,
        call_id: impl Into<String>,
        effect: ToolEffect,
        arguments_hash: impl Into<String>,
        approval: Option<ApprovalBinding>,
    ) -> Result<PublishDisposition, LifecycleError> {
        let call_id = call_id.into();
        let arguments_hash = arguments_hash.into();
        if call_id.is_empty() || arguments_hash.is_empty() {
            return Err(LifecycleError::InvalidCallId);
        }
        if let Some(existing) = self.calls.get(&call_id) {
            if existing.effect != effect || existing.arguments_hash != arguments_hash {
                return Err(LifecycleError::IdempotencyConflict);
            }
            return Ok(PublishDisposition::AlreadyPublished(existing.state.clone()));
        }

        let mut approval_id_for_disposition = None;
        let state = match effect {
            ToolEffect::ReadOnly => ToolCallState::Published,
            ToolEffect::Write | ToolEffect::Unknown => {
                let binding = approval.ok_or(LifecycleError::InvalidApproval)?;
                if binding.approval_id.is_empty()
                    || binding.arguments_hash != arguments_hash
                    || binding.run_id.is_empty()
                {
                    return Err(LifecycleError::InvalidApproval);
                }
                let approval_id = binding.approval_id.clone();
                if self.approvals.contains_key(&approval_id)
                    || self.approval_bindings.contains_key(&approval_id)
                    || self.consumed_approvals.contains(&approval_id)
                {
                    return Err(LifecycleError::IdempotencyConflict);
                }
                self.approvals.insert(approval_id.clone(), call_id.clone());
                self.approval_bindings.insert(approval_id.clone(), binding);
                approval_id_for_disposition = Some(approval_id);
                ToolCallState::AwaitingApproval
            }
        };
        let disposition = match &state {
            ToolCallState::Published => PublishDisposition::Published,
            ToolCallState::AwaitingApproval => {
                let approval_id =
                    approval_id_for_disposition.ok_or(LifecycleError::InvalidApproval)?;
                PublishDisposition::AwaitingApproval { approval_id }
            }
            _ => unreachable!("new tool calls cannot be terminal"),
        };
        self.calls.insert(
            call_id,
            ToolCallRecord {
                effect,
                arguments_hash,
                state,
                dispatch_fence: None,
                result: None,
            },
        );
        Ok(disposition)
    }

    /// Consume an approval exactly once and only for the original scope and
    /// arguments. No approval can widen a call's authority or be replayed.
    pub fn approve(
        &mut self,
        approval_id: &str,
        scope: ApprovalScope<'_>,
        now_ms: u64,
    ) -> Result<(), LifecycleError> {
        if self.consumed_approvals.contains(approval_id) {
            return Err(LifecycleError::ApprovalReplay);
        }
        let call_id = self
            .approvals
            .get(approval_id)
            .ok_or(LifecycleError::ApprovalNotFound)?
            .clone();
        let binding = self
            .approval_bindings
            .get_mut(approval_id)
            .ok_or(LifecycleError::ApprovalNotFound)?;
        if binding.expires_at_ms <= now_ms {
            return Err(LifecycleError::ApprovalExpired);
        }
        if binding.tenant_id != scope.tenant_id
            || binding.user_id != scope.user_id
            || binding.session_id != scope.session_id
            || binding.run_id != scope.run_id
            || binding.arguments_hash != scope.arguments_hash
        {
            return Err(LifecycleError::ApprovalScopeMismatch);
        }
        if binding.approval_id.is_empty() {
            return Err(LifecycleError::ApprovalNotFound);
        }
        let call = self
            .calls
            .get_mut(&call_id)
            .ok_or(LifecycleError::ApprovalNotFound)?;
        if !matches!(call.state, ToolCallState::AwaitingApproval) {
            return Err(LifecycleError::ApprovalReplay);
        }
        call.state = ToolCallState::Published;
        // Removing the binding makes a second approval a replay, while the
        // call remains idempotently executable after approval.
        self.approval_bindings.remove(approval_id);
        self.consumed_approvals.insert(approval_id.to_string());
        Ok(())
    }

    /// Establish the single dispatch fence for a call. A different worker
    /// cannot claim an already-dispatched call.
    pub fn dispatch(
        &mut self,
        call_id: &str,
        dispatch_fence: impl Into<String>,
    ) -> Result<(), LifecycleError> {
        let dispatch_fence = dispatch_fence.into();
        if dispatch_fence.is_empty() {
            return Err(LifecycleError::DispatchFence);
        }
        let call = self
            .calls
            .get_mut(call_id)
            .ok_or(LifecycleError::NotDispatchable)?;
        match &call.state {
            ToolCallState::Published => {
                call.state = ToolCallState::Dispatched;
                call.dispatch_fence = Some(dispatch_fence);
                Ok(())
            }
            ToolCallState::Dispatched
                if call.dispatch_fence.as_deref() == Some(&dispatch_fence) =>
            {
                Ok(())
            }
            ToolCallState::Dispatched => Err(LifecycleError::DispatchFence),
            ToolCallState::AwaitingApproval | ToolCallState::Terminal { .. } => {
                Err(LifecycleError::NotDispatchable)
            }
        }
    }

    /// Record the sole terminal tool_result. Replaying the same result is
    /// safe; a different result or fence is rejected.
    pub fn terminal(
        &mut self,
        call_id: &str,
        dispatch_fence: &str,
        status: ToolTerminalStatus,
        detail: impl Into<String>,
    ) -> Result<ToolResult, LifecycleError> {
        let detail = detail.into();
        let call = self
            .calls
            .get_mut(call_id)
            .ok_or(LifecycleError::ResultFence)?;
        if call.dispatch_fence.as_deref() != Some(dispatch_fence) {
            return Err(LifecycleError::ResultFence);
        }
        if let Some(result) = &call.result {
            if result.status == status && result.detail == detail {
                return Ok(result.clone());
            }
            return Err(LifecycleError::ResultFence);
        }
        if !matches!(call.state, ToolCallState::Dispatched) {
            return Err(LifecycleError::ResultFence);
        }
        let result = ToolResult {
            call_id: call_id.to_string(),
            status,
            detail,
        };
        call.state = ToolCallState::Terminal { status };
        call.result = Some(result.clone());
        Ok(result)
    }

    /// Close one call when a turn is interrupted or the worker crashes. A
    /// dispatched write/unknown call is explicitly uncertain and must not be
    /// retried; a read call can safely become a timeout.
    pub fn recover_call(&mut self, call_id: &str) -> Result<ToolResult, LifecycleError> {
        let (state, fence, effect) = {
            let call = self.calls.get(call_id).ok_or(LifecycleError::ResultFence)?;
            (call.state.clone(), call.dispatch_fence.clone(), call.effect)
        };
        match state {
            ToolCallState::Published | ToolCallState::AwaitingApproval => {
                let Some(call) = self.calls.get_mut(call_id) else {
                    return Err(LifecycleError::ResultFence);
                };
                let result = ToolResult {
                    call_id: call_id.to_string(),
                    status: ToolTerminalStatus::Cancelled,
                    detail: "not_dispatched".to_string(),
                };
                call.state = ToolCallState::Terminal {
                    status: result.status,
                };
                call.result = Some(result.clone());
                Ok(result)
            }
            ToolCallState::Dispatched => {
                let status = match effect {
                    ToolEffect::ReadOnly => ToolTerminalStatus::TimedOut,
                    ToolEffect::Write | ToolEffect::Unknown => {
                        ToolTerminalStatus::SideEffectUnknown
                    }
                };
                self.terminal(
                    call_id,
                    fence.as_deref().ok_or(LifecycleError::ResultFence)?,
                    status,
                    match status {
                        ToolTerminalStatus::TimedOut => "dispatch_interrupted",
                        ToolTerminalStatus::SideEffectUnknown => "dispatch_effect_unknown",
                        _ => unreachable!(),
                    },
                )
            }
            ToolCallState::Terminal { .. } => self
                .calls
                .get(call_id)
                .and_then(|call| call.result.clone())
                .ok_or(LifecycleError::ResultFence),
        }
    }

    pub fn recover_all(&mut self) -> Vec<ToolResult> {
        let mut ids: Vec<String> = self.calls.keys().cloned().collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|call_id| self.recover_call(&call_id).ok())
            .collect()
    }

    pub fn state(&self, call_id: &str) -> Option<&ToolCallState> {
        self.calls.get(call_id).map(|call| &call.state)
    }

    pub fn result(&self, call_id: &str) -> Option<&ToolResult> {
        self.calls
            .get(call_id)
            .and_then(|call| call.result.as_ref())
    }

    /// A turn can emit its terminal event only after every published tool-use
    /// has exactly one terminal tool-result.
    pub fn ensure_turn_closed(&self) -> Result<(), LifecycleError> {
        if self.calls.values().all(|call| {
            matches!(call.state, ToolCallState::Terminal { .. }) && call.result.is_some()
        }) {
            Ok(())
        } else {
            Err(LifecycleError::TurnNotClosed)
        }
    }

    pub fn tool_use_count(&self) -> usize {
        self.calls.len()
    }

    pub fn tool_result_count(&self) -> usize {
        self.calls
            .values()
            .filter(|call| call.result.is_some())
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(id: &str, hash: &str) -> ApprovalBinding {
        ApprovalBinding {
            approval_id: id.to_string(),
            tenant_id: "tenant".to_string(),
            user_id: "user".to_string(),
            session_id: "session".to_string(),
            run_id: "run".to_string(),
            arguments_hash: hash.to_string(),
            expires_at_ms: 100,
        }
    }

    fn scope<'a>(hash: &'a str) -> ApprovalScope<'a> {
        ApprovalScope {
            tenant_id: "tenant",
            user_id: "user",
            session_id: "session",
            run_id: "run",
            arguments_hash: hash,
        }
    }

    #[test]
    fn write_requires_exact_one_time_approval() {
        let mut ledger = ToolLifecycleLedger::new();
        assert!(matches!(
            ledger.publish(
                "write-1",
                ToolEffect::Write,
                "hash",
                Some(approval("a-1", "hash"))
            ),
            Ok(PublishDisposition::AwaitingApproval { .. })
        ));
        assert_eq!(
            ledger.dispatch("write-1", "fence"),
            Err(LifecycleError::NotDispatchable)
        );
        assert_eq!(
            ledger.approve("a-1", scope("wrong"), 1),
            Err(LifecycleError::ApprovalScopeMismatch)
        );
        ledger.approve("a-1", scope("hash"), 1).expect("approval");
        assert_eq!(
            ledger.approve("a-1", scope("hash"), 1),
            Err(LifecycleError::ApprovalReplay)
        );
        ledger.dispatch("write-1", "fence").expect("dispatch");
    }

    #[test]
    fn dispatch_fence_and_terminal_result_are_idempotent() {
        let mut ledger = ToolLifecycleLedger::new();
        ledger
            .publish("read-1", ToolEffect::ReadOnly, "hash", None)
            .expect("publish");
        ledger.dispatch("read-1", "fence").expect("dispatch");
        ledger
            .dispatch("read-1", "fence")
            .expect("same dispatch replay");
        assert_eq!(
            ledger.dispatch("read-1", "other"),
            Err(LifecycleError::DispatchFence)
        );
        let result = ledger
            .terminal("read-1", "fence", ToolTerminalStatus::Succeeded, "ok")
            .expect("result");
        assert_eq!(
            ledger.terminal("read-1", "stale-fence", ToolTerminalStatus::Succeeded, "ok"),
            Err(LifecycleError::ResultFence)
        );
        assert_eq!(
            ledger.terminal("read-1", "fence", ToolTerminalStatus::Succeeded, "ok"),
            Ok(result)
        );
        assert_eq!(ledger.tool_use_count(), ledger.tool_result_count());
    }

    #[test]
    fn interruption_closes_undispatched_and_unknown_effects_without_retry() {
        let mut ledger = ToolLifecycleLedger::new();
        ledger
            .publish("pending", ToolEffect::ReadOnly, "p", None)
            .expect("publish");
        ledger
            .publish("write", ToolEffect::Unknown, "w", Some(approval("a", "w")))
            .expect("publish");
        ledger.approve("a", scope("w"), 1).expect("approval");
        ledger.dispatch("write", "write-fence").expect("dispatch");
        let results = ledger.recover_all();
        assert_eq!(results.len(), 2);
        assert_eq!(
            ledger.result("pending").map(|r| r.status),
            Some(ToolTerminalStatus::Cancelled)
        );
        assert_eq!(
            ledger.result("write").map(|r| r.status),
            Some(ToolTerminalStatus::SideEffectUnknown)
        );
        assert_eq!(
            ledger
                .recover_call("write")
                .expect("idempotent recovery")
                .status,
            ToolTerminalStatus::SideEffectUnknown
        );
        assert!(ledger.ensure_turn_closed().is_ok());
    }

    #[test]
    fn crash_recovery_times_out_dispatched_reads() {
        let mut ledger = ToolLifecycleLedger::new();
        ledger
            .publish("read", ToolEffect::ReadOnly, "hash", None)
            .expect("publish");
        ledger.dispatch("read", "fence").expect("dispatch");
        assert_eq!(
            ledger.recover_call("read").expect("recover").status,
            ToolTerminalStatus::TimedOut
        );
    }

    #[test]
    fn duplicate_call_id_cannot_change_arguments_or_effect() {
        let mut ledger = ToolLifecycleLedger::new();
        ledger
            .publish("same", ToolEffect::ReadOnly, "hash", None)
            .expect("publish");
        assert_eq!(
            ledger.publish(
                "same",
                ToolEffect::Write,
                "hash",
                Some(approval("a", "hash"))
            ),
            Err(LifecycleError::IdempotencyConflict)
        );
        assert_eq!(
            ledger.publish("same", ToolEffect::ReadOnly, "other", None),
            Err(LifecycleError::IdempotencyConflict)
        );
    }

    #[test]
    fn consumed_approval_id_cannot_be_published_again() {
        let mut ledger = ToolLifecycleLedger::new();
        ledger
            .publish(
                "first",
                ToolEffect::Write,
                "hash",
                Some(approval("approval-1", "hash")),
            )
            .expect("first publish");
        ledger
            .approve("approval-1", scope("hash"), 1)
            .expect("first approval");
        assert_eq!(
            ledger.publish(
                "second",
                ToolEffect::Write,
                "hash-2",
                Some(approval("approval-1", "hash-2")),
            ),
            Err(LifecycleError::IdempotencyConflict)
        );
    }
}
