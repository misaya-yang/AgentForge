"""LangGraph service connector normalization helpers."""

from __future__ import annotations

from typing import Any


def _normalize_domain_policy(value: Any) -> str:
    policy = str(value or "").strip().lower()
    if not policy or policy == "none":
        return "none"
    if len(policy) <= 64 and all(ch.isalnum() or ch in "._-" for ch in policy):
        return policy
    return "none"


def _normalize_url(url: object) -> str | None:
    """Normalize connector URL values for stable comparisons and persistence."""
    if url is None:
        return None
    value = str(url).strip()
    if not value:
        return None
    return value.rstrip("/")


def _normalize_langgraph_connector_config(definition: dict) -> None:
    """
    Keep LangGraph connector URL fields in sync.

    Historical payloads can diverge (`base_url` updated, `upstream_url` stale),
    which breaks transparent proxy routing. We normalize to one canonical URL.
    """
    connector_config = definition.get("connector_config")
    if not isinstance(connector_config, dict):
        return

    metadata = definition.get("metadata")
    metadata = dict(metadata) if isinstance(metadata, dict) else {}
    definition["metadata"] = metadata
    service_type = definition.get("service_type")
    service_type_value = (
        service_type.value if hasattr(service_type, "value") else str(service_type or "")
    )
    adapter_type = str(metadata.get("adapter_type") or connector_config.get("adapter_type") or "")
    proxy_mode = str(connector_config.get("proxy_mode") or metadata.get("proxy_mode") or "")
    graph_id = connector_config.get("graph_id")
    assistant_id = connector_config.get("assistant_id")

    is_langgraph = (
        service_type_value == "langgraph"
        or adapter_type == "langgraph"
        or (proxy_mode == "transparent" and bool(graph_id or assistant_id))
    )
    if not is_langgraph:
        return

    metadata["adapter_type"] = "langgraph"
    metadata["domain_policy"] = _normalize_domain_policy(metadata.get("domain_policy"))
    if proxy_mode:
        metadata["proxy_mode"] = proxy_mode

    base_url = _normalize_url(connector_config.get("base_url"))
    upstream_url = _normalize_url(connector_config.get("upstream_url"))

    if base_url and upstream_url and base_url != upstream_url and proxy_mode == "transparent":
        # In transparent proxy mode, UI edits typically target deployment URL (base_url).
        # Treat base_url as the user intent and heal stale upstream_url.
        upstream_url = base_url

    canonical_url = upstream_url or base_url
    if canonical_url:
        connector_config["base_url"] = canonical_url
        connector_config["upstream_url"] = canonical_url

    if graph_id and not assistant_id:
        connector_config["assistant_id"] = graph_id
    elif assistant_id and not graph_id:
        connector_config["graph_id"] = assistant_id


def _is_langgraph_definition(definition: dict) -> bool:
    connector_config = definition.get("connector_config")
    connector_config = connector_config if isinstance(connector_config, dict) else {}
    metadata = definition.get("metadata")
    metadata = metadata if isinstance(metadata, dict) else {}
    service_type = definition.get("service_type")
    service_type_value = (
        service_type.value if hasattr(service_type, "value") else str(service_type or "")
    )
    adapter_type = str(metadata.get("adapter_type") or connector_config.get("adapter_type") or "")
    proxy_mode = str(connector_config.get("proxy_mode") or metadata.get("proxy_mode") or "")
    return (
        service_type_value == "langgraph"
        or adapter_type == "langgraph"
        or (
            proxy_mode == "transparent"
            and bool(connector_config.get("graph_id") or connector_config.get("assistant_id"))
        )
    )
