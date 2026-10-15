"""General-VLM hard-page fallback backend (Qwen3-VL, PRD: 硬页回退层=通用 VLM).

A general VLM returns markdown (the model's best rendering of the page); this
module contains the structural markdown→IR block converter so even the
fallback tier feeds the same IR, not raw text.
"""

from __future__ import annotations

import re
from collections.abc import Awaitable, Callable

from ..base import PageJob, ParserUnavailable
from ..ir import Block, BlockType, FormulaContent, PageIR, TableContent

VlmCallable = Callable[[PageJob], Awaitable[str]]

_TABLE_LINE = re.compile(r"^\s*\|.*\|\s*$")
_FORMULA_DISPLAY = re.compile(r"\$\$(.+?)\$\$", re.DOTALL)
_IMAGE_MD = re.compile(r"!\[([^\]]*)\]\(([^)]*)\)")


def markdown_to_blocks(markdown: str, *, backend: str, version: str) -> list[Block]:
    """Split VLM markdown into typed IR blocks in reading order.

    Recognises headings (#), pipe tables, $$display$$ formulas, and image
    references; everything else falls back to paragraph TEXT blocks.
    """
    blocks: list[Block] = []
    order = 0

    def _mk(btype: BlockType, text: str, **kw) -> Block:
        nonlocal order
        b = Block(block_id=f"{backend}-{order}", type=btype, text=text.strip(), order=order, parser=backend, parser_version=version, **kw)
        order += 1
        return b

    lines = (markdown or "").splitlines()
    i = 0
    while i < len(lines):
        line = lines[i]
        stripped = line.strip()
        if not stripped:
            i += 1
            continue
        heading = re.match(r"^(#{1,6})\s+(.*)$", stripped)
        if heading:
            blocks.append(
                _mk(BlockType.HEADING, heading.group(2), metadata={"level": len(heading.group(1))})
            )
            i += 1
            continue
        if _TABLE_LINE.match(stripped):
            rows: list[str] = []
            while i < len(lines) and _TABLE_LINE.match(lines[i].strip()):
                rows.append(lines[i].strip())
                i += 1
            md_table = "\n".join(rows)
            table = TableContent(markdown=md_table, n_rows=sum(1 for r in rows if "-" * 2 not in r) - 1, n_cols=rows[0].count("|") - 1 if rows else 0)
            blocks.append(_mk(BlockType.TABLE, md_table, table=table))
            continue
        if stripped.startswith("$$"):
            buf = [stripped]
            while not _FORMULA_DISPLAY.search("\n".join(buf)) and i + 1 < len(lines):
                i += 1
                buf.append(lines[i].strip())
            match = _FORMULA_DISPLAY.search("\n".join(buf))
            latex = match.group(1).strip() if match else "\n".join(buf).strip("$ ")
            blocks.append(_mk(BlockType.FORMULA, latex, formula=FormulaContent(latex=latex, display=True)))
            i += 1
            continue
        # paragraph: gather until blank line or structural line
        para: list[str] = [stripped]
        while i + 1 < len(lines):
            nxt = lines[i + 1].strip()
            if (
                not nxt
                or re.match(r"^#{1,6}\s", nxt)
                or _TABLE_LINE.match(nxt)
                or nxt.startswith("$$")
            ):
                break
            para.append(nxt)
            i += 1
        text = "\n".join(para)
        img = _IMAGE_MD.search(text)
        if img:
            from ..ir import FigureContent

            blocks.append(
                _mk(
                    BlockType.FIGURE,
                    img.group(1),
                    figure=FigureContent(image_ref=img.group(2) or None, description=img.group(1) or None),
                )
            )
        else:
            blocks.append(_mk(BlockType.TEXT, text))
        i += 1
    return blocks


class GeneralVLMFallbackBackend:
    """Last-stage fallback: ask a general VLM (Qwen3-VL) to render the page."""

    name = "general_vlm_fallback"
    version = "qwen3-vl"

    def __init__(self, generate: VlmCallable | None = None, *, confidence: float = 0.7) -> None:
        self._generate = generate
        self._confidence = confidence

    def is_available(self) -> bool:
        return self._generate is not None

    def can_handle(self, job: PageJob) -> bool:
        return self._generate is not None and job.image_bytes is not None

    async def parse_page(self, job: PageJob) -> PageIR:
        if self._generate is None:
            raise ParserUnavailable("general_vlm_fallback has no generate callable configured")
        markdown = await self._generate(job)
        blocks = markdown_to_blocks(markdown, backend=self.name, version=self.version)
        return PageIR(
            page_number=job.page_number,
            blocks=blocks,
            parser=self.name,
            parser_version=self.version,
            page_width=job.page_width,
            page_height=job.page_height,
            confidence=self._confidence if blocks else 0.0,
        )
