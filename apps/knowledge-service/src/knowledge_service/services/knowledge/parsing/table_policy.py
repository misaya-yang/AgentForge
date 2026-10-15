"""Table strategy from IR blocks (PRD §3.2 / T4 item 4).

Size-tiered splitting of table blocks:

* ≤ ``whole_max_rows`` (default 20): keep the table as one markdown block.
* ``whole_max_rows``–``segment_max_rows`` (default 100): split into segments
  with the header repeated on every segment, plus ``overlap_rows`` data rows
  carried over at boundaries.
* > ``segment_max_rows``: emit row-level blocks (header + one data row).

Tables stay dual-stored: markdown for rendering/embedding, HTML kept on the
original IR block (merged-cell fidelity for TEDS evaluation lives in the IR,
not the derived segments).  ``summary``/column semantics are generated
upstream (LLM pass) and copied onto every derived segment as retrieval
context.

NOTE (PRD §9 constraint): applying this changes chunk boundaries, so it is a
*mechanism only* — off by default, gated behind evaluation.  ``apply_table_policy``
is never called by the cascade itself.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum

from .ir import Block, BlockType, DocIR, TableContent

WHOLE_MAX_ROWS = 20
SEGMENT_MAX_ROWS = 100
DEFAULT_OVERLAP_ROWS = 2


class TableTier(str, Enum):
    WHOLE = "whole"
    SEGMENTED = "segmented"
    ROW_LEVEL = "row_level"


def classify_table(n_data_rows: int, *, whole_max: int = WHOLE_MAX_ROWS, segment_max: int = SEGMENT_MAX_ROWS) -> TableTier:
    if n_data_rows <= whole_max:
        return TableTier.WHOLE
    if n_data_rows <= segment_max:
        return TableTier.SEGMENTED
    return TableTier.ROW_LEVEL


def parse_markdown_rows(markdown: str) -> tuple[list[str], list[str]]:
    """Split markdown-table text into (header_lines, data_lines).

    Header lines = the leading ``|``-rows up to and including the ``|---|``
    separator; data lines = the remaining ``|``-rows.  A table with no
    separator is treated as headerless (all rows are data).
    """
    rows = [ln.strip() for ln in (markdown or "").splitlines() if ln.strip().startswith("|")]
    if not rows:
        return [], []

    def _is_separator(row: str) -> bool:
        compact = row.replace(" ", "")
        return "-" in compact and all(ch in "|-:" for ch in compact)

    sep_idx = next((k for k, r in enumerate(rows) if _is_separator(r)), None)
    if sep_idx is not None:
        return rows[: sep_idx + 1], rows[sep_idx + 1 :]
    return [], rows


def _render_table(header: list[str], data: list[str]) -> str:
    return "\n".join(header + data)


@dataclass
class TableSplitResult:
    tier: TableTier
    blocks: list[Block]


def split_table_block(
    block: Block,
    *,
    rows_per_segment: int = 20,
    overlap_rows: int = DEFAULT_OVERLAP_ROWS,
    whole_max: int = WHOLE_MAX_ROWS,
    segment_max: int = SEGMENT_MAX_ROWS,
) -> TableSplitResult:
    """Apply the size-tiered policy to one TABLE block.

    Returns the original block untouched when the policy does not apply
    (not a table, no usable markdown, or WHOLE tier).
    """
    if block.type is not BlockType.TABLE or not block.table or not block.table.markdown:
        return TableSplitResult(TableTier.WHOLE, [block])
    table = block.table
    header, data = parse_markdown_rows(table.markdown)
    tier = classify_table(len(data), whole_max=whole_max, segment_max=segment_max)
    if tier is TableTier.WHOLE:
        return TableSplitResult(tier, [block])

    derived: list[Block] = []
    if tier is TableTier.SEGMENTED:
        step = max(1, rows_per_segment - max(0, overlap_rows))
        for i, start in enumerate(range(0, len(data), step)):
            window = data[start : start + rows_per_segment]
            derived.append(_derive(block, _render_table(header, window), n_rows=len(window), seq=i))
    else:  # ROW_LEVEL
        for i, row in enumerate(data):
            derived.append(_derive(block, _render_table(header, [row]), n_rows=1, seq=i))
    return TableSplitResult(tier, derived)


def _derive(block: Block, markdown: str, *, n_rows: int, seq: int) -> Block:
    src = block.table
    assert src is not None  # caller guarantee
    new_table = TableContent(
        markdown=markdown,
        html=None,  # HTML stays on the parent IR; segments embed markdown only
        n_rows=n_rows,
        n_cols=src.n_cols,
        header_rows=src.header_rows,
        has_merged_cells=src.has_merged_cells,
        column_semantics=list(src.column_semantics),
        summary=src.summary,  # table summary doubles as retrieval context
    )
    return Block(
        block_id=f"{block.block_id}#t{seq}",
        type=BlockType.TABLE,
        text=markdown,
        order=block.order,
        bbox=block.bbox,
        parser=block.parser,
        parser_version=block.parser_version,
        confidence=block.confidence,
        table=new_table,
        metadata=dict(block.metadata),
    )


def apply_table_policy(doc: DocIR, **policy_kwargs) -> DocIR:
    """Rewrite a DocIR with every TABLE block replaced by its tiered blocks.

    Pure function — returns a new DocIR; the input is not mutated.
    """
    new_doc = DocIR.from_dict(doc.to_dict())
    for page in new_doc.pages:
        rebuilt: list[Block] = []
        for block in page.blocks:
            if block.type is BlockType.TABLE and block.table and block.table.markdown:
                rebuilt.extend(split_table_block(block, **policy_kwargs).blocks)
            else:
                rebuilt.append(block)
        page.blocks = rebuilt
    return new_doc
