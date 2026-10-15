from contextlib import asynccontextmanager
from decimal import Decimal

import pytest
from ai_gateway_core.billing import build_pricing_snapshot, validate_pricing_snapshot
from ai_gateway_core.metrics.usage_recorder import UsageRecord, UsageRecorder


class Prices:
    enabled = True

    def __init__(self):
        self._pool = self
        self.rows = {
            ("a", "p1"): Decimal("0.1"), ("a", "p2"): Decimal("0.2"),
            ("b", "p1"): Decimal("0.3"),
        }

    @asynccontextmanager
    async def acquire(self):
        yield self

    async def fetch(self, query, tenant, model, provider):
        assert "FROM llm_models" in query
        assert model == "same-name"
        return [
            {"provider_id": pid, "input_price_per_1k": price, "output_price_per_1k": price}
            for (tid, pid), price in self.rows.items()
            if tid == tenant and (provider is None or pid == provider)
        ]


@pytest.mark.asyncio
async def test_same_model_price_isolated_by_tenant_and_provider():
    recorder = UsageRecorder(Prices())
    for tenant, provider, expected in [("a", "p1", 100000), ("a", "p2", 200000), ("b", "p1", 300000)]:
        record = UsageRecord(tenant, "user", "same-name", provider=provider, input_tokens=1000)
        await recorder.record(record)
        assert record.input_cost_cents == expected
        assert record.metadata["pricing_snapshot"]["tenant_id"] == tenant


@pytest.mark.asyncio
async def test_running_request_keeps_frozen_price_after_admin_update():
    db = Prices()
    recorder = UsageRecorder(db)
    first = UsageRecord("a", "u", "same-name", provider="p1", input_tokens=1000, request_id="request")
    await recorder.record(first)
    db.rows[("a", "p1")] = Decimal("9")
    final = UsageRecord("a", "u", "same-name", provider="p1", input_tokens=2000, request_id="request")
    await recorder.record(final)
    assert final.input_cost_cents == 200000
    assert final.metadata["pricing_snapshot"] == first.metadata["pricing_snapshot"]


def test_snapshot_rejects_tenant_rebinding_and_tampering():
    receipt = build_pricing_snapshot(tenant_id="a", provider_id="p", model_id="m",
                                     input_price_per_1k="0.01", output_price_per_1k="0.02")
    with pytest.raises(ValueError, match="identity_mismatch"):
        validate_pricing_snapshot(receipt, tenant_id="b", provider_id="p", model_id="m")
    with pytest.raises(ValueError, match="version_mismatch"):
        validate_pricing_snapshot({**receipt, "input_price_per_1k": "9"},
                                  tenant_id="a", provider_id="p", model_id="m")
