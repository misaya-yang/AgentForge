"""T4 parse-quality metrics harness: edit distance, TEDS-lite table match,
normalised LaTeX match, reading-order consistency."""

from __future__ import annotations

import pytest
from knowledge_service.services.knowledge.parsing import Block, BlockType, DocIR, PageIR
from knowledge_service.services.knowledge.parsing.eval import (
    edit_distance_similarity,
    formula_similarity,
    levenshtein,
    normalize_latex,
    reading_order_score,
    score_document,
    table_similarity,
)


def _text_block(bid: str, text: str, order: int) -> Block:
    return Block(block_id=bid, type=BlockType.TEXT, text=text, order=order)


# ---------------------------------------------------------------- text


def test_levenshtein_known_values():
    assert levenshtein("kitten", "sitting") == 3
    assert levenshtein("", "abc") == 3
    assert levenshtein("same", "same") == 0
    assert edit_distance_similarity("中文段落", "中文段落") == 1.0
    assert edit_distance_similarity("", "") == 1.0
    assert edit_distance_similarity("abc", "") == 0.0
    assert 0.0 < edit_distance_similarity("扫描件识别文字", "扫描件识别的文字") < 1.0


# ---------------------------------------------------------------- tables


def test_table_similarity_identical_and_degraded():
    t = "| a | b |\n|---|---|\n| 1 | 2 |"
    assert table_similarity(t, t) == 1.0
    assert table_similarity("", "") == 1.0
    degraded = "| a | b |\n|---|---|\n| 1 |  |"
    assert 0.0 < table_similarity(t, degraded) < 1.0
    assert table_similarity(t, "| x | y |\n|---|---|\n| 9 | 8 |") == 0.0


# ---------------------------------------------------------------- formulas


def test_normalize_latex_and_formula_similarity():
    assert normalize_latex("$\\left( x \\right)$") == "(x)"
    assert formula_similarity("\\frac{1}{2}", "\\frac{1}{2}") == 1.0
    assert formula_similarity("$E=mc^2$", "E = mc^2") == 1.0
    assert formula_similarity("\\alpha", "\\beta") < 1.0


# ---------------------------------------------------------------- reading order


def test_reading_order_score():
    expected = [_text_block("a", "first para", 0), _text_block("b", "second para", 1), _text_block("c", "third para", 2)]
    assert reading_order_score(expected, expected) == 1.0
    assert reading_order_score(expected, expected[::-1]) == pytest.approx(0.0)
    assert reading_order_score(expected, expected[:1]) == 1.0  # <2 matched: nothing to order
    swapped = [expected[1], expected[0], expected[2]]
    score = reading_order_score(expected, swapped)
    assert 0.5 < score < 1.0  # one discordant pair out of three


# ---------------------------------------------------------------- document level


def test_score_document_identical_is_perfect():
    blocks = [_text_block("x", "内容甲", 0), _text_block("y", "内容乙", 1)]
    page = PageIR(page_number=1, blocks=blocks)
    doc = DocIR(doc_id="d", pages=[page])
    report = score_document(doc, doc)
    assert report.pages == 1
    assert report.text_similarity == 1.0
    assert report.reading_order == 1.0
    assert report.table_similarity == 1.0  # no tables on either side → vacuous pass
    assert report.formula_similarity == 1.0
    assert report.to_dict()["reading_order"] == 1.0


def test_score_document_penalises_bad_parse():
    expected = DocIR(
        doc_id="e",
        pages=[PageIR(page_number=1, blocks=[_text_block("a", "第一页文本", 0), _text_block("b", "第二页文本", 1)])],
    )
    actual = DocIR(
        doc_id="a",
        pages=[PageIR(page_number=1, blocks=[_text_block("a", "完全错误的内容", 0), _text_block("b", "第二页文本", 1)])],
    )
    report = score_document(expected, actual)
    assert 0.0 < report.text_similarity < 1.0
