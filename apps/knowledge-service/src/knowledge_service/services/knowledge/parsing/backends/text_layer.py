"""Text-layer backend: PDF text layer / DOCX extraction, no OCR.

Fastest and most faithful tier for born-digital pages (the existing
``_extract_pdf_text_sync`` / docx paths feed ``PageJob.text_layer``; an
optional injected ``extract`` callable handles job-level extraction when the
caller can't pre-extract).
"""

from __future__ import annotations

from collections.abc import Callable

from ..base import PageJob
from ..ir import Block, BlockType, PageIR


def paragraph_blocks(text: str, *, page_number: int, parser: str, version: str) -> list[Block]:
    """Split plain text into TEXT blocks on blank-line paragraph boundaries."""
    blocks: list[Block] = []
    order = 0
    for para in (p.strip() for p in (text or "").split("\n\n")):
        if not para:
            continue
        blocks.append(
            Block(
                block_id=f"p{page_number}b{order}",
                type=BlockType.TEXT,
                text=para,
                order=order,
                parser=parser,
                parser_version=version,
            )
        )
        order += 1
    return blocks


class TextLayerBackend:
    """Serves pages that carry a real text layer; reports 0.0 confidence otherwise
    so scanned pages escalate to the OCR/VLM stages."""

    name = "text_layer"
    version = "1"

    def __init__(
        self,
        extract: Callable[[PageJob], str] | None = None,
        *,
        confidence_floor_chars: int = 1,
        preserve_boundaries: bool = False,
    ) -> None:
        self._extract = extract
        self._floor = confidence_floor_chars
        self._preserve_boundaries = bool(preserve_boundaries)
        if self._preserve_boundaries:
            # Output-affecting options are part of the backend version.  This
            # prevents a legacy cache row from changing chunk byte boundaries.
            self.version = "1-boundary"

    def is_available(self) -> bool:
        return True  # pure function of the job's text layer

    def can_handle(self, job: PageJob) -> bool:
        return bool(job.text_layer.strip()) or self._extract is not None

    async def parse_page(self, job: PageJob) -> PageIR:
        text = job.text_layer or (self._extract(job) if self._extract else "")
        if self._preserve_boundaries and text:
            blocks = [
                Block(
                    block_id=f"p{job.page_number}b0",
                    type=BlockType.TEXT,
                    text=text,
                    order=0,
                    parser=self.name,
                    parser_version=self.version,
                )
            ]
        else:
            blocks = paragraph_blocks(
                text,
                page_number=job.page_number,
                parser=self.name,
                version=self.version,
            )
        stripped = text.strip()
        confidence = 1.0 if len(stripped) >= self._floor else 0.0
        return PageIR(
            page_number=job.page_number,
            blocks=blocks,
            parser=self.name,
            parser_version=self.version,
            page_width=job.page_width,
            page_height=job.page_height,
            confidence=confidence,
        )
