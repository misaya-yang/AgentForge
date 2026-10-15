"""T4 IR model: JSON round-trip, reading order, derivations (render/citation/payload)."""

from __future__ import annotations

import json

import pytest
from knowledge_service.services.knowledge.parsing import (
    IR_SCHEMA_VERSION,
    BBox,
    Block,
    BlockType,
    DocIR,
    FigureContent,
    FormulaContent,
    PageIR,
    TableContent,
    citation_anchor,
    derive_chunk_payload,
    iter_chunk_payloads,
    render_document_markdown,
    render_page_markdown,
)


def _sample_doc() -> DocIR:
    return DocIR(
        doc_id="doc-1",
        content_hash="sha256:abc",
        filename="annual.pdf",
        mime="application/pdf",
        pages=[
            PageIR(
                page_number=1,
                page_width=595.0,
                page_height=842.0,
                confidence=0.97,
                parser="mineru",
                parser_version="3.0",
                blocks=[
                    Block(
                        block_id="p1b1",
                        type=BlockType.HEADING,
                        text="第一章 总则",
                        order=1,
                        bbox=BBox(0, 0, 100, 20),
                        parser="mineru",
                        parser_version="3.0",
                        metadata={"level": 2},
                    ),
                    Block(
                        block_id="p1b0",
                        type=BlockType.TEXT,
                        text="正文内容。",
                        order=0,
                        parser="mineru",
                        parser_version="3.0",
                    ),
                    Block(
                        block_id="p1b2",
                        type=BlockType.TABLE,
                        text="| a | b |\n|---|---|\n| 1 | 2 |",
                        order=2,
                        bbox=BBox(0, 300, 400, 500),
                        parser="mineru",
                        parser_version="3.0",
                        table=TableContent(
                            markdown="| a | b |\n|---|---|\n| 1 | 2 |",
                            html="<table><thead><tr><th>a</th><th>b</th></tr></thead><tbody><tr><td colspan='2'>1</td><td>2</td></tr></tbody></table>",
                            n_rows=1,
                            n_cols=2,
                            column_semantics=["名称", "数量"],
                            has_merged_cells=True,
                            summary="一个两列表",
                        ),
                    ),
                ],
            ),
            PageIR(
                page_number=2,
                parser="general_vlm_fallback",
                parser_version="qwen3-vl",
                hard_page=True,
                blocks=[
                    Block(
                        block_id="p2b0",
                        type=BlockType.FORMULA,
                        text="E=mc^2",
                        order=0,
                        formula=FormulaContent(latex="E=mc^2", display=True),
                    ),
                    Block(
                        block_id="p2b1",
                        type=BlockType.FIGURE,
                        text="流程图",
                        order=1,
                        figure=FigureContent(image_ref="assets/doc-1/p2/fig1.png", mime="image/png", description="流程图"),
                    ),
                ],
            ),
        ],
        metadata={"notes": "x"},
    )


def test_round_trip_exact():
    doc = _sample_doc()
    clone = DocIR.from_json(doc.to_json())
    assert clone.to_json() == doc.to_json()
    assert clone.pages[0].blocks[2].table.has_merged_cells is True
    assert clone.pages[0].blocks[0].bbox == BBox(0, 0, 100, 20)
    assert clone.pages[1].hard_page is True


def test_round_trip_survives_json_dumps_loads():
    doc = _sample_doc()
    clone = DocIR.from_dict(json.loads(json.dumps(doc.to_dict())))
    assert clone.to_dict() == doc.to_dict()


def test_to_json_is_deterministic():
    doc = _sample_doc()
    assert doc.to_json() == DocIR.from_json(doc.to_json()).to_json()


def test_schema_version_preserved():
    doc = _sample_doc()
    assert doc.schema_version == IR_SCHEMA_VERSION
    data = doc.to_dict()
    data["schema_version"] = "99"
    assert DocIR.from_dict(data).schema_version == "99"


def test_unknown_block_type_degrades_to_unknown():
    data = {
        "doc_id": "d",
        "pages": [{"page_number": 1, "blocks": [{"block_id": "x", "type": "sideways_chart"}]}],
    }
    block = DocIR.from_dict(data).pages[0].blocks[0]
    assert block.type is BlockType.UNKNOWN


def test_reading_order_and_page_text():
    doc = _sample_doc()
    page1 = doc.page(1)
    assert [b.block_id for b in page1.sorted_blocks()] == ["p1b0", "p1b1", "p1b2"]
    assert doc.page(99) is None
    # Global iteration: page number first, then in-page order.
    assert [b.block_id for b in doc.iter_blocks()] == ["p1b0", "p1b1", "p1b2", "p2b0", "p2b1"]


def test_stats():
    stats = _sample_doc().stats()
    assert stats["pages"] == 2
    assert stats["blocks"] == 5
    assert stats["tables"] == 1 and stats["formulas"] == 1 and stats["figures"] == 1
    assert stats["hard_pages"] == 1
    assert stats["by_type"]["heading"] == 1
    assert stats["parsers"] == ["general_vlm_fallback", "mineru"]


def test_render_block_markdown_types():
    doc = _sample_doc()
    page1, page2 = doc.pages
    md = render_page_markdown(page1)
    assert md.index("正文内容。") < md.index("## 第一章 总则") < md.index("| a | b |")
    assert "| 1 | 2 |" in md  # table dual store → markdown is the render form
    md2 = render_page_markdown(page2)
    assert "$$E=mc^2$$" in md2
    assert "![流程图](assets/doc-1/p2/fig1.png)" in md2


def test_render_table_html_only_falls_back_to_summary():
    page = PageIR(
        page_number=1,
        blocks=[
            Block(
                block_id="b",
                type=BlockType.TABLE,
                table=TableContent(markdown="", html="<table>...</table>", summary="季度财务汇总表"),
            )
        ],
    )
    assert render_page_markdown(page) == "季度财务汇总表"


def test_render_document_page_markers():
    doc = _sample_doc()
    plain = render_document_markdown(doc)
    marked = render_document_markdown(doc, page_markers=True)
    assert "[Page 1]" in marked and "[Page 2]" not in plain
    assert marked.count("[Page") == 2


def test_citation_anchor_and_payload():
    doc = _sample_doc()
    block = doc.page(1).sorted_blocks()[2]
    anchor = citation_anchor(block, 1)
    assert anchor["block_id"] == "p1b2"
    assert anchor["page_number"] == 1
    assert anchor["bbox"] == {"x0": 0.0, "y0": 300.0, "x1": 400.0, "y1": 500.0}
    assert anchor["parser"] == "mineru"

    payload = derive_chunk_payload(block, 1)
    assert payload["block_type"] == "table"
    assert payload["table_html"].startswith("<table>")
    assert payload["table_summary"] == "一个两列表"
    assert payload["citation"]["page_number"] == 1

    formula_payload = derive_chunk_payload(doc.page(2).sorted_blocks()[0], 2)
    assert formula_payload["formula_latex"] == "E=mc^2"


def test_iter_chunk_payloads_global_order():
    payloads = iter_chunk_payloads(_sample_doc())
    assert [p["citation"]["page_number"] for p in payloads] == [1, 1, 1, 2, 2]
    assert all(p["citation"]["block_id"] for p in payloads)


def test_bbox_from_list_guards():
    assert BBox.from_list([1, 2, 3, 4]) == BBox(1, 2, 3, 4)
    assert BBox.from_list([1, 2]) is None
    assert BBox.from_list(None) is None


@pytest.mark.parametrize("level,expected", [(1, "# "), (3, "### "), (9, "###### ")])
def test_heading_level_clamped(level, expected):
    block = Block(block_id="h", type=BlockType.HEADING, text="T", metadata={"level": level})
    from knowledge_service.services.knowledge.parsing import render_block_markdown

    assert render_block_markdown(block).startswith(expected)
