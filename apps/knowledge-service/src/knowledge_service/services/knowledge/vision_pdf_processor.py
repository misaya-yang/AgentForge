"""
Vision PDF Processor

Page-as-image processing for scanned PDFs using multimodal embedding.
"""

from __future__ import annotations

import asyncio
import hashlib
import logging
import uuid
from collections.abc import Awaitable, Callable
from dataclasses import dataclass
from typing import Any

from qdrant_client.http import models as qmodels

logger = logging.getLogger(__name__)


@dataclass
class ProcessingResult:
    success: bool
    total_pages: int
    processed_pages: int
    failed_pages: int
    segments_created: int = 0
    error: str | None = None
    extracted_texts: dict[int, str] | None = None  # page_number -> OCR text
    segment_ids: list[str] | None = None


@dataclass
class VisionCandidatePlan:
    """Unpublished page images and vectors for one durable execution."""

    generation_id: str
    document_id: str
    dataset_id: str
    points_by_collection: dict[str, list[qmodels.PointStruct]]
    segment_rows: list[dict[str, Any]]
    object_manifest: list[dict[str, Any]]
    content: str
    source_hash: str
    segment_ids: list[str]
    total_pages: int
    processed_pages: int
    failed_pages: int
    extracted_texts: dict[int, str]


class VisionPDFProcessor:
    """
    Render PDF pages to images and embed each page using a multimodal embedder.
    """

    def __init__(
        self,
        embedder: Any,
        vector_store: Any,
        database: Any,
        render_dpi: int = 200,
        max_image_bytes: int = 3 * 1024 * 1024,
        batch_size: int = 8,
        position_offset: int = 1_000_000,
    ) -> None:
        self.embedder = embedder
        self.vector_store = vector_store
        self.db = database
        self.render_dpi = render_dpi
        self.max_image_bytes = max_image_bytes
        self.batch_size = batch_size
        self.position_offset = position_offset

    @staticmethod
    def _get_fitz():
        try:
            import pymupdf as fitz  # type: ignore
        except ImportError:
            import fitz  # type: ignore
        return fitz

    @staticmethod
    def _candidate_generation_id(generation_id: str) -> str:
        try:
            return str(uuid.UUID(str(generation_id)))
        except (TypeError, ValueError, AttributeError) as exc:
            raise ValueError("generation_id must be a durable UUID") from exc

    @staticmethod
    def _candidate_attachment_id(page_number: int, generation_id: str) -> str:
        return f"page_{page_number}_g{uuid.UUID(generation_id).hex}"

    async def planned_object_keys(
        self,
        pdf_bytes: bytes,
        document_id: str,
        *,
        generation_id: str,
        tenant_id: str,
        storage_service: Any,
        page_offset: int = 0,
    ) -> list[str]:
        """Enumerate every candidate page key before any image is uploaded."""

        generation = self._candidate_generation_id(generation_id)
        if not document_id or not tenant_id or page_offset < 0:
            raise ValueError("document, tenant and nonnegative page_offset are required")
        generate_key = getattr(storage_service, "_generate_key", None)
        if not callable(generate_key):
            raise RuntimeError("candidate image storage keys are unavailable")
        total_pages = await asyncio.to_thread(self._pdf_page_count, pdf_bytes)
        if total_pages <= 0:
            raise ValueError("scanned candidate has no pages")
        keys = [
            str(generate_key(
                tenant_id, document_id,
                self._candidate_attachment_id(page_offset + page_index + 1, generation),
                f"page_{page_offset + page_index + 1}.png",
            ))
            for page_index in range(total_pages)
        ]
        if any(not key.strip() for key in keys) or len(set(keys)) != total_pages:
            raise RuntimeError("candidate image storage keys are invalid")
        return keys

    def _render_page(self, page: Any) -> tuple[bytes | None, tuple[int, int] | None]:
        """Render a PDF page to PNG bytes, shrinking if needed to fit size limits."""
        fitz = self._get_fitz()
        dpi_candidates = [self.render_dpi, 150, 120]
        for dpi in dpi_candidates:
            try:
                pix = page.get_pixmap(dpi=dpi, alpha=False)
                img_bytes = pix.tobytes("png")
                if len(img_bytes) <= self.max_image_bytes:
                    return img_bytes, (pix.width, pix.height)
            except Exception as exc:
                logger.debug(f"Render failed at dpi={dpi}: {exc}")
        # Final attempt: scale down aggressively
        try:
            mat = fitz.Matrix(1.0, 1.0)
            pix = page.get_pixmap(matrix=mat, alpha=False)
            img_bytes = pix.tobytes("png")
            if len(img_bytes) <= self.max_image_bytes:
                return img_bytes, (pix.width, pix.height)
        except BaseException as exc:
            logger.debug(f"Render failed at low scale: {exc}")
        return None, None

    def _pdf_page_count(self, pdf_bytes: bytes) -> int:
        """Open, inspect, and close a PDF within one worker thread."""
        doc = self._get_fitz().open(stream=pdf_bytes, filetype="pdf")
        try:
            return len(doc)
        finally:
            doc.close()

    def _render_page_batch(
        self,
        pdf_bytes: bytes,
        start: int,
        stop: int,
    ) -> list[tuple[int, bytes | None, tuple[int, int] | None]]:
        """Render a bounded page batch without sharing PyMuPDF objects."""
        doc = self._get_fitz().open(stream=pdf_bytes, filetype="pdf")
        try:
            return [
                (page_index, *self._render_page(doc[page_index]))
                for page_index in range(start, min(stop, len(doc)))
            ]
        finally:
            doc.close()

    async def process(
        self,
        pdf_bytes: bytes,
        document_id: str,
        dataset_id: str,
        collection: str,
        on_progress: Callable[[int, int], Awaitable[None]] | None = None,
        storage_service: Any | None = None,
        tenant_id: str = "default",
        text_extractor: Callable[[bytes], Awaitable[str]] | None = None,
        page_offset: int = 0,
    ) -> ProcessingResult:
        mutated_point_ids: set[str] = set()
        original_point_snapshots: dict[str, Any] = {}
        existing_segments: list[dict[str, Any]] = []
        try:
            # PyMuPDF does not support sharing Document/Page objects across
            # worker threads. Each bounded render batch opens, uses, and closes
            # its own document entirely inside one worker.
            total_pages = await asyncio.to_thread(self._pdf_page_count, pdf_bytes)
            processed_pages = 0
            failed_pages = 0
            segments_created = 0
            extracted_texts: dict[int, str] = {}
            published_segment_ids: list[str] = []

            get_existing = getattr(
                self.db,
                "get_image_segments_by_document",
                None,
            )
            existing_segments = (
                await get_existing(document_id)
                if callable(get_existing)
                else []
            )
            existing_ids_by_position = {
                int(segment.get("position") or 0): str(
                    segment.get("segment_id") or ""
                ).strip()
                for segment in existing_segments
                if str(segment.get("segment_id") or "").strip()
            }
            planned_ids = [
                existing_ids_by_position.get(
                    self.position_offset + page_offset + page_index
                )
                or str(
                    uuid.uuid5(
                        uuid.NAMESPACE_URL,
                        "ai-platform:kb-segment:"
                        f"{document_id}:image:"
                        f"{self.position_offset + page_offset + page_index}",
                    )
                )
                for page_index in range(total_pages)
            ]
            snapshot_points = getattr(self.vector_store, "snapshot_points", None)
            existing_point_ids = {
                str(
                    segment.get("vector_id")
                    or segment.get("segment_id")
                    or ""
                ).strip()
                for segment in existing_segments
                if str(
                    segment.get("vector_id")
                    or segment.get("segment_id")
                    or ""
                ).strip()
            }
            overwritten_ids = sorted(existing_point_ids.intersection(planned_ids))
            if overwritten_ids and not callable(snapshot_points):
                raise RuntimeError(
                    "scanned image replacement requires vector rollback snapshots"
                )
            if callable(snapshot_points):
                original_point_snapshots = await snapshot_points(
                    collection,
                    planned_ids,
                    tenant_id=tenant_id,
                    dataset_id=dataset_id,
                )

            from qdrant_client.http import models as qmodels

            batch_images: list[bytes] = []
            batch_meta: list[dict] = []

            async def flush_batch() -> None:
                nonlocal segments_created
                if not batch_images:
                    return
                vectors = await self.embedder.embed_images(batch_images)
                if not vectors:
                    return
                points = []
                for idx, vector in enumerate(vectors):
                    meta = batch_meta[idx]
                    seg_id = meta["segment_id"]
                    payload = {
                        "tenant_id": tenant_id,
                        "dataset_id": dataset_id,
                        "document_id": document_id,
                        "segment_id": seg_id,
                        "position": meta["position"],
                        "text": meta["text"],
                        "content_type": "image",
                        "image_id": meta["image_id"],
                        "image_mime_type": meta["image_mime_type"],
                        "image_width": meta["image_width"],
                        "image_height": meta["image_height"],
                        "image_page": meta["page_number"],
                    }
                    points.append(qmodels.PointStruct(id=seg_id, vector=vector, payload=payload))

                await self.vector_store.upsert(collection_name=collection, points=points)
                mutated_point_ids.update(str(point.id) for point in points)

                segment_rows = [
                    {
                            "segment_id": meta["segment_id"],
                            "document_id": document_id,
                            "dataset_id": dataset_id,
                            "position": meta["position"],
                            "text": meta["text"],
                            "content_type": "image",
                            "image_url": meta.get("image_url"),
                            "image_attachment_id": meta.get("image_attachment_id"),
                            "image_filename": meta.get("image_filename"),
                            "image_media_type": meta.get("image_media_type"),
                            "image_file_size": meta.get("image_file_size"),
                            "vector_id": meta["segment_id"],
                            "metadata": {
                                "page_number": meta["page_number"],
                                "width": meta["image_width"],
                                "height": meta["image_height"],
                                "source_position": meta["source_position"],
                            },
                        }
                    for meta in batch_meta
                ]
                store_batch = getattr(self.db, "store_image_segments", None)
                if callable(store_batch):
                    await store_batch(segment_rows)
                else:
                    for row in segment_rows:
                        await self.db.save_image_segment(row)
                published_segment_ids.extend(
                    str(meta["segment_id"]) for meta in batch_meta
                )
                segments_created += len(batch_meta)
                batch_images.clear()
                batch_meta.clear()

            for batch_start in range(0, total_pages, self.batch_size):
                rendered_pages = await asyncio.to_thread(
                    self._render_page_batch,
                    pdf_bytes,
                    batch_start,
                    batch_start + self.batch_size,
                )
                for page_index, img_bytes, dims in rendered_pages:
                    if not img_bytes or not dims:
                        failed_pages += 1
                        continue

                    width, height = dims
                    global_page_index = page_offset + page_index
                    page_number = global_page_index + 1

                    # Extract text via VLM OCR callback (reuses rendered image)
                    if text_extractor and img_bytes:
                        try:
                            page_text = await text_extractor(img_bytes)
                            if page_text:
                                extracted_texts[page_number] = page_text
                        except Exception as exc:
                            logger.debug(
                                f"Text extraction failed for page {page_number}: {exc}"
                            )

                    image_url = None
                    image_attachment_id = None
                    image_filename = None
                    image_media_type = "image/png"
                    image_file_size = len(img_bytes)

                    if storage_service:
                        try:
                            image_attachment_id = f"page_{page_number}"
                            image_filename = f"page_{page_number}.png"
                            image_url = await storage_service.upload_image(
                                tenant_id=tenant_id,
                                document_id=document_id,
                                attachment_id=image_attachment_id,
                                filename=image_filename,
                                content=img_bytes,
                                content_type=image_media_type,
                                metadata={
                                    "width": str(width),
                                    "height": str(height),
                                    "page_number": str(page_number),
                                },
                            )
                        except Exception as exc:
                            logger.warning(
                                f"Failed to upload page image {page_number}: {exc}"
                            )

                    batch_images.append(img_bytes)
                    position = self.position_offset + global_page_index
                    segment_id = existing_ids_by_position.get(position) or str(
                        uuid.uuid5(
                            uuid.NAMESPACE_URL,
                            f"ai-platform:kb-segment:{document_id}:image:{position}",
                        )
                    )
                    batch_meta.append(
                        {
                            "segment_id": segment_id,
                            "position": position,
                            "text": f"[Page {page_number}]",
                            "image_id": f"{document_id}_page_{page_number}",
                            "image_mime_type": image_media_type,
                            "image_width": width,
                            "image_height": height,
                            "page_number": page_number,
                            "source_position": global_page_index,
                            "image_url": image_url,
                            "image_attachment_id": image_attachment_id,
                            "image_filename": image_filename,
                            "image_media_type": image_media_type,
                            "image_file_size": image_file_size,
                        }
                    )

                    processed_pages += 1
                    if on_progress:
                        await on_progress(processed_pages, total_pages)

                    if len(batch_images) >= self.batch_size:
                        await flush_batch()

            await flush_batch()

            return ProcessingResult(
                success=True,
                total_pages=total_pages,
                processed_pages=processed_pages,
                failed_pages=failed_pages,
                segments_created=segments_created,
                extracted_texts=extracted_texts if extracted_texts else None,
                segment_ids=published_segment_ids,
            )
        except Exception as exc:
            rollback_failures: list[str] = []
            if mutated_point_ids:
                try:
                    snapshots = [
                        point
                        for point_id, point in original_point_snapshots.items()
                        if point_id in mutated_point_ids
                    ]
                    if snapshots:
                        await self.vector_store.upsert(
                            collection_name=collection,
                            points=snapshots,
                        )
                    new_point_ids = sorted(
                        mutated_point_ids - set(original_point_snapshots)
                    )
                    if new_point_ids:
                        await self.vector_store.delete_points(
                            collection,
                            new_point_ids,
                            tenant_id=tenant_id,
                            dataset_id=dataset_id,
                        )
                except Exception:
                    rollback_failures.append("qdrant")
                    logger.exception("Failed to restore scanned image points")
                try:
                    store_batch = getattr(self.db, "store_image_segments", None)
                    if existing_segments and callable(store_batch):
                        await store_batch(existing_segments)
                    original_segment_ids = {
                        str(segment.get("segment_id") or "").strip()
                        for segment in existing_segments
                        if str(segment.get("segment_id") or "").strip()
                    }
                    delete_segment = getattr(self.db, "delete_segment", None)
                    new_segment_ids = sorted(
                        mutated_point_ids - original_segment_ids
                    )
                    if new_segment_ids and not callable(delete_segment):
                        raise RuntimeError("segment rollback delete is unavailable")
                    for segment_id in new_segment_ids:
                        await delete_segment(segment_id)
                except Exception:
                    rollback_failures.append("postgres")
                    logger.exception("Failed to restore scanned image rows")
            suffix = (
                f"; incomplete rollback: {','.join(rollback_failures)}"
                if rollback_failures
                else ""
            )
            if not isinstance(exc, Exception):
                raise
            logger.error(f"VisionPDFProcessor failed: {exc}")
            return ProcessingResult(
                success=False,
                total_pages=0,
                processed_pages=0,
                failed_pages=0,
                error=f"{exc}{suffix}",
            )

    async def prepare_document(
        self,
        pdf_bytes: bytes,
        document_id: str,
        dataset_id: str,
        collection: str,
        *,
        generation_id: str,
        tenant_id: str,
        storage_service: Any,
        text_extractor: Callable[[bytes], Awaitable[str]] | None = None,
        pinned_page_texts: dict[int, str] | None = None,
        page_offset: int = 0,
        on_progress: Callable[[int, int], Awaitable[None]] | None = None,
    ) -> VisionCandidatePlan:
        """Prepare a complete page generation without publishing old point IDs.

        Image objects use the durable execution ID in their storage key, so a
        failed or interrupted candidate cannot replace the serving image bytes.
        The caller persists the returned manifest and publishes the points and
        rows under one cross-store index publication fence.
        """

        generation = self._candidate_generation_id(generation_id)
        if not all((document_id, dataset_id, collection, tenant_id)) or page_offset < 0:
            raise ValueError("document, dataset, collection, tenant and nonnegative page_offset are required")
        upload_image = getattr(storage_service, "upload_image", None)
        if not callable(upload_image):
            raise RuntimeError("candidate image object storage is unavailable")
        image_exists = getattr(storage_service, "image_exists", None)
        download_image = getattr(storage_service, "download_image", None)
        if not callable(image_exists) or not callable(download_image):
            raise RuntimeError("candidate image object storage must support read-back")
        from .ingestion_service import _require_extracted_text_budget

        total_pages = await asyncio.to_thread(self._pdf_page_count, pdf_bytes)
        if total_pages <= 0:
            raise ValueError("scanned candidate has no pages")
        if pinned_page_texts is not None:
            expected_numbers = set(range(page_offset + 1, page_offset + total_pages + 1))
            if (
                not isinstance(pinned_page_texts, dict)
                or any(type(number) is not int or not isinstance(value, str)
                       for number, value in pinned_page_texts.items())
                or set(pinned_page_texts) != expected_numbers
            ):
                raise ValueError("pinned page texts must cover exactly the rendered pages")
            _require_extracted_text_budget("\n\n".join(
                f"[Page {number}]\n{pinned_page_texts[number]}"
                for number in sorted(pinned_page_texts) if pinned_page_texts[number]
            ))
        points: list[qmodels.PointStruct] = []
        segment_rows: list[dict[str, Any]] = []
        object_manifest: list[dict[str, Any]] = []
        extracted_texts: dict[int, str] = {}
        processed_pages = 0

        try:
            for batch_start in range(0, total_pages, self.batch_size):
                rendered = await asyncio.to_thread(
                    self._render_page_batch,
                    pdf_bytes,
                    batch_start,
                    min(batch_start + self.batch_size, total_pages),
                )
                expected_pages = range(batch_start, min(batch_start + self.batch_size, total_pages))
                if [page_index for page_index, _image, _dims in rendered] != list(expected_pages):
                    raise RuntimeError("scanned candidate render batch is incomplete")
                batch_images: list[bytes] = []
                batch_meta: list[dict[str, Any]] = []
                for page_index, image_bytes, dimensions in rendered:
                    if not image_bytes or not dimensions:
                        raise RuntimeError(f"scanned candidate page {page_index + 1} could not render")
                    page_number = page_offset + page_index + 1
                    if pinned_page_texts is not None:
                        page_text = pinned_page_texts[page_number]
                        if page_text:
                            extracted_texts[page_number] = page_text
                    elif text_extractor is not None:
                        page_text = await text_extractor(image_bytes)
                        if page_text:
                            extracted_texts[page_number] = _require_extracted_text_budget(page_text)

                    attachment_id = self._candidate_attachment_id(page_number, generation)
                    filename = f"page_{page_number}.png"
                    object_identity = {
                        "tenant_id": tenant_id,
                        "document_id": document_id,
                        "attachment_id": attachment_id,
                        "filename": filename,
                    }
                    image_hash = hashlib.sha256(image_bytes).hexdigest()
                    already_exists = await image_exists(**object_identity)
                    if already_exists:
                        existing_bytes = await download_image(**object_identity)
                        if hashlib.sha256(existing_bytes).hexdigest() != image_hash:
                            raise RuntimeError(
                                f"scanned candidate page {page_number} generation object differs"
                            )
                        get_image_url = getattr(storage_service, "get_image_url", None)
                        image_url = (
                            get_image_url(**object_identity)
                            if callable(get_image_url) else None
                        )
                    else:
                        image_url = None
                    if not image_url:
                        image_url = await upload_image(
                            **object_identity,
                            content=image_bytes,
                            content_type="image/png",
                            metadata={
                                "width": str(dimensions[0]),
                                "height": str(dimensions[1]),
                                "page_number": str(page_number),
                                "generation_id": generation,
                            },
                        )
                    if not str(image_url or "").strip():
                        raise RuntimeError(f"scanned candidate page {page_number} upload returned no URL")
                    stored_bytes = await download_image(**object_identity)
                    if hashlib.sha256(stored_bytes).hexdigest() != image_hash:
                        raise RuntimeError(
                            f"scanned candidate page {page_number} stored object differs"
                        )
                    object_receipt = {
                        "tenant_id": tenant_id,
                        "document_id": document_id,
                        "attachment_id": attachment_id,
                        "filename": filename,
                        "storage_url": image_url,
                        "generation_id": generation,
                        "sha256": image_hash,
                    }
                    generate_key = getattr(storage_service, "_generate_key", None)
                    if callable(generate_key):
                        object_receipt["storage_key"] = generate_key(
                            tenant_id, document_id, attachment_id, filename
                        )
                    object_manifest.append(object_receipt)
                    position = self.position_offset + page_offset + page_index
                    segment_id = str(
                        uuid.uuid5(
                            uuid.NAMESPACE_URL,
                            "ai-platform:kb-image-candidate:"
                            f"{dataset_id}:{document_id}:{generation}:{position}",
                        )
                    )
                    batch_images.append(image_bytes)
                    batch_meta.append({
                        "segment_id": segment_id,
                        "position": position,
                        "page_number": page_number,
                        "width": dimensions[0],
                        "height": dimensions[1],
                        "image_url": image_url,
                        "attachment_id": attachment_id,
                        "filename": filename,
                        "image_file_size": len(image_bytes),
                        "text": (
                            f"[Page {page_number}]\n{extracted_texts[page_number]}"
                            if page_number in extracted_texts else f"[Page {page_number}]"
                        ),
                    })
                    processed_pages += 1
                    if on_progress is not None:
                        await on_progress(processed_pages, total_pages)

                vectors = await self.embedder.embed_images(batch_images)
                if len(vectors) != len(batch_meta) or any(vector is None for vector in vectors):
                    raise RuntimeError("scanned candidate image embedding is incomplete")
                for meta, vector in zip(batch_meta, vectors, strict=True):
                    segment_id = meta["segment_id"]
                    page_number = meta["page_number"]
                    points.append(qmodels.PointStruct(
                        id=segment_id,
                        vector=vector,
                        payload={
                            "tenant_id": tenant_id,
                            "dataset_id": dataset_id,
                            "document_id": document_id,
                            "segment_id": segment_id,
                            "position": meta["position"],
                            "text": meta["text"],
                            "content_type": "image",
                            "image_id": f"{document_id}_page_{page_number}",
                            "image_mime_type": "image/png",
                            "image_width": meta["width"],
                            "image_height": meta["height"],
                            "image_page": page_number,
                            "generation_id": generation,
                        },
                    ))
                    segment_rows.append({
                        "segment_id": segment_id,
                        "document_id": document_id,
                        "dataset_id": dataset_id,
                        "position": meta["position"],
                        "text": meta["text"],
                        "content_type": "image",
                        "image_url": meta["image_url"],
                        "image_attachment_id": meta["attachment_id"],
                        "image_filename": meta["filename"],
                        "image_media_type": "image/png",
                        "image_file_size": meta["image_file_size"],
                        "vector_id": segment_id,
                        "enabled": False,
                        "status": "indexing",
                        "metadata": {
                            "page_number": page_number,
                            "width": meta["width"],
                            "height": meta["height"],
                            "source_position": meta["position"] - self.position_offset,
                            "generation_id": generation,
                        },
                    })
        except Exception:
            # A prior attempt may have durably recorded this generation. The
            # publisher/GC owns orphan cleanup after checking its manifest.
            raise

        content = "\n\n".join(
            f"[Page {page_number}]\n{extracted_texts[page_number]}"
            for page_number in sorted(extracted_texts)
        )
        _require_extracted_text_budget(content)
        return VisionCandidatePlan(
            generation_id=generation,
            document_id=document_id,
            dataset_id=dataset_id,
            points_by_collection={collection: points},
            segment_rows=segment_rows,
            object_manifest=object_manifest,
            content=content,
            source_hash=hashlib.sha256(pdf_bytes).hexdigest(),
            segment_ids=[str(point.id) for point in points],
            total_pages=total_pages,
            processed_pages=processed_pages,
            failed_pages=0,
            extracted_texts=extracted_texts,
        )
