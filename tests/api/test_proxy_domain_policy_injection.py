from __future__ import annotations

import json

from src.proxy.langgraph_run_body import inject_domain_policy_metadata_bytes
from src.proxy.config_loader import ProxyServiceConfig


def _langgraph_proxy_config(domain_policy: str = "agent") -> ProxyServiceConfig:
    return ProxyServiceConfig(
        service_id="svc_langgraph_agent",
        service_name="langgraph-agent",
        upstream_url="http://langgraph:8123",
        assistant_id="assistant_agent",
        metadata={"adapter_type": "langgraph", "domain_policy": domain_policy},
    )


def test_injects_domain_policy_for_langgraph_run_payload() -> None:
    config = _langgraph_proxy_config(domain_policy="agent")
    body = json.dumps({"input": {"messages": [{"role": "user", "content": "hello"}]}}).encode(
        "utf-8"
    )

    updated_body = inject_domain_policy_metadata_bytes(
        body=body,
        method="POST",
        path="threads/t1/runs/stream",
        service_config=config,
    )

    payload = json.loads((updated_body or b"{}").decode("utf-8"))
    assert payload["metadata"]["gateway"]["domain_policy"] == "agent"


def test_does_not_inject_for_non_run_path() -> None:
    config = _langgraph_proxy_config(domain_policy="agent")
    body = json.dumps({"input": {"messages": [{"role": "user", "content": "hello"}]}}).encode(
        "utf-8"
    )

    updated_body = inject_domain_policy_metadata_bytes(
        body=body,
        method="POST",
        path="assistants/search",
        service_config=config,
    )

    assert updated_body == body


def test_does_not_inject_when_service_policy_is_none() -> None:
    config = _langgraph_proxy_config(domain_policy="none")
    body = json.dumps({"input": {"messages": [{"role": "user", "content": "hello"}]}}).encode(
        "utf-8"
    )

    updated_body = inject_domain_policy_metadata_bytes(
        body=body,
        method="POST",
        path="runs/wait",
        service_config=config,
    )

    assert updated_body == body


def test_preserves_existing_gateway_domain_policy() -> None:
    config = _langgraph_proxy_config(domain_policy="agent")
    body = json.dumps(
        {
            "input": {"messages": [{"role": "user", "content": "hello"}]},
            "metadata": {"gateway": {"domain_policy": "custom"}},
        }
    ).encode("utf-8")

    updated_body = inject_domain_policy_metadata_bytes(
        body=body,
        method="POST",
        path="runs/wait",
        service_config=config,
    )

    payload = json.loads((updated_body or b"{}").decode("utf-8"))
    assert payload["metadata"]["gateway"]["domain_policy"] == "custom"


def test_injects_for_root_runs_path_without_leading_slash() -> None:
    config = _langgraph_proxy_config(domain_policy="agent")
    body = json.dumps({"input": {"messages": [{"role": "user", "content": "hello"}]}}).encode(
        "utf-8"
    )

    updated_body = inject_domain_policy_metadata_bytes(
        body=body,
        method="POST",
        path="runs/wait",
        service_config=config,
    )

    payload = json.loads((updated_body or b"{}").decode("utf-8"))
    assert payload["metadata"]["gateway"]["domain_policy"] == "agent"
