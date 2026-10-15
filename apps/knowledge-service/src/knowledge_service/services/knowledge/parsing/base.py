"""Stable parser-backend interface for the pluggable cascade (PRD T4 item 2).

A backend is *anything* that can turn one page of source material into a
:class:`~...parsing.ir.PageIR`: the existing VLM/OCR path, a text-layer
extractor, MinerU, PaddleOCR, or a general VLM.  Backends are constructed by
the registry from config, must be cheap to import, and signal runtime
unreadiness through :meth:`ParserBackend.is_available` (missing client/
credentials ⇒ unavailable) instead of raising at import time.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any, Protocol, runtime_checkable

from .ir import PageIR


class ParserError(Exception):
    """Base error for parsing backends and cascade failures."""


class ParserUnavailable(ParserError):
    """Backend is registered but cannot serve now (client not configured)."""


class PageParseFailed(ParserError):
    """Every cascade stage failed for one page."""

    def __init__(self, page_number: int, attempts: list[str], reasons: dict[str, str]) -> None:
        self.page_number = page_number
        self.attempts = attempts
        self.reasons = reasons
        super().__init__(
            f"all cascade stages failed for page {page_number}: "
            + "; ".join(f"{a}: {reasons.get(a, '?')}" for a in attempts)
        )


@dataclass
class PageSignals:
    """Cheap pre-parse facts about a page used for stage routing.

    Derived from the existing streaming loader (``PageContent``): whether the
    page has a real text layer, how long it is, and how many embedded images
    it carries.  No backend should need anything heavier to route on.
    """

    has_text_layer: bool = False
    text_length: int = 0
    image_count: int = 0
    mime: str | None = None

    @classmethod
    def derive(cls, *, text_layer: str = "", image_count: int = 0, mime: str | None = None) -> PageSignals:
        return cls(
            has_text_layer=bool(text_layer.strip()),
            text_length=len(text_layer or ""),
            image_count=image_count,
            mime=mime,
        )


@dataclass
class PageJob:
    """Everything a backend may need to parse exactly one page.

    ``content_hash`` is the hash of the *page-level* source (page bytes, or
    document content hash + page number as fallback) and feeds the
    (content-hash + parser-version) cache key (PRD T4 item 3).
    """

    doc_id: str
    page_number: int  # 1-indexed
    content_hash: str = ""
    image_bytes: bytes | None = None
    text_layer: str = ""
    page_width: float | None = None
    page_height: float | None = None
    filename: str = ""
    mime: str | None = None
    signals: PageSignals = field(default_factory=PageSignals)
    options: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if not self.signals.has_text_layer and self.text_layer.strip():
            self.signals = PageSignals.derive(
                text_layer=self.text_layer,
                image_count=self.signals.image_count or (1 if self.image_bytes else 0),
                mime=self.mime or self.signals.mime,
            )


@runtime_checkable
class ParserBackend(Protocol):
    """The stable interface every cascade backend implements."""

    name: str
    version: str

    def is_available(self) -> bool:
        """Cheap check: can this backend serve jobs right now?"""
        ...

    def can_handle(self, job: PageJob) -> bool:
        """Whether the backend is applicable for this page (before invoking)."""
        ...

    async def parse_page(self, job: PageJob) -> PageIR:
        """Parse one page into IR.  Raises ParserError subclasses on failure."""
        ...


def page_confidence(page: PageIR) -> float:
    """Effective confidence of a parsed page (0.0 when unset).

    Page-level value wins; otherwise the mean of block confidences; otherwise
    a page with any non-empty text block scores 1.0, an empty page 0.0.
    """
    if page.confidence is not None:
        return float(page.confidence)
    block_scores = [b.confidence for b in page.blocks if b.confidence is not None]
    if block_scores:
        return sum(block_scores) / len(block_scores)
    return 1.0 if any(b.text.strip() for b in page.blocks) else 0.0
