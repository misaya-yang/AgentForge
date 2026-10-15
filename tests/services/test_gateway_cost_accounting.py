from __future__ import annotations

from decimal import Decimal

import pytest

from src.services.billing.model_pricing import ModelPricingService
from src.services.billing.pricing_catalog import microcents_to_usd, usd_to_microcents
from src.services.metrics.usage_parser import extract_model, extract_provider
from src.services.metrics.usage_recorder import UsageRecord, UsageRecorder


@pytest.mark.asyncio
async def test_pricing_service_marks_unknown_model_pricing_status():
    service = ModelPricingService(database=None)

    result = await service.calculate_cost(
        model="new-provider/not-yet-priced",
        input_tokens=1000,
        output_tokens=500,
    )

    assert result["pricing_status"] == "unknown"
    assert result["provider"] == "unknown"


@pytest.mark.asyncio
async def test_usage_record_metadata_contains_pricing_and_token_source():
    recorder = UsageRecorder(database=None)
    record = UsageRecord(
        tenant_id="tenant-a",
        user_id="user-a",
        model="new-provider/not-yet-priced",
        input_tokens=0,
        output_tokens=0,
        request_id="req-unknown-pricing",
        service_id="svc-a",
        request_type="proxy_run_wait",
        status="error",
        metadata={"source": "transparent_proxy_non_stream"},
    )

    await recorder.record(record)

    assert record.metadata["provider"] == "unknown"
    assert record.metadata["model"] == "new-provider/not-yet-priced"
    assert record.metadata["pricing_status"] == "unknown"
    assert record.metadata["token_source"] == "zero_on_failure"


def test_effective_gateway_model_payload_wins_for_cost_attribution():
    payload = {
        "config": {
            "configurable": {
                "gateway_model": {
                    "provider": "dashscope",
                    "model": "qwen-max",
                }
            }
        },
        "model": "service-default-should-not-win",
    }

    assert extract_model(payload) == "qwen-max"
    assert extract_provider(payload) == "dashscope"


def test_microcent_totals_convert_without_rounding_drift():
    input_cost = usd_to_microcents(Decimal("0.00015"))
    output_cost = usd_to_microcents(Decimal("0.0006"))

    assert input_cost == 150
    assert output_cost == 600
    assert microcents_to_usd(input_cost + output_cost) == 0.00075
