"""
PDF Splitter for Large Scanned Documents.

Splits oversized PDFs into smaller parts (each ≤ max_size_bytes) using PyMuPDF,
so they can be processed within upload/memory limits.
"""

from __future__ import annotations

import logging
import tempfile
from dataclasses import dataclass
from pathlib import Path

logger = logging.getLogger(__name__)


@dataclass(frozen=True)
class SplitResult:
    """Result of a single PDF split part."""

    part_index: int
    page_start: int  # 0-based inclusive
    page_end: int  # 0-based exclusive
    path: str
    size_bytes: int


class PDFSplitter:
    """Split large PDF files into smaller parts that fit within size constraints."""

    def __init__(
        self,
        max_size_bytes: int = 20 * 1024 * 1024,
        min_pages_per_part: int = 5,
    ) -> None:
        self.max_size_bytes = max_size_bytes
        self.min_pages_per_part = max(1, min_pages_per_part)

    @staticmethod
    def _get_fitz():
        try:
            import pymupdf as fitz  # type: ignore
        except ImportError:
            import fitz  # type: ignore
        return fitz

    def split_pdf(
        self,
        source_path: str,
        tmp_dir: str | None = None,
    ) -> list[SplitResult]:
        """Split a PDF into parts each ≤ max_size_bytes.

        If the file is already small enough, returns a single SplitResult
        pointing at the original path (zero-copy).

        Args:
            source_path: Path to the source PDF file.
            tmp_dir: Directory for temporary split files. If None, uses system temp.

        Returns:
            List of SplitResult, one per part.
        """
        file_size = Path(source_path).stat().st_size
        if file_size <= self.max_size_bytes:
            return [
                SplitResult(
                    part_index=0,
                    page_start=0,
                    page_end=0,  # filled below
                    path=source_path,
                    size_bytes=file_size,
                )
            ]

        fitz = self._get_fitz()
        doc = fitz.open(source_path)
        total_pages = len(doc)

        if total_pages == 0:
            doc.close()
            return []

        # Update the zero-copy case page_end
        if file_size <= self.max_size_bytes:
            doc.close()
            return [
                SplitResult(
                    part_index=0,
                    page_start=0,
                    page_end=total_pages,
                    path=source_path,
                    size_bytes=file_size,
                )
            ]

        # Estimate pages per part based on average page size
        avg_page_size = file_size / total_pages
        est_pages_per_part = max(
            self.min_pages_per_part,
            int(self.max_size_bytes * 0.85 / avg_page_size),  # 85% safety margin
        )

        results: list[SplitResult] = []
        part_index = 0
        page_cursor = 0

        while page_cursor < total_pages:
            page_end = min(page_cursor + est_pages_per_part, total_pages)

            # Ensure minimum pages per part (except for the last part)
            if page_end - page_cursor < self.min_pages_per_part and page_end < total_pages:
                page_end = min(page_cursor + self.min_pages_per_part, total_pages)

            part_path = self._write_part(
                fitz, doc, page_cursor, page_end, part_index, tmp_dir
            )
            part_size = Path(part_path).stat().st_size

            # If the part is still too large and has more than min_pages, shrink it
            if part_size > self.max_size_bytes and (page_end - page_cursor) > self.min_pages_per_part:
                Path(part_path).unlink(missing_ok=True)
                # Binary search for the right number of pages
                page_end = self._find_max_pages(
                    fitz, doc, page_cursor, page_end, part_index, tmp_dir
                )
                part_path = self._write_part(
                    fitz, doc, page_cursor, page_end, part_index, tmp_dir
                )
                part_size = Path(part_path).stat().st_size

            results.append(
                SplitResult(
                    part_index=part_index,
                    page_start=page_cursor,
                    page_end=page_end,
                    path=part_path,
                    size_bytes=part_size,
                )
            )

            logger.info(
                f"[PDFSplitter] Part {part_index}: pages {page_cursor}-{page_end - 1}, "
                f"size={part_size / 1024 / 1024:.1f}MB"
            )

            page_cursor = page_end
            part_index += 1

        doc.close()
        return results

    def _write_part(
        self,
        fitz,
        source_doc,
        page_start: int,
        page_end: int,
        part_index: int,
        tmp_dir: str | None,
    ) -> str:
        """Write a range of pages to a new PDF file."""
        part_doc = fitz.open()
        part_doc.insert_pdf(source_doc, from_page=page_start, to_page=page_end - 1)

        suffix = f"_part{part_index:03d}.pdf"
        fd = tempfile.NamedTemporaryFile(
            delete=False, suffix=suffix, dir=tmp_dir, prefix="pdfsplit_"
        )
        part_path = fd.name
        fd.close()

        part_doc.save(part_path)
        part_doc.close()
        return part_path

    def _find_max_pages(
        self,
        fitz,
        source_doc,
        page_start: int,
        page_end_upper: int,
        part_index: int,
        tmp_dir: str | None,
    ) -> int:
        """Binary search for the maximum number of pages that fit within max_size_bytes."""
        lo = page_start + self.min_pages_per_part
        hi = page_end_upper

        best = lo
        while lo <= hi:
            mid = (lo + hi) // 2
            path = self._write_part(fitz, source_doc, page_start, mid, part_index, tmp_dir)
            size = Path(path).stat().st_size
            Path(path).unlink(missing_ok=True)

            if size <= self.max_size_bytes:
                best = mid
                lo = mid + 1
            else:
                hi = mid - 1

        return best
