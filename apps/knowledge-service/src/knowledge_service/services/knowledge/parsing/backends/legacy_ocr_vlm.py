"""Existing VLM/OCR path wrapped as a cascade backend (PRD: 现有 VLM/OCR 作为后端之一).

The adapter does not reimplement OCR: it takes an async callable
``ocr(image_bytes) -> str`` — typically built from
``ocr_utils.ocr_image_bytes_auto`` with the service's ``VLMOCRService`` — via
:func:`make_ocr_callable` or supplied directly in tests.
"""

from __future__ import annotations

from collections.abc import Awaitable, Callable
from typing import Any

from ..base import PageJob, ParserUnavailable
from ..ir import PageIR
from .text_layer import paragraph_blocks

OcrCallable = Callable[[bytes], Awaitable[str]]


class LegacyOCRVLMBackend:
    """OCR of the page raster through the current tesseract/VLM strategy."""

    name = "legacy_ocr_vlm"
    version = "1"

    def __init__(self, ocr: OcrCallable | None = None, *, confidence: float = 0.8) -> None:
        self._ocr = ocr
        self._confidence = confidence

    def is_available(self) -> bool:
        return self._ocr is not None

    def can_handle(self, job: PageJob) -> bool:
        return job.image_bytes is not None or job.signals.image_count > 0

    async def parse_page(self, job: PageJob) -> PageIR:
        if self._ocr is None:
            raise ParserUnavailable("legacy_ocr_vlm backend has no OCR callable configured")
        if job.image_bytes is None:
            raise ParserUnavailable("legacy_ocr_vlm needs page image bytes")
        text = await self._ocr(job.image_bytes)
        blocks = paragraph_blocks(text, page_number=job.page_number, parser=self.name, version=self.version)
        return PageIR(
            page_number=job.page_number,
            blocks=blocks,
            parser=self.name,
            parser_version=self.version,
            page_width=job.page_width,
            page_height=job.page_height,
            confidence=self._confidence if text.strip() else 0.0,
        )


def make_ocr_callable(
    *,
    ocr_strategy: str = "hybrid",
    vlm_ocr_service: Any | None = None,
    settings: Any | None = None,
) -> OcrCallable:
    """Bind the repo's existing OCR utilities into an async callable.

    Imports lazily so merely registering the backend never loads tesseract or
    the VLM client (import-safety rule for this package).
    """

    async def _ocr(image_bytes: bytes) -> str:
        from ...ocr_utils import OCRCConfig, ocr_image_bytes_auto

        config = OCRCConfig.from_settings(settings)
        return await ocr_image_bytes_auto(
            image_bytes,
            vlm_ocr_service=vlm_ocr_service,
            config=config,
            strategy=ocr_strategy,
        )

    return _ocr
