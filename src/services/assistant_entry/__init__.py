"""Shared Gateway entry services for the Assistant, Responses and chat edges.

These modules hold the Gateway-owned policy that every public chat entry point
needs (model authorization, session binding, run/approval lookups) so that
route modules stay thin and never import each other as libraries.

Extraction target: ARC-01 (platform-architecture-convergence-prd-2026-08).
"""

from .launch_resolution import (
    AgentLaunchResolutionError,
    ResolvedAgentLaunchV1,
    resolve_agent_launch,
)
from .model_access import (
    assistant_model_service,
    chat_body_with_model,
    check_model_permission,
    effective_chat_model_id,
    load_visible_assistant_models,
    user_can_access_model,
    visible_assistant_models,
)
from .run_queries import (
    agent_runtime_control,
    fetch_agent_runtime_run,
    fetch_approval_run_owner,
    fetch_cancellable_run,
)
from .session_binding import (
    ASSISTANT_SERVICE_IDS,
    ensure_agent_runtime_session,
    get_session_manager,
    session_runtime_assignment,
    validate_chat_session_access,
)

__all__ = [
    "ASSISTANT_SERVICE_IDS",
    "AgentLaunchResolutionError",
    "ResolvedAgentLaunchV1",
    "agent_runtime_control",
    "assistant_model_service",
    "chat_body_with_model",
    "check_model_permission",
    "effective_chat_model_id",
    "ensure_agent_runtime_session",
    "fetch_agent_runtime_run",
    "fetch_approval_run_owner",
    "fetch_cancellable_run",
    "get_session_manager",
    "load_visible_assistant_models",
    "resolve_agent_launch",
    "session_runtime_assignment",
    "user_can_access_model",
    "validate_chat_session_access",
    "visible_assistant_models",
]
