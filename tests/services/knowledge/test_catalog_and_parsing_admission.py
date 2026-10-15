"""Dataset admission and complete catalog traversal exercise the production services."""

from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes.knowledge import dedupe_segments
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge.dataset_service import DatasetService
from knowledge_service.services.knowledge.ingestion_service import IngestionService
from knowledge_service.services.knowledge.parsing.config_validation import (
    parsing_config_report,
    resolve_parsing_config,
)

USER = UserContext(user_id="u", tenant_id="t")


@pytest.mark.parametrize(
    "parsing",
    [
        [],
        {"enabled": "false"},
        {"enabled": True, "cascade": {"stages": []}},
        {"enabled": True, "cascade": {"stages": [{"backend": "typo"}]}},
        {
            "enabled": True,
            "cascade": {"stages": [{"backend": "text_layer", "min_confidence": float("nan")}]},
        },
        {"enabled": True, "cascade": {"stages": [{"backend": "text_layer"}], "parallelism": True}},
        {
            "enabled": True,
            "cascade": {
                "stages": [{"backend": "text_layer"}],
                "backend_options": {"text_layer": {"extract": "untrusted"}},
            },
        },
    ],
)
@pytest.mark.parametrize("action", ["create", "update"])
async def test_invalid_parser_configuration_is_rejected_before_database_write(parsing, action):
    database = AsyncMock()
    service = DatasetService(SimpleNamespace(), database)
    payload = {"index_config": {"parsing": parsing}}
    with pytest.raises(ValidationFailedError, match="index_config.parsing"):
        if action == "create":
            await service.create_dataset(USER, payload)
        else:
            await service.update_dataset(USER, "d", payload)
    assert not database.mock_calls


def test_parser_admission_and_ingestion_share_effective_boundaries():
    config = {"parsing": {"enabled": True, "cascade": {"stages": [{"backend": "text_layer"}]}}}
    expected = resolve_parsing_config(config)[1]
    assert IngestionService._parsing_cascade_config(config)[1] == expected
    assert expected["backend_options"]["text_layer"]["preserve_boundaries"] is True
    assert config["parsing"]["cascade"].get("backend_options") is None
    assert resolve_parsing_config({}) is None
    report = parsing_config_report({"parsing": {"enabled": True}})
    assert report["warnings"]
    assert {b["name"] for b in report["backends"] if b["available"]} == {"text_layer"}


class CatalogStore:
    def __init__(self, *, hidden_first=False):
        self.rows = [
            {
                "dataset_id": f"d-{i:03}",
                "tenant_id": "t",
                "created_by": "other",
                "visibility": "private" if hidden_first and i > 1 else "tenant",
                "created_at": datetime(2026, 1, 1, tzinfo=timezone.utc),
            }
            for i in range(201, 0, -1)
        ]

    async def list_datasets(self, *, tenant_id, limit, before_dataset_id=None, **_kwargs):
        assert tenant_id == "t"
        return [
            dict(r)
            for r in self.rows
            if before_dataset_id is None or r["dataset_id"] < before_dataset_id
        ][:limit]

    async def get_dataset_permissions_batch(self, _ids, **_kwargs):
        return {}

    async def get_dataset_permission(self, *_args):
        return None

    async def get_datasets_statistics_batch(self, _ids):
        return {}


@pytest.mark.parametrize("hidden_first", [False, True])
async def test_201_dataset_catalog_advances_even_when_acl_hides_a_whole_page(hidden_first):
    service = DatasetService(SimpleNamespace(), CatalogStore(hidden_first=hidden_first))
    page = await service.list_datasets_page(USER)
    assert bool(page["items"]) is not hidden_first
    assert page["next_cursor"]
    tail = await service.list_datasets_page(USER, cursor=page["next_cursor"])
    assert [r["dataset_id"] for r in tail["items"]] == ["d-001"]
    assert tail["next_cursor"] is None
    assert len(page["items"] + tail["items"]) == (1 if hidden_first else 201)


@pytest.mark.parametrize("cursor", ["not-a-cursor", "x" * 1025])
async def test_bad_catalog_cursor_fails_before_lookup(cursor):
    store = AsyncMock()
    with pytest.raises(ValidationFailedError, match="cursor"):
        await DatasetService(SimpleNamespace(), store).list_datasets_page(USER, cursor=cursor)
    store.list_datasets.assert_not_awaited()


@pytest.mark.parametrize("dry_run", [True, False])
async def test_10001_segments_never_become_a_partial_dedupe_success(dry_run):
    svc = SimpleNamespace(
        require_dataset_access=AsyncMock(
            return_value={
                "dataset_id": "d",
                "tenant_id": "t",
                "content_revision": 1,
                "index_config": {},
            }
        ),
        db=SimpleNamespace(list_segments=AsyncMock(return_value=[{}] * 10001)),
        delete_segment=AsyncMock(),
    )
    with pytest.raises(HTTPException) as error:
        await dedupe_segments(
            "d", SimpleNamespace(), dry_run=dry_run, svc=svc, user=USER, settings=SimpleNamespace()
        )
    assert error.value.status_code == 422
    assert error.value.detail["incomplete"] is True
    assert error.value.detail["observed_at_least"] == 10001
    svc.delete_segment.assert_not_awaited()
