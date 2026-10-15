"""PRD T3 unit tier (no PostgreSQL): version-metadata helpers + the blue-green
planning surface with in-memory fakes.

Covers: shadow collection naming (dims still encoded in the name), embedding
identity comparison, mixed-model query rejection (T3 item 1), and
EmbeddingMigrationService.start_migration validation + zero-orphan cleanup.
The persistence behavior lives in tests/database/
test_kb_embedding_versioning_migration.py (tier-b, live PostgreSQL).
"""

from __future__ import annotations

import asyncio
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import Any

import pytest
from knowledge_service.persistence.embedding_version_store import (
    BindingConflictError,
    MigrationStateError,
)
from knowledge_service.services.knowledge.embedding_migration import (
    EmbeddingMigrationError,
    EmbeddingMigrationService,
    MixedModelEmbeddingError,
    assert_query_embedding_identity,
    embedding_identity,
    identities_match,
    make_shadow_collection_name,
)


def _binding(**overrides: Any) -> dict[str, Any]:
    base = {
        "binding_id": "00000000-0000-0000-0000-000000000001",
        "dataset_id": "ds-a",
        "tenant_id": "tenant-a",
        "collection_name": "kb_ds-a_1024",
        "embedding_provider": "dashscope",
        "embedding_model": "text-embedding-v4",
        "embedding_model_version": "2026-01",
        "embedding_dimension": 1024,
        "state": "serving",
        "capabilities": [],
    }
    base.update(overrides)
    return base


# --------------------------------------------------------------------- naming


def test_shadow_collection_name_encodes_dimension_and_is_deterministic():
    first = make_shadow_collection_name("ds-a", 1024, "m1")
    second = make_shadow_collection_name("ds-a", 1024, "m1")
    assert first == second
    # The dimension is still readable from the name (PRD §6.3 convention).
    assert first == "kb_ds-a_1024_vm1"
    # An empty tag falls back to a stable digest, not to the live name.
    tagged = make_shadow_collection_name("ds-a", 1024, "")
    assert tagged != "kb_ds-a_1024"
    assert tagged.startswith("kb_ds-a_1024_v")


def test_shadow_collection_name_sanitizes_illegal_characters():
    name = make_shadow_collection_name("ds a/we!rd", 512, "v 2!")
    assert name == "kb_ds_a_we_rd_512_vv_2"


# ------------------------------------------------------------------ identity


def test_identities_match_requires_full_identity():
    serving = _binding()
    assert identities_match(serving, embedding_identity(serving))
    assert not identities_match(
        serving, {**embedding_identity(serving), "embedding_model_version": "2026-08"}
    )
    assert not identities_match(
        serving, {**embedding_identity(serving), "embedding_dimension": 2048}
    )
    # A zero-dimension identity is never a match, even on empty rows.
    assert not identities_match({}, {})


@pytest.mark.parametrize(
    ("kwargs", "message_fragment"),
    [
        (
            {"embedding_provider": "gemini", "embedding_model": "text-embedding-v4"},
            "provider",
        ),
        (
            {"embedding_provider": "dashscope", "embedding_model": "qwen3-embedding"},
            "model",
        ),
        (
            {
                "embedding_provider": "dashscope",
                "embedding_model": "text-embedding-v4",
                "embedding_dimension": 2048,
            },
            "dimension",
        ),
    ],
)
def test_mixed_model_query_is_rejected(kwargs: dict[str, Any], message_fragment: str):
    with pytest.raises(MixedModelEmbeddingError) as excinfo:
        assert_query_embedding_identity(_binding(), **kwargs)
    assert message_fragment in str(excinfo.value)


def test_matching_query_and_legacy_unknown_binding_are_accepted():
    assert_query_embedding_identity(
        _binding(),
        embedding_provider="DashScope",
        embedding_model="TEXT-EMBEDDING-V4",
        embedding_dimension=1024,
    )
    # A pre-T3 binding with no recorded identity must not break queries.
    assert_query_embedding_identity(
        _binding(embedding_provider="", embedding_model=""),
        embedding_provider="anything",
        embedding_model="at-all",
    )


# --------------------------------------------------- start_migration planning


class FakeStore:
    """In-memory stand-in for the subset of EmbeddingVersionStore the
    planning path touches (persistence itself is tier-b tested)."""

    def __init__(self, *, serving: dict[str, Any] | None) -> None:
        self.serving = serving
        self.bindings: dict[str, dict[str, Any]] = {}
        self.migrations: dict[str, dict[str, Any]] = {}
        self.registered_from_rows: list[dict[str, Any]] = []
        self.progress_merges: list[dict[str, Any]] = []
        self._next_id = 2

    async def get_serving_binding(self, dataset_id: str) -> dict[str, Any] | None:
        if self.serving and self.serving["dataset_id"] == dataset_id:
            return self.serving
        return None

    async def register_serving_binding_from_dataset_row(
        self, dataset: dict[str, Any]
    ) -> dict[str, Any] | None:
        self.registered_from_rows.append(dict(dataset))
        self.serving = _binding(
            dataset_id=str(dataset["dataset_id"]),
            collection_name=str(dataset.get("collection_name") or ""),
            embedding_provider=str(dataset.get("embedding_provider") or ""),
            embedding_model=str(dataset.get("embedding_model") or ""),
            embedding_dimension=int(dataset.get("embedding_dimension") or 0),
        )
        return self.serving

    async def create_binding(self, **kwargs: Any) -> dict[str, Any]:
        if kwargs.get("collection_name") == "taken":
            raise BindingConflictError("reserved")
        binding_id = f"binding-{self._next_id}"
        self._next_id += 1
        binding = {"binding_id": binding_id, "state": "shadow", **kwargs}
        self.bindings[binding_id] = binding
        return binding

    async def begin_migration(
        self, *, dataset_id: str, source_binding_id: str | None, target_binding_id: str
    ) -> dict[str, Any]:
        migration_id = f"migration-{self._next_id}"
        self._next_id += 1
        migration = {
            "migration_id": migration_id,
            "dataset_id": dataset_id,
            "source_binding_id": source_binding_id,
            "target_binding_id": target_binding_id,
            "state": "shadow_build",
        }
        self.migrations[migration_id] = migration
        return migration

    async def merge_migration_progress(self, migration_id: str, **kwargs: Any) -> None:
        self.progress_merges.append({"migration_id": migration_id, **kwargs})

    async def get_migration(self, migration_id: str) -> dict[str, Any] | None:
        return self.migrations.get(migration_id)

    async def get_live_migration(self, dataset_id: str) -> dict[str, Any] | None:
        live = {"shadow_build", "backfilling", "verified", "gating", "ready"}
        for migration in reversed(list(self.migrations.values())):
            if migration.get("dataset_id") == dataset_id and migration.get("state") in live:
                return migration
        return None

    async def get_recoverable_migration(self, dataset_id: str) -> dict[str, Any] | None:
        for migration in reversed(list(self.migrations.values())):
            if migration.get("dataset_id") == dataset_id and migration.get("state") in {
                "failed",
                "gate_failed",
            }:
                return migration
        return None

    async def get_binding_by_collection_name(
        self, collection_name: str
    ) -> dict[str, Any] | None:
        for binding in self.bindings.values():
            if binding.get("collection_name") == collection_name and binding.get(
                "state"
            ) != "retired":
                return binding
        return None

    async def get_migration_by_target_binding(
        self, binding_id: str
    ) -> dict[str, Any] | None:
        for migration in reversed(list(self.migrations.values())):
            if migration.get("target_binding_id") == binding_id:
                return migration
        return None

    async def count_pending_segments(
        self, migration_id: str, *, dataset_id: str | None = None
    ) -> int:
        del migration_id, dataset_id
        return 3

    async def count_enabled_segments(self, dataset_id: str) -> int:
        del dataset_id
        return 5

    async def require_action_job_store(self) -> None:
        return None

    async def describe_action_jobs(
        self, dataset_id: str, *, terminal_limit: int = 10
    ) -> tuple[None, list[dict[str, Any]]]:
        del dataset_id, terminal_limit
        return None, []


class FakeVectorStore:
    def __init__(self, *, fail_binding_creation: bool = False) -> None:
        del fail_binding_creation
        self.ensured: list[dict[str, Any]] = []
        self.deleted: list[str] = []

    async def ensure_collection(self, **kwargs: Any) -> str:
        self.ensured.append(kwargs)
        return str(kwargs["collection_name"])

    async def delete_collection(self, collection_name: str) -> None:
        self.deleted.append(str(collection_name))


def _service(
    serving: dict[str, Any] | None = None,
) -> tuple[EmbeddingMigrationService, FakeStore, FakeVectorStore]:
    store = FakeStore(serving=serving)
    vector_store = FakeVectorStore()
    service = EmbeddingMigrationService(store=store, vector_store=vector_store)
    return service, store, vector_store


@pytest.mark.asyncio
async def test_start_migration_creates_differently_named_shadow_generation():
    service, store, vector_store = _service(_binding())
    result = await service.start_migration(
        {"dataset_id": "ds-a", "tenant_id": "tenant-a"},
        target_provider="local",
        target_model="qwen3-embedding-4b",
        target_model_version="2026-08",
        target_dimension=1536,
        capabilities=["text"],
        migration_tag="qwen3",
    )
    assert result["target_binding"]["collection_name"] == "kb_ds-a_1536_vqwen3"
    ensured = vector_store.ensured[0]
    assert ensured["dimension"] == 1536
    assert ensured["tenant_id"] == "tenant-a"
    assert store.serving["collection_name"] == "kb_ds-a_1024"  # untouched
    assert result["migration"]["source_binding_id"] == store.serving["binding_id"]
    assert result["migration"]["target_binding_id"] == result["target_binding"]["binding_id"]


@pytest.mark.asyncio
async def test_start_migration_rejects_identical_identity():
    service, _store, _vector_store = _service(_binding())
    with pytest.raises(EmbeddingMigrationError, match="reembed"):
        await service.start_migration(
            {"dataset_id": "ds-a"},
            target_provider="dashscope",
            target_model="text-embedding-v4",
            target_model_version="2026-01",
            target_dimension=1024,
        )


@pytest.mark.asyncio
async def test_start_migration_requires_target_identity():
    service, _store, _vector_store = _service(_binding())
    with pytest.raises(EmbeddingMigrationError, match="dimension"):
        await service.start_migration(
            {"dataset_id": "ds-a"},
            target_provider="local",
            target_model="qwen3-embedding-4b",
            target_dimension=0,
        )
    with pytest.raises(EmbeddingMigrationError, match="provider and model"):
        await service.start_migration(
            {"dataset_id": "ds-a"},
            target_provider="",
            target_model="qwen3-embedding-4b",
            target_dimension=1536,
        )


@pytest.mark.asyncio
async def test_start_migration_cleans_up_orphan_collection_on_binding_conflict():
    service, store, vector_store = _service(_binding())

    async def _conflicting_create_binding(**kwargs: Any) -> dict[str, Any]:
        raise BindingConflictError("reserved")

    store.create_binding = _conflicting_create_binding  # type: ignore[method-assign]
    with pytest.raises(BindingConflictError):
        await service.start_migration(
            {"dataset_id": "ds-a"},
            target_provider="local",
            target_model="m",
            target_dimension=1536,
            migration_tag="qwen3",
        )
    # Zero orphans: no binding row reserves the name, so the collection
    # created before the failed DB reservation is deleted again.
    assert vector_store.deleted == ["kb_ds-a_1536_vqwen3"]


def _seed_shadow_attempt(
    store: FakeStore, *, state: str, collection_name: str = "kb_ds-a_1536_vqwen3"
) -> tuple[dict[str, Any], dict[str, Any]]:
    binding = {
        "binding_id": "binding-shadow",
        "dataset_id": "ds-a",
        "tenant_id": "tenant-a",
        "collection_name": collection_name,
        "embedding_provider": "local",
        "embedding_model": "m",
        "embedding_model_version": "",
        "embedding_dimension": 1536,
        "state": "shadow",
    }
    store.bindings[binding["binding_id"]] = binding
    migration = {
        "migration_id": "migration-prior",
        "dataset_id": "ds-a",
        "source_binding_id": store.serving["binding_id"] if store.serving else None,
        "target_binding_id": binding["binding_id"],
        "state": state,
    }
    store.migrations[migration["migration_id"]] = migration
    return binding, migration


def _start_args() -> dict[str, Any]:
    return {
        "target_provider": "local",
        "target_model": "m",
        "target_dimension": 1536,
        "migration_tag": "qwen3",
    }


@pytest.mark.asyncio
async def test_start_migration_resumes_failed_attempt_instead_of_racing_binding():
    """Deterministic shadow names mean a retry hits the prior attempt's
    reservation; start must RESUME it, never race a new binding for the same
    name (the old behavior answered 409 and deleted the in-flight generation
    on the way out)."""
    service, store, vector_store = _service(_binding())
    binding, _migration = _seed_shadow_attempt(store, state="failed")

    async def _must_not_be_called(**kwargs: Any) -> dict[str, Any]:
        del kwargs
        raise AssertionError("resume must not create a new binding")

    store.create_binding = _must_not_be_called  # type: ignore[method-assign]
    result = await service.start_migration({"dataset_id": "ds-a"}, **_start_args())
    assert result["resumed"] is True
    assert result["migration"]["migration_id"] == "migration-prior"
    assert result["target_binding"]["binding_id"] == binding["binding_id"]
    # Nothing new is provisioned; the prior shadow generation is intact.
    assert vector_store.ensured == []
    assert vector_store.deleted == []


@pytest.mark.asyncio
async def test_start_migration_resumes_gate_failed_attempt():
    service, store, vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="gate_failed")
    result = await service.start_migration({"dataset_id": "ds-a"}, **_start_args())
    assert result["resumed"] is True
    assert result["migration"]["state"] == "gate_failed"
    assert vector_store.ensured == []


@pytest.mark.asyncio
async def test_start_migration_refuses_when_attempt_still_live():
    service, store, vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="backfilling")
    with pytest.raises(MigrationStateError, match="live embedding migration"):
        await service.start_migration({"dataset_id": "ds-a"}, **_start_args())
    assert vector_store.ensured == []
    assert vector_store.deleted == []


@pytest.mark.asyncio
async def test_start_migration_terminal_reservation_points_at_abort():
    service, store, _vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="completed")
    with pytest.raises(MigrationStateError, match="abort that migration"):
        await service.start_migration({"dataset_id": "ds-a"}, **_start_args())


@pytest.mark.asyncio
async def test_start_migration_reopens_rolled_back_attempt():
    """P1 wedge fix: rollback(keep_shadow=True) leaves the migration
    rolled_back with the shadow binding reserved. Start must reopen that
    attempt (rolled_back -> backfilling) instead of answering 409 "abort that
    migration" — which used to be un-abortable too, deadlocking the dataset."""
    service, store, vector_store = _service(_binding())
    binding, migration = _seed_shadow_attempt(store, state="rolled_back")

    async def _reopen(migration_id: str) -> dict[str, Any] | None:
        target = store.migrations.get(migration_id)
        if target is None or target.get("state") != "rolled_back":
            return None
        target["state"] = "backfilling"
        target["error"] = None
        return target

    store.reopen_migration_for_retry = _reopen  # type: ignore[method-assign]

    result = await service.start_migration({"dataset_id": "ds-a"}, **_start_args())
    assert result["resumed"] is True
    assert result["reopened"] is True
    assert result["migration"]["migration_id"] == migration["migration_id"]
    assert result["migration"]["state"] == "backfilling"
    assert result["target_binding"]["binding_id"] == binding["binding_id"]
    # Reuse, not reprovision: the shadow collection already exists.
    assert vector_store.ensured == []
    assert vector_store.deleted == []


@pytest.mark.asyncio
async def test_start_migration_rolled_back_without_reopen_points_at_abort():
    """A store without the reopen seam (or a shadow that is no longer
    retryable) still yields the actionable abort guidance, not a silent wedge."""
    service, store, vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="rolled_back")
    with pytest.raises(MigrationStateError, match="abort that migration"):
        await service.start_migration({"dataset_id": "ds-a"}, **_start_args())
    assert vector_store.ensured == []
    assert vector_store.deleted == []


@pytest.mark.asyncio
async def test_start_migration_reopen_refused_when_shadow_not_retryable():
    """reopen returning None (target binding no longer 'shadow') must fall
    back to the abort guidance rather than resurrect a vector-less attempt."""
    service, store, _vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="rolled_back")

    async def _reopen_none(_migration_id: str) -> None:
        return None

    store.reopen_migration_for_retry = _reopen_none  # type: ignore[method-assign]
    with pytest.raises(MigrationStateError, match="abort that migration"):
        await service.start_migration({"dataset_id": "ds-a"}, **_start_args())


@pytest.mark.asyncio
async def test_start_migration_resume_refused_when_another_migration_is_live():
    """A failed attempt is resumable only while the dataset has no OTHER live
    migration; otherwise the resume would collide on the one-live-per-dataset
    slot at the first backfilling transition."""
    service, store, _vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="failed")
    store.migrations["migration-other"] = {
        "migration_id": "migration-other",
        "dataset_id": "ds-a",
        "source_binding_id": store.serving["binding_id"],
        "target_binding_id": "binding-other",
        "state": "backfilling",
    }
    with pytest.raises(MigrationStateError, match="live embedding migration"):
        await service.start_migration({"dataset_id": "ds-a"}, **_start_args())


@pytest.mark.asyncio
async def test_start_migration_resume_allowed_for_the_same_migration():
    """The live-check must not refuse the attempt itself: a migration in a
    recoverable state is not live, so get_live_migration returning None lets
    the resume proceed (guards against comparing a migration to itself)."""
    service, store, _vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="gate_failed")
    result = await service.start_migration({"dataset_id": "ds-a"}, **_start_args())
    assert result["resumed"] is True


@pytest.mark.asyncio
async def test_start_migration_adopts_orphan_binding_left_by_crashed_start():
    """create_binding landed but begin_migration did not: the binding without
    a migration is adopted (and the collection re-ensured idempotently)
    instead of reserving the name forever."""
    service, store, vector_store = _service(_binding())
    binding, _migration = _seed_shadow_attempt(store, state="failed")
    del store.migrations["migration-prior"]

    result = await service.start_migration({"dataset_id": "ds-a"}, **_start_args())
    assert "resumed" not in result
    assert result["migration"]["target_binding_id"] == binding["binding_id"]
    assert result["migration"]["state"] == "shadow_build"
    assert [e["collection_name"] for e in vector_store.ensured] == [
        "kb_ds-a_1536_vqwen3"
    ]
    assert vector_store.deleted == []


@pytest.mark.asyncio
async def test_start_migration_refuses_foreign_collection_reservation():
    service, store, vector_store = _service(_binding())
    binding, _migration = _seed_shadow_attempt(store, state="failed")
    store.bindings[binding["binding_id"]]["dataset_id"] = "ds-other"
    with pytest.raises(BindingConflictError, match="reserved by another dataset"):
        await service.start_migration({"dataset_id": "ds-a"}, **_start_args())
    # Refused before touching the vector store at all.
    assert vector_store.ensured == []
    assert vector_store.deleted == []


@pytest.mark.asyncio
async def test_start_migration_conflict_never_deletes_a_referenced_collection():
    """P1 regression: when the DB reservation conflicts but a live binding
    row still references the collection name, the cleanup must NOT delete the
    collection (it belongs to a retryable generation). Simulates the race
    where the reservation lands between the pre-check and create_binding."""
    service, store, vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="failed")

    original_lookup = store.get_binding_by_collection_name
    calls = {"n": 0}

    async def _lookup_after_race(collection_name: str) -> dict[str, Any] | None:
        calls["n"] += 1
        if calls["n"] == 1:
            return None  # the pre-check saw nothing...
        return await original_lookup(collection_name)  # ...cleanup sees the row

    store.get_binding_by_collection_name = _lookup_after_race  # type: ignore[method-assign]

    async def _conflicting_create_binding(**kwargs: Any) -> dict[str, Any]:
        del kwargs
        raise BindingConflictError("reserved")

    store.create_binding = _conflicting_create_binding  # type: ignore[method-assign]
    with pytest.raises(BindingConflictError):
        await service.start_migration({"dataset_id": "ds-a"}, **_start_args())
    # The referenced collection survives; only true orphans get deleted.
    assert vector_store.deleted == []


@pytest.mark.asyncio
async def test_start_migration_adopts_legacy_dataset_row():
    dataset = {
        "dataset_id": "ds-a",
        "tenant_id": "tenant-a",
        "collection_name": "kb_ds-a_1024",
        "embedding_provider": "dashscope",
        "embedding_model": "text-embedding-v4",
        "embedding_dimension": 1024,
    }
    service, store, _vector_store = _service(None)
    result = await service.start_migration(
        dataset,
        target_provider="local",
        target_model="qwen3-embedding-4b",
        target_dimension=1536,
        migration_tag="qwen3",
    )
    assert store.registered_from_rows == [dataset]
    assert result["serving_binding"]["binding_id"] == store.serving["binding_id"]


@pytest.mark.asyncio
async def test_resolve_serving_binding_requires_collection_identity():
    service, _store, _vector_store = _service(None)
    with pytest.raises(EmbeddingMigrationError, match="dataset_id is required"):
        await service.resolve_serving_binding({})
    assert (
        await service.resolve_serving_binding({"dataset_id": "ds-empty", "collection_name": ""})
        is None
    )


@pytest.mark.asyncio
async def test_describe_surfaces_failed_migration_when_none_live():
    """A failed/gate_failed attempt stays operator-actionable; describe must
    surface it (with its migration_id) or the dataset is wedged behind an
    invisible shadow reservation."""
    service, store, _vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="failed")

    result = await service.describe(
        {"dataset_id": "ds-a", "collection_name": "kb_ds-a_1024"}
    )
    assert result["live_migration"] is not None
    assert result["live_migration"]["migration_id"] == "migration-prior"
    assert result["live_migration"]["state"] == "failed"
    assert result["latest_migration"]["migration_id"] == "migration-prior"
    assert result["recent_migrations"][0]["migration_id"] == "migration-prior"
    assert result["collection_health"]["status"] == "unknown"
    assert result["collection_health"]["checked_live"] is False
    # 'failed' participates in the pending-progress accounting.
    assert result["pending_chunks"] == 3


@pytest.mark.asyncio
async def test_describe_prefers_live_migration_over_recoverable():
    service, store, _vector_store = _service(_binding())
    _seed_shadow_attempt(store, state="gate_failed")
    store.migrations["migration-live"] = {
        "migration_id": "migration-live",
        "dataset_id": "ds-a",
        "target_binding_id": "binding-shadow",
        "state": "backfilling",
    }
    result = await service.describe(
        {"dataset_id": "ds-a", "collection_name": "kb_ds-a_1024"}
    )
    assert result["live_migration"]["migration_id"] == "migration-live"


@pytest.mark.asyncio
async def test_describe_degrades_when_serving_binding_cannot_be_adopted():
    """describe is a read-only surface: a legacy dataset whose collection
    name is reserved elsewhere degrades to serving_binding=None instead of
    letting a GET answer 409 forever."""
    service, store, _vector_store = _service(None)

    async def _conflict_registration(_dataset: dict[str, Any]) -> dict[str, Any] | None:
        raise BindingConflictError("reserved by another dataset")

    store.register_serving_binding_from_dataset_row = _conflict_registration  # type: ignore[method-assign]
    result = await service.describe(
        {
            "dataset_id": "ds-a",
            "collection_name": "kb_ds-a_1024",
            "embedding_dimension": 1024,
        }
    )
    assert result["serving_binding"] is None
    assert result["live_migration"] is None
    assert result["enabled_chunks"] == 5


# ------------------------------------- seam S2: auxiliary-collection guard


class ProbingVectorStore(FakeVectorStore):
    """FakeVectorStore plus the ``collection_exists`` seam the seam-S2 guard
    uses to detect hierarchical auxiliary siblings of the serving collection."""

    def __init__(
        self, existing: set[str] | None = None, *, fail_probe: bool = False
    ) -> None:
        super().__init__()
        self.existing = set(existing or ())
        self.fail_probe = fail_probe
        self.probed: list[str] = []

    async def collection_exists(self, collection_name: str) -> bool:
        self.probed.append(str(collection_name))
        if self.fail_probe:
            raise RuntimeError("vector store unreachable")
        return str(collection_name) in self.existing


def _probing_service(
    existing: set[str] | None = None, *, fail_probe: bool = False
) -> tuple[EmbeddingMigrationService, FakeStore, ProbingVectorStore]:
    store = FakeStore(serving=_binding())
    vector_store = ProbingVectorStore(existing, fail_probe=fail_probe)
    service = EmbeddingMigrationService(store=store, vector_store=vector_store)
    return service, store, vector_store


@pytest.mark.asyncio
@pytest.mark.parametrize("suffix", ["_summary", "_sections"])
async def test_start_migration_refused_when_auxiliary_sibling_exists(suffix: str):
    # The blue-green enumeration only re-embeds text segments into the shadow
    # collection; a dataset whose serving generation owns `_summary`/`_sections`
    # siblings would strand them on the retired generation at cutover.
    service, store, vector_store = _probing_service(
        existing={f"kb_ds-a_1024{suffix}"}
    )
    with pytest.raises(EmbeddingMigrationError, match="auxiliary"):
        await service.start_migration(
            {"dataset_id": "ds-a"},
            target_provider="local",
            target_model="qwen3-embedding-4b",
            target_dimension=1536,
        )
    # Refused before any shadow side effect exists.
    assert vector_store.ensured == []
    assert store.bindings == {}


@pytest.mark.asyncio
async def test_start_migration_refused_when_auxiliary_probe_fails():
    # Unknown is treated as present: refusing a migration is cheap, stranding
    # a live retrieval store at cutover is not (fail closed).
    service, store, vector_store = _probing_service(fail_probe=True)
    with pytest.raises(EmbeddingMigrationError, match="auxiliary"):
        await service.start_migration(
            {"dataset_id": "ds-a"},
            target_provider="local",
            target_model="qwen3-embedding-4b",
            target_dimension=1536,
        )
    assert vector_store.ensured == []
    assert store.bindings == {}


@pytest.mark.asyncio
async def test_start_migration_proceeds_when_no_auxiliary_sibling_present():
    service, _store, vector_store = _probing_service(
        existing={"kb_other_dataset_1024_summary"}
    )
    result = await service.start_migration(
        {"dataset_id": "ds-a"},
        target_provider="local",
        target_model="qwen3-embedding-4b",
        target_dimension=1536,
        migration_tag="qwen3",
    )
    assert result["target_binding"]["collection_name"] == "kb_ds-a_1536_vqwen3"
    assert vector_store.probed == ["kb_ds-a_1024_summary", "kb_ds-a_1024_sections"]


@pytest.mark.asyncio
async def test_start_migration_skips_guard_without_probe_seam():
    # A vector store without collection_exists keeps legacy behaviour (the
    # hierarchical indexer is not wired into production main.py yet).
    service, _store, vector_store = _service(_binding())
    result = await service.start_migration(
        {"dataset_id": "ds-a"},
        target_provider="local",
        target_model="qwen3-embedding-4b",
        target_dimension=1536,
        migration_tag="qwen3",
    )
    assert result["target_binding"]["collection_name"] == "kb_ds-a_1536_vqwen3"
    assert len(vector_store.ensured) == 1


# ------------------------------------------------ seam S2: capabilities


@pytest.mark.asyncio
async def test_start_migration_inherits_serving_capabilities_when_unspecified():
    service, _store, _vector_store = _service(_binding(capabilities=["vision", "text"]))
    result = await service.start_migration(
        {"dataset_id": "ds-a"},
        target_provider="local",
        target_model="qwen3-embedding-4b",
        target_dimension=1536,
        migration_tag="qwen3",
    )
    # The new generation replaces the serving one wholesale: an unspecified
    # capability list inherits instead of silently dropping to empty.
    assert result["target_binding"]["capabilities"] == ["vision", "text"]


@pytest.mark.asyncio
async def test_start_migration_explicit_empty_capabilities_are_respected():
    service, _store, _vector_store = _service(_binding(capabilities=["vision"]))
    result = await service.start_migration(
        {"dataset_id": "ds-a"},
        target_provider="local",
        target_model="qwen3-embedding-4b",
        target_dimension=1536,
        migration_tag="qwen3",
        capabilities=[],
    )
    assert result["target_binding"]["capabilities"] == []


@pytest.mark.asyncio
async def test_start_migration_explicit_capabilities_win_over_inheritance():
    service, _store, _vector_store = _service(_binding(capabilities=["vision"]))
    result = await service.start_migration(
        {"dataset_id": "ds-a"},
        target_provider="local",
        target_model="qwen3-embedding-4b",
        target_dimension=1536,
        migration_tag="qwen3",
        capabilities=["text"],
    )
    assert result["target_binding"]["capabilities"] == ["text"]


@pytest.mark.asyncio
async def test_start_migration_inheritance_from_binding_without_capabilities():
    binding = _binding()
    binding.pop("capabilities")
    service, _store, _vector_store = _service(binding)
    result = await service.start_migration(
        {"dataset_id": "ds-a"},
        target_provider="local",
        target_model="qwen3-embedding-4b",
        target_dimension=1536,
        migration_tag="qwen3",
    )
    assert result["target_binding"]["capabilities"] == []


# ------------------------------------------- seam S2: backfill vector cache

CACHE_IDENTITY = ("local", "hash-384", "v9")


def _pending_row(index: int, content_hash: str) -> dict[str, Any]:
    return {
        "segment_id": f"seg-{index}",
        "document_id": f"doc-{index}",
        "position": index,
        "vector_id": f"vec-{index}",
        "content_hash": content_hash,
        "text": f"chunk text {index}",
        "token_count": 4,
        "metadata": {},
    }


class BackfillFakeStore(FakeStore):
    """FakeStore plus the corpus/backfill surface the loop touches."""

    def __init__(
        self, *, serving: dict[str, Any] | None, pending: list[dict[str, Any]]
    ) -> None:
        super().__init__(serving=serving)
        self.pending = list(pending)
        self.receipts: list[dict[str, Any]] = []
        self.transitions: list[dict[str, Any]] = []
        self.cache: dict[tuple[str, str, str, str], list[float]] = {}
        self.cache_lookups: list[list[str]] = []
        self.cache_stores: list[list[tuple[str, Any]]] = []
        self.fail_cache_lookup = False
        self.fail_cache_store = False
        self.enabled_count = len(pending)
        self.backfill_claimed = False
        self.authority = {
            "authority_kind": "postgres-enabled-text-segments-v1",
            "dataset_id": "ds-a",
            "tenant_id": "tenant-a",
            "serving_collection_name": "kb_ds-a_1024",
            "content_revision": 7,
            "point_count": len(pending),
            "point_ids_sha256": f"point-digest-{len(pending)}",
            "source_text_sha256": f"source-digest-{len(pending)}",
        }

    @asynccontextmanager
    async def backfill_lease(self, migration_id: str) -> AsyncIterator[None]:
        del migration_id
        if self.backfill_claimed:
            raise MigrationStateError("embedding migration backfill is already running")
        self.backfill_claimed = True
        try:
            yield
        finally:
            self.backfill_claimed = False

    async def transition_migration(
        self, migration_id: str, *, to_state: str, from_states: Any = None
    ) -> dict[str, Any] | None:
        self.transitions.append(
            {
                "migration_id": migration_id,
                "to_state": to_state,
                "from_states": tuple(from_states or ()),
            }
        )
        migration = self.migrations.get(migration_id)
        if migration is None:
            return None
        moved = {**migration, "state": to_state}
        self.migrations[migration_id] = moved
        return moved

    async def record_gate_verdict(
        self, migration_id: str, *, verdict: dict[str, Any], passed: bool
    ) -> dict[str, Any] | None:
        # Mirrors the real CAS: only a migration currently 'gating' accepts
        # the verdict (the escape into ready/gate_failed).
        migration = self.migrations.get(migration_id)
        if migration is None or migration.get("state") != "gating":
            return None
        moved = {
            **migration,
            "state": "ready" if passed else "gate_failed",
            "gate": verdict,
        }
        self.migrations[migration_id] = moved
        return moved

    async def get_binding(self, binding_id: str) -> dict[str, Any] | None:
        return self.bindings.get(binding_id)

    async def list_pending_segments(
        self, migration_id: str, *, dataset_id: str | None = None, limit: int | None = None
    ) -> list[dict[str, Any]]:
        del migration_id, dataset_id
        return list(self.pending[:limit])

    async def record_progress_receipts(
        self, migration_id: str, receipts: list[dict[str, Any]]
    ) -> None:
        del migration_id
        self.receipts.extend(receipts)
        # Mirror the real receipt join: receipted content hashes leave the
        # pending enumeration, which is what makes the loop terminate.
        receipted = {r["content_hash"] for r in receipts if r.get("content_hash")}
        self.pending = [
            row for row in self.pending if row.get("content_hash") not in receipted
        ]

    async def count_pending_segments(
        self, migration_id: str, *, dataset_id: str | None = None
    ) -> int:
        del migration_id, dataset_id
        return len(self.pending)

    async def count_enabled_segments(self, dataset_id: str) -> int:
        del dataset_id
        return self.enabled_count

    async def authority_snapshot(self, dataset_id: str) -> dict[str, Any]:
        assert dataset_id == "ds-a"
        return dict(self.authority)

    async def lookup_embeddings_batch(
        self,
        *,
        embedding_provider: str,
        embedding_model: str,
        embedding_model_version: str,
        content_hashes: Any,
    ) -> dict[str, list[float]]:
        self.cache_lookups.append([str(h) for h in content_hashes])
        if self.fail_cache_lookup:
            raise RuntimeError("vector cache unavailable")
        hits: dict[str, list[float]] = {}
        for content_hash in content_hashes:
            vector = self.cache.get(
                (embedding_provider, embedding_model, embedding_model_version, str(content_hash))
            )
            if vector is not None:
                hits[str(content_hash)] = list(vector)
        return hits

    async def store_embeddings_batch(
        self,
        *,
        embedding_provider: str,
        embedding_model: str,
        embedding_model_version: str,
        entries: Any,
    ) -> int:
        self.cache_stores.append(list(entries))
        if self.fail_cache_store:
            raise RuntimeError("vector cache write unavailable")
        for content_hash, vector in entries:
            self.cache[
                (embedding_provider, embedding_model, embedding_model_version, str(content_hash))
            ] = [float(component) for component in vector]
        return len(list(entries))


class LegacyBackfillStore(BackfillFakeStore):
    """A store predating the T3.4 vector cache: the seams resolve to None."""

    lookup_embeddings_batch = None  # type: ignore[assignment]
    store_embeddings_batch = None  # type: ignore[assignment]


class UpsertingVectorStore(FakeVectorStore):
    def __init__(self) -> None:
        super().__init__()
        self.upserts: list[dict[str, Any]] = []
        self.scope_evidence: dict[str, Any] = {}

    async def upsert(self, *, collection_name: str, points: Any, **kwargs: Any) -> None:
        del kwargs
        self.upserts.append({"collection_name": collection_name, "points": list(points)})

    async def scan_embedding_migration_scope(
        self, _collection_name: str, **_kwargs: Any
    ) -> dict[str, Any]:
        return dict(self.scope_evidence)


class RecordingEmbedder:
    def __init__(self, dimension: int = 3, fill: float = 0.1) -> None:
        self.dimension = dimension
        self.fill = fill
        self.calls: list[list[str]] = []

    async def embed_texts(self, texts: list[str], text_type: str | None = None) -> list[list[float]]:
        del text_type
        self.calls.append(list(texts))
        return [[self.fill] * self.dimension for _ in texts]


class BlockingEmbedder(RecordingEmbedder):
    def __init__(self) -> None:
        super().__init__()
        self.started = asyncio.Event()
        self.release = asyncio.Event()

    async def embed_texts(
        self, texts: list[str], text_type: str | None = None
    ) -> list[list[float]]:
        del text_type
        self.calls.append(list(texts))
        self.started.set()
        await self.release.wait()
        return [[self.fill] * self.dimension for _ in texts]


def _backfill_setup(
    pending_rows: list[dict[str, Any]],
    *,
    store_cls: type[BackfillFakeStore] = BackfillFakeStore,
    cache: dict[tuple[str, str, str, str], list[float]] | None = None,
    fail_lookup: bool = False,
    fail_store: bool = False,
) -> tuple[EmbeddingMigrationService, BackfillFakeStore, UpsertingVectorStore, RecordingEmbedder]:
    store = store_cls(serving=_binding(), pending=pending_rows)
    target = _binding(
        binding_id="binding-target",
        collection_name="kb_ds-a_1536_vt",
        embedding_provider=CACHE_IDENTITY[0],
        embedding_model=CACHE_IDENTITY[1],
        embedding_model_version=CACHE_IDENTITY[2],
        embedding_dimension=3,
        state="shadow",
    )
    store.bindings[target["binding_id"]] = target
    store.migrations["mig-1"] = {
        "migration_id": "mig-1",
        "dataset_id": "ds-a",
        "source_binding_id": store.serving["binding_id"] if store.serving else None,
        "target_binding_id": target["binding_id"],
        "state": "shadow_build",
    }
    store.cache.update(cache or {})
    store.fail_cache_lookup = fail_lookup
    store.fail_cache_store = fail_store
    vector_store = UpsertingVectorStore()
    vector_store.scope_evidence = {
        key: store.authority[key]
        for key in ("point_count", "point_ids_sha256", "source_text_sha256")
    }
    store.migrations["mig-1"]["totals"] = {
        "verified_authority": dict(store.authority),
        "verified_target_scope": dict(vector_store.scope_evidence),
    }
    service = EmbeddingMigrationService(store=store, vector_store=vector_store)
    embedder = RecordingEmbedder()
    return service, store, vector_store, embedder


@pytest.mark.asyncio
async def test_concurrent_backfill_has_one_claim_and_one_paid_embed_call() -> None:
    service, store, _vector_store, _embedder = _backfill_setup(
        [_pending_row(1, "hash-1")]
    )
    first_embedder = BlockingEmbedder()
    duplicate_embedder = RecordingEmbedder()
    first = asyncio.create_task(service.backfill("mig-1", first_embedder))
    await asyncio.wait_for(first_embedder.started.wait(), timeout=1)
    try:
        with pytest.raises(MigrationStateError, match="already running"):
            await service.backfill("mig-1", duplicate_embedder)
    finally:
        first_embedder.release.set()

    result = await asyncio.wait_for(first, timeout=1)
    assert result["embedded"] == 1
    assert first_embedder.calls == [["chunk text 1"]]
    assert duplicate_embedder.calls == []
    assert store.backfill_claimed is False


@pytest.mark.asyncio
async def test_backfill_reuses_cached_vectors_without_calling_the_embedder():
    rows = [_pending_row(1, "hash-1"), _pending_row(2, "hash-2")]
    cached_vector = [0.9, 0.9, 0.9]
    service, store, vector_store, embedder = _backfill_setup(
        rows,
        cache={
            (*CACHE_IDENTITY, "hash-1"): list(cached_vector),
            (*CACHE_IDENTITY, "hash-2"): list(cached_vector),
        },
    )
    result = await service.backfill("mig-1", embedder)
    assert result["embedded"] == 2
    assert result["pending"] == 0
    assert embedder.calls == []  # nothing was embedded
    assert store.cache_stores == []  # nothing new to write back
    points = vector_store.upserts[0]["points"]
    assert [list(point.vector) for point in points] == [cached_vector] * 2
    assert {receipt["content_hash"] for receipt in store.receipts} == {"hash-1", "hash-2"}


@pytest.mark.asyncio
async def test_backfill_embeds_cache_misses_and_writes_them_back():
    rows = [_pending_row(1, "hash-1"), _pending_row(2, "hash-2")]
    service, store, vector_store, embedder = _backfill_setup(
        rows, cache={(*CACHE_IDENTITY, "hash-1"): [0.9, 0.9, 0.9]}
    )
    result = await service.backfill("mig-1", embedder)
    assert result["embedded"] == 2
    # Only the miss hit the embedder...
    assert embedder.calls == [["chunk text 2"]]
    # ...and only the miss was written back, under the target identity.
    assert store.cache_stores == [[("hash-2", [0.1, 0.1, 0.1])]]
    assert store.cache[(*CACHE_IDENTITY, "hash-2")] == [0.1, 0.1, 0.1]
    points = vector_store.upserts[0]["points"]
    assert list(points[0].vector) == [0.9, 0.9, 0.9]  # cached
    assert list(points[1].vector) == [0.1, 0.1, 0.1]  # freshly embedded


@pytest.mark.asyncio
async def test_backfill_reembeds_when_cached_vector_dimension_drifts():
    rows = [_pending_row(1, "hash-1")]
    service, store, _vector_store, embedder = _backfill_setup(
        rows, cache={(*CACHE_IDENTITY, "hash-1"): [0.9, 0.9]}  # dim 2, target is 3
    )
    result = await service.backfill("mig-1", embedder)
    assert result["embedded"] == 1
    assert embedder.calls == [["chunk text 1"]]
    # The corrupt entry was replaced by the freshly embedded dim-3 vector.
    assert store.cache[(*CACHE_IDENTITY, "hash-1")] == [0.1, 0.1, 0.1]


@pytest.mark.asyncio
async def test_backfill_degrades_to_full_embedding_when_cache_lookup_fails():
    rows = [_pending_row(1, "hash-1"), _pending_row(2, "hash-2")]
    service, store, _vector_store, embedder = _backfill_setup(rows, fail_lookup=True)
    result = await service.backfill("mig-1", embedder)
    assert result["embedded"] == 2
    assert embedder.calls == [["chunk text 1", "chunk text 2"]]
    # Write-back still primes the cache for the next round.
    assert store.cache_stores and len(store.cache_stores[0]) == 2


@pytest.mark.asyncio
async def test_backfill_survives_cache_write_back_failure():
    rows = [_pending_row(1, "hash-1")]
    service, store, vector_store, embedder = _backfill_setup(rows, fail_store=True)
    result = await service.backfill("mig-1", embedder)
    # The cache is a cost optimization; the receipt ledger is the authority.
    assert result["embedded"] == 1
    assert result["pending"] == 0
    assert [receipt["content_hash"] for receipt in store.receipts] == ["hash-1"]
    assert vector_store.upserts and len(vector_store.upserts[0]["points"]) == 1


@pytest.mark.asyncio
async def test_backfill_works_on_a_store_without_cache_seams():
    rows = [_pending_row(1, "hash-1")]
    service, store, vector_store, embedder = _backfill_setup(
        rows, store_cls=LegacyBackfillStore
    )
    result = await service.backfill("mig-1", embedder)
    assert result["embedded"] == 1
    assert embedder.calls == [["chunk text 1"]]
    assert store.cache_stores == []
    assert vector_store.upserts and len(vector_store.upserts[0]["points"]) == 1


# ------------------------------------------------------ T0 gate wedge escapes


@pytest.mark.asyncio
async def test_verify_persists_durable_action_job_correlation() -> None:
    service, store, _vector_store, _embedder = _backfill_setup(
        [_pending_row(1, "hash-1")]
    )
    store.pending = []
    store.migrations["mig-1"]["state"] = "backfilling"
    job_id = "11111111-2222-4333-8444-555555555555"

    result = await service.verify("mig-1", action_job_id=job_id)

    assert result["migration"]["state"] == "verified"
    verify_merge = next(
        merge
        for merge in reversed(store.progress_merges)
        if "verified_authority" in merge.get("totals", {})
    )
    assert verify_merge["totals"]["verify_action_job_id"] == job_id


@pytest.mark.asyncio
async def test_run_gate_recovers_migration_stranded_in_gating():
    """A migration stranded in 'gating' (verdict recording crashed) must be
    able to escape FORWARD by re-running the gate, not only via abort."""
    service, store, _vector_store, _embedder = _backfill_setup(
        [_pending_row(1, "hash-1")]
    )
    store.pending = []
    store.migrations["mig-1"]["state"] = "gating"

    async def _verdict(_context: dict[str, Any]) -> dict[str, Any]:
        return {"passed": True}

    result = await service.run_gate("mig-1", _verdict)
    assert result["passed"] is True
    assert store.migrations["mig-1"]["state"] == "ready"
    gating_entry = [t for t in store.transitions if t["to_state"] == "gating"][-1]
    assert "gating" in gating_entry["from_states"]


@pytest.mark.asyncio
async def test_run_gate_non_dict_verdict_records_failure_and_escapes_gating():
    """A malformed (non-dict) evaluator return must record a failing verdict
    and move the migration to gate_failed, not strand it in 'gating'."""
    service, store, _vector_store, _embedder = _backfill_setup(
        [_pending_row(1, "hash-1")]
    )
    store.pending = []
    store.migrations["mig-1"]["state"] = "verified"

    async def _bad_verdict(_context: dict[str, Any]) -> str:
        return "not-a-dict"

    with pytest.raises(EmbeddingMigrationError, match="must return a dict verdict"):
        await service.run_gate("mig-1", _bad_verdict)
    assert store.migrations["mig-1"]["state"] == "gate_failed"
    assert store.migrations["mig-1"]["gate"]["passed"] is False


@pytest.mark.asyncio
async def test_run_gate_rejects_authority_change_during_evaluation():
    service, store, _vector_store, _embedder = _backfill_setup(
        [_pending_row(1, "hash-1")]
    )
    store.pending = []
    store.migrations["mig-1"]["state"] = "verified"

    async def _verdict_after_corpus_change(
        _context: dict[str, Any],
    ) -> dict[str, Any]:
        store.authority = {
            **store.authority,
            "content_revision": store.authority["content_revision"] + 1,
        }
        return {"passed": True}

    with pytest.raises(EmbeddingMigrationError, match="authority changed"):
        await service.run_gate("mig-1", _verdict_after_corpus_change)

    migration = store.migrations["mig-1"]
    assert migration["state"] == "gate_failed"
    assert migration["gate"]["passed"] is False
    assert migration["gate"]["phase"] == "postcheck"
