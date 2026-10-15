"""Qdrant integration smoke for the KB evaluation foundation (PRD T0-#4).

Exercises the real ``VectorStore`` against a live Qdrant from the
``docker-compose.kbms.yml`` stack: create collection, dense+sparse upsert,
native hybrid (Prefetch + server-side RRF), and delete-by-filter — the exact
surface ``scripts/regen_rag_observations.py`` replays against.  Skips itself
when the stack is down, so it is safe to invoke at any time:

    make kb-integration-smoke
    # or: QDRANT_URL=http://localhost:6333 uv run --all-packages --extra test \
    #     pytest -q --no-cov -m integration tests/knowledge/test_qdrant_integration_smoke.py

The test creates only one disposable, scope-tagged collection with a random
suffix and deletes it in teardown; it never touches real dataset collections.
"""

from __future__ import annotations

import asyncio
import os
import socket
import uuid
from urllib.parse import urlparse

import pytest
from knowledge_service.services.knowledge.lexical_config import (
    COLLECTION_SCOPE_METADATA_KEY,
)
from knowledge_service.services.knowledge.retrieval import text_to_sparse_vector
from knowledge_service.services.knowledge.vector_store import (
    CollectionReadAuthorityError,
    VectorStore,
)
from qdrant_client.http import models as qmodels

QDRANT_URL = os.environ.get("QDRANT_URL", "http://localhost:6333")
DIMENSION = 8
TENANT_ID = "kb-eval-smoke-tenant"

# Development machines behind a system HTTP proxy (macOS ``scutil --proxy``)
# otherwise see synthetic 502s on Docker-forwarded ports; keep the Qdrant host
# out of every proxy path regardless of environment.
_qdrant_host = urlparse(QDRANT_URL).hostname or "localhost"
for _var in ("no_proxy", "NO_PROXY"):
    _existing = os.environ.get(_var, "")
    if _qdrant_host not in _existing:
        os.environ[_var] = f"{_existing},{_qdrant_host}".strip(",")


def _qdrant_reachable(url: str, timeout_seconds: float = 1.0) -> bool:
    parsed = urlparse(url)
    host = parsed.hostname or "localhost"
    port = parsed.port or (443 if parsed.scheme == "https" else 80)
    try:
        with socket.create_connection((host, port), timeout=timeout_seconds):
            return True
    except OSError:
        return False


pytestmark = [
    pytest.mark.integration,
    pytest.mark.skipif(
        not _qdrant_reachable(QDRANT_URL),
        reason=f"Qdrant stack not reachable at {QDRANT_URL} — start docker-compose.kbms.yml",
    ),
]


def _payload(document_id: str, segment_id: str, text: str) -> dict[str, object]:
    return {
        "document_id": document_id,
        "segment_id": segment_id,
        "text": text,
        "level": 0,
        "content_type": "text",
        "content_revision": 1,
    }


async def _run_smoke() -> None:
    store = VectorStore(url=QDRANT_URL, timeout_seconds=10.0)
    dataset_id = f"kb-eval-smoke-{uuid.uuid4().hex[:8]}"
    collection = ""
    try:
        # 1. Create a collection (unbound bootstrap: this smoke has no
        #    PostgreSQL dataset row and must not create one).
        collection = await store.ensure_collection(
            dataset_id=dataset_id,
            dimension=DIMENSION,
            tenant_id=TENANT_ID,
            bootstrap_unbound_dataset=True,
        )
        info = await store._client.get_collection(collection)
        # Storage authority mirrors VectorStore: collection metadata lives on
        # ``info.config.metadata``, not the top-level response.
        scope = store._collection_metadata(info).get(COLLECTION_SCOPE_METADATA_KEY)
        assert isinstance(scope, dict)
        assert scope["dataset_id"] == dataset_id and scope["tenant_id"] == TENANT_ID

        # 2. dense+sparse upsert (the sparse leg is derived server-side from
        #    the canonical ``text`` payload by the v1 lexical path).  Point
        #    ids are the segment ids, as production writers do (UUID strings;
        #    Qdrant point ids must be UUID or unsigned int).
        seg_a, seg_b, seg_c = (str(uuid.uuid4()) for _ in range(3))
        points = [
            qmodels.PointStruct(
                id=seg_a,
                vector=[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                payload=_payload("doc-a", seg_a, "quarterly invoice download billing history"),
            ),
            qmodels.PointStruct(
                id=seg_b,
                vector=[0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                payload=_payload("doc-b", seg_b, "password reset link verified email channel"),
            ),
            qmodels.PointStruct(
                id=seg_c,
                vector=[0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                payload=_payload("doc-c", seg_c, "garden gnomes ceramic patio decoration"),
            ),
        ]
        await store.upsert(collection_name=collection, points=points, lifecycle_lease_held=True)

        # 3. Native hybrid + RRF: dense closest to seg-a and lexical term match
        #    on the same text must fuse it to first place.
        query_text = "quarterly invoice download billing history"
        sparse_indices, sparse_values = text_to_sparse_vector(query_text)
        assert sparse_indices, "lexical encoder produced an empty query"
        hits = await store.hybrid_search_native(
            collection_name=collection,
            query_vector=[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            sparse_indices=sparse_indices,
            sparse_values=sparse_values,
            top_k=3,
            tenant_id=TENANT_ID,
            dataset_id=dataset_id,
            query_text=query_text,
        )
        by_segment = [hit.payload.get("segment_id") for hit in hits]
        assert by_segment[0] == seg_a
        assert set(by_segment) == {seg_a, seg_b, seg_c}
        assert all(hit.score > 0 for hit in hits)

        # 3b. Fail-closed tenancy: a foreign tenant must not read this scope.
        with pytest.raises(CollectionReadAuthorityError):
            await store.hybrid_search_native(
                collection_name=collection,
                query_vector=[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                sparse_indices=sparse_indices,
                sparse_values=sparse_values,
                top_k=3,
                tenant_id="some-other-tenant",
                dataset_id=dataset_id,
                query_text=query_text,
            )

        # 4. Delete by filter (segment identity + payload scope, not point ids).
        touched = await store.delete_segment_points(
            tenant_id=TENANT_ID,
            dataset_id=dataset_id,
            document_id="doc-a",
            segment_id=seg_a,
            lifecycle_lease_held=True,
        )
        assert collection in touched
        remaining = await store.hybrid_search_native(
            collection_name=collection,
            query_vector=[1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            sparse_indices=sparse_indices,
            sparse_values=sparse_values,
            top_k=3,
            tenant_id=TENANT_ID,
            dataset_id=dataset_id,
            query_text=query_text,
        )
        # seg-b and seg-c are dense-tied against the seg-a query; only their
        # set membership is deterministic after the filtered delete.
        assert [hit.payload.get("segment_id") for hit in remaining] != [seg_a]
        assert {hit.payload.get("segment_id") for hit in remaining} == {seg_b, seg_c}
    finally:
        if collection:
            await store.delete_collection(collection)
        await store.close()


def test_qdrant_integration_smoke() -> None:
    asyncio.run(_run_smoke())


@pytest.mark.asyncio
async def test_special_cleanup_deletes_legacy_tenantless_point_and_skips_missing_collection() -> None:
    store = VectorStore(url=QDRANT_URL, timeout_seconds=10.0)
    dataset_id = f"kb-special-cleanup-{uuid.uuid4().hex[:8]}"
    collection = ""
    try:
        collection = await store.ensure_collection(
            dataset_id=dataset_id, dimension=DIMENSION,
            tenant_id=TENANT_ID, bootstrap_unbound_dataset=True,
        )
        old_id, other_id = str(uuid.uuid4()), str(uuid.uuid4())
        await store._client.upsert(
            collection_name=collection,
            points=[
                qmodels.PointStruct(id=old_id, vector=[1.0] + [0.0] * 7,
                                    payload={"dataset_id": dataset_id, "document_id": "doc-old"}),
                qmodels.PointStruct(id=other_id, vector=[0.0, 1.0] + [0.0] * 6,
                                    payload={"dataset_id": dataset_id, "document_id": "doc-other"}),
            ],
            wait=True,
        )
        frozen = await store.document_point_ids_by_collection(
            tenant_id=TENANT_ID, dataset_id=dataset_id, document_id="doc-old",
        )
        assert frozen[collection] == [old_id]

        await store.delete_document_points_by_ids(
            collection, [old_id, other_id], tenant_id=TENANT_ID,
            dataset_id=dataset_id, document_id="doc-old", lifecycle_lease_held=True,
        )
        remaining = await store._client.retrieve(
            collection_name=collection, ids=[old_id, other_id], with_payload=True,
        )
        assert [str(point.id) for point in remaining] == [other_id]
        await store.delete_document_points_by_ids(
            f"{collection}_not_created", [old_id], tenant_id=TENANT_ID,
            dataset_id=dataset_id, document_id="doc-old", lifecycle_lease_held=True,
        )
    finally:
        if collection:
            await store.delete_collection(collection)
        await store.close()
