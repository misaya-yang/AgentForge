"""MinerU 3.x hybrid-engine backend adapter (PRD T4 item 2 candidate default).

License note: MinerU ships under an Apache-2.0 variant with extra weight
conditions — legal review is a PRD §10 gate before it can go into a
multi-tenant default path; registering it as an *optional* stage is fine.

The adapter is transport-agnostic: pass any callable
``client(job) -> dict`` that returns one page of MinerU output.  The expected
payload shape (MinerU 3.x content-list style, normalised):

    {
      "width": float, "height": float,
      "blocks": [
        {"type": "text|title|table|formula|image|list|code|caption",
         "text": str,             # content for text-ish blocks
         "html": str,             # tables (kept dual-stored in the IR)
         "latex": str,            # formulas
         "image": {"ref": str, "mime": str, "description": str},
         "bbox": [x0, y0, x1, y1], "order": int, "score": float}
      ]
    }
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from ..base import PageJob, ParserUnavailable
from ..ir import BBox, Block, BlockType, FigureContent, FormulaContent, PageIR, TableContent

MINERU_TYPE_MAP: dict[str, BlockType] = {
    "text": BlockType.TEXT,
    "title": BlockType.HEADING,
    "heading": BlockType.HEADING,
    "table": BlockType.TABLE,
    "formula": BlockType.FORMULA,
    "equation": BlockType.FORMULA,
    "image": BlockType.FIGURE,
    "figure": BlockType.FIGURE,
    "list": BlockType.LIST,
    "code": BlockType.CODE,
    "caption": BlockType.CAPTION,
}


def mineru_payload_to_page_ir(payload: dict[str, Any], *, backend: str, version: str) -> PageIR:
    """Convert a MinerU page payload into IR.  Unknown types become UNKNOWN."""
    blocks: list[Block] = []
    for i, raw in enumerate(payload.get("blocks") or []):
        btype = MINERU_TYPE_MAP.get(str(raw.get("type", "text")).lower(), BlockType.UNKNOWN)
        order = int(raw.get("order", i))
        block = Block(
            block_id=f"mineru-{order}",
            type=btype,
            text=str(raw.get("text") or ""),
            order=order,
            bbox=BBox.from_list(raw.get("bbox")),
            parser=backend,
            parser_version=version,
            confidence=raw.get("score"),
        )
        if btype is BlockType.TABLE:
            md = str(raw.get("text") or raw.get("markdown") or "")
            html = raw.get("html")
            block.table = TableContent(
                markdown=md,
                html=html,
                n_rows=int(raw.get("n_rows", _count_md_rows(md))),
                n_cols=int(raw.get("n_cols", 0)),
                column_semantics=list(raw.get("column_semantics") or []),
                has_merged_cells=bool(
                    raw.get("has_merged_cells", False)
                    or "<td colspan" in (html or "")
                    or "<td rowspan" in (html or "")
                ),
                summary=raw.get("summary"),
            )
        elif btype is BlockType.FORMULA:
            block.formula = FormulaContent(
                latex=str(raw.get("latex") or raw.get("text") or ""),
                display=bool(raw.get("display", True)),
            )
        elif btype is BlockType.FIGURE:
            img = raw.get("image") or {}
            block.figure = FigureContent(
                image_ref=img.get("ref") or raw.get("image_ref"),
                mime=img.get("mime"),
                description=img.get("description") or raw.get("text") or None,
            )
            block.text = raw.get("text") or block.figure.description or ""
        blocks.append(block)
    return PageIR(
        page_number=int(payload.get("page_number", 0) or 0),
        blocks=blocks,
        parser=backend,
        parser_version=version,
        page_width=payload.get("width"),
        page_height=payload.get("height"),
        confidence=payload.get("score"),
    )


def _count_md_rows(markdown: str) -> int:
    return sum(1 for ln in (markdown or "").splitlines() if ln.strip().startswith("|"))


class MinerUBackend:
    """MinerU 3.x client adapter; requires an injected page-parse callable."""

    name = "mineru"
    version = "3.0"

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
            raise ParserUnavailable("mineru backend has no client configured")
        payload = self._client(job)
        page = mineru_payload_to_page_ir(payload, backend=self.name, version=self.version)
        page.page_number = job.page_number
        return page
