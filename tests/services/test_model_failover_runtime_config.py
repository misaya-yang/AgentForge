from __future__ import annotations

import json
from typing import Any

import pytest

from src.services.llm.model_failover import (
    build_runtime_model_override_config,
    has_secret_field,
)


class FakeProviderService:
    def __init__(self, providers: dict[str, dict[str, Any]]):
        self.providers = providers

    async def get_runtime_provider_config(self, tenant_id: str, provider_id: str) -> dict[str, Any]:
        assert tenant_id == "tenant-a"
        provider = self.providers.get(provider_id)
        if not provider:
            raise ValueError(provider_id)
        return provider


class FakeModelService:
    def __init__(self, models: dict[tuple[str, str], dict[str, Any]]):
        self.models = models

    async def get_provider_model(
        self,
        tenant_id: str,
        provider_id: str,
        model_id: str,
    ) -> dict[str, Any] | None:
        assert tenant_id == "tenant-a"
        return self.models.get((provider_id, model_id))


def _provider(
    provider_id: str,
    *,
    runtime_provider: str,
    base_url: str | None = None,
    api_key: str | None = "runtime-secret",
    enabled: bool = True,
    metadata: dict[str, Any] | None = None,
    allow_environment_credentials: bool = False,
) -> dict[str, Any]:
    return {
        "provider_id": provider_id,
        "is_enabled": enabled,
        "runtime_provider": runtime_provider,
        "runtime_base_url": base_url,
        "api_key": api_key,
        "metadata": metadata or {},
        "allow_environment_credentials": allow_environment_credentials,
    }


def _model(provider_id: str, model_id: str, *, enabled: bool = True) -> dict[str, Any]:
    return {"provider_id": provider_id, "model_id": model_id, "is_enabled": enabled}


@pytest.mark.asyncio
async def test_runtime_candidate_cache_key_fields_are_present():
    runtime = await build_runtime_model_override_config(
        tenant_id="tenant-a",
        model_override={
            "enabled": True,
            "provider_id": "dashscope-cn",
            "model_id": "qwen-max",
            "temperature": 0.1,
            "cache_epoch": 8,
            "failover": {
                "enabled": True,
                "max_attempts": 2,
                "candidates": [
                    {"provider_id": "google-ai-studio", "model_id": "gemini-3.5-flash"}
                ],
            },
        },
        provider_service=FakeProviderService(
            {
                "dashscope-cn": _provider(
                    "dashscope-cn",
                    runtime_provider="dashscope",
                    base_url="https://dashscope.aliyuncs.com/compatible-mode/v1",
                    api_key="cn-secret",
                ),
                "google-ai-studio": _provider(
                    "google-ai-studio",
                    runtime_provider="gemini",
                    base_url="https://generativelanguage.googleapis.com",
                    api_key="gemini-secret",
                ),
            }
        ),
        model_service=FakeModelService(
            {
                ("dashscope-cn", "qwen-max"): _model("dashscope-cn", "qwen-max"),
                ("google-ai-studio", "gemini-3.5-flash"): _model(
                    "google-ai-studio",
                    "gemini-3.5-flash",
                ),
            }
        ),
    )

    candidates = runtime["failover"]["candidates"]
    assert [(c["provider_id"], c["model_id"]) for c in candidates] == [
        ("dashscope-cn", "qwen-max"),
        ("google-ai-studio", "gemini-3.5-flash"),
    ]
    for candidate in candidates:
        assert candidate["tenant_id"] == "tenant-a"
        assert candidate["provider_id"]
        assert "base_url" in candidate
        assert candidate["api_key_fingerprint"]
        assert candidate["cache_epoch"] == "8"
    assert "cn-secret" in candidates[0]["_api_key"]
    assert "cn-secret" not in json.dumps(
        {
            "provider_id": candidates[0]["provider_id"],
            "api_key_fingerprint": candidates[0]["api_key_fingerprint"],
        }
    )


@pytest.mark.asyncio
async def test_invalid_fallback_candidate_is_skipped_with_safe_warning():
    runtime = await build_runtime_model_override_config(
        tenant_id="tenant-a",
        model_override={
            "enabled": True,
            "provider_id": "dashscope-cn",
            "model_id": "qwen-max",
            "cache_epoch": 1,
            "failover": {
                "enabled": True,
                "candidates": [
                    {"provider_id": "google-ai-studio", "model_id": "missing-model"}
                ],
            },
        },
        provider_service=FakeProviderService(
            {
                "dashscope-cn": _provider("dashscope-cn", runtime_provider="dashscope"),
                "google-ai-studio": _provider(
                    "google-ai-studio",
                    runtime_provider="gemini",
                ),
            }
        ),
        model_service=FakeModelService(
            {("dashscope-cn", "qwen-max"): _model("dashscope-cn", "qwen-max")}
        ),
    )

    failover = runtime["failover"]
    assert len(failover["candidates"]) == 1
    assert failover["warnings"] == [
        {
            "provider_id": "google-ai-studio",
            "model_id": "missing-model",
            "code": "MODEL_OVERRIDE_FAILOVER_MODEL_NOT_FOUND",
        }
    ]
    assert "api_key" not in json.dumps(failover["warnings"])


@pytest.mark.asyncio
async def test_runtime_failover_attempts_reach_configured_fallback():
    runtime = await build_runtime_model_override_config(
        tenant_id="tenant-a",
        model_override={
            "enabled": True,
            "provider_id": "dashscope-cn",
            "model_id": "qwen-max",
            "cache_epoch": 1,
            "failover": {
                "enabled": True,
                "max_attempts": 1,
                "candidates": [
                    {"provider_id": "google-ai-studio", "model_id": "gemini-3.5-flash"}
                ],
            },
        },
        provider_service=FakeProviderService(
            {
                "dashscope-cn": _provider("dashscope-cn", runtime_provider="dashscope"),
                "google-ai-studio": _provider(
                    "google-ai-studio",
                    runtime_provider="gemini",
                ),
            }
        ),
        model_service=FakeModelService(
            {
                ("dashscope-cn", "qwen-max"): _model("dashscope-cn", "qwen-max"),
                ("google-ai-studio", "gemini-3.5-flash"): _model(
                    "google-ai-studio",
                    "gemini-3.5-flash",
                ),
            }
        ),
    )

    assert runtime["failover"]["max_attempts"] == 2


def test_nested_secret_detection_catches_failover_candidate_keys():
    assert has_secret_field(
        {
            "enabled": True,
            "failover": {
                "candidates": [
                    {"provider_id": "p", "model_id": "m", "credentials": {"api_key": "x"}}
                ]
            },
        }
    )
    assert has_secret_field({"metadata": {"service_account_json": "{}"}})


@pytest.mark.asyncio
async def test_vertex_runtime_metadata_is_forwarded_without_browser_secret():
    runtime = await build_runtime_model_override_config(
        tenant_id="tenant-a",
        model_override={
            "enabled": True,
            "provider_id": "google-vertex",
            "model_id": "gemini-3.5-flash",
            "cache_epoch": 3,
        },
        provider_service=FakeProviderService(
            {
                "google-vertex": _provider(
                    "google-vertex",
                    runtime_provider="vertex",
                    base_url="https://aiplatform.googleapis.com",
                    api_key=None,
                    allow_environment_credentials=True,
                    metadata={
                        "project": "hjz-csgmn-260422",
                        "location": "us-central1",
                    },
                ),
            }
        ),
        model_service=FakeModelService(
            {
                ("google-vertex", "gemini-3.5-flash"): _model(
                    "google-vertex",
                    "gemini-3.5-flash",
                ),
            }
        ),
    )

    assert runtime["provider"] == "vertex"
    assert runtime["project"] == "hjz-csgmn-260422"
    assert runtime["location"] == "us-central1"
    assert "_api_key" not in runtime
    assert runtime["api_key_fingerprint"] is None
