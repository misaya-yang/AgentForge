"""
Document Type Detector - Intelligent document classification

Automatically detects document type and recommends the optimal processing mode:
- native_pdf: Digital PDF with text layer -> text_only
- scanned_pdf: Scanned PDF (image-only) -> scanned
- mixed_pdf: Mixed content (some pages text, some images) -> multimodal
- Other formats (docx, txt, markdown, html, image)

Detection is performed by sampling the first few pages to minimize I/O.
"""

import asyncio
import contextlib
import os
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from ...core.observability.logging import get_logger
from .processing_mode import ProcessingMode

logger = get_logger(__name__)

DocumentContent = bytes | str | os.PathLike[str]


class DocumentType(str, Enum):
    """Detected document types."""

    NATIVE_PDF = "native_pdf"  # Digital PDF with text layer
    SCANNED_PDF = "scanned_pdf"  # Scanned PDF (pure images)
    MIXED_PDF = "mixed_pdf"  # Mixed: some pages text, some images
    DOCX = "docx"  # Microsoft Word
    TXT = "txt"  # Plain text
    MARKDOWN = "markdown"  # Markdown
    HTML = "html"  # HTML
    IMAGE = "image"  # Image file (jpg, png, etc.)
    UNKNOWN = "unknown"  # Unknown type


@dataclass
class DetectionResult:
    """Result of document type detection."""

    document_type: DocumentType
    recommended_mode: ProcessingMode
    confidence: float  # 0.0 - 1.0
    file_size: int
    page_count: int | None = None
    has_images: bool = False
    text_coverage: float = 0.0  # 0-1, ratio of pages with text
    is_large_file: bool = False  # > 50MB
    sample_pages_checked: int = 0
    details: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "document_type": self.document_type.value,
            "recommended_mode": self.recommended_mode.value,
            "confidence": self.confidence,
            "file_size": self.file_size,
            "page_count": self.page_count,
            "has_images": self.has_images,
            "text_coverage": self.text_coverage,
            "is_large_file": self.is_large_file,
            "sample_pages_checked": self.sample_pages_checked,
            "details": self.details,
        }


class DocumentTypeDetector:
    """
    Intelligent document type detector.

    Analyzes document content to determine optimal processing mode.
    Uses sampling (first N pages) for large files to minimize I/O.
    """

    # Threshold defaults (mirror KnowledgeSettings)
    DEFAULT_LARGE_FILE_THRESHOLD = 50 * 1024 * 1024  # 50MB
    DEFAULT_SAMPLE_PAGES = 5

    # Text coverage thresholds for PDF classification
    DEFAULT_NATIVE_PDF_THRESHOLD = 0.8
    DEFAULT_SCANNED_PDF_THRESHOLD = 0.2

    # Minimum characters per page to consider it "has text"
    DEFAULT_MIN_CHARS_PER_PAGE = 50

    # Maximum content size for memory processing
    DEFAULT_MAX_MEMORY_SIZE = 100 * 1024 * 1024  # 100MB

    def __init__(self, knowledge_settings: Any | None = None):
        """Initialize detector."""
        self._fitz = None
        ks = knowledge_settings or {}
        self.large_file_threshold = getattr(
            ks, "large_file_threshold", self.DEFAULT_LARGE_FILE_THRESHOLD
        )
        self.sample_pages = getattr(ks, "detection_sample_pages", self.DEFAULT_SAMPLE_PAGES)
        self.native_pdf_threshold = getattr(
            ks, "detection_native_pdf_threshold", self.DEFAULT_NATIVE_PDF_THRESHOLD
        )
        self.scanned_pdf_threshold = getattr(
            ks, "detection_scanned_pdf_threshold", self.DEFAULT_SCANNED_PDF_THRESHOLD
        )
        self.min_chars_per_page = getattr(
            ks, "detection_min_chars_per_page", self.DEFAULT_MIN_CHARS_PER_PAGE
        )
        self.max_memory_size = getattr(
            ks, "max_memory_processing_size", self.DEFAULT_MAX_MEMORY_SIZE
        )

    def _get_fitz(self):
        """Lazy load PyMuPDF."""
        if self._fitz is None:
            try:
                import pymupdf as fitz

                self._fitz = fitz
            except ImportError:
                try:
                    import fitz  # type: ignore

                    self._fitz = fitz
                except ImportError as exc:
                    raise ImportError(
                        "PyMuPDF is required for document type detection. "
                        "Install with: pip install 'ai-gateway[documents]' or pip install pymupdf"
                    ) from exc
        return self._fitz

    async def detect(
        self,
        content: DocumentContent,
        filename: str | None = None,
        mime_type: str | None = None,
    ) -> DetectionResult:
        """
        Detect document type and recommend processing mode.

        Args:
            content: File content as bytes or file path
            filename: Original filename (for extension-based detection)
            mime_type: MIME type if known

        Returns:
            DetectionResult with type, recommended mode, and metadata
        """
        file_size = self._resolve_file_size(content)

        # FIX: Check content size for memory-based processing
        if isinstance(content, bytes) and len(content) > self.max_memory_size:
            raise ValueError(
                f"Content too large for memory processing ({len(content)} bytes). "
                f"Maximum allowed: {self.max_memory_size} bytes. Use file path instead."
            )

        is_large = file_size > self.large_file_threshold

        # Normalize filename
        name = (filename or "").strip().lower()
        mime = (mime_type or "").strip().lower()

        # Detect by extension/mime type first
        if name.endswith(".pdf") or "application/pdf" in mime:
            return await self._detect_pdf(content, file_size, is_large)

        elif name.endswith(".docx") or "officedocument.wordprocessing" in mime:
            return self._create_result(
                doc_type=DocumentType.DOCX,
                mode=ProcessingMode.TEXT_ONLY,
                confidence=0.95,
                file_size=file_size,
                is_large=is_large,
            )

        elif name.endswith(".txt") or "text/plain" in mime:
            return self._create_result(
                doc_type=DocumentType.TXT,
                mode=ProcessingMode.TEXT_ONLY,
                confidence=0.99,
                file_size=file_size,
                is_large=is_large,
            )

        elif name.endswith(".md") or name.endswith(".markdown"):
            return self._create_result(
                doc_type=DocumentType.MARKDOWN,
                mode=ProcessingMode.TEXT_ONLY,
                confidence=0.99,
                file_size=file_size,
                is_large=is_large,
            )

        elif name.endswith(".html") or name.endswith(".htm") or "text/html" in mime:
            return self._create_result(
                doc_type=DocumentType.HTML,
                mode=ProcessingMode.TEXT_ONLY,
                confidence=0.95,
                file_size=file_size,
                is_large=is_large,
            )

        elif any(name.endswith(ext) for ext in [".jpg", ".jpeg", ".png", ".gif", ".webp", ".bmp"]):
            return self._create_result(
                doc_type=DocumentType.IMAGE,
                mode=ProcessingMode.SCANNED,
                confidence=0.99,
                file_size=file_size,
                is_large=is_large,
            )

        else:
            # Try to detect PDF by magic bytes
            if self._is_pdf_magic(content):
                return await self._detect_pdf(content, file_size, is_large)

            # Unknown type, default to text processing
            return self._create_result(
                doc_type=DocumentType.UNKNOWN,
                mode=ProcessingMode.TEXT_ONLY,
                confidence=0.5,
                file_size=file_size,
                is_large=is_large,
            )

    async def _detect_pdf(
        self,
        content: DocumentContent,
        file_size: int,
        is_large: bool,
    ) -> DetectionResult:
        """
        Detect PDF type by analyzing page content.

        Samples first N pages to determine if PDF is:
        - Native (has text layer)
        - Scanned (image-only)
        - Mixed (some pages text, some images)

        The fitz page sampling is CPU-bound, so it runs off the event loop
        (T7 provisioning reliability): a large upload must not stall other
        requests sharing this worker.
        """
        return await asyncio.to_thread(self._detect_pdf_sync, content, file_size, is_large)

    def _detect_pdf_sync(
        self,
        content: DocumentContent,
        file_size: int,
        is_large: bool,
    ) -> DetectionResult:
        """Synchronous PDF sampling; call via ``_detect_pdf`` only."""
        fitz = self._get_fitz()
        doc = None

        try:
            # Open PDF document
            if isinstance(content, bytes):
                doc = fitz.open(stream=content, filetype="pdf")
            else:
                doc = fitz.open(os.fspath(content))

            total_pages = len(doc)
            pages_to_check = min(self.sample_pages, total_pages)

            pages_with_text = 0
            pages_with_images = 0
            total_chars = 0

            # Sample pages (first N + some from middle/end for better coverage)
            sample_indices = self._get_sample_indices(total_pages, pages_to_check)

            for page_idx in sample_indices:
                page = doc[page_idx]

                # Check text content
                text = page.get_text("text").strip()
                char_count = len(text)
                total_chars += char_count

                if char_count >= self.min_chars_per_page:
                    pages_with_text += 1

                # Check for images
                image_list = page.get_images()
                if image_list:
                    pages_with_images += 1

            # Calculate metrics
            text_coverage = pages_with_text / pages_to_check if pages_to_check > 0 else 0
            has_images = pages_with_images > 0

            # Determine type and mode
            if text_coverage >= self.native_pdf_threshold:
                doc_type = DocumentType.NATIVE_PDF
                mode = ProcessingMode.MULTIMODAL if has_images else ProcessingMode.TEXT_ONLY
                confidence = 0.9
            elif text_coverage <= self.scanned_pdf_threshold:
                doc_type = DocumentType.SCANNED_PDF
                mode = ProcessingMode.SCANNED
                confidence = 0.9
            else:
                doc_type = DocumentType.MIXED_PDF
                mode = ProcessingMode.MULTIMODAL
                confidence = 0.8

            return DetectionResult(
                document_type=doc_type,
                recommended_mode=mode,
                confidence=confidence,
                file_size=file_size,
                page_count=total_pages,
                has_images=has_images,
                text_coverage=text_coverage,
                is_large_file=is_large,
                sample_pages_checked=pages_to_check,
                details={
                    "pages_with_text": pages_with_text,
                    "pages_with_images": pages_with_images,
                    "total_chars_sampled": total_chars,
                    "sample_indices": sample_indices,
                },
            )

        except Exception as e:
            logger.error(f"Error during PDF detection: {e}")
            return self._create_result(
                doc_type=DocumentType.UNKNOWN,
                mode=ProcessingMode.TEXT_ONLY,
                confidence=0.3,
                file_size=file_size,
                is_large=is_large,
            )
        finally:
            # FIX: Ensure document is always closed in finally block
            if doc:
                with contextlib.suppress(Exception):
                    doc.close()

    def _get_sample_indices(self, total_pages: int, sample_count: int) -> list[int]:
        """
        Get page indices to sample for detection.

        Samples from beginning, middle, and end for better coverage.
        """
        if total_pages <= sample_count:
            return list(range(total_pages))

        indices = []

        # First few pages
        first_count = min(3, sample_count)
        indices.extend(range(first_count))

        remaining = sample_count - first_count
        if remaining > 0:
            # Middle page
            middle = total_pages // 2
            if middle not in indices:
                indices.append(middle)
                remaining -= 1

            # Last few pages
            if remaining > 0:
                for i in range(1, remaining + 1):
                    idx = total_pages - i
                    if idx not in indices and idx >= 0:
                        indices.append(idx)

        return sorted(indices)

    def _create_result(
        self,
        doc_type: DocumentType,
        mode: ProcessingMode,
        confidence: float,
        file_size: int,
        is_large: bool,
        **kwargs,
    ) -> DetectionResult:
        """Create a detection result with given parameters."""
        return DetectionResult(
            document_type=doc_type,
            recommended_mode=mode,
            confidence=confidence,
            file_size=file_size,
            is_large_file=is_large,
            **kwargs,
        )

    @staticmethod
    def _resolve_file_size(content: DocumentContent) -> int:
        if isinstance(content, bytes):
            return len(content)
        try:
            return os.path.getsize(os.fspath(content))
        except OSError:
            return 0

    @staticmethod
    def _is_pdf_magic(content: DocumentContent) -> bool:
        if isinstance(content, bytes):
            return content[:4] == b"%PDF"
        try:
            with open(os.fspath(content), "rb") as handle:
                return handle.read(4) == b"%PDF"
        except OSError:
            return False

    @staticmethod
    def get_mode_display_name(mode: ProcessingMode) -> str:
        """Get human-readable name for processing mode."""
        names = {
            ProcessingMode.TEXT_ONLY: "Pure Text (OCR + Text Embedding)",
            ProcessingMode.SCANNED: "Scanned Document (Vision Embedding)",
            ProcessingMode.MULTIMODAL: "Multimodal (Text + Image Embedding)",
        }
        return names.get(mode, str(mode.value))
