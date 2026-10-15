"""Exercise production catalog upserts with a file-only SQL boundary fixture."""

import re
from copy import deepcopy
from decimal import Decimal
from unittest.mock import MagicMock, patch

import pytest

from src.services.llm.model_service import ModelService


class PolicyDatabase:
    """Apply emitted parameterized statements, rather than pre-canning results.

    Only the SELECT, INSERT and UPDATE forms used by this service are supported.
    This is a SQL-boundary unit fixture, not PostgreSQL/concurrency acceptance.
    """

    def __init__(self, rows=(), before_update=None):
        self.rows = deepcopy(list(rows))
        self.queries = []
        self.before_update = before_update

    async def fetchrow(self, query, *params):
        sql = " ".join(query.split())
        self.queries.append(sql)
        if sql.startswith("INSERT INTO llm_models"):
            columns, values = re.search(r"\((.*?)\) VALUES \((.*?)\)", sql).groups()
            row = {
                column.strip(): params[int(value.strip()[1:]) - 1]
                if value.strip().startswith("$") else int(value)
                for column, value in zip(columns.split(","), values.split(","), strict=True)
            }
            self.rows.append(row)
            return deepcopy(row)
        where = sql.split(" WHERE ")[1].split(" RETURNING ")[0]
        predicates = re.findall(r"(tenant_id|provider_id|model_id) = \$(\d+)", where)
        assert len(predicates) == 3
        row = next((r for r in self.rows if all(r[k] == params[int(i) - 1] for k, i in predicates)), None)
        if row is None:
            return None
        if sql.startswith("UPDATE "):
            if self.before_update:
                self.before_update(row)
                self.before_update = None
            for assignment in sql.split(" SET ")[1].split(" WHERE ")[0].split(", "):
                column, value = assignment.split(" = ")
                row[column] = params[int(value[1:]) - 1] if value.startswith("$") else row[column] + 1
        else:
            assert sql.startswith("SELECT ")
        return deepcopy(row)


def policy_row(**overrides):
    row = {
        "tenant_id": "tenant-a", "provider_id": "test-provider", "model_id": "custom-model",
        "display_name": "Old catalog label", "context_window": 1000, "max_output_tokens": 100,
        "supports_tools": True, "supports_vision": False,
        "catalog_capabilities": {}, "capability_overrides": {"reasoning": {"default_option": "off"}},
        "capability_revision": 3, "input_price_per_1k": Decimal("0.125"),
        "output_price_per_1k": Decimal("0.375"), "access_level": "admin",
        "is_enabled": True, "sort_order": 1,
    }
    row.update(overrides)
    return row


def catalog_fields():
    return dict(
        tenant_id="tenant-a", provider_id="test-provider", model_id="custom-model",
        display_name="New catalog label", context_window=32000, max_output_tokens=8192,
        supports_vision=True, sort_order=9,
        input_price_per_1k=Decimal("0.001"), output_price_per_1k=Decimal("0.002"),
        access_level="public", catalog_capabilities={"reasoning": {"default_option": "off"}},
    )


@pytest.fixture(autouse=True)
def pricing_cache():
    with patch("src.services.llm.model_service.get_pricing_service", return_value=MagicMock()):
        yield


@pytest.mark.asyncio
@pytest.mark.parametrize("access,enabled", [("admin", True), ("premium", False), ("public", True)])
async def test_catalog_preserves_existing_policy_and_refreshes_metadata(access, enabled):
    existing = policy_row(access_level=access, is_enabled=enabled)
    sibling = policy_row(provider_id="other-provider")
    other_tenant = policy_row(tenant_id="tenant-b")
    db = PolicyDatabase([existing, sibling, other_tenant])
    service = ModelService(db)
    initial_snapshot = service._row_to_dict(existing)["pricing_snapshot"]
    for expected_change in [True, False]:
        status, result, changed = await service.upsert_model_from_catalog(**catalog_fields())
        assert status == "updated"
        assert result["access_level"] == access
        assert result["input_price_per_1k"] == 0.125
        assert result["output_price_per_1k"] == 0.375
        assert result["is_enabled"] is enabled
        assert result["capability_overrides"] == existing["capability_overrides"]
        assert result["display_name"] == "New catalog label"
        assert result["context_window"] == 32000
        assert result["max_output_tokens"] == 8192
        assert result["supports_vision"] is True
        assert result["sort_order"] == 9
        assert result["pricing_snapshot"] == initial_snapshot
        assert result["catalog_capabilities"] == catalog_fields()["catalog_capabilities"]
        assert changed is expected_change
    assert db.rows[1:] == [sibling, other_tenant]
    for sql in db.queries:
        if sql.startswith("UPDATE "):
            assignments = sql.split(" SET ")[1].split(" WHERE ")[0]
            for protected in ["access_level", "input_price_per_1k", "output_price_per_1k", "is_enabled", "capability_overrides"]:
                assert f"{protected} =" not in assignments


@pytest.mark.asyncio
@pytest.mark.parametrize("enable_new", [True, False])
async def test_catalog_initializes_new_model_policy(enable_new):
    db = PolicyDatabase()
    status, result, changed = await ModelService(db).upsert_model_from_catalog(
        **catalog_fields(), enable_new=enable_new,
    )
    assert (status, changed) == ("created", True)
    assert result["access_level"] == "public"
    assert result["input_price_per_1k"] == 0.001
    assert result["output_price_per_1k"] == 0.002
    assert result["is_enabled"] is enable_new
    assert result["capability_overrides"] == {}
    assert len(db.rows) == 1


@pytest.mark.asyncio
async def test_explicit_administrator_policy_update_still_applies():
    db = PolicyDatabase([policy_row()])
    result = await ModelService(db).update_model(
        tenant_id="tenant-a", provider_id="test-provider", model_id="custom-model",
        access_level="premium", input_price_per_1k=Decimal("0.5"), output_price_per_1k=Decimal("1.25"),
    )
    assert result["access_level"] == "premium"
    assert result["input_price_per_1k"] == 0.5
    assert result["output_price_per_1k"] == 1.25


@pytest.mark.asyncio
async def test_catalog_does_not_write_back_policy_read_before_an_admin_edit():
    def admin_edit(row):
        row.update(access_level="premium", input_price_per_1k=Decimal("0.5"), output_price_per_1k=Decimal("1.25"))
    db = PolicyDatabase([policy_row()], before_update=admin_edit)
    _, result, _ = await ModelService(db).upsert_model_from_catalog(**catalog_fields())
    assert result["access_level"] == "premium"
    assert result["input_price_per_1k"] == 0.5
    assert result["output_price_per_1k"] == 1.25


@pytest.mark.asyncio
@pytest.mark.parametrize("price", [None, Decimal("0")])
async def test_catalog_preserves_persisted_null_and_zero_prices(price):
    db = PolicyDatabase([policy_row(input_price_per_1k=price, output_price_per_1k=price)])
    _, result, _ = await ModelService(db).upsert_model_from_catalog(**catalog_fields())
    assert db.rows[0]["input_price_per_1k"] == price
    assert db.rows[0]["output_price_per_1k"] == price
    assert result["input_price_per_1k"] == price
    assert result["output_price_per_1k"] == price
