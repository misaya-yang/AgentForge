"""Read-only recovery of an existing Gateway run, without resubmitting execution."""

from __future__ import annotations

import math
import re
from typing import TYPE_CHECKING, Any

from ai_assistant.exceptions import AssistantError

if TYPE_CHECKING:
    from ai_assistant.transport.http import HTTPTransport

_RUN_ID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z")
_ENGINES = {"agent_runtime", "openai_agents", "claude_agent_sdk"}
_FIELDS = (
    "run_id", "session_id", "engine", "status", "terminal_reason",
    "started_at", "finished_at", "updated_at",
)
_TOKEN_FIELDS = (
    "input_tokens", "output_tokens", "total_tokens", "cached_tokens", "cache_write_tokens",
    "cache_read_input_tokens", "cache_creation_input_tokens",
)
_USAGE_STATES = {
    "state": {"reported", "unavailable"},
    "cost_state": {"reported", "unavailable"},
    "provider_outcome": {"completed", "unknown"},
    "platform_pricing_state": {"two_rate_compatible", "reconciliation_required"},
    "audit_persistence": {"durable", "unresolved_admission", "unavailable", "identity_conflict"},
}


def validate_run_id(run_id: str) -> None:
    if not isinstance(run_id, str) or not _RUN_ID.fullmatch(run_id):
        raise ValueError("Run ID must be a canonical lowercase UUID")


def _tokens(value: Any) -> dict[str, Any]:
    if not isinstance(value, dict):
        return {}
    result: dict[str, Any] = {
        key: count for key in _TOKEN_FIELDS
        if type(count := value.get(key)) is int and 0 <= count <= 2**63 - 1
    }
    details = value.get("input_tokens_details")
    if isinstance(details, dict):
        # Only one level is part of the public contract. Never traverse raw metadata.
        result["input_tokens_details"] = {
            key: count for key in ("cached_tokens", "cache_write_tokens")
            if type(count := details.get(key)) is int and 0 <= count <= 2**63 - 1
        }
    return result


def run_view(run_id: str, value: Any) -> dict[str, Any]:
    """Match the native client projection; absent usage is never zero or settled."""
    run = value.get("run") if isinstance(value, dict) else None
    if (
        not isinstance(run, dict)
        or run.get("run_id") != run_id
        or not isinstance(run.get("engine"), str)
        or run["engine"] not in _ENGINES
        or not isinstance(run.get("status"), str)
        or not run["status"]
    ):
        raise AssistantError("Invalid run identity or status response")
    result = {key: run[key] for key in _FIELDS if key in run}
    usage = run.get("usage")
    if isinstance(usage, dict):
        filtered = _tokens(usage)
        sdk = usage.get("sdk_usage")
        if isinstance(sdk, dict):
            metadata: dict[str, Any] = {
                key: item for key, allowed in _USAGE_STATES.items()
                if isinstance(item := sdk.get(key), str) and item in allowed
            }
            metadata["accounting_status"] = "not_asserted"
            cost = sdk.get("provider_cost_usd")
            if type(cost) in (int, float) and 0 <= cost <= 1.7976931348623157e308:
                if math.isfinite(cost):
                    metadata["provider_cost_usd"] = cost
            raw = sdk.get("provider_usage")
            if isinstance(raw, dict):
                metadata["provider_usage"] = _tokens(raw)
            elif isinstance(raw, list) and len(raw) <= 32:
                metadata["provider_usage"] = [_tokens(item) for item in raw]
            filtered["sdk_usage"] = metadata
        result["usage"] = filtered
    return {"run": result}


class RunModule:
    """Inspect owner-authorized managed or SDK runs through the Gateway only.

    No session, thread, turn, approval or provider request is created by a read.
    SDK identities must come from the Gateway's ``assistant_run_id`` metadata.
    """

    def __init__(self, transport: HTTPTransport) -> None:
        self._transport = transport

    async def get(self, run_id: str) -> dict[str, Any]:
        """Read current status/usage. Authentication and ownership are server-authoritative.

        A finished execution can still have unavailable usage. Refresh with another
        GET when needed; do not repeat the original submission to recover a stream.
        """
        validate_run_id(run_id)
        response = await self._transport.request("GET", f"/api/v1/assistant/runs/{run_id}")
        try:
            value = response.json()
        except ValueError as exc:
            raise AssistantError("Invalid run response") from exc
        return run_view(run_id, value)
