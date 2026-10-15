"""Unpublished ordinary-text generation for a historical version restore."""

from __future__ import annotations

import asyncio
import hashlib
import math
import uuid
from dataclasses import dataclass, field
from typing import Any

from qdrant_client.http.models import PointStruct

from .chunking import ChunkingConfig, require_chunk_output_budget
from .ingestion_service import _process_standard_chunks, _require_extracted_text_budget


@dataclass
class TextRestoreCandidatePlan:
    generation_id: str
    document_id: str
    dataset_id: str
    points_by_collection: dict[str, list[PointStruct]]
    segment_rows: list[dict[str, Any]]
    content: str
    source_hash: str
    segment_ids: list[str]
    source_kind: str = "text"
    object_manifest: list[dict[str, Any]] = field(default_factory=list)
    summary_row: None = None


class TextRestoreCandidatePlanner:
    """Prepare text rows and vectors without changing either serving store."""

    def __init__(self, service: Any) -> None:
        self.service = service

    @staticmethod
    def _candidate_id(
        dataset_id: str, document_id: str, generation_id: str, position: int,
    ) -> str:
        return str(uuid.uuid5(
            uuid.NAMESPACE_URL,
            "ai-platform:kb-text-restore:"
            f"{dataset_id}:{document_id}:{generation_id}:{position}",
        ))

    async def prepare(
        self, *, document_id: str, dataset: dict[str, Any],
        generation_id: str, content: str, title: str,
        chunking: dict[str, Any],
    ) -> TextRestoreCandidatePlan:
        try:
            generation = str(uuid.UUID(str(generation_id)))
        except (TypeError, ValueError, AttributeError) as exc:
            raise ValueError("text restore generation must be a durable UUID") from exc
        dataset_id = str(dataset.get("dataset_id") or "").strip()
        tenant_id = str(dataset.get("tenant_id") or "").strip()
        collection = str(dataset.get("collection_name") or "").strip()
        if not document_id or not dataset_id or not tenant_id or not collection:
            raise ValueError("text restore dataset or document identity is incomplete")
        if not isinstance(content, str) or not content.strip():
            raise ValueError("text restore requires nonempty historical content")
        if not isinstance(chunking, dict):
            raise ValueError("text restore chunking configuration is invalid")
        text = _require_extracted_text_budget(content)
        chunks = await asyncio.to_thread(
            _process_standard_chunks,
            text.strip(), ChunkingConfig.from_dict(chunking),
            document_id, str(title or document_id), dataset_id,
        )
        require_chunk_output_budget(chunks)
        if not chunks:
            raise RuntimeError("text restore produced no candidate segments")

        embedding_config = dataset.get("embedding_config") or {}
        if not isinstance(embedding_config, dict):
            raise ValueError("text restore embedding configuration is invalid")
        multimodal = self.service._is_multimodal_dataset(dataset)
        embedder = (
            await self.service._get_unified_multimodal_embedder(dataset, embedding_config)
            if multimodal else
            await self.service._get_text_embedder(dataset, embedding_config)
        )
        try:
            settings = getattr(getattr(self.service, "settings", None), "knowledge", None)
            configured_batch = (
                10 if multimodal else getattr(settings, "text_embedding_batch_size", 10)
            )
            batch_size = max(1, int(configured_batch or 10))
            vectors: list[list[float]] = []
            for start in range(0, len(chunks), batch_size):
                batch = chunks[start:start + batch_size]
                result = await embedder.embed_documents([chunk.text for chunk in batch])
                if len(result) != len(batch):
                    raise RuntimeError("text restore embedding batch is incomplete")
                for vector in result:
                    if (
                        not isinstance(vector, list) or not vector
                        or any(not isinstance(value, (int, float)) or not math.isfinite(value)
                               for value in vector)
                    ):
                        raise RuntimeError("text restore embedding vector is invalid")
                    vectors.append(vector)
            dimension = len(vectors[0])
            expected_dimension = int(dataset.get("embedding_dimension") or 0)
            if any(len(vector) != dimension for vector in vectors) or (
                expected_dimension > 0 and dimension != expected_dimension
            ):
                raise RuntimeError("text restore embedding dimension differs")
        finally:
            await embedder.close()

        rows: list[dict[str, Any]] = []
        points: list[PointStruct] = []
        ids: list[str] = []
        payload_keys = (
            "source_type", "citation_text", "source_reference", "section_title",
            "section_full_path", "page_number", "chunk_index", "paragraph_index",
            "source_document", "document_title", "madhab", "language",
        )
        for position, (chunk, vector) in enumerate(zip(chunks, vectors, strict=True)):
            segment_id = self._candidate_id(
                dataset_id, document_id, generation, position,
            )
            metadata = dict(chunk.metadata or {})
            metadata["position"] = position
            display_text = metadata.pop("original_text", chunk.text)
            content_hash = hashlib.sha256(
                str(display_text or chunk.text).encode("utf-8")
            ).hexdigest()
            payload_meta = {
                key: metadata[key] for key in payload_keys if metadata.get(key) is not None
            }
            points.append(PointStruct(
                id=segment_id, vector=vector,
                payload={
                    "tenant_id": tenant_id, "dataset_id": dataset_id,
                    "document_id": document_id, "segment_id": segment_id,
                    "position": position, "text": chunk.text,
                    "token_count": chunk.token_count,
                    "source_type": payload_meta.get("source_type", "unknown"),
                    "language": payload_meta.get("language", "en"),
                    "metadata": payload_meta,
                    "citation_text": payload_meta.get("citation_text"),
                    "source_reference": payload_meta.get("source_reference"),
                    "content_type": "text", "enabled": True,
                },
            ))
            rows.append({
                "segment_id": segment_id, "dataset_id": dataset_id,
                "document_id": document_id, "position": position,
                "text": display_text, "token_count": chunk.token_count,
                "vector_id": segment_id, "content_hash": content_hash,
                "content_type": "text", "metadata": metadata,
                "enabled": False, "status": "indexing",
                "index_node_id": f"{document_id}::text::{position}",
                "index_node_hash": content_hash,
            })
            ids.append(segment_id)
        return TextRestoreCandidatePlan(
            generation_id=generation, document_id=document_id,
            dataset_id=dataset_id, points_by_collection={collection: points},
            segment_rows=rows, content=text,
            source_hash=hashlib.sha256(text.encode("utf-8")).hexdigest(),
            segment_ids=ids,
        )
