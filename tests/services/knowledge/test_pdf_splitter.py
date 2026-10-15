"""Tests for PDFSplitter."""

from __future__ import annotations

import pytest
from knowledge_service.services.knowledge.pdf_splitter import PDFSplitter, SplitResult


def _fitz_or_skip():
    try:
        import pymupdf as fitz
    except Exception as pymupdf_exc:  # noqa: BLE001
        try:
            import fitz
        except Exception as fitz_exc:  # noqa: BLE001
            pytest.skip(f"PyMuPDF is not loadable in this environment: {fitz_exc or pymupdf_exc}")
    return fitz


@pytest.fixture
def splitter():
    return PDFSplitter(max_size_bytes=20 * 1024 * 1024, min_pages_per_part=5)


@pytest.fixture
def small_splitter():
    """Splitter with very small max size for testing."""
    return PDFSplitter(max_size_bytes=1024, min_pages_per_part=2)


class TestSplitResult:
    def test_dataclass_frozen(self):
        sr = SplitResult(
            part_index=0, page_start=0, page_end=10, path="/tmp/test.pdf", size_bytes=1000
        )
        assert sr.part_index == 0
        assert sr.page_start == 0
        assert sr.page_end == 10
        with pytest.raises(AttributeError):
            sr.part_index = 1


class TestPDFSplitter:
    def test_init_defaults(self):
        s = PDFSplitter()
        assert s.max_size_bytes == 20 * 1024 * 1024
        assert s.min_pages_per_part == 5

    def test_init_custom(self):
        s = PDFSplitter(max_size_bytes=10_000, min_pages_per_part=3)
        assert s.max_size_bytes == 10_000
        assert s.min_pages_per_part == 3

    def test_min_pages_clamped_to_1(self):
        s = PDFSplitter(min_pages_per_part=0)
        assert s.min_pages_per_part == 1

    def test_small_file_returns_original_path(self, splitter, tmp_path):
        """Files smaller than max_size should return original path unchanged."""
        pdf_path = tmp_path / "small.pdf"
        # Create a minimal valid PDF
        fitz = _fitz_or_skip()

        doc = fitz.open()
        page = doc.new_page()
        page.insert_text((72, 72), "Hello World")
        doc.save(str(pdf_path))
        doc.close()

        results = splitter.split_pdf(str(pdf_path))

        assert len(results) == 1
        assert results[0].path == str(pdf_path)
        assert results[0].part_index == 0
        assert results[0].size_bytes == pdf_path.stat().st_size

    def test_split_produces_multiple_parts(self, tmp_path):
        """A large-enough PDF should be split into multiple parts."""
        fitz = _fitz_or_skip()

        # Create a multi-page PDF with enough content to exceed small limit
        pdf_path = tmp_path / "big.pdf"
        doc = fitz.open()
        for i in range(20):
            page = doc.new_page()
            # Add substantial text to increase file size
            for y in range(50, 750, 15):
                page.insert_text((72, y), f"Page {i + 1} line at y={y} " + "X" * 80)
        doc.save(str(pdf_path))
        doc.close()

        file_size = pdf_path.stat().st_size
        # Use a small max_size to force splitting
        splitter = PDFSplitter(max_size_bytes=file_size // 3, min_pages_per_part=2)
        results = splitter.split_pdf(str(pdf_path), tmp_dir=str(tmp_path))

        assert len(results) >= 2
        # Verify all pages are covered
        total_pages = sum(r.page_end - r.page_start for r in results)
        assert total_pages == 20
        # Verify parts are contiguous
        for i in range(1, len(results)):
            assert results[i].page_start == results[i - 1].page_end
        # Verify each part is a valid PDF
        for r in results:
            part_doc = fitz.open(r.path)
            assert len(part_doc) == r.page_end - r.page_start
            part_doc.close()

    def test_split_empty_pdf(self, tmp_path):
        """An empty PDF (1 blank page) should return single result."""
        fitz = _fitz_or_skip()

        pdf_path = tmp_path / "empty.pdf"
        doc = fitz.open()
        doc.new_page()  # PyMuPDF requires at least 1 page to save
        doc.save(str(pdf_path))
        doc.close()

        splitter = PDFSplitter(max_size_bytes=1024 * 1024)  # 1MB — larger than blank PDF
        results = splitter.split_pdf(str(pdf_path))
        assert len(results) == 1

    def test_split_single_page_pdf(self, tmp_path):
        """A single-page PDF should not be split even if large."""
        fitz = _fitz_or_skip()

        pdf_path = tmp_path / "single.pdf"
        doc = fitz.open()
        page = doc.new_page()
        page.insert_text((72, 72), "Single page content")
        doc.save(str(pdf_path))
        doc.close()

        splitter = PDFSplitter(max_size_bytes=100, min_pages_per_part=1)
        results = splitter.split_pdf(str(pdf_path), tmp_dir=str(tmp_path))
        # Should still produce result(s) even if oversized
        assert len(results) >= 1

    def test_part_indices_sequential(self, tmp_path):
        """Part indices should be sequential starting from 0."""
        fitz = _fitz_or_skip()

        pdf_path = tmp_path / "multi.pdf"
        doc = fitz.open()
        for i in range(10):
            page = doc.new_page()
            for y in range(50, 750, 15):
                page.insert_text((72, y), f"Page {i + 1} " + "X" * 100)
        doc.save(str(pdf_path))
        doc.close()

        file_size = pdf_path.stat().st_size
        splitter = PDFSplitter(max_size_bytes=file_size // 4, min_pages_per_part=2)
        results = splitter.split_pdf(str(pdf_path), tmp_dir=str(tmp_path))

        for i, r in enumerate(results):
            assert r.part_index == i
