from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Any


@dataclass(frozen=True)
class CapacityBudget:
    key: str
    limit: int
    queue_max: int
    queue_timeout_ms: int
    scope: str
    source: str
    enforced: bool
    source_status: str = "real"
    shared: bool = False

    def to_status(self, *, inflight: int = 0, queue_depth: int = 0) -> dict[str, Any]:
        data = asdict(self)
        data.update({"inflight": inflight, "queue_depth": queue_depth})
        return data


DEFAULT_UAT_CAPACITY_BUDGETS: dict[str, CapacityBudget] = {
    "gateway.total_inflight": CapacityBudget(
        key="gateway.total_inflight",
        limit=64,
        queue_max=128,
        queue_timeout_ms=3000,
        scope="gateway",
        source="default",
        enforced=True,
    ),
    "gateway.stream_inflight": CapacityBudget(
        key="gateway.stream_inflight",
        limit=16,
        queue_max=128,
        queue_timeout_ms=3000,
        scope="gateway",
        source="default",
        enforced=True,
    ),
    "gateway.non_stream_inflight": CapacityBudget(
        key="gateway.non_stream_inflight",
        limit=48,
        queue_max=128,
        queue_timeout_ms=3000,
        scope="gateway",
        source="default",
        enforced=True,
    ),
    "upstream.langgraph_agent": CapacityBudget(
        key="upstream.langgraph_agent",
        limit=4,
        queue_max=16,
        queue_timeout_ms=3000,
        scope="upstream",
        source="default",
        enforced=True,
        shared=True,
    ),
    "upstream.agent_runtime": CapacityBudget(
        key="upstream.agent_runtime",
        limit=8,
        queue_max=32,
        queue_timeout_ms=3000,
        scope="upstream",
        source="default",
        enforced=True,
        shared=True,
    ),
    "upstream.knowledge_service": CapacityBudget(
        key="upstream.knowledge_service",
        limit=12,
        queue_max=48,
        queue_timeout_ms=3000,
        scope="upstream",
        source="default",
        enforced=True,
        shared=True,
    ),
    "upstream.image_generation": CapacityBudget(
        key="upstream.image_generation",
        limit=2,
        queue_max=8,
        queue_timeout_ms=3000,
        scope="upstream",
        source="default",
        enforced=True,
        shared=True,
    ),
    "provider.dashscope_cn": CapacityBudget(
        key="provider.dashscope_cn",
        limit=8,
        queue_max=16,
        queue_timeout_ms=3000,
        scope="provider",
        source="default",
        enforced=True,
        shared=True,
    ),
    "provider.dashscope_intl": CapacityBudget(
        key="provider.dashscope_intl",
        limit=8,
        queue_max=16,
        queue_timeout_ms=3000,
        scope="provider",
        source="default",
        enforced=True,
        shared=True,
    ),
    "provider.google_gemini": CapacityBudget(
        key="provider.google_gemini",
        limit=4,
        queue_max=16,
        queue_timeout_ms=3000,
        scope="provider",
        source="default",
        enforced=True,
        shared=True,
    ),
    "provider.google_vertex": CapacityBudget(
        key="provider.google_vertex",
        limit=1,
        queue_max=4,
        queue_timeout_ms=3000,
        scope="provider",
        source="default",
        enforced=True,
        shared=True,
    ),
}


SERVICE_UPSTREAM_GROUPS: dict[str, str] = {
    "local-2024-agent": "langgraph_agent",
    "langgraph-agent": "langgraph_agent",
    "agent": "langgraph_agent",
    "assistant": "agent_runtime",
    "agent-runtime": "agent_runtime",
    "knowledge": "knowledge_service",
    "knowledge-service": "knowledge_service",
    "image-generation": "image_generation",
}


PROVIDER_GROUPS: dict[str, str] = {
    "google": "google_gemini",
    "gemini": "google_gemini",
    "google-gemini": "google_gemini",
    "dashscope": "dashscope_cn",
    "dashscope-cn": "dashscope_cn",
    "dashscope_intl": "dashscope_intl",
    "dashscope-intl": "dashscope_intl",
    "vertex": "google_vertex",
    "google-vertex": "google_vertex",
}


def normalize_capacity_group(value: Any, *, prefix: str) -> str | None:
    if value is None:
        return None
    normalized = str(value).strip().lower().replace("-", "_")
    if not normalized:
        return None
    if normalized.startswith(f"{prefix}."):
        normalized = normalized.split(".", 1)[1]
    return normalized


def capacity_config_from_service(service_config: Any | None) -> dict[str, Any]:
    if service_config is None:
        return {}
    if isinstance(service_config, dict):
        raw = service_config.get("capacity") or service_config.get("capacity_config") or {}
        result = dict(raw) if isinstance(raw, dict) else {}
        legacy_limit = service_config.get("concurrency_limit")
    else:
        raw = getattr(service_config, "capacity_config", None)
        config = getattr(service_config, "service_config", None)
        if not isinstance(raw, dict) and config is not None:
            capacity = getattr(config, "capacity", None)
            raw = asdict(capacity) if capacity is not None else None
        metadata = getattr(service_config, "metadata", None)
        if not isinstance(raw, dict) and isinstance(metadata, dict):
            raw = metadata.get("capacity") or metadata.get("capacity_config")
        result = dict(raw) if isinstance(raw, dict) else {}
        legacy_limit = getattr(service_config, "concurrency_limit", None)
    if legacy_limit is not None and result.get("concurrency_limit") is None:
        result["concurrency_limit"] = legacy_limit
    return result


def service_upstream_group(service_id: str, configured_group: Any = None) -> str | None:
    explicit = normalize_capacity_group(configured_group, prefix="upstream")
    if explicit:
        return explicit
    normalized_service = str(service_id or "").strip().lower()
    return SERVICE_UPSTREAM_GROUPS.get(normalized_service)


def provider_budget_key(provider_id: Any) -> str | None:
    normalized = normalize_capacity_group(provider_id, prefix="provider")
    if not normalized:
        return None
    group = PROVIDER_GROUPS.get(normalized, normalized)
    key = f"provider.{group}"
    return key if key in DEFAULT_UAT_CAPACITY_BUDGETS else None


def _apply_override(default: CapacityBudget, capacity_config: dict[str, Any]) -> CapacityBudget:
    if not capacity_config:
        return default

    limit = capacity_config.get("concurrency_limit", capacity_config.get("limit"))
    queue_max = capacity_config.get("queue_max", capacity_config.get("max_queue_size"))
    timeout = capacity_config.get("queue_timeout_ms")
    if timeout is None and capacity_config.get("queue_timeout_seconds") is not None:
        timeout = float(capacity_config["queue_timeout_seconds"]) * 1000

    return CapacityBudget(
        key=default.key,
        limit=max(int(limit if limit is not None else default.limit), 1),
        queue_max=max(int(queue_max if queue_max is not None else default.queue_max), 0),
        queue_timeout_ms=max(
            int(timeout if timeout is not None else default.queue_timeout_ms),
            1,
        ),
        scope=default.scope,
        source="service_config",
        enforced=default.enforced,
        source_status=default.source_status,
        shared=default.shared,
    )


class CapacityResolver:
    """Resolve UAT capacity budgets into concrete admission budgets."""

    def __init__(self, *, mode: str = "single-node", cluster_epoch: str = "uat-2026-05") -> None:
        self.mode = mode
        self.cluster_epoch = cluster_epoch

    async def resolve(
        self,
        *,
        tenant_id: str,
        service_id: str,
        request_class: str,
        upstream_group: str | None,
        provider_id: str | None,
        is_admin_read: bool = False,
        service_config: Any | None = None,
        resource_kind: str = "request",
    ) -> list[CapacityBudget]:
        del tenant_id
        capacity_config = capacity_config_from_service(service_config)
        if resource_kind not in {"request", "run", "provider", "capability", "job", "sse"}:
            raise ValueError("unknown_capacity_resource_kind")
        if resource_kind == "request":
            budgets = [DEFAULT_UAT_CAPACITY_BUDGETS["gateway.total_inflight"]]
            request_class_key = (
                "gateway.stream_inflight" if request_class == "stream"
                else "gateway.non_stream_inflight"
            )
            budgets.append(DEFAULT_UAT_CAPACITY_BUDGETS[request_class_key])
        else:
            # Parent runs and callbacks own distinct resources. A callback must
            # never need the same final slot held by the run awaiting it.
            limits = {"run": 8, "provider": 32, "capability": 16, "job": 4, "sse": 64}
            budgets = [CapacityBudget(
                key=f"gateway.{resource_kind}_inflight", limit=limits[resource_kind],
                queue_max=32, queue_timeout_ms=3000, scope=resource_kind,
                source="default", enforced=True, shared=resource_kind != "sse",
            )]
        if resource_kind == "sse":
            return budgets

        if is_admin_read:
            return self._dedupe(budgets)

        group = service_upstream_group(
            service_id,
            capacity_config.get("upstream_group") or upstream_group,
        )
        if group:
            key = f"upstream.{group}"
            default = DEFAULT_UAT_CAPACITY_BUDGETS.get(key)
            if default is not None:
                budgets.append(_apply_override(default, capacity_config))
            else:
                budgets.append(self._configured_or_missing(key, capacity_config))
        else:
            budgets.append(self._configured_or_missing(f"service.{service_id or 'unknown'}", capacity_config))

        if resource_kind != "request":
            budgets = [
                CapacityBudget(**{**asdict(budget), "key": f"{resource_kind}.{budget.key}"})
                if budget.scope in {"upstream", "service"} else budget
                for budget in budgets
            ]
        provider_key = provider_budget_key(provider_id)
        if provider_key and resource_kind in {"request", "provider"}:
            budgets.append(DEFAULT_UAT_CAPACITY_BUDGETS[provider_key])

        return self._dedupe(budgets)

    def status_budgets(self, snapshot: dict[str, dict[str, int]] | None = None) -> list[dict[str, Any]]:
        snapshot = snapshot or {}
        rows: list[dict[str, Any]] = []
        for key in sorted(DEFAULT_UAT_CAPACITY_BUDGETS):
            budget = DEFAULT_UAT_CAPACITY_BUDGETS[key]
            usage = snapshot.get(key, {})
            rows.append(
                budget.to_status(
                    inflight=int(usage.get("inflight", 0) or 0),
                    queue_depth=int(usage.get("queue_depth", 0) or 0),
                )
            )
        return rows


    @classmethod
    def _configured_or_missing(cls, key: str, config: dict[str, Any]) -> CapacityBudget:
        missing = cls._missing_budget(key)
        if config.get("concurrency_limit", config.get("limit")) is None:
            return missing
        configured = CapacityBudget(**{
            **asdict(missing), "enforced": True, "shared": True, "source_status": "real",
            "queue_max": 16, "queue_timeout_ms": 3000,
        })
        return _apply_override(configured, config)

    @staticmethod
    def _missing_budget(key: str) -> CapacityBudget:
        return CapacityBudget(
            key=key,
            limit=1,
            queue_max=0,
            queue_timeout_ms=1,
            scope="service" if key.startswith("service.") else "upstream",
            source="missing",
            enforced=False,
            source_status="missing",
        )

    @staticmethod
    def _dedupe(budgets: list[CapacityBudget]) -> list[CapacityBudget]:
        deduped: dict[str, CapacityBudget] = {}
        for budget in budgets:
            deduped[budget.key] = budget
        return list(deduped.values())
