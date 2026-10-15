from __future__ import annotations

from src.api.v1._langgraph_connector_config import (
    _normalize_langgraph_connector_config as extracted_normalize_langgraph_connector_config,
)
from src.api.v1.services import _normalize_langgraph_connector_config
from src.proxy.config_loader import ProxyConfigLoader
from src.services.llm.provider_service import ProviderService


def test_services_module_reexports_langgraph_connector_normalizer():
    assert _normalize_langgraph_connector_config is extracted_normalize_langgraph_connector_config


def test_normalize_langgraph_connector_config_syncs_urls_and_ids():
    definition = {
        "service_type": "langgraph",
        "metadata": {"adapter_type": "langgraph"},
        "connector_config": {
            "proxy_mode": "transparent",
            "base_url": "http://localhost:2025/",
            "upstream_url": "http://localhost:2024",
            "graph_id": "customer-agent",
        },
    }

    _normalize_langgraph_connector_config(definition)
    connector = definition["connector_config"]

    assert connector["base_url"] == "http://localhost:2025"
    assert connector["upstream_url"] == "http://localhost:2025"
    assert connector["assistant_id"] == "customer-agent"


def test_normalize_langgraph_connector_config_backfills_graph_id():
    definition = {
        "service_type": "langgraph",
        "metadata": {"adapter_type": "langgraph"},
        "connector_config": {
            "base_url": "http://localhost:2025",
            "assistant_id": "Agent",
        },
    }

    _normalize_langgraph_connector_config(definition)
    connector = definition["connector_config"]

    assert connector["graph_id"] == "Agent"
    assert connector["base_url"] == "http://localhost:2025"
    assert connector["upstream_url"] == "http://localhost:2025"


def test_proxy_config_loader_heals_transparent_url_mismatch():
    loader = ProxyConfigLoader(database=None)
    row = {
        "service_id": "customer-agent",
        "name": "Customer Agent",
        "connector_config": {
            "proxy_mode": "transparent",
            "base_url": "http://localhost:2025/",
            "upstream_url": "http://localhost:2024",
        },
        "service_config": {},
        "metadata": {},
        "status": "active",
    }

    config = loader._parse_service_row(row)

    assert config.upstream_url == "http://localhost:2025"


def test_provider_service_maps_dashscope_runtime_defaults():
    provider = {
        "provider_id": "dashscope-main",
        "api_type": "openai-compatible",
        "base_url": "",
    }

    assert ProviderService.to_runtime_provider(provider) == "dashscope"
    assert (
        ProviderService.normalize_runtime_base_url(provider)
        == "https://dashscope.aliyuncs.com/compatible-mode/v1"
    )


def test_provider_service_normalizes_dashscope_compatible_mode_url():
    provider = {
        "provider_id": "aliyun-prod",
        "api_type": "dashscope",
        "base_url": "https://dashscope.aliyuncs.com/compatible-mode/",
    }

    assert (
        ProviderService.normalize_runtime_base_url(provider)
        == "https://dashscope.aliyuncs.com/compatible-mode/v1"
    )


def test_provider_service_maps_google_studio_to_gemini():
    provider = {
        "provider_id": "gemini",
        "api_type": "google-ai-studio",
        "base_url": "https://generativelanguage.googleapis.com",
    }

    assert ProviderService.to_runtime_provider(provider) == "gemini"


def test_provider_service_decodes_jsonb_metadata_strings():
    service = ProviderService(database=None)

    row = {
        "provider_id": "google-vertex",
        "tenant_id": "default",
        "display_name": "Google Vertex AI",
        "api_type": "google-vertex",
        "base_url": "https://aiplatform.googleapis.com",
        "api_key_encrypted": "encrypted",
        "metadata": '{"project":"hjz-csgmn-260422","location":"us-central1"}',
        "is_enabled": True,
        "created_at": None,
        "updated_at": None,
    }

    result = service._row_to_dict(row)

    assert result["has_api_key"] is True
    assert result["metadata"] == {
        "project": "hjz-csgmn-260422",
        "location": "us-central1",
    }
