"""T4 parser backend adapters: every registered backend satisfies the protocol
with its own injection contract, and payload/markdown → IR conversions are
exact."""

from __future__ import annotations

import pytest
from knowledge_service.services.knowledge.parsing import (
    BlockType,
    PageJob,
    ParserUnavailable,
    register_defaults,
)
from knowledge_service.services.knowledge.parsing.backends.legacy_ocr_vlm import LegacyOCRVLMBackend
from knowledge_service.services.knowledge.parsing.backends.mineru import (
    MinerUBackend,
    mineru_payload_to_page_ir,
)
from knowledge_service.services.knowledge.parsing.backends.paddleocr import (
    PaddleOCRVLBackend,
    PPStructureV3Backend,
    paddle_regions_to_page_ir,
)
from knowledge_service.services.knowledge.parsing.backends.qwen3_vl import (
    GeneralVLMFallbackBackend,
    markdown_to_blocks,
)
from knowledge_service.services.knowledge.parsing.backends.text_layer import TextLayerBackend
from knowledge_service.services.knowledge.parsing.base import ParserBackend


def _job(**kw) -> PageJob:
    base = {"doc_id": "d", "page_number": 1, "content_hash": "h"}
    base.update(kw)
    return PageJob(**base)


# ---------------------------------------------------------------- registry


def test_all_default_backends_registered_and_protocol_shaped():
    reg = register_defaults()
    assert reg.names() == [
        "general_vlm_fallback",
        "legacy_ocr_vlm",
        "mineru",
        "paddle_ppstructure_v3",
        "paddleocr_vl",
        "text_layer",
    ]
    for name in reg.names():
        backend = reg.create(name)
        assert isinstance(backend, ParserBackend), name
        # Unconfigured engine backends must be unavailable, never explode.
        assert backend.is_available() in (True, False)
        assert backend.can_handle(_job()) in (True, False)


# ---------------------------------------------------------------- text layer


async def test_text_layer_paragraphs_and_confidence():
    backend = TextLayerBackend()
    assert backend.is_available()
    page = await backend.parse_page(_job(text_layer="第一段。\n\n第二段。"))
    assert [b.type for b in page.sorted_blocks()] == [BlockType.TEXT, BlockType.TEXT]
    assert page.confidence == 1.0
    assert page.blocks[1].text == "第二段。"

    blank = await backend.parse_page(_job(text_layer="   "))
    assert blank.confidence == 0.0  # scanned page ⇒ escalate


async def test_text_layer_uses_injected_extract():
    backend = TextLayerBackend(extract=lambda job: f"extracted p{job.page_number}")
    page = await backend.parse_page(_job(page_number=4))
    assert page.blocks[0].text == "extracted p4"


# ---------------------------------------------------------------- legacy ocr/vlm


async def test_legacy_ocr_requires_callable_and_image():
    backend = LegacyOCRVLMBackend()
    assert backend.is_available() is False
    with pytest.raises(ParserUnavailable):
        await backend.parse_page(_job(image_bytes=b"img"))


async def test_legacy_ocr_wraps_text_into_blocks():
    async def fake_ocr(image: bytes) -> str:
        assert image == b"img"
        return "扫描件文字。\n\n第二段。"

    backend = LegacyOCRVLMBackend(ocr=fake_ocr, confidence=0.75)
    page = await backend.parse_page(_job(image_bytes=b"img"))
    assert len(page.blocks) == 2
    assert page.confidence == 0.75
    empty = LegacyOCRVLMBackend(ocr=async_return(""))
    assert (await empty.parse_page(_job(image_bytes=b"img"))).confidence == 0.0


def async_return(value: str):
    async def _ocr(_image: bytes) -> str:
        return value

    return _ocr


# ---------------------------------------------------------------- mineru


MINERU_PAYLOAD = {
    "width": 612,
    "height": 792,
    "blocks": [
        {"type": "title", "text": "Report", "bbox": [10, 10, 200, 30], "order": 0, "score": 0.99},
        {"type": "table", "text": "| h |\n|---|\n| v |", "html": "<table><td colspan=2>v</td></table>", "order": 1, "score": 0.9},
        {"type": "formula", "latex": "\\alpha + \\beta", "display": False, "order": 2},
        {"type": "image", "image": {"ref": "assets/p1/fig.png", "mime": "image/png", "description": "图一"}, "order": 3},
        {"type": "sideways", "text": "weird", "order": 4},
    ],
}


def test_mineru_payload_to_ir_types_and_dual_store():
    page = mineru_payload_to_page_ir(MINERU_PAYLOAD, backend="mineru", version="3.0")
    blocks = page.sorted_blocks()
    assert [b.type for b in blocks] == [
        BlockType.HEADING,
        BlockType.TABLE,
        BlockType.FORMULA,
        BlockType.FIGURE,
        BlockType.UNKNOWN,
    ]
    from knowledge_service.services.knowledge.parsing.ir import BBox

    assert blocks[0].bbox == BBox(10, 10, 200, 30)
    table = blocks[1].table
    assert table.markdown and table.html  # 双存
    assert table.has_merged_cells is True  # colspan in html
    assert blocks[2].formula.display is False
    assert blocks[3].figure.image_ref == "assets/p1/fig.png"


async def test_mineru_backend_injection_contract():
    plain = MinerUBackend()
    assert not plain.is_available()
    with pytest.raises(ParserUnavailable):
        await plain.parse_page(_job())

    backend = MinerUBackend(client=lambda _job: MINERU_PAYLOAD, version="3.1")
    assert backend.is_available()
    page = await backend.parse_page(_job(page_number=5))
    assert page.page_number == 5
    assert page.parser == "mineru" and page.parser_version == "3.1"
    assert page.blocks[0].parser == "mineru"  # blocks carry provenance too


# ---------------------------------------------------------------- paddle


PADDLE_PAYLOAD = {
    "width": 595,
    "height": 842,
    "regions": [
        {"label": "paragraph_title", "text": "标题", "bbox": [0, 0, 50, 10], "confidence": 0.98},
        {"label": "table", "text": "| a |\n|---|\n| b |", "html": "<table/>", "rows": 1, "cols": 1, "confidence": 0.9},
        {"label": "inline_formula", "latex": "x^2", "confidence": 0.8},
        {"label": "chart", "text": "柱状图", "bbox": [0, 100, 50, 200], "confidence": 0.7},
        {"label": "handwritten_note", "text": "??", "confidence": 0.1},
    ],
}


def test_paddle_regions_to_ir():
    page = paddle_regions_to_page_ir(PADDLE_PAYLOAD, backend="paddle_ppstructure_v3", version="ppstructurev3-1")
    blocks = page.sorted_blocks()
    assert [b.type for b in blocks] == [
        BlockType.HEADING,
        BlockType.TABLE,
        BlockType.FORMULA,
        BlockType.FIGURE,
        BlockType.UNKNOWN,
    ]
    assert blocks[2].formula.display is False  # inline_formula ⇒ inline
    assert blocks[1].table.n_rows == 1 and blocks[1].table.html == "<table/>"
    assert blocks[4].confidence == 0.1


def test_paddle_two_tiers_distinct_versions():
    fast, acc = PPStructureV3Backend(), PaddleOCRVLBackend()
    assert (fast.name, fast.version) == ("paddle_ppstructure_v3", "ppstructurev3-1")
    assert (acc.name, acc.version) == ("paddleocr_vl", "paddleocr-vl-1.6")
    assert not fast.is_available()  # pure Apache-2.0 candidates, off until wired


async def test_paddle_backend_with_client():
    backend = PPStructureV3Backend(client=lambda _job: PADDLE_PAYLOAD)
    page = await backend.parse_page(_job(page_number=2))
    assert page.page_number == 2 and page.blocks


# ---------------------------------------------------------------- general VLM


def test_markdown_to_blocks_structures():
    md = "\n".join(
        [
            "# 标题一",
            "普通段落第一行",
            "续行。",
            "",
            "| 列A | 列B |",
            "|---|---|",
            "| 1 | 2 |",
            "",
            "$$\\sum_{i=1}^{n} i$$",
            "",
            "![架构图](assets/fig1.png)",
        ]
    )
    blocks = markdown_to_blocks(md, backend="general_vlm_fallback", version="qwen3-vl")
    types = [b.type for b in blocks]
    assert types == [BlockType.HEADING, BlockType.TEXT, BlockType.TABLE, BlockType.FORMULA, BlockType.FIGURE]
    assert blocks[0].metadata["level"] == 1
    assert blocks[1].text == "普通段落第一行\n续行。"
    assert blocks[2].table.markdown.startswith("| 列A | 列B |")
    assert blocks[2].table.n_cols == 2
    assert blocks[3].formula.latex == "\\sum_{i=1}^{n} i"
    assert blocks[4].figure.image_ref == "assets/fig1.png"
    assert [b.order for b in blocks] == list(range(len(blocks)))


async def test_general_vlm_backend_requires_generate_and_image():
    plain = GeneralVLMFallbackBackend()
    assert not plain.is_available()
    with pytest.raises(ParserUnavailable):
        await plain.parse_page(_job())

    async def generate(job: PageJob) -> str:
        return "# 结果\n\n页面文本。"

    backend = GeneralVLMFallbackBackend(generate=generate)
    page = await backend.parse_page(_job(image_bytes=b"img"))
    assert page.blocks[0].type is BlockType.HEADING
    assert page.confidence == 0.7
    # No image on the job ⇒ can_handle refuses (it cannot see the page).
    assert backend.can_handle(_job(text_layer="x")) is False
