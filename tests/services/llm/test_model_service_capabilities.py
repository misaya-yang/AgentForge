"""
Capability-profile tests for ModelService (ADR-005).

Covers create-time validation against the builtin catalog, the atomic
capability_revision guard on update, and the value-conditional revision
bump that drives model-config invalidations.
"""

import json
from decimal import Decimal
from unittest.mock import AsyncMock, MagicMock, patch

import pytest
from ai_gateway_core.models import ModelCapabilityError

from src.services.llm.model_service import ModelCapabilityRevisionConflict, ModelService


def _row(**overrides):
    row = {
        "model_id": "custom-model",
        "tenant_id": "tenant-a",
        "provider_id": "test-provider",
        "display_name": "Custom",
        "context_window": 128000,
        "max_output_tokens": 4096,
        "supports_vision": False,
        "supports_tools": True,
        "input_price_per_1k": Decimal("0"),
        "output_price_per_1k": Decimal("0"),
        "access_level": "public",
        "is_enabled": True,
        "sort_order": 0,
        "catalog_capabilities": json.dumps({}),
        "capability_overrides": json.dumps({}),
        "capability_revision": 3,
        "created_at": "2026-05-25T00:00:00Z",
        "updated_at": "2026-05-25T00:00:00Z",
    }
    row.update(overrides)
    return row


@pytest.fixture
def db():
    database = MagicMock()
    database.fetchrow = AsyncMock()
    database.fetch = AsyncMock(return_value=[])
    database.execute = AsyncMock()
    return database


@pytest.fixture
def service(db):
    return ModelService(database=db)


def _no_pricing():
    return patch.object(ModelService, "_sync_single_model_pricing", AsyncMock())


@pytest.mark.asyncio
async def test_create_model_validates_overrides_against_builtin_catalog(service, db):
    # No explicit catalog layer: validation must fall back to the same
    # builtin source the read path synthesizes, so an override that is
    # valid for the model's builtin profile is accepted.
    db.fetchrow.return_value = _row(model_id="qwen3.7-plus", provider_id="dashscope")

    with _no_pricing():
        result = await service.create_model(
            tenant_id="tenant-a",
            model_id="qwen3.7-plus",
            provider_id="dashscope",
            display_name="Qwen",
            capability_overrides={"reasoning": {"default_option": "medium"}},
        )

    assert result["model_id"] == "qwen3.7-plus"
    db.fetchrow.assert_called_once()


@pytest.mark.asyncio
async def test_create_model_rejects_overrides_conflicting_with_builtin_catalog(service, db):
    with pytest.raises(ModelCapabilityError):
        await service.create_model(
            tenant_id="tenant-a",
            model_id="qwen3.7-plus",
            provider_id="dashscope",
            display_name="Qwen",
            capability_overrides={"reasoning": {"default_option": "does-not-exist"}},
        )
    # Fail BEFORE any database write.
    db.fetchrow.assert_not_called()


@pytest.mark.asyncio
async def test_create_model_without_builtin_entry_keeps_safe_validation(service, db):
    # The builtin fallback must not become a blanket bypass: a model with
    # no catalog entry still validates against the safe profile.
    with pytest.raises(ModelCapabilityError):
        await service.create_model(
            tenant_id="tenant-a",
            model_id="custom-model",
            provider_id="test-provider",
            display_name="Custom",
            capability_overrides={"reasoning": {"default_option": "medium"}},
        )
    db.fetchrow.assert_not_called()


@pytest.mark.asyncio
async def test_update_model_revision_guard_is_atomic_in_where_clause(service, db):
    db.fetchrow.side_effect = [_row(), _row(capability_revision=4)]

    with _no_pricing():
        result = await service.update_model(
            tenant_id="tenant-a",
            model_id="custom-model",
            provider_id="test-provider",
            supports_tools=True,
            capability_overrides={"reasoning": {"default_option": "off"}},
            expected_capability_revision=3,
        )

    assert result is not None
    update_sql = db.fetchrow.call_args_list[-1].args[0].split("RETURNING")[0]
    # Value changed -> bump; guard -> atomic WHERE, revision param last.
    assert "capability_revision = capability_revision + 1" in update_sql
    assert "AND capability_revision = $" in update_sql
    assert db.fetchrow.call_args_list[-1].args[-1] == 3


@pytest.mark.asyncio
async def test_update_model_stale_revision_conflicts_before_write(service, db):
    db.fetchrow.side_effect = [_row(capability_revision=4)]

    with pytest.raises(ModelCapabilityRevisionConflict):
        await service.update_model(
            tenant_id="tenant-a",
            model_id="custom-model",
            supports_tools=True,
            expected_capability_revision=3,
        )

    # Only the pre-check read ran; the UPDATE was never issued.
    assert db.fetchrow.call_count == 1


@pytest.mark.asyncio
async def test_update_model_reports_conflict_when_guarded_update_misses(service, db):
    # Pre-check passes, a concurrent writer bumps the revision before the
    # UPDATE (guard matches nothing), and the existence re-check finds the
    # row -> conflict, not a silent None.
    db.fetchrow.side_effect = [_row(), None, _row(capability_revision=4)]

    with pytest.raises(ModelCapabilityRevisionConflict):
        await service.update_model(
            tenant_id="tenant-a",
            model_id="custom-model",
            supports_tools=True,
            expected_capability_revision=3,
        )

    assert db.fetchrow.call_count == 3


@pytest.mark.asyncio
async def test_update_model_guarded_miss_on_deleted_row_returns_none(service, db):
    # Same race, but the row was deleted concurrently -> 404 semantics.
    db.fetchrow.side_effect = [_row(), None, None]

    result = await service.update_model(
        tenant_id="tenant-a",
        model_id="custom-model",
        supports_tools=True,
        expected_capability_revision=3,
    )

    assert result is None


@pytest.mark.asyncio
async def test_update_model_identical_capability_write_does_not_bump_revision(service, db):
    db.fetchrow.side_effect = [_row(), _row()]

    with _no_pricing():
        await service.update_model(
            tenant_id="tenant-a",
            model_id="custom-model",
            supports_tools=True,
            supports_vision=False,
            capability_overrides={},
            expected_capability_revision=3,
        )

    update_sql = db.fetchrow.call_args_list[1].args[0]
    assert "capability_overrides =" in update_sql
    assert "capability_revision = capability_revision + 1" not in update_sql


@pytest.mark.asyncio
async def test_update_model_supports_flag_change_bumps_revision(service, db):
    db.fetchrow.side_effect = [_row(), _row(capability_revision=4)]

    with _no_pricing():
        await service.update_model(
            tenant_id="tenant-a",
            model_id="custom-model",
            supports_tools=False,
            expected_capability_revision=3,
        )

    update_sql = db.fetchrow.call_args_list[1].args[0]
    assert "capability_revision = capability_revision + 1" in update_sql


@pytest.mark.asyncio
async def test_upsert_reports_capability_change_only_on_revision_delta(service, db):
    # No-op catalog resend: values unchanged -> no bump -> no invalidation.
    db.fetchrow.side_effect = [_row(), _row(), _row()]

    with _no_pricing():
        status, result, capability_changed = await service.upsert_model_from_catalog(
            tenant_id="tenant-a",
            provider_id="test-provider",
            model_id="custom-model",
            display_name="Custom",
            supports_tools=True,
        )

    assert status == "updated"
    assert capability_changed is False


@pytest.mark.asyncio
async def test_upsert_reports_capability_change_on_revision_bump(service, db):
    new_catalog = {"reasoning": {"default_option": "off"}}
    db.fetchrow.side_effect = [_row(), _row(), _row(capability_revision=4)]

    with _no_pricing():
        status, result, capability_changed = await service.upsert_model_from_catalog(
            tenant_id="tenant-a",
            provider_id="test-provider",
            model_id="custom-model",
            display_name="Custom",
            supports_tools=True,
            catalog_capabilities=new_catalog,
        )

    assert status == "updated"
    assert capability_changed is True


@pytest.mark.asyncio
async def test_upsert_created_model_reports_capability_change(service, db):
    db.fetchrow.side_effect = [None, _row(capability_revision=1)]

    with _no_pricing():
        status, result, capability_changed = await service.upsert_model_from_catalog(
            tenant_id="tenant-a",
            provider_id="test-provider",
            model_id="custom-model",
            display_name="Custom",
        )

    assert status == "created"
    assert capability_changed is True
