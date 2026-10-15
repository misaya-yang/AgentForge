"""Parsing intermediate representation (IR) — PRD T4 item 1 ("IR 先行").

A lossless, chunk-agnostic structural description of a parsed document:
document → pages → blocks.  Every block carries type, reading order, page
number, optional bbox, and the parser + parser version that produced it, so
that re-chunking never requires re-parsing (the H2 foundation) and every
citation can be traced back to a page/region.

Design rules (PRD §T4 / §3.2-parsing):

* Plain stdlib dataclasses only — import-safe, no side effects, no pydantic
  dependency, JSON round-trip is exact and deterministic.
* Binary attachments (page/figure images) do NOT live inside the IR; blocks
  reference them by ``image_ref`` keys owned by the existing document asset
  storage.  This keeps the Postgres ``jsonb`` payload small and stable.
* Markdown is *rendered from* the IR (see ``render.py``), never the reverse;
  the IR is the single source of truth.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

IR_SCHEMA_VERSION = "1"


class BlockType(str, Enum):
    """Semantic type of a content block (PRD: 块类型)."""

    TEXT = "text"
    HEADING = "heading"
    TABLE = "table"
    FORMULA = "formula"
    FIGURE = "figure"
    LIST = "list"
    CAPTION = "caption"
    CODE = "code"
    UNKNOWN = "unknown"


@dataclass(frozen=True)
class BBox:
    """Page-region bounding box in page coordinate units (PRD: bbox/区域引用)."""

    x0: float
    y0: float
    x1: float
    y1: float

    def __post_init__(self) -> None:
        # Normalise to float so JSON round-trips are byte-identical
        # (int 0 in, 0.0 out otherwise would break exact round-trip checks).
        for attr in ("x0", "y0", "x1", "y1"):
            object.__setattr__(self, attr, float(getattr(self, attr)))

    def to_dict(self) -> dict[str, float]:
        return {"x0": self.x0, "y0": self.y0, "x1": self.x1, "y1": self.y1}

    @classmethod
    def from_dict(cls, data: dict[str, Any] | None) -> BBox | None:
        if not data:
            return None
        return cls(
            x0=float(data["x0"]),
            y0=float(data["y0"]),
            x1=float(data["x1"]),
            y1=float(data["y1"]),
        )

    @classmethod
    def from_list(cls, data: Any) -> BBox | None:
        """Accept ``[x0, y0, x1, y1]`` (the shape most parsers emit)."""
        if not data or not isinstance(data, (list, tuple)) or len(data) < 4:
            return None
        return cls(x0=float(data[0]), y0=float(data[1]), x1=float(data[2]), y1=float(data[3]))


@dataclass
class TableContent:
    """Table payload with markdown + HTML dual storage (PRD T4 item 4)."""

    markdown: str = ""
    html: str | None = None
    n_rows: int = 0
    n_cols: int = 0
    header_rows: int = 1
    has_merged_cells: bool = False
    column_semantics: list[str] = field(default_factory=list)
    summary: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "markdown": self.markdown,
            "html": self.html,
            "n_rows": self.n_rows,
            "n_cols": self.n_cols,
            "header_rows": self.header_rows,
            "has_merged_cells": self.has_merged_cells,
            "column_semantics": list(self.column_semantics),
            "summary": self.summary,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any] | None) -> TableContent | None:
        if not data:
            return None
        return cls(
            markdown=data.get("markdown", ""),
            html=data.get("html"),
            n_rows=int(data.get("n_rows", 0)),
            n_cols=int(data.get("n_cols", 0)),
            header_rows=int(data.get("header_rows", 1)),
            has_merged_cells=bool(data.get("has_merged_cells", False)),
            column_semantics=list(data.get("column_semantics") or []),
            summary=data.get("summary"),
        )


@dataclass
class FormulaContent:
    """Formula payload: LaTeX + display/inline flag (PRD: 公式→LaTeX)."""

    latex: str = ""
    display: bool = True

    def to_dict(self) -> dict[str, Any]:
        return {"latex": self.latex, "display": self.display}

    @classmethod
    def from_dict(cls, data: dict[str, Any] | None) -> FormulaContent | None:
        if not data:
            return None
        return cls(latex=data.get("latex", ""), display=bool(data.get("display", True)))


@dataclass
class FigureContent:
    """Figure payload: reference to the stored image + VLM description (PRD: 图)."""

    image_ref: str | None = None
    mime: str | None = None
    description: str | None = None

    def to_dict(self) -> dict[str, Any]:
        return {"image_ref": self.image_ref, "mime": self.mime, "description": self.description}

    @classmethod
    def from_dict(cls, data: dict[str, Any] | None) -> FigureContent | None:
        if not data:
            return None
        return cls(
            image_ref=data.get("image_ref"),
            mime=data.get("mime"),
            description=data.get("description"),
        )


@dataclass
class Block:
    """One structural unit inside a page (paragraph, table, formula, ...)."""

    block_id: str
    type: BlockType = BlockType.TEXT
    text: str = ""
    order: int = 0  # reading order within the page (PRD: 阅读序)
    bbox: BBox | None = None
    parser: str = ""
    parser_version: str = ""
    confidence: float | None = None
    table: TableContent | None = None
    formula: FormulaContent | None = None
    figure: FigureContent | None = None
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "block_id": self.block_id,
            "type": self.type.value,
            "text": self.text,
            "order": self.order,
            "bbox": self.bbox.to_dict() if self.bbox else None,
            "parser": self.parser,
            "parser_version": self.parser_version,
            "confidence": self.confidence,
            "table": self.table.to_dict() if self.table else None,
            "formula": self.formula.to_dict() if self.formula else None,
            "figure": self.figure.to_dict() if self.figure else None,
            "metadata": dict(self.metadata),
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> Block:
        block_type = data.get("type", BlockType.TEXT.value)
        try:
            btype = BlockType(block_type)
        except ValueError:
            btype = BlockType.UNKNOWN
        return cls(
            block_id=data["block_id"],
            type=btype,
            text=data.get("text", ""),
            order=int(data.get("order", 0)),
            bbox=BBox.from_dict(data.get("bbox")),
            parser=data.get("parser", ""),
            parser_version=data.get("parser_version", ""),
            confidence=data.get("confidence"),
            table=TableContent.from_dict(data.get("table")),
            formula=FormulaContent.from_dict(data.get("formula")),
            figure=FigureContent.from_dict(data.get("figure")),
            metadata=dict(data.get("metadata") or {}),
        )


@dataclass
class PageIR:
    """Parsed structure of one page (PRD: 每页独立任务 → 页级 IR)."""

    page_number: int  # 1-indexed
    blocks: list[Block] = field(default_factory=list)
    parser: str = ""
    parser_version: str = ""
    page_width: float | None = None
    page_height: float | None = None
    confidence: float | None = None
    hard_page: bool = False  # set when the cascade exhausted its stages
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "page_number": self.page_number,
            "blocks": [b.to_dict() for b in self.blocks],
            "parser": self.parser,
            "parser_version": self.parser_version,
            "page_width": self.page_width,
            "page_height": self.page_height,
            "confidence": self.confidence,
            "hard_page": self.hard_page,
            "metadata": dict(self.metadata),
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> PageIR:
        return cls(
            page_number=int(data["page_number"]),
            blocks=[Block.from_dict(b) for b in data.get("blocks") or []],
            parser=data.get("parser", ""),
            parser_version=data.get("parser_version", ""),
            page_width=data.get("page_width"),
            page_height=data.get("page_height"),
            confidence=data.get("confidence"),
            hard_page=bool(data.get("hard_page", False)),
            metadata=dict(data.get("metadata") or {}),
        )

    def sorted_blocks(self) -> list[Block]:
        """Blocks in reading order (stable on equal ``order``)."""
        return sorted(self.blocks, key=lambda b: b.order)

    def page_text(self) -> str:
        """Concatenated plain text of the page, in reading order."""
        return "\n\n".join(b.text for b in self.sorted_blocks() if b.text)


@dataclass
class DocIR:
    """Document-level IR: ordered pages + provenance (PRD: 文档→页→块)."""

    doc_id: str
    content_hash: str = ""
    filename: str = ""
    mime: str | None = None
    schema_version: str = IR_SCHEMA_VERSION
    pages: list[PageIR] = field(default_factory=list)
    metadata: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "doc_id": self.doc_id,
            "content_hash": self.content_hash,
            "filename": self.filename,
            "mime": self.mime,
            "schema_version": self.schema_version,
            "pages": [p.to_dict() for p in self.pages],
            "metadata": dict(self.metadata),
        }

    def to_json(self, *, indent: int | None = None) -> str:
        return json.dumps(self.to_dict(), ensure_ascii=False, sort_keys=True, indent=indent)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> DocIR:
        return cls(
            doc_id=data["doc_id"],
            content_hash=data.get("content_hash", ""),
            filename=data.get("filename", ""),
            mime=data.get("mime"),
            schema_version=str(data.get("schema_version", IR_SCHEMA_VERSION)),
            pages=[PageIR.from_dict(p) for p in data.get("pages") or []],
            metadata=dict(data.get("metadata") or {}),
        )

    @classmethod
    def from_json(cls, text: str) -> DocIR:
        return cls.from_dict(json.loads(text))

    def page(self, page_number: int) -> PageIR | None:
        return next((p for p in self.pages if p.page_number == page_number), None)

    def iter_blocks(self) -> list[Block]:
        """All blocks in global reading order: page number, then in-page order."""
        out: list[Block] = []
        for p in sorted(self.pages, key=lambda pg: pg.page_number):
            out.extend(p.sorted_blocks())
        return out

    def stats(self) -> dict[str, Any]:
        """Lightweight summary used by ingestion receipts/logging."""
        blocks = self.iter_blocks()
        by_type: dict[str, int] = {}
        for b in blocks:
            by_type[b.type.value] = by_type.get(b.type.value, 0) + 1
        return {
            "pages": len(self.pages),
            "blocks": len(blocks),
            "by_type": by_type,
            "text_chars": sum(len(b.text) for b in blocks),
            "tables": sum(1 for b in blocks if b.type is BlockType.TABLE),
            "formulas": sum(1 for b in blocks if b.type is BlockType.FORMULA),
            "figures": sum(1 for b in blocks if b.type is BlockType.FIGURE),
            "hard_pages": sum(1 for p in self.pages if p.hard_page),
            "parsers": sorted({b.parser for b in blocks if b.parser} | {p.parser for p in self.pages if p.parser}),
        }
