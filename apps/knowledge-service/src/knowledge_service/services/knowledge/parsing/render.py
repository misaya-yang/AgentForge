"""Derivations from the IR: markdown, Qdrant payloads, citation anchors.

PRD T4 item 1: "markdown 渲染自 IR；Qdrant payload 从 IR 派生；引用回溯到
页/区域".  Everything here is a pure function of the IR — chunking and
embedding remain the existing pipeline's job; only the *inputs* they read
come from here once T4 is wired in.
"""

from __future__ import annotations

from typing import Any

from .ir import Block, BlockType, DocIR, PageIR

_HEADING_MARKER = "#"


def render_block_markdown(block: Block) -> str:
    """Render one block to markdown.  Tables prefer stored markdown, fall back
    to a trivial HTML-strip note when only HTML exists (merged-cell case)."""
    if block.type is BlockType.HEADING:
        level = int(block.metadata.get("level", 1) or 1)
        level = min(max(level, 1), 6)
        return f"{_HEADING_MARKER * level} {block.text.strip()}"
    if block.type is BlockType.TABLE and block.table:
        if block.table.markdown:
            return block.table.markdown
        if block.table.html:
            # Merged-cell tables degrade markdown (PRD §3.2); keep a citation
            # placeholder so reading order survives; HTML stays in the IR.
            return block.table.summary or "[table]"
        return block.text
    if block.type is BlockType.FORMULA and block.formula:
        delim = "$$" if block.formula.display else "$"
        return f"{delim}{block.formula.latex}{delim}"
    if block.type is BlockType.FIGURE and block.figure:
        alt = (block.figure.description or block.text or "").replace("\n", " ").strip()
        ref = block.figure.image_ref or ""
        return f"![{alt}]({ref})"
    if block.type is BlockType.CODE:
        return f"```\n{block.text}\n```"
    return block.text


def render_page_markdown(page: PageIR) -> str:
    """Page markdown: blocks joined in reading order."""
    rendered = [render_block_markdown(b) for b in page.sorted_blocks()]
    return "\n\n".join(r for r in rendered if r.strip())


def render_document_markdown(doc: DocIR, *, page_markers: bool = False) -> str:
    """Document markdown from IR.  ``page_markers`` inserts ``[Page N]`` lines
    mirroring the current worker layout for backward-compatible rendering."""
    parts: list[str] = []
    for page in sorted(doc.pages, key=lambda p: p.page_number):
        body = render_page_markdown(page)
        if page_markers:
            parts.append(f"[Page {page.page_number}]\n{body}")
        else:
            parts.append(body)
    return "\n\n".join(p for p in parts if p.strip())


def citation_anchor(block: Block, page_number: int) -> dict[str, Any]:
    """Traceable citation to page/region (PRD: 引用回溯到页/区域)."""
    return {
        "block_id": block.block_id,
        "page_number": page_number,
        "bbox": block.bbox.to_dict() if block.bbox else None,
        "parser": block.parser,
        "parser_version": block.parser_version,
    }


def derive_chunk_payload(block: Block, page_number: int) -> dict[str, Any]:
    """Qdrant point payload derived from IR — the shape retrieval reads.

    Deliberately a superset of what current payloads carry (type/page/bbox/
    provenance); mapping onto existing payload field names is the integrator's
    seam, not this function's.
    """
    payload: dict[str, Any] = {
        "text": render_block_markdown(block),
        "block_type": block.type.value,
        "page_number": page_number,
        "citation": citation_anchor(block, page_number),
    }
    if block.type is BlockType.TABLE and block.table:
        payload["table_html"] = block.table.html
        payload["table_summary"] = block.table.summary
    if block.type is BlockType.FORMULA and block.formula:
        payload["formula_latex"] = block.formula.latex
    if block.confidence is not None:
        payload["confidence"] = block.confidence
    return payload


def iter_chunk_payloads(doc: DocIR) -> list[dict[str, Any]]:
    """All block payloads in global reading order — one pass for a bulk upsert."""
    out: list[dict[str, Any]] = []
    for page in sorted(doc.pages, key=lambda p: p.page_number):
        for block in page.sorted_blocks():
            out.append(derive_chunk_payload(block, page.page_number))
    return out
