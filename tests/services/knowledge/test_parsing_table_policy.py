"""T4 table strategy: size tiers (≤20 / 21–100 / >100), header repetition,
boundary overlap, dual-store retention, purity of the document rewrite."""

from __future__ import annotations

from knowledge_service.services.knowledge.parsing import BlockType, DocIR, PageIR
from knowledge_service.services.knowledge.parsing.ir import Block, TableContent
from knowledge_service.services.knowledge.parsing.table_policy import (
    TableTier,
    apply_table_policy,
    classify_table,
    parse_markdown_rows,
    split_table_block,
)


def _table_block(n_rows: int, *, html: str | None = "<table/>", summary: str | None = "季度表", block_id: str = "tbl") -> Block:
    rows = "\n".join(f"| c1-{i} | c2-{i} |" for i in range(n_rows))
    md = f"| 列1 | 列2 |\n|---|---|\n{rows}"
    return Block(
        block_id=block_id,
        type=BlockType.TABLE,
        text=md,
        order=3,
        parser="mineru",
        parser_version="3.0",
        table=TableContent(
            markdown=md,
            html=html,
            n_rows=n_rows,
            n_cols=2,
            column_semantics=["名称", "数量"],
            summary=summary,
        ),
    )


def test_classify_boundaries():
    assert classify_table(20) is TableTier.WHOLE
    assert classify_table(21) is TableTier.SEGMENTED
    assert classify_table(100) is TableTier.SEGMENTED
    assert classify_table(101) is TableTier.ROW_LEVEL


def test_parse_markdown_rows_header_split():
    header, data = parse_markdown_rows("| h1 | h2 |\n|---|---|\n| a | b |")
    assert header == ["| h1 | h2 |", "|---|---|"]
    assert data == ["| a | b |"]
    header, data = parse_markdown_rows("no pipes here")
    assert header == [] and data == []


def test_small_table_untouched():
    block = _table_block(20)
    res = split_table_block(block)
    assert res.tier is TableTier.WHOLE
    assert res.blocks == [block]


def test_medium_table_segments_repeat_header_with_overlap():
    block = _table_block(50)
    res = split_table_block(block, rows_per_segment=20, overlap_rows=2)
    assert res.tier is TableTier.SEGMENTED
    assert len(res.blocks) == 3  # windows at starts 0, 18, 36
    for segment in res.blocks:
        lines = segment.table.markdown.splitlines()
        assert lines[0] == "| 列1 | 列2 |" and lines[1] == "|---|---|"  # 重复表头
        assert segment.table.html is None  # dual store: HTML stays on the parent IR
        assert segment.table.summary == "季度表"  # 表摘要作检索上下文
        assert segment.table.column_semantics == ["名称", "数量"]
    # Boundary overlap: last row of segment 1 appears in segment 2.
    seg0_last = res.blocks[0].table.markdown.splitlines()[-1]
    assert seg0_last in res.blocks[1].table.markdown.splitlines()
    # Every data row is covered by at least one segment.
    covered = {ln for seg in res.blocks for ln in seg.table.markdown.splitlines()[2:]}
    assert len(covered) == 50
    assert all(seg.order == 3 and seg.parser == "mineru" for seg in res.blocks)
    assert len({seg.block_id for seg in res.blocks}) == 3


def test_large_table_row_level():
    block = _table_block(120)
    res = split_table_block(block)
    assert res.tier is TableTier.ROW_LEVEL
    assert len(res.blocks) == 120
    first = res.blocks[0].table.markdown.splitlines()
    assert first == ["| 列1 | 列2 |", "|---|---|", "| c1-0 | c2-0 |"]


def test_html_only_table_not_split():
    block = _table_block(200)
    block.table.markdown = ""
    res = split_table_block(block)
    assert res.blocks == [block]  # no markdown rows to split on; keep parent


def test_apply_table_policy_is_pure_and_ids_unique():
    doc = DocIR(
        doc_id="d",
        pages=[
            PageIR(
                page_number=1,
                blocks=[
                    Block(block_id="intro", type=BlockType.TEXT, text="前言", order=0),
                    _table_block(50, block_id="tbl-big"),
                    _table_block(5, block_id="tbl-small"),
                ],
            )
        ],
    )
    before = doc.to_json()
    out = apply_table_policy(doc, rows_per_segment=20, overlap_rows=2)
    assert doc.to_json() == before  # input untouched
    blocks = out.iter_blocks()
    ids = [b.block_id for b in blocks]
    assert len(ids) == len(set(ids))
    small_table = next(b for b in blocks if b.block_id == "tbl-small")
    assert small_table.table.html == "<table/>"  # WHOLE tier keeps the parent verbatim
    assert blocks[0].block_id == "intro"
    assert blocks[-1].block_id == "tbl-small"
    assert all(b.block_id.startswith("tbl-big#t") for b in blocks[1:-1])
