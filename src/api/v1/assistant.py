"""Stable facade for the ``/api/v1/assistant`` surface (ARC-01).

The route handlers live in ``src/api/v1/_assistant_routes`` split by use case
(catalog / runs / chat / sessions / artifacts / metrics) and the shared
entry-side services live in ``src/services/assistant_entry``.  This module
only builds the public router and keeps time-limited compatibility
re-exports; it must not grow handler logic again.

Contract baseline: ``tmp/assistant-api-routes-before.json`` — the split is
verified zero-drift for paths, methods, operation ids and status codes, with
the single sanctioned exception of 500-error detail sanitization (ARC-01
deliverable 6).
"""

from __future__ import annotations

from ai_gateway_core.storage import get_artifact_storage as get_artifact_storage
from fastapi import APIRouter

from ...services.assistant_entry.model_access import (
    assistant_model_service as _assistant_model_service,
)
from ...services.assistant_entry.model_access import (
    check_model_permission as _check_model_permission,
)
from ...services.assistant_entry.model_access import (
    effective_chat_model_id as _effective_chat_model_id,
)
from ...services.assistant_entry.model_access import (
    load_visible_assistant_models as _load_visible_assistant_models,
)
from ...services.assistant_entry.model_access import user_can_access_model as _user_can_access_model
from ...services.assistant_entry.model_access import (
    visible_assistant_models as _visible_assistant_models,
)
from ...services.assistant_entry.run_queries import agent_runtime_control as _agent_runtime_control
from ...services.assistant_entry.session_binding import (
    ensure_agent_runtime_session as _ensure_agent_runtime_session,
)
from ...services.assistant_entry.session_binding import get_session_manager as get_session_manager
from ...services.assistant_entry.session_binding import (
    session_runtime_assignment as _session_runtime_assignment,
)
from ...services.assistant_entry.session_binding import (
    validate_chat_session_access as _validate_chat_session_access,
)
from ..schemas.artifacts import ArtifactCreateRequest as ArtifactCreateRequest
from ..schemas.artifacts import ArtifactInfo as ArtifactInfo
from ..schemas.artifacts import ArtifactListResponse as ArtifactListResponse
from ..schemas.assistant import AssistantChatRequest as AssistantChatRequest
from ..schemas.assistant import AssistantChatResponse as AssistantChatResponse
from ..schemas.assistant import AssistantConfigResponse as AssistantConfigResponse
from ..schemas.assistant import DatasetsListResponse as DatasetsListResponse
from ..schemas.assistant import ModelsListResponse as ModelsListResponse
from ._artifact_headers import attachment_content_disposition as attachment_content_disposition
from ._assistant_routes.artifacts import (
    _browser_artifact_download_url as _browser_artifact_download_url,
)
from ._assistant_routes.artifacts import (
    _is_missing_artifact_schema_error as _is_missing_artifact_schema_error,
)
from ._assistant_routes.artifacts import (
    _raise_artifact_not_found_if_schema_missing as _raise_artifact_not_found_if_schema_missing,
)
from ._assistant_routes.artifacts import create_artifact as create_artifact
from ._assistant_routes.artifacts import delete_artifact as delete_artifact
from ._assistant_routes.artifacts import download_artifact as download_artifact
from ._assistant_routes.artifacts import get_artifact as get_artifact
from ._assistant_routes.artifacts import list_session_artifacts as list_session_artifacts
from ._assistant_routes.artifacts import router as _artifacts_router
from ._assistant_routes.catalog import get_config as get_config
from ._assistant_routes.catalog import get_policies as get_policies
from ._assistant_routes.catalog import list_datasets as list_datasets
from ._assistant_routes.catalog import list_models as list_models
from ._assistant_routes.catalog import list_tools as list_tools
from ._assistant_routes.catalog import router as _catalog_router
from ._assistant_routes.chat import _start_agent_runtime_turn as _start_agent_runtime_turn
from ._assistant_routes.chat import chat as chat
from ._assistant_routes.chat import chat_stream as chat_stream
from ._assistant_routes.chat import router as _chat_router
from ._assistant_routes.memory import router as _memory_router
from ._assistant_routes.metrics import get_session_metrics as get_session_metrics
from ._assistant_routes.metrics import get_tenant_metrics as get_tenant_metrics
from ._assistant_routes.metrics import router as _metrics_router
from ._assistant_routes.runs import approve_tool_call as approve_tool_call
from ._assistant_routes.runs import cancel_task as cancel_task
from ._assistant_routes.runs import get_run_status as get_run_status
from ._assistant_routes.runs import prepare_run_resume as prepare_run_resume
from ._assistant_routes.runs import router as _runs_router
from ._assistant_routes.schemas import ApprovalRequest as ApprovalRequest
from ._assistant_routes.schemas import ApprovalResponse as ApprovalResponse
from ._assistant_routes.schemas import AssistantPoliciesResponse as AssistantPoliciesResponse
from ._assistant_routes.schemas import ContextMetricsResponse as ContextMetricsResponse
from ._assistant_routes.schemas import ResumeRequest as ResumeRequest
from ._assistant_routes.schemas import ResumeResponse as ResumeResponse
from ._assistant_routes.schemas import RunStatusResponse as RunStatusResponse
from ._assistant_routes.schemas import SessionCreateRequest as SessionCreateRequest
from ._assistant_routes.schemas import SessionHistoryMessage as SessionHistoryMessage
from ._assistant_routes.schemas import SessionHistoryResponse as SessionHistoryResponse
from ._assistant_routes.schemas import SessionListResponse as SessionListResponse
from ._assistant_routes.schemas import SessionResponse as SessionResponse
from ._assistant_routes.schemas import TaskCancelRequest as TaskCancelRequest
from ._assistant_routes.schemas import TaskCancelResponse as TaskCancelResponse
from ._assistant_routes.schemas import TenantMetricsResponse as TenantMetricsResponse
from ._assistant_routes.schemas import ToolInfoResponse as ToolInfoResponse
from ._assistant_routes.schemas import ToolsListResponse as ToolsListResponse
from ._assistant_routes.sessions import (
    _list_assistant_session_summaries as _list_assistant_session_summaries,
)
from ._assistant_routes.sessions import _list_assistant_sessions as _list_assistant_sessions
from ._assistant_routes.sessions import create_session as create_session
from ._assistant_routes.sessions import delete_session as delete_session
from ._assistant_routes.sessions import get_session as get_session
from ._assistant_routes.sessions import get_session_history as get_session_history
from ._assistant_routes.sessions import list_sessions as list_sessions
from ._assistant_routes.sessions import router as _sessions_router
from ._assistant_routes.tools import router as _tools_router

router = APIRouter(prefix="/assistant", tags=["assistant"])

# Registration groups routes by use case.  The assistant path patterns were
# verified pairwise non-overlapping before the split, so grouping cannot
# change matching behaviour (see the contract diff cited in the module
# docstring).
for _sub_router in (
    _catalog_router,
    _runs_router,
    _chat_router,
    _memory_router,
    _tools_router,
    _sessions_router,
    _artifacts_router,
    _metrics_router,
):
    router.include_router(_sub_router)


# ---------------------------------------------------------------------------
# Time-limited compatibility surface (ARC-01).
#
# Everything below the router construction is a re-export kept for
# pre-ARC-01 import paths.  Removal condition: delete after ARC-08 once an
# import-scan gate (`rg "from src.api.v1.assistant import"` /
# `from .assistant import` across src/, tests/, scripts/, apps/, sdk/)
# shows zero hits for these names.  Do not add new consumers — import from
# the real home listed per group above.
#
# Known consumers at split time:
#   - router: src/api/router.py (permanent, not compat)
#   - _start_agent_runtime_turn: no Python import consumers, but the textual
#     single-kernel gate (scripts/harness/agent_runtime_single_kernel_gate.py)
#     asserts this name exists in this file's source
#   - prepare_run_resume: tests/api/test_gateway_capability_matrix.py
#   - get_artifact_storage: former monkeypatch seam of
#     tests/api/test_assistant_sessions.py (migrated to patch
#     src.api.v1._assistant_routes.artifacts directly)
#   - underscore helper aliases: src/api/v1/agent_runtime.py and
#     src/api/v1/responses.py were migrated in the same change; no remaining
#     consumers are known.
# ---------------------------------------------------------------------------
__all__ = [
    "router",
    # Handlers (home: src/api/v1/_assistant_routes/*)
    "approve_tool_call",
    "cancel_task",
    "chat",
    "chat_stream",
    "create_artifact",
    "create_session",
    "delete_artifact",
    "delete_session",
    "download_artifact",
    "get_artifact",
    "get_config",
    "get_policies",
    "get_run_status",
    "get_session",
    "get_session_history",
    "get_session_metrics",
    "get_tenant_metrics",
    "list_datasets",
    "list_models",
    "list_session_artifacts",
    "list_sessions",
    "list_tools",
    "prepare_run_resume",
    # Private schemas (home: src/api/v1/_assistant_routes/schemas.py)
    "ApprovalRequest",
    "ApprovalResponse",
    "AssistantPoliciesResponse",
    "ContextMetricsResponse",
    "ResumeRequest",
    "ResumeResponse",
    "RunStatusResponse",
    "SessionCreateRequest",
    "SessionHistoryMessage",
    "SessionHistoryResponse",
    "SessionListResponse",
    "SessionResponse",
    "TaskCancelRequest",
    "TaskCancelResponse",
    "TenantMetricsResponse",
    "ToolInfoResponse",
    "ToolsListResponse",
    # Public schemas re-exported (home: src/api/schemas/*)
    "ArtifactCreateRequest",
    "ArtifactInfo",
    "ArtifactListResponse",
    "AssistantChatRequest",
    "AssistantChatResponse",
    "AssistantConfigResponse",
    "DatasetsListResponse",
    "ModelsListResponse",
    # Helpers (home: src/services/assistant_entry/*, listed under legacy names)
    "_agent_runtime_control",
    "_assistant_model_service",
    "_browser_artifact_download_url",
    "_check_model_permission",
    "_effective_chat_model_id",
    "_ensure_agent_runtime_session",
    "_is_missing_artifact_schema_error",
    "_list_assistant_session_summaries",
    "_list_assistant_sessions",
    "_load_visible_assistant_models",
    "_raise_artifact_not_found_if_schema_missing",
    "_session_runtime_assignment",
    "_start_agent_runtime_turn",
    "_user_can_access_model",
    "_validate_chat_session_access",
    "_visible_assistant_models",
    "attachment_content_disposition",
    "get_artifact_storage",
    "get_session_manager",
]
