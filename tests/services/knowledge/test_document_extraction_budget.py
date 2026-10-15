from __future__ import annotations

import io
import sys
import zipfile
from copy import deepcopy
from types import SimpleNamespace
from typing import Any

import pytest
from knowledge_service.api.routes import knowledge as routes
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import ValidationFailedError
from knowledge_service.services.knowledge import document_processor as processor_module
from knowledge_service.services.knowledge.document_processor import DocumentProcessor
from knowledge_service.services.knowledge.document_service import (
    DocumentService,
    _redacted_source_url,
)
from knowledge_service.services.knowledge.ingestion_service import (
    MAX_EXTRACTED_TEXT_BYTES as INGESTION_MAX_BYTES,
)
from knowledge_service.services.knowledge.ingestion_service import (
    MAX_EXTRACTED_TEXT_CHARS as INGESTION_MAX_CHARS,
)

_DOCX_XML = b"""<?xml version="1.0" encoding="UTF-8"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
  <w:body><w:p/></w:body>
</w:document>
"""


def _settings() -> SimpleNamespace:
    return SimpleNamespace(
        knowledge=SimpleNamespace(
            ocr_enabled=False,
            pdf_min_text_chars_for_ocr=200,
        )
    )


def _zip_bytes(entries: dict[str, bytes], *, compression: int = zipfile.ZIP_STORED) -> bytes:
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, "w", compression=compression) as archive:
        for name, value in entries.items():
            archive.writestr(name, value)
    return stream.getvalue()


def test_source_url_redaction_removes_reusable_credentials() -> None:
    redacted = _redacted_source_url(
        "https://example.test/doc?token=secret-value&visible=1#fragment-secret"
    )

    assert "secret-value" not in redacted
    assert "fragment-secret" not in redacted
    assert "token=%2A%2A%2A" in redacted
    assert "visible=1" in redacted


def test_source_url_rejects_userinfo() -> None:
    with pytest.raises(ValidationFailedError, match="userinfo"):
        _redacted_source_url("https://user:password@example.test/doc")


def test_document_processor_budget_defaults_match_ingestion_contract() -> None:
    assert processor_module.MAX_EXTRACTED_TEXT_CHARS == INGESTION_MAX_CHARS == 16_000_000
    assert processor_module.MAX_EXTRACTED_TEXT_BYTES == INGESTION_MAX_BYTES == 48 * 1024 * 1024
    assert processor_module.MAX_DOCX_ZIP_SINGLE_UNCOMPRESSED_BYTES == 16 * 1024 * 1024
    assert processor_module.MAX_DOCX_ZIP_TOTAL_UNCOMPRESSED_BYTES == INGESTION_MAX_BYTES
    assert processor_module.MAX_PDF_SOURCE_BYTES == 16 * 1024 * 1024


def test_public_upload_parser_budget_matches_format_limits() -> None:
    routes.require_parser_budget(
        filename="paper.pdf",
        extension=".pdf",
        size_bytes=16 * 1024 * 1024,
    )

    with pytest.raises(ValidationFailedError, match="4MB parser limit"):
        routes.require_parser_budget(
            filename="page.html",
            extension=".html",
            size_bytes=4 * 1024 * 1024 + 1,
        )


def test_docx_rejects_entry_count_before_python_docx_materialization(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    content = _zip_bytes({"one.xml": b"one", "two.xml": b"two"})
    monkeypatch.setattr(processor_module, "MAX_DOCX_ZIP_ENTRIES", 1)
    imported_docx = False

    original_import = __import__

    def guarded_import(name: str, *args: Any, **kwargs: Any) -> Any:
        nonlocal imported_docx
        if name == "docx":
            imported_docx = True
        return original_import(name, *args, **kwargs)

    monkeypatch.setattr("builtins.__import__", guarded_import)
    monkeypatch.setattr(
        processor_module.zipfile,
        "ZipFile",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            AssertionError("ZipFile must not materialize over-budget entries")
        ),
    )

    with pytest.raises(ValidationFailedError, match="entry limit"):
        processor.extract_text_from_docx_bytes(content)

    assert imported_docx is False


def test_docx_rejects_dishonest_eocd_count_before_zipfile_materialization(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    content = bytearray(
        _zip_bytes(
            {
                f"word/empty-{index:04d}.xml": b""
                for index in range(processor_module.MAX_DOCX_ZIP_ENTRIES + 1)
            }
        )
    )
    assert len(content) < 500_000
    eocd_offset = content.rfind(b"PK\x05\x06")
    assert eocd_offset >= 0
    # Forge both EOCD entry counts to one while retaining all 4097 central
    # records. The preflight must count records itself rather than trust this.
    content[eocd_offset + 8 : eocd_offset + 12] = b"\x01\x00\x01\x00"
    monkeypatch.setattr(
        processor_module.zipfile,
        "ZipFile",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            AssertionError("ZipFile must not see a dishonest central directory")
        ),
    )

    with pytest.raises(ValidationFailedError, match="entry limit"):
        processor.extract_text_from_docx_bytes(bytes(content))


def test_docx_rejects_excessive_compression_ratio(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    content = _zip_bytes(
        {"word/document.xml": b"A" * 4_096},
        compression=zipfile.ZIP_DEFLATED,
    )
    monkeypatch.setattr(processor_module, "MAX_DOCX_ZIP_COMPRESSION_RATIO", 2.0)

    with pytest.raises(ValidationFailedError, match="compression ratio"):
        processor.extract_text_from_docx_bytes(content)


def test_docx_rejects_single_and_total_uncompressed_limits(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())

    monkeypatch.setattr(
        processor_module,
        "MAX_DOCX_ZIP_SINGLE_UNCOMPRESSED_BYTES",
        3,
    )
    with pytest.raises(ValidationFailedError, match="entry exceeds"):
        processor.extract_text_from_docx_bytes(_zip_bytes({"word/document.xml": b"1234"}))

    monkeypatch.setattr(
        processor_module,
        "MAX_DOCX_ZIP_SINGLE_UNCOMPRESSED_BYTES",
        INGESTION_MAX_BYTES,
    )
    monkeypatch.setattr(
        processor_module,
        "MAX_DOCX_ZIP_TOTAL_UNCOMPRESSED_BYTES",
        5,
    )
    with pytest.raises(ValidationFailedError, match="total uncompressed"):
        processor.extract_text_from_docx_bytes(
            _zip_bytes({"word/document.xml": b"123", "word/styles.xml": b"456"})
        )


def test_docx_xml_structure_rejects_before_python_docx_dom(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    xml = b"""<w:document xmlns:w="urn:w"><w:body>
    <w:p/><w:p/><w:p/></w:body></w:document>"""
    content = _zip_bytes({"word/document.xml": xml})
    monkeypatch.setattr(processor_module, "MAX_DOCX_PARAGRAPHS", 1)
    imported_docx = False
    original_import = __import__

    def guarded_import(name: str, *args: Any, **kwargs: Any) -> Any:
        nonlocal imported_docx
        if name == "docx":
            imported_docx = True
        return original_import(name, *args, **kwargs)

    monkeypatch.setattr("builtins.__import__", guarded_import)

    with pytest.raises(ValidationFailedError, match="p limit"):
        processor.extract_text_from_docx_bytes(content)

    assert imported_docx is False


def test_docx_paragraphs_are_budgeted_incrementally(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    content = _zip_bytes({"word/document.xml": _DOCX_XML})
    fake_document = SimpleNamespace(
        iter_inner_content=lambda: iter(
            [SimpleNamespace(text="12345"), SimpleNamespace(text="67890")]
        ),
    )
    monkeypatch.setitem(
        sys.modules,
        "docx",
        SimpleNamespace(Document=lambda _stream: fake_document),
    )
    monkeypatch.setattr(processor_module, "MAX_EXTRACTED_TEXT_CHARS", 8)

    with pytest.raises(ValidationFailedError, match="8 character limit"):
        processor.extract_text_from_docx_bytes(content)


def test_docx_table_rows_are_budgeted_incrementally(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    content = _zip_bytes({"word/document.xml": _DOCX_XML})
    rows = [
        SimpleNamespace(cells=[SimpleNamespace(text="12345", grid_span=1)]),
        SimpleNamespace(cells=[SimpleNamespace(text="67890", grid_span=1)]),
    ]
    fake_document = SimpleNamespace(
        iter_inner_content=lambda: iter([SimpleNamespace(rows=rows)]),
    )
    monkeypatch.setitem(
        sys.modules,
        "docx",
        SimpleNamespace(Document=lambda _stream: fake_document),
    )
    monkeypatch.setattr(processor_module, "MAX_EXTRACTED_TEXT_CHARS", 20)

    with pytest.raises(ValidationFailedError, match="20 character limit"):
        processor.extract_text_from_docx_bytes(content)


def test_pdf_pages_are_budgeted_before_fallback(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())

    class FakeReader:
        def __init__(self, _stream: Any) -> None:
            assert (
                filter_limits.MAX_ARRAY_BASED_STREAM_OUTPUT_LENGTH
                == processor_module.MAX_PDF_DECOMPRESSED_STREAM_BYTES
            )
            self.pages = [
                SimpleNamespace(extract_text=lambda: "12345"),
                SimpleNamespace(extract_text=lambda: "67890"),
            ]

    filter_limits = SimpleNamespace(
        ZLIB_MAX_OUTPUT_LENGTH=75_000_000,
        LZW_MAX_OUTPUT_LENGTH=75_000_000,
        RUN_LENGTH_MAX_OUTPUT_LENGTH=75_000_000,
        JBIG2_MAX_OUTPUT_LENGTH=75_000_000,
        MAX_ARRAY_BASED_STREAM_OUTPUT_LENGTH=75_000_000,
    )
    monkeypatch.setitem(
        sys.modules,
        "pypdf",
        SimpleNamespace(
            PdfReader=FakeReader,
            filters=filter_limits,
        ),
    )
    monkeypatch.setattr(processor_module, "MAX_EXTRACTED_TEXT_CHARS", 8)

    with pytest.raises(ValidationFailedError, match="8 character limit"):
        processor.extract_text_from_pdf_bytes(b"%PDF-fake")

    assert {
        filter_limits.ZLIB_MAX_OUTPUT_LENGTH,
        filter_limits.LZW_MAX_OUTPUT_LENGTH,
        filter_limits.RUN_LENGTH_MAX_OUTPUT_LENGTH,
        filter_limits.JBIG2_MAX_OUTPUT_LENGTH,
        filter_limits.MAX_ARRAY_BASED_STREAM_OUTPUT_LENGTH,
    } == {processor_module.MAX_PDF_DECOMPRESSED_STREAM_BYTES}


def test_pdf_raw_budget_rejects_before_pypdf_import(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    monkeypatch.setattr(processor_module, "MAX_PDF_SOURCE_BYTES", 4)
    imported_pypdf = False
    original_import = __import__

    def guarded_import(name: str, *args: Any, **kwargs: Any) -> Any:
        nonlocal imported_pypdf
        if name == "pypdf":
            imported_pypdf = True
        return original_import(name, *args, **kwargs)

    monkeypatch.setattr("builtins.__import__", guarded_import)

    with pytest.raises(ValidationFailedError, match="PDF source"):
        processor.extract_text_from_pdf_bytes(b"%PDF-too-large")

    assert imported_pypdf is False


def test_pdf_falls_back_to_bounded_pymupdf_when_pypdf_rejects_stream(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())

    class RejectingReader:
        def __init__(self, _stream: Any) -> None:
            raise RuntimeError("Limit reached while decompressing")

    monkeypatch.setitem(
        sys.modules,
        "pypdf",
        SimpleNamespace(
            PdfReader=RejectingReader,
            filters=SimpleNamespace(
                ZLIB_MAX_OUTPUT_LENGTH=0,
                LZW_MAX_OUTPUT_LENGTH=0,
                RUN_LENGTH_MAX_OUTPUT_LENGTH=0,
                JBIG2_MAX_OUTPUT_LENGTH=0,
                MAX_ARRAY_BASED_STREAM_OUTPUT_LENGTH=0,
            ),
        ),
    )

    class FakePage:
        def get_text(self, _mode: str) -> str:
            return "A real reinforcement-learning guide"

    class FakeDocument:
        page_count = 1

        def load_page(self, page_number: int) -> FakePage:
            assert page_number == 0
            return FakePage()

        def close(self) -> None:
            return None

    monkeypatch.setattr(
        processor_module,
        "import_pymupdf",
        lambda: SimpleNamespace(open=lambda **_kwargs: FakeDocument()),
    )

    extracted = processor.extract_text_from_pdf_bytes(b"%PDF-valid")

    assert "reinforcement-learning guide" in extracted


def test_pdf_cleaner_rejects_long_line_before_regex(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    monkeypatch.setattr(processor_module, "MAX_PDF_LINE_CHARS", 8)
    monkeypatch.setattr(
        processor_module.re,
        "search",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(
            AssertionError("regex must not inspect an oversized line")
        ),
    )

    with pytest.raises(ValidationFailedError) as exc_info:
        processor.clean_pdf_content("." * 9)
    assert "PDF line" in str(exc_info.value)


def test_pdf_table_renderer_does_not_copy_the_whole_table() -> None:
    processor = DocumentProcessor(_settings())

    class PoisonRows:
        def __iter__(self):
            yield ("a", "b")
            yield ("c", "d")

        def __len__(self) -> int:
            raise AssertionError("whole-table sizing/list materialization is forbidden")

    rendered = processor.pdf_table_to_markdown(PoisonRows())  # type: ignore[arg-type]

    assert "| a | b |" in rendered
    assert "| c | d |" in rendered


def test_html_and_plain_text_enforce_the_same_tiny_budget(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    monkeypatch.setattr(processor_module, "MAX_EXTRACTED_TEXT_CHARS", 8)

    with pytest.raises(ValidationFailedError, match="8 character limit"):
        processor.extract_text_from_html("<p>12345</p><p>67890</p>")
    with pytest.raises(ValidationFailedError, match="8 character limit"):
        processor.extract_text_from_bytes(b"123456789", "large.txt", "text/plain")


def test_html_structure_rejects_before_beautifulsoup_dom(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    monkeypatch.setattr(processor_module, "MAX_HTML_TAGS", 1)
    imported_bs4 = False
    original_import = __import__

    def guarded_import(name: str, *args: Any, **kwargs: Any) -> Any:
        nonlocal imported_bs4
        if name == "bs4":
            imported_bs4 = True
        return original_import(name, *args, **kwargs)

    monkeypatch.setattr("builtins.__import__", guarded_import)

    with pytest.raises(ValidationFailedError, match="tag parser limit"):
        processor.extract_text_from_html("<main><p>x</p></main>")

    assert imported_bs4 is False


def test_plain_text_enforces_utf8_byte_budget(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    monkeypatch.setattr(processor_module, "MAX_EXTRACTED_TEXT_CHARS", 100)
    monkeypatch.setattr(processor_module, "MAX_EXTRACTED_TEXT_BYTES", 5)

    with pytest.raises(ValidationFailedError, match="5 byte limit"):
        processor.extract_text_from_bytes("€€".encode(), "large.txt", "text/plain")


def test_unbounded_legacy_doc_and_ocr_helpers_fail_before_import(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    processor = DocumentProcessor(_settings())
    imported: list[str] = []
    original_import = __import__

    def guarded_import(name: str, *args: Any, **kwargs: Any) -> Any:
        if name == "textract" or name.endswith("ocr_utils"):
            imported.append(name)
        return original_import(name, *args, **kwargs)

    monkeypatch.setattr("builtins.__import__", guarded_import)

    with pytest.raises(ValidationFailedError, match="Legacy DOC extraction is disabled"):
        processor.extract_text_from_doc_bytes(b"poison")
    with pytest.raises(ValidationFailedError, match="OCR text extraction is disabled"):
        processor.ocr_pdf_bytes(b"poison")

    assert imported == []


def test_builder_rejects_fragment_amplification_without_string_coalescing() -> None:
    builder = processor_module._BoundedTextBuilder(
        max_chars=100,
        max_bytes=100,
        max_parts=2,
    )
    builder.append("a")
    builder.append("b")

    with pytest.raises(ValidationFailedError, match="2 fragment limit"):
        builder.append("c")


class _NoWriteDatabase:
    def __init__(self) -> None:
        self.events: list[str] = []

    async def insert_document(self, *_args: Any, **_kwargs: Any) -> None:
        self.events.append("insert")

    async def get_document(self, *_args: Any, **_kwargs: Any) -> None:
        self.events.append("get")
        return None


class _UrlKnowledgeService:
    def __init__(self, processor: DocumentProcessor) -> None:
        self.processor = processor
        self.events: list[str] = []

    async def require_dataset_access(
        self,
        _user: UserContext,
        dataset_id: str,
        *,
        required: str,
    ) -> dict[str, Any]:
        assert dataset_id == "dataset-a"
        assert required == "editor"
        self.events.append("authorize")
        return deepcopy(
            {
                "dataset_id": "dataset-a",
                "tenant_id": "tenant-a",
                "collection_name": "collection-a",
                "embedding_provider": "dashscope",
                "embedding_model": "text-embedding-v4",
                "embedding_dimension": 1_024,
                "embedding_config": {},
                "index_config": {},
            }
        )

    def _extract_text_from_bytes(
        self,
        content: bytes,
        filename: str | None,
        mime_type: str | None,
    ) -> tuple[str, str]:
        self.events.append("extract")
        return self.processor.extract_text_from_bytes(content, filename, mime_type)


@pytest.mark.asyncio
async def test_url_oversize_has_zero_persistence_or_queue_side_effects(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    async def fake_safe_fetch(*_args: Any, **_kwargs: Any) -> bytes:
        return b"123456789"

    monkeypatch.setattr("ai_gateway_core.security.safe_fetch", fake_safe_fetch)
    monkeypatch.setattr(processor_module, "MAX_EXTRACTED_TEXT_CHARS", 8)
    database = _NoWriteDatabase()
    knowledge_service = _UrlKnowledgeService(DocumentProcessor(_settings()))
    service = DocumentService(_settings(), database)  # type: ignore[arg-type]
    service._ks = knowledge_service  # type: ignore[assignment]

    with pytest.raises(ValidationFailedError, match="8 character limit"):
        await service.create_document_from_url(
            UserContext(user_id="editor-a", tenant_id="tenant-a"),
            "dataset-a",
            "https://example.test/large.txt",
        )

    assert knowledge_service.events == ["authorize", "extract"]
    assert database.events == []
