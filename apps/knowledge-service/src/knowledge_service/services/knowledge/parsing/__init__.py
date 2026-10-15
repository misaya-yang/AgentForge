"""T4 parsing package: lossless IR + pluggable parser cascade.

Public surface (stable interface for the worker/ingestion integration):

* IR model: :class:`DocIR`, :class:`PageIR`, :class:`Block`, :class:`BlockType`,
  :class:`BBox`, table/formula/figure payloads — JSON round-trip exact.
* Derivations: :func:`render_document_markdown`, :func:`derive_chunk_payload`,
  :func:`citation_anchor`.
* Cascade: :func:`build_cascade`, :class:`ParserCascade`,
  :class:`CascadeConfig`/`default_cascade_config`, :class:`PageJob`,
  :class:`PageCache`/`InMemoryPageCache`.
* Registry: :class:`ParserRegistry`, :func:`register_defaults` (backends:
  ``text_layer``, ``legacy_ocr_vlm``, ``mineru``, ``paddle_ppstructure_v3``,
  ``paddleocr_vl``, ``general_vlm_fallback``).
* Versioning: :func:`page_cache_key`, :func:`cascade_bundle_version`,
  :func:`needs_reparse`.

Import-safe and side-effect free: registering backends never loads tesseract,
VLM clients, or GPU stacks — those arrive only via injected callables.
"""

from __future__ import annotations

from .base import (
    PageJob,
    PageParseFailed,
    PageSignals,
    ParserBackend,
    ParserError,
    ParserUnavailable,
    page_confidence,
)
from .cascade import (
    CascadeConfig,
    CascadeStage,
    InMemoryPageCache,
    PageCache,
    PageOutcome,
    ParserCascade,
    build_cascade,
    default_cascade_config,
)
from .ir import (
    IR_SCHEMA_VERSION,
    BBox,
    Block,
    BlockType,
    DocIR,
    FigureContent,
    FormulaContent,
    PageIR,
    TableContent,
)
from .persistence import PostgresPageCache
from .registry import ParserRegistry, UnknownBackend, register_defaults
from .render import (
    citation_anchor,
    derive_chunk_payload,
    iter_chunk_payloads,
    render_block_markdown,
    render_document_markdown,
    render_page_markdown,
)
from .table_policy import TableTier, apply_table_policy, classify_table, split_table_block
from .versioning import cascade_bundle_version, needs_reparse, page_cache_key

__all__ = [
    "IR_SCHEMA_VERSION",
    "BBox",
    "Block",
    "BlockType",
    "CascadeConfig",
    "CascadeStage",
    "DocIR",
    "FigureContent",
    "FormulaContent",
    "InMemoryPageCache",
    "PageCache",
    "PageIR",
    "PageJob",
    "PageOutcome",
    "PageParseFailed",
    "PageSignals",
    "ParserBackend",
    "ParserCascade",
    "ParserError",
    "ParserRegistry",
    "ParserUnavailable",
    "PostgresPageCache",
    "TableContent",
    "TableTier",
    "UnknownBackend",
    "apply_table_policy",
    "build_cascade",
    "cascade_bundle_version",
    "citation_anchor",
    "classify_table",
    "default_cascade_config",
    "derive_chunk_payload",
    "iter_chunk_payloads",
    "needs_reparse",
    "page_cache_key",
    "page_confidence",
    "register_defaults",
    "render_block_markdown",
    "render_document_markdown",
    "render_page_markdown",
    "split_table_block",
]
