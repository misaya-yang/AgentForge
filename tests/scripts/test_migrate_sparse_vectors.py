from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any

import pytest

from scripts import migrate_sparse_vectors


@pytest.mark.asyncio
async def test_executable_migration_fails_before_qdrant_client_construction(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    constructed = False

    def client_factory(**_kwargs: Any) -> None:
        nonlocal constructed
        constructed = True

    monkeypatch.setattr("qdrant_client.AsyncQdrantClient", client_factory)

    with pytest.raises(
        migrate_sparse_vectors.LegacySparseMigrationRetired,
        match="backfill_bm25_v2.py",
    ):
        await migrate_sparse_vectors.migrate(
            "kb_dataset_1024",
            "http://qdrant.invalid",
            dry_run=False,
        )

    assert constructed is False


@pytest.mark.asyncio
async def test_dry_run_reads_metadata_without_calling_any_mutation(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    calls: list[tuple[str, Any]] = []

    class ReadOnlyClient:
        async def get_collection(self, collection: str) -> SimpleNamespace:
            calls.append(("get_collection", collection))
            return SimpleNamespace(
                points_count=17,
                config=SimpleNamespace(
                    params=SimpleNamespace(
                        vectors=SimpleNamespace(size=1024, distance="Cosine")
                    )
                ),
            )

        async def close(self) -> None:
            calls.append(("close", None))

        def __getattr__(self, name: str) -> Any:
            pytest.fail(f"dry run attempted unexpected Qdrant method: {name}")

    monkeypatch.setattr(
        "qdrant_client.AsyncQdrantClient",
        lambda **_kwargs: ReadOnlyClient(),
    )

    plan = await migrate_sparse_vectors.migrate(
        "kb_dataset_1024",
        "http://qdrant.invalid",
        dry_run=True,
    )

    assert calls == [("get_collection", "kb_dataset_1024"), ("close", None)]
    assert plan == migrate_sparse_vectors.ReadOnlyMigrationPlan(
        collection="kb_dataset_1024",
        points_count=17,
        dense_vector_size=1024,
        distance="Cosine",
    )


@pytest.mark.parametrize("flag", ["--apply", "--execute"])
def test_cli_has_no_explicit_execution_escape_hatch(
    flag: str,
    capsys: pytest.CaptureFixture[str],
) -> None:
    with pytest.raises(SystemExit) as exc_info:
        migrate_sparse_vectors.main([flag])

    assert exc_info.value.code == 2
    assert "unrecognized arguments" in capsys.readouterr().err


def test_cli_default_fails_closed_and_directs_operator_to_backfill(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    invoked = False

    async def unexpected_migrate(*_args: Any, **_kwargs: Any) -> None:
        nonlocal invoked
        invoked = True

    monkeypatch.setattr(migrate_sparse_vectors, "migrate", unexpected_migrate)

    with pytest.raises(SystemExit) as exc_info:
        migrate_sparse_vectors.main(["--collection", "kb_dataset_1024"])

    assert exc_info.value.code == 2
    assert invoked is False
    assert "scripts/backfill_bm25_v2.py" in capsys.readouterr().err


def test_cli_dry_run_prints_read_only_retirement_plan(
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    observed: dict[str, Any] = {}

    async def fake_migrate(
        collection: str,
        qdrant_url: str,
        batch_size: int,
        dry_run: bool,
    ) -> migrate_sparse_vectors.ReadOnlyMigrationPlan:
        observed.update(
            collection=collection,
            qdrant_url=qdrant_url,
            batch_size=batch_size,
            dry_run=dry_run,
        )
        return migrate_sparse_vectors.ReadOnlyMigrationPlan(
            collection=collection,
            points_count=3,
            dense_vector_size=768,
            distance="Cosine",
        )

    monkeypatch.setattr(migrate_sparse_vectors, "migrate", fake_migrate)

    assert (
        migrate_sparse_vectors.main(
            [
                "--collection",
                "kb_dataset_768",
                "--qdrant-url",
                "http://qdrant.invalid",
                "--dry-run",
            ]
        )
        == 0
    )
    assert observed == {
        "collection": "kb_dataset_768",
        "qdrant_url": "http://qdrant.invalid",
        "batch_size": 100,
        "dry_run": True,
    }
    assert json.loads(capsys.readouterr().out) == {
        "collection": "kb_dataset_768",
        "dense_vector_size": 768,
        "distance": "Cosine",
        "points_count": 3,
        "replacement": "scripts/backfill_bm25_v2.py",
        "status": "retired",
    }
