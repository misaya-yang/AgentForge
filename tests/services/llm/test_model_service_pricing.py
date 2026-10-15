"""
Tests for ModelService pricing synchronization.

Tests that model create/update operations properly sync pricing
to the model_pricing table for usage recording.
"""

import json
from decimal import Decimal
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from src.services.llm.model_service import ModelService


@pytest.fixture
def mock_db():
    """Create a mock database storage."""
    db = MagicMock()
    db.fetchrow = AsyncMock()
    db.fetch = AsyncMock(return_value=[])
    db.execute = AsyncMock()
    return db


@pytest.fixture
def model_service(mock_db):
    """Create a ModelService instance with mock database."""
    return ModelService(database=mock_db)


@pytest.fixture
def sample_model_row():
    """Sample model row returned from database."""
    return {
        "model_id": "gpt-4o",
        "tenant_id": "test-tenant",
        "provider_id": "openai",
        "display_name": "GPT-4o",
        "context_window": 128000,
        "max_output_tokens": 4096,
        "supports_vision": True,
        "supports_tools": True,
        "input_price_per_1k": Decimal("0.0025"),
        "output_price_per_1k": Decimal("0.01"),
        "access_level": "public",
        "is_enabled": True,
        "sort_order": 0,
        "created_at": "2024-01-01T00:00:00Z",
        "updated_at": "2024-01-01T00:00:00Z",
    }


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["create", "update", "startup"])
async def test_tenant_model_does_not_publish_global_price(
    model_service, mock_db, sample_model_row, operation,
):
    mock_db.fetchrow.return_value = sample_model_row
    mock_db.fetch.return_value = [sample_model_row]
    with patch("src.services.llm.model_service.get_pricing_service") as get_pricing:
        pricing = get_pricing.return_value
        pricing.update_pricing = AsyncMock()
        if operation == "create":
            result = await model_service.create_model(
                tenant_id="test-tenant", model_id="gpt-4o", provider_id="openai", display_name="GPT",
            )
        elif operation == "update":
            result = await model_service.update_model(
                tenant_id="test-tenant", model_id="gpt-4o", input_price_per_1k=Decimal("0.0025"),
            )
            query, *params = mock_db.fetchrow.call_args.args
            assert "provider_id =" in query
            assert "openai" in params
        else:
            assert await model_service.sync_pricing_from_llm_models("test-tenant") == 1
            result = model_service._row_to_dict(sample_model_row)
        pricing.update_pricing.assert_not_called()
        pricing.invalidate_cache.assert_called_once()
        receipt = result["pricing_snapshot"]
        assert receipt["tenant_id"] == "test-tenant"
        assert receipt["provider_id"] == "openai"
        assert receipt["input_price_per_1k"] == "0.0025"


@pytest.mark.asyncio
@pytest.mark.parametrize("operation", ["get", "update", "delete"])
async def test_ambiguous_model_requires_provider(model_service, mock_db, sample_model_row, operation):
    mock_db.fetchrow.return_value = {**sample_model_row, "provider_matches": 2}
    method = getattr(model_service, f"{operation}_model")
    with pytest.raises(ValueError, match="model_provider_required"):
        await method(tenant_id="test-tenant", model_id="gpt-4o")
    mock_db.execute.assert_not_called()
    assert mock_db.fetchrow.await_count == 1


def test_snapshot_is_scoped_precise_and_versioned(model_service, sample_model_row):
    row = {**sample_model_row, "input_price_per_1k": Decimal("0.000000000123456789")}
    first = model_service._row_to_dict(row)["pricing_snapshot"]
    other = model_service._row_to_dict({**row, "tenant_id": "other"})["pricing_snapshot"]
    assert first["input_price_per_1k"] == "0.000000000123456789"
    assert first["version"] != other["version"]
    assert model_service._row_to_dict(row)["pricing_snapshot"] == first


def test_row_capability_json_is_decoded(model_service, sample_model_row):
    row = {**sample_model_row, "catalog_capabilities": json.dumps({}), "capability_overrides": "{}"}
    result = model_service._row_to_dict(row)
    assert isinstance(result["catalog_capabilities"], dict)
    assert result["effective_capabilities"]["schema_version"] == 1
