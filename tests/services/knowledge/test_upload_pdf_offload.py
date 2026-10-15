"""T7 PDF upload memory and event-loop safety contracts.

Pins two contracts:

1. ``_split_pdf_parts_to_temp_sync`` slices a path-backed source into bounded
   temporary files, enforces page/output budgets, and cleans partial output.
   The upload route never reads the whole source on the split path and only
   materializes one part at a time.
2. ``DocumentTypeDetector._detect_pdf`` — the async entry delegates the fitz
   page sampling to ``_detect_pdf_sync`` via ``asyncio.to_thread``, accepts a
   filesystem path, and preserves the existing detection verdicts.
"""

from __future__ import annotations

import asyncio
import io
import tempfile
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

fitz = pytest.importorskip("pymupdf")

from fastapi import HTTPException, UploadFile  # noqa: E402
from knowledge_service.api.routes import knowledge as routes  # noqa: E402
from knowledge_service.services.knowledge.document_detector import (  # noqa: E402
    DocumentType,
    DocumentTypeDetector,
)
from knowledge_service.services.knowledge.processing_mode import (  # noqa: E402
    ProcessingMode,
)


def _make_pdf(pages: int, *, with_text: bool = True) -> bytes:
    doc = fitz.open()
    for i in range(pages):
        page = doc.new_page()
        if with_text:
            # Well above the detector's min_chars_per_page (50).
            page.insert_text((72, 72), f"marker page {i + 1} " * 10)
    data = doc.tobytes()
    doc.close()
    return data


def _upload_settings(**overrides: Any) -> SimpleNamespace:
    values = {
        "kb_max_file_size_mb": 48,
        "kb_pdf_split_max_size_mb": 0,
        "kb_pdf_split_pages_per_part": 2,
        "kb_pdf_max_pages": 2_000,
        "kb_pdf_split_max_output_bytes": 96 * 1024 * 1024,
    }
    values.update(overrides)
    return SimpleNamespace(**values)


def _patch_upload_guards(
    monkeypatch: pytest.MonkeyPatch,
    enqueued: list[str] | None = None,
) -> None:
    async def fake_editor(_svc: Any, _user: Any, dataset_id: str) -> dict[str, Any]:
        return {"dataset_id": dataset_id, "index_config": {}}

    async def fake_enqueue(_worker: Any, _dataset_id: str, document_id: str) -> None:
        if enqueued is not None:
            enqueued.append(document_id)

    monkeypatch.setattr(routes, "_require_authenticated_dataset_editor", fake_editor)
    monkeypatch.setattr(routes, "_require_dataset_index_writable", lambda _dataset: None)
    monkeypatch.setattr(routes, "_enqueue_document_or_conflict", fake_enqueue)


# ---------------------------------------------------------------- splitter


def test_split_pdf_parts_to_temp_slices_pages_into_valid_parts(tmp_path: Path) -> None:
    source = tmp_path / "source.pdf"
    source.write_bytes(_make_pdf(5))

    total_pages, parts = routes._split_pdf_parts_to_temp_sync(
        source,
        2,
        max_pages=100,
        max_output_bytes=10 * 1024 * 1024,
        temp_dir=str(tmp_path),
    )

    assert total_pages == 5
    assert [(part.first_page, part.last_page) for part in parts] == [
        (1, 2),
        (3, 4),
        (5, 5),
    ]
    for split_part in parts:
        part = fitz.open(split_part.path)
        try:
            assert len(part) == split_part.last_page - split_part.first_page + 1
            # The source pages' text layer survives the split.
            assert f"page {split_part.first_page}" in part[0].get_text("text")
        finally:
            part.close()


def test_split_pdf_parts_to_temp_single_part_when_below_page_window(tmp_path: Path) -> None:
    source = tmp_path / "source.pdf"
    source.write_bytes(_make_pdf(3))

    total_pages, parts = routes._split_pdf_parts_to_temp_sync(
        source,
        500,
        max_pages=100,
        max_output_bytes=10 * 1024 * 1024,
        temp_dir=str(tmp_path),
    )

    assert total_pages == 3
    assert [(part.first_page, part.last_page) for part in parts] == [(1, 3)]


def test_split_pdf_rejects_high_page_count_before_writing_parts(tmp_path: Path) -> None:
    source = tmp_path / "source.pdf"
    source.write_bytes(_make_pdf(6, with_text=False))

    with pytest.raises(routes._PDFSplitLimitExceeded, match="6 pages"):
        routes._split_pdf_parts_to_temp_sync(
            source,
            2,
            max_pages=5,
            max_output_bytes=10 * 1024 * 1024,
            temp_dir=str(tmp_path),
        )

    assert list(tmp_path.glob("kb_pdf_part_*")) == []


def test_split_pdf_rejects_expanded_output_and_cleans_parts(tmp_path: Path) -> None:
    source = tmp_path / "source.pdf"
    source.write_bytes(_make_pdf(3))

    with pytest.raises(routes._PDFSplitLimitExceeded, match="cumulative safety limit"):
        routes._split_pdf_parts_to_temp_sync(
            source,
            1,
            max_pages=100,
            max_output_bytes=1,
            temp_dir=str(tmp_path),
        )

    assert list(tmp_path.glob("kb_pdf_part_*")) == []


def test_split_pdf_cleans_current_part_on_unexpected_failure(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    source = tmp_path / "source.pdf"
    source.write_bytes(_make_pdf(3))
    real_stat = Path.stat

    def failing_part_stat(path: Path, *args: Any, **kwargs: Any):
        if path.name.startswith("kb_pdf_part_"):
            raise OSError("injected stat failure")
        return real_stat(path, *args, **kwargs)

    monkeypatch.setattr(Path, "stat", failing_part_stat)

    with pytest.raises(routes._PDFSplitError, match="could not be split safely"):
        routes._split_pdf_parts_to_temp_sync(
            source,
            1,
            max_pages=100,
            max_output_bytes=10 * 1024 * 1024,
            temp_dir=str(tmp_path),
        )

    assert list(tmp_path.glob("kb_pdf_part_*")) == []


# ------------------------------------- upload route offloads the CPU work


@pytest.mark.asyncio
async def test_upload_route_offloads_file_read_and_pdf_split(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Detection/split/part reads run off-loop; the source is never read whole."""
    to_thread_targets: list[Any] = []
    read_paths: list[Path] = []
    real_to_thread = asyncio.to_thread
    real_read_bytes = Path.read_bytes

    async def recording_to_thread(fn, *args, **kwargs):
        to_thread_targets.append(fn)
        return await real_to_thread(fn, *args, **kwargs)

    monkeypatch.setattr(asyncio, "to_thread", recording_to_thread)

    def recording_read_bytes(path: Path) -> bytes:
        read_paths.append(path)
        return real_read_bytes(path)

    monkeypatch.setattr(Path, "read_bytes", recording_read_bytes)

    enqueued: list[str] = []
    _patch_upload_guards(monkeypatch, enqueued)

    created: list[dict[str, Any]] = []

    class FakeService:
        async def create_document_from_upload(self, _user, _dataset_id, **kwargs):
            doc = {
                "document_id": f"doc-{len(created)}",
                "filename": kwargs["filename"],
                "size_bytes": len(kwargs["content_bytes"]),
            }
            created.append(doc)
            return doc

    upload = UploadFile(file=io.BytesIO(_make_pdf(5)), filename="big.pdf")

    result = await routes.upload_document(
        "dataset-a",
        file=upload,
        processing_mode="auto",
        svc=FakeService(),
        worker=object(),
        user=object(),
        settings=_upload_settings(),
    )

    assert result["status"] == "split_and_queued"
    assert result["total_pages"] == 5
    assert result["parts"] == 3
    assert len(created) == 3
    assert enqueued == [doc["document_id"] for doc in created]
    # Part naming keeps the 1-based inclusive page range of each part.
    assert created[0]["filename"] == "big_Part_1_p1-2.pdf"
    assert created[2]["filename"] == "big_Part_3_p5-5.pdf"
    assert routes._split_pdf_parts_to_temp_sync in to_thread_targets
    assert any(getattr(fn, "__name__", "") == "_detect_pdf_sync" for fn in to_thread_targets)
    assert len(read_paths) == 3
    assert all(path.name.startswith("kb_pdf_part_") for path in read_paths)
    assert all(not path.exists() for path in read_paths)


@pytest.mark.asyncio
async def test_upload_route_returns_413_for_high_page_pdf_and_cleans_workspace(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(tempfile, "tempdir", str(tmp_path))
    _patch_upload_guards(monkeypatch)

    class RejectUnexpectedCreate:
        async def create_document_from_upload(self, *_args: Any, **_kwargs: Any):
            raise AssertionError("high-page PDF must be rejected before publication")

    upload = UploadFile(file=io.BytesIO(_make_pdf(3)), filename="many-pages.pdf")
    with pytest.raises(HTTPException) as exc_info:
        await routes.upload_document(
            "dataset-a",
            file=upload,
            processing_mode="auto",
            svc=RejectUnexpectedCreate(),
            worker=object(),
            user=object(),
            settings=_upload_settings(kb_pdf_max_pages=2),
        )

    assert exc_info.value.status_code == 413
    assert "3 pages" in str(exc_info.value.detail)
    assert list(tmp_path.iterdir()) == []


@pytest.mark.asyncio
async def test_upload_route_keeps_compressed_upload_limit_as_413(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(tempfile, "tempdir", str(tmp_path))
    _patch_upload_guards(monkeypatch)

    class RejectUnexpectedCreate:
        async def create_document_from_upload(self, *_args: Any, **_kwargs: Any):
            raise AssertionError("oversized upload must be rejected before publication")

    upload = UploadFile(
        file=io.BytesIO(b"x" * (1024 * 1024 + 1)),
        filename="oversized.txt",
    )
    with pytest.raises(HTTPException) as exc_info:
        await routes.upload_document(
            "dataset-a",
            file=upload,
            processing_mode="auto",
            svc=RejectUnexpectedCreate(),
            worker=object(),
            user=object(),
            settings=_upload_settings(kb_max_file_size_mb=1),
        )

    assert exc_info.value.status_code == 413
    assert "limit of 1MB" in str(exc_info.value.detail)
    assert list(tmp_path.iterdir()) == []


@pytest.mark.asyncio
async def test_upload_route_returns_413_for_split_output_budget_and_cleans_workspace(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(tempfile, "tempdir", str(tmp_path))
    _patch_upload_guards(monkeypatch)

    class RejectUnexpectedCreate:
        async def create_document_from_upload(self, *_args: Any, **_kwargs: Any):
            raise AssertionError("over-budget split must be rejected before publication")

    upload = UploadFile(file=io.BytesIO(_make_pdf(3)), filename="expanded.pdf")
    with pytest.raises(HTTPException) as exc_info:
        await routes.upload_document(
            "dataset-a",
            file=upload,
            processing_mode="auto",
            svc=RejectUnexpectedCreate(),
            worker=object(),
            user=object(),
            settings=_upload_settings(kb_pdf_split_max_output_bytes=1),
        )

    assert exc_info.value.status_code == 413
    assert "cumulative safety limit" in str(exc_info.value.detail)
    assert list(tmp_path.iterdir()) == []


@pytest.mark.asyncio
async def test_upload_route_cleans_all_temp_files_when_part_publication_fails(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    monkeypatch.setattr(tempfile, "tempdir", str(tmp_path))
    _patch_upload_guards(monkeypatch)

    class FailingService:
        async def create_document_from_upload(self, *_args: Any, **_kwargs: Any):
            raise RuntimeError("injected storage failure")

    upload = UploadFile(file=io.BytesIO(_make_pdf(5)), filename="failure.pdf")
    with pytest.raises(RuntimeError, match="injected storage failure"):
        await routes.upload_document(
            "dataset-a",
            file=upload,
            processing_mode="auto",
            svc=FailingService(),
            worker=object(),
            user=object(),
            settings=_upload_settings(),
        )

    assert list(tmp_path.iterdir()) == []


# ------------------------------------------------------------- detector


def test_detect_pdf_sync_classifies_native_scanned_and_degrades_on_corrupt() -> None:
    detector = DocumentTypeDetector()

    native = detector._detect_pdf_sync(_make_pdf(3), file_size=100, is_large=False)
    assert native.document_type == DocumentType.NATIVE_PDF
    assert native.recommended_mode == ProcessingMode.TEXT_ONLY
    assert native.page_count == 3

    blank = detector._detect_pdf_sync(
        _make_pdf(3, with_text=False), file_size=100, is_large=False
    )
    assert blank.document_type == DocumentType.SCANNED_PDF
    assert blank.recommended_mode == ProcessingMode.SCANNED

    corrupt = detector._detect_pdf_sync(b"not a pdf", file_size=7, is_large=False)
    assert corrupt.document_type == DocumentType.UNKNOWN
    assert corrupt.confidence <= 0.3


@pytest.mark.asyncio
async def test_detect_pdf_delegates_sampling_off_event_loop(
    monkeypatch: pytest.MonkeyPatch,
    tmp_path: Path,
) -> None:
    detector = DocumentTypeDetector()
    to_thread_targets: list[Any] = []
    real_to_thread = asyncio.to_thread

    async def recording_to_thread(fn, *args, **kwargs):
        to_thread_targets.append(fn)
        return await real_to_thread(fn, *args, **kwargs)

    monkeypatch.setattr(asyncio, "to_thread", recording_to_thread)

    pdf_path = tmp_path / "detector.pdf"
    pdf_path.write_bytes(_make_pdf(1))
    result = await detector._detect_pdf(
        pdf_path,
        file_size=pdf_path.stat().st_size,
        is_large=False,
    )

    assert result.document_type == DocumentType.NATIVE_PDF
    assert any(
        getattr(fn, "__name__", "") == "_detect_pdf_sync" for fn in to_thread_targets
    )
