"""PaddleOCR cascade backends — PP-StructureV3 (fast tier) and PaddleOCR-VL
(accuracy tier) (PRD T4 item 2 candidate default; pure Apache-2.0).

Same transport-agnostic pattern as the MinerU adapter: inject a callable
``client(job) -> dict`` returning per-page region detection output:

    {
      "regions": [
        {"label": "text|title|table|formula|figure|list|header|footer|...",
         "text": str, "html": str, "latex": str,
         "bbox": [x0, y0, x1, y1], "confidence": float}
      ],
      "width": float, "height": float
    }

Region order in the list is the engine's reading order.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from ..base import PageJob, ParserUnavailable
from ..ir import BBox, Block, BlockType, FormulaContent, PageIR, TableContent

PADDLE_LABEL_MAP: dict[str, BlockType] = {
    "text": BlockType.TEXT,
    "abstract": BlockType.TEXT,
    "content": BlockType.TEXT,
    "title": BlockType.HEADING,
    "paragraph_title": BlockType.HEADING,
    "table": BlockType.TABLE,
    "formula": BlockType.FORMULA,
    "display_formula": BlockType.FORMULA,
    "inline_formula": BlockType.FORMULA,
    "figure": BlockType.FIGURE,
    "image": BlockType.FIGURE,
    "chart": BlockType.FIGURE,
    "list": BlockType.LIST,
    "reference": BlockType.LIST,
    "table_caption": BlockType.CAPTION,
    "figure_caption": BlockType.CAPTION,
    "header": BlockType.TEXT,
    "footer": BlockType.TEXT,
}


def paddle_regions_to_page_ir(payload: dict[str, Any], *, backend: str, version: str) -> PageIR:
    """Convert PaddleOCR PP-StructureV3 / VL page output into IR blocks."""
    blocks: list[Block] = []
    for i, raw in enumerate(payload.get("regions") or []):
        label = str(raw.get("label", "text")).lower()
        btype = PADDLE_LABEL_MAP.get(label, BlockType.UNKNOWN)
        text = str(raw.get("text") or "")
        block = Block(
            block_id=f"{backend}-{i}",
            type=btype,
            text=text,
            order=i,
            bbox=BBox.from_list(raw.get("bbox")),
            parser=backend,
            parser_version=version,
            confidence=raw.get("confidence"),
        )
        if btype is BlockType.TABLE:
            html = raw.get("html")
            block.table = TableContent(
                markdown=text or str(raw.get("markdown") or ""),
                html=html,
                n_rows=int(raw.get("rows", 0)),
                n_cols=int(raw.get("cols", 0)),
                summary=raw.get("summary"),
            )
        elif btype is BlockType.FORMULA:
            block.formula = FormulaContent(
                latex=str(raw.get("latex") or text),
                display="inline" not in label,
            )
        blocks.append(block)
    return PageIR(
        page_number=int(payload.get("page_number", 0) or 0),
        blocks=blocks,
        parser=backend,
        parser_version=version,
        page_width=payload.get("width"),
        page_height=payload.get("height"),
    )


class _PaddleClientBackend:
    """Shared machinery: an injected client callable over Paddle page payloads."""

    name = "paddle"
    version = "1"

    def __init__(self, client: Callable[[PageJob], dict[str, Any]] | None = None, *, version: str | None = None) -> None:
        self._client = client
        if version:
            self.version = version

    def is_available(self) -> bool:
        return self._client is not None

    def can_handle(self, _job: PageJob) -> bool:
        return self._client is not None

    async def parse_page(self, job: PageJob) -> PageIR:
        if self._client is None:
            raise ParserUnavailable(f"{self.name} backend has no client configured")
        page = paddle_regions_to_page_ir(self._client(job), backend=self.name, version=self.version)
        page.page_number = job.page_number
        return page


class PPStructureV3Backend(_PaddleClientBackend):
    """Fast deterministic tier (Apache-2.0)."""

    name = "paddle_ppstructure_v3"
    version = "ppstructurev3-1"


class PaddleOCRVLBackend(_PaddleClientBackend):
    """Accuracy tier (PaddleOCR-VL-1.6, Apache-2.0)."""

    name = "paddleocr_vl"
    version = "paddleocr-vl-1.6"
