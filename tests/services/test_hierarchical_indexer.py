import pytest
from knowledge_service.services.knowledge.hierarchical_indexer import (
    HierarchicalIndexer,
    IndexLevel,
)
from knowledge_service.services.knowledge.lexical_config import BM25_V2_FIELD, BM25_V2_MODEL


def _index_config(*, active: str = "lexical_v1"):
    return {
        "retrieval": {
            "lexical": {
                "active_version": active,
                "bm25_v2": {
                    "shadow_write_enabled": True,
                    "field": BM25_V2_FIELD,
                    "model": BM25_V2_MODEL,
                    "k": 1.2,
                    "b": 0.75,
                    "avg_len": 256,
                    "tokenizer": "multilingual",
                    "language": "none",
                    "lowercase": True,
                    "ascii_folding": False,
                    "filtering": {
                        "required_payload_indexes": ["tenant_id", "dataset_id"],
                        "strict_unindexed_filtering": False,
                    },
                },
            }
        }
    }


class StubEmbedder:
    def __init__(self):
        self.dimension = 3

    async def embed_documents(self, texts):
        return [[0.1, 0.2, 0.3] for _ in texts]


class StubVectorStore:
    def __init__(self):
        self.collections = []
        self.upserts = []

    async def ensure_collection(self, dataset_id, dimension, collection_name=None, **kwargs):
        self.collections.append((dataset_id, dimension, collection_name, kwargs))
        return collection_name or f"kb_{dataset_id}_{dimension}"

    async def upsert(self, collection_name, points):
        self.upserts.append((collection_name, points))


class StubDb:
    def __init__(self, index_config=None):
        self.inserted = []
        self.index_config = index_config or {}

    async def get_dataset(self, dataset_id):
        return {
            "dataset_id": dataset_id,
            "collection_name": None,
            "tenant_id": "tenant-a",
            "index_config": self.index_config,
        }

    async def insert_segments(self, segments):
        self.inserted.extend(segments)

    async def save_document_summary(self, data):
        return True


@pytest.mark.asyncio
async def test_indexer_sets_parent_segment_ids():
    text = "# Title\n\nSection one content.\n\n## Sub\n\nMore text."

    db = StubDb(index_config=_index_config())
    indexer = HierarchicalIndexer(
        vector_store=StubVectorStore(),
        database=db,
        embedder=StubEmbedder(),
        summary_generator=None,
    )

    result = await indexer.index_document("doc1", "ds1", text)

    assert result.l2_count >= 1
    assert result.l3_count >= 1

    parents = {s["segment_id"] for s in db.inserted if s.get("level") == IndexLevel.SECTION}
    children = [s for s in db.inserted if s.get("level") == IndexLevel.PARAGRAPH]
    assert children[0]["parent_segment_id"] in parents
    for _collection, points in indexer.vector_store.upserts:
        assert all(point.payload["tenant_id"] == "tenant-a" for point in points)
    assert all(call[3]["tenant_id"] == "tenant-a" for call in indexer.vector_store.collections)
    base_calls = [
        call for call in indexer.vector_store.collections if call[2] in {None, "kb_ds1_3"}
    ]
    derived_calls = [
        call for call in indexer.vector_store.collections if call[2] not in {None, "kb_ds1_3"}
    ]
    assert base_calls and "lexical_config" in base_calls[0][3]
    assert derived_calls
    assert all(call[3]["lexical_config"].configured is False for call in derived_calls)
    assert all(call[3]["allow_lexical_transition"] is True for call in derived_calls)


@pytest.mark.asyncio
async def test_indexer_rejects_all_writes_while_bm25_v2_is_active():
    store = StubVectorStore()
    db = StubDb(index_config=_index_config(active=BM25_V2_FIELD))
    indexer = HierarchicalIndexer(
        vector_store=store,
        database=db,
        embedder=StubEmbedder(),
        summary_generator=None,
    )

    result = await indexer.index_document("doc1", "ds1", "# Title\n\nBody")

    assert any("active mode is read-only" in error for error in result.errors)
    assert store.upserts == []
