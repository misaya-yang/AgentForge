"""Pluggable, ordered parser cascade with fallback (PRD T4 item 2/3).

A cascade is an ordered list of :class:`CascadeStage` entries over registered
backends.  For each page the cascade walks the stages in order, skipping
backends that are unavailable or not applicable to the page signals, and
accepts the first result that clears the stage's ``min_confidence`` bar;
low-confidence results escalate to the next stage (the general-VLM hard-page
fallback sits last), and backend exceptions fall through.  Page results are
cached under a (content-hash, backend, version) key so a crashed or resumed
ingestion replays only unparsed pages, and a parser upgrade self-invalidates
the cache (version-keyed, canary-friendly).

Per-tenant / per-dataset configuration is a plain JSON dict — see
:func:`default_cascade_config` for the shape.
"""

from __future__ import annotations

import asyncio
import inspect
from collections.abc import Awaitable
from dataclasses import dataclass, field
from typing import Any, Protocol

from .base import (
    PageJob,
    PageParseFailed,
    ParserBackend,
    ParserError,
    ParserUnavailable,
    page_confidence,
)
from .ir import DocIR, PageIR
from .registry import ParserRegistry, register_defaults
from .versioning import cascade_bundle_version, page_cache_key


@dataclass
class CascadeStage:
    """One ordered stage: which backend, when it applies, and when to escalate."""

    backend: str
    min_confidence: float = 0.0
    require_text_layer: bool = False
    require_image: bool = False

    def applies(self, job: PageJob) -> bool:
        if self.require_text_layer and not job.signals.has_text_layer:
            return False
        has_image = job.signals.image_count > 0 or job.image_bytes is not None
        return not (self.require_image and not has_image)

    def to_dict(self) -> dict[str, Any]:
        return {
            "backend": self.backend,
            "min_confidence": self.min_confidence,
            "require_text_layer": self.require_text_layer,
            "require_image": self.require_image,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> CascadeStage:
        if "backend" not in data:
            raise ParserError("cascade stage requires a 'backend' name")
        return cls(
            backend=str(data["backend"]),
            min_confidence=float(data.get("min_confidence", 0.0)),
            require_text_layer=bool(data.get("require_text_layer", False)),
            require_image=bool(data.get("require_image", False)),
        )


@dataclass
class CascadeConfig:
    """Declarative cascade definition — JSON round-trippable for tenant config."""

    stages: list[CascadeStage] = field(default_factory=list)
    backend_options: dict[str, dict[str, Any]] = field(default_factory=dict)
    parallelism: int = 1  # pages parsed concurrently by parse_document
    allow_partial: bool = False  # tolerate failed pages, mark them in metadata

    def to_dict(self) -> dict[str, Any]:
        return {
            "stages": [s.to_dict() for s in self.stages],
            "backend_options": {k: dict(v) for k, v in self.backend_options.items()},
            "parallelism": self.parallelism,
            "allow_partial": self.allow_partial,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> CascadeConfig:
        return cls(
            stages=[CascadeStage.from_dict(s) for s in data.get("stages") or []],
            backend_options={k: dict(v) for k, v in (data.get("backend_options") or {}).items()},
            parallelism=max(1, int(data.get("parallelism", 1))),
            allow_partial=bool(data.get("allow_partial", False)),
        )


def default_cascade_config() -> CascadeConfig:
    """The PRD-default cascade: text layer → existing OCR/VLM → general VLM.

    MinerU / PaddleOCR stages are registered backends but not in the default
    order yet — per PRD §7 the engine swap itself is a Phase-3 canary, and
    adopting them is a tenant/dataset config change, not a code change.
    """
    return CascadeConfig(
        stages=[
            CascadeStage(backend="text_layer", require_text_layer=True),
            CascadeStage(backend="legacy_ocr_vlm", require_image=True, min_confidence=0.0),
            # Hard-page fallback (PRD: 通用 VLM 作硬页回退).  Always last, always
            # accepts — a page that only a general VLM can read still yields IR.
            CascadeStage(backend="general_vlm_fallback"),
        ]
    )


class PageCache(Protocol):
    """Storage for per-page IR under a version-keyed cache key."""

    def get(self, key: str) -> PageIR | None | Awaitable[PageIR | None]: ...

    def put(self, key: str, page: PageIR) -> None | Awaitable[None]: ...


class InMemoryPageCache:
    """Reference implementation; the persistent form lives in Postgres (T4 SQL)."""

    def __init__(self) -> None:
        self._pages: dict[str, PageIR] = {}

    def get(self, key: str) -> PageIR | None:
        page = self._pages.get(key)
        if page is None:
            return None
        # Hand out copies: callers annotate metadata, the cache must stay pristine.
        return PageIR.from_dict(page.to_dict())

    def put(self, key: str, page: PageIR) -> None:
        self._pages[key] = page

    def __len__(self) -> int:
        return len(self._pages)


@dataclass
class PageOutcome:
    """What served one page: the IR, the winning backend, and the fall-throughs."""

    page: PageIR
    backend: str
    attempts: list[str] = field(default_factory=list)
    from_cache: bool = False


def _stamp(page: PageIR, backend: ParserBackend) -> PageIR:
    """Guarantee parser provenance on page and blocks regardless of backend effort."""
    page.parser = page.parser or backend.name
    page.parser_version = page.parser_version or backend.version
    for block in page.blocks:
        block.parser = block.parser or backend.name
        block.parser_version = block.parser_version or backend.version
    return page


class ParserCascade:
    """Runnable cascade: config + registry → ordered prepared backends."""

    def __init__(
        self,
        config: CascadeConfig,
        registry: ParserRegistry | None = None,
        *,
        cache: PageCache | None = None,
    ) -> None:
        if not config.stages:
            raise ParserError("cascade config has no stages")
        self.config = config
        self._registry = registry or register_defaults()
        self._cache = cache
        self._stages: list[tuple[CascadeStage, ParserBackend]] = []
        for stage in config.stages:
            backend = self._registry.create(stage.backend, **config.backend_options.get(stage.backend, {}))
            self._stages.append((stage, backend))

    def bundle_version(self) -> str:
        """Aggregate (backend, version) fingerprint for receipts / reparse checks."""
        return cascade_bundle_version([(b.name, b.version) for _, b in self._stages])

    def stage_backends(self) -> list[str]:
        return [b.name for _, b in self._stages]

    def _cache_key(self, job: PageJob, backend: ParserBackend) -> str:
        return page_cache_key(
            job.content_hash or f"doc:{job.doc_id}",
            job.page_number,
            backend.name,
            backend.version,
            str(job.options.get("parser_config_hash") or ""),
        )

    async def _cache_get(self, key: str) -> PageIR | None:
        if self._cache is None:
            return None
        value = self._cache.get(key)
        if inspect.isawaitable(value):
            value = await value
        return value

    async def _cache_put(self, key: str, page: PageIR) -> None:
        if self._cache is None:
            return
        result = self._cache.put(key, page)
        if inspect.isawaitable(result):
            await result

    async def parse_page(self, job: PageJob) -> PageOutcome:
        """Parse one page, escalating/falling back through stages.

        Raises :class:`PageParseFailed` when no stage produced usable IR.
        """
        attempts: list[str] = []
        reasons: dict[str, str] = {}
        best: PageIR | None = None
        best_score = -1.0
        best_backend = ""

        for stage, backend in self._stages:
            if not stage.applies(job):
                continue
            if not backend.is_available() or not backend.can_handle(job):
                continue
            key = self._cache_key(job, backend)
            cached = await self._cache_get(key)
            if cached is not None:
                return PageOutcome(page=cached, backend=backend.name, attempts=attempts, from_cache=True)
            try:
                page = _stamp(await backend.parse_page(job), backend)
            except ParserUnavailable:
                continue
            except Exception as exc:  # noqa: BLE001 — fall-through is the contract
                attempts.append(backend.name)
                reasons[backend.name] = f"{type(exc).__name__}: {exc}"
                continue
            score = page_confidence(page)
            if score >= stage.min_confidence:
                await self._cache_put(key, page)
                return PageOutcome(page=page, backend=backend.name, attempts=attempts)
            attempts.append(backend.name)
            reasons[backend.name] = f"low-confidence {score:.2f} < {stage.min_confidence:.2f}"
            if score > best_score:
                best, best_score, best_backend = page, score, backend.name

        if best is not None:
            # Every stage either failed or came in under its bar: hand back the
            # best partial page and mark it hard for monitoring / canary re-parse.
            best.hard_page = True
            best.metadata["cascade_attempts"] = attempts
            await self._cache_put(
                self._cache_key(job, self._backend_by_name(best_backend)),
                best,
            )
            return PageOutcome(page=best, backend=best_backend, attempts=attempts)
        raise PageParseFailed(job.page_number, attempts, reasons)

    def _backend_by_name(self, name: str) -> ParserBackend:
        for _, backend in self._stages:
            if backend.name == name:
                return backend
        raise ParserError(f"backend {name!r} not in cascade")  # pragma: no cover

    async def parse_document(self, doc_id: str, jobs: list[PageJob], **doc_fields: Any) -> DocIR:
        """Parse all pages (page-level parallelism, resumable via cache).

        With ``allow_partial`` a failing page degrades to an empty hard page and
        is listed in ``metadata['failed_pages']`` instead of aborting the run —
        this is what makes 断点续传 (resume) cheap: the next pass only re-pays
        for failed/unparsed pages, everything else hits the version-keyed cache.
        """
        pages = sorted(jobs, key=lambda j: j.page_number)
        sem = asyncio.Semaphore(max(1, self.config.parallelism))

        async def _one(job: PageJob) -> PageOutcome | PageParseFailed:
            async with sem:
                try:
                    return await self.parse_page(job)
                except PageParseFailed as exc:
                    return exc

        results = await asyncio.gather(*(_one(j) for j in pages))

        doc = DocIR(
            doc_id=doc_id,
            content_hash=str(doc_fields.get("content_hash", "")),
            filename=str(doc_fields.get("filename", "")),
            mime=doc_fields.get("mime"),
            metadata=dict(doc_fields.get("metadata") or {}),
        )
        doc.metadata.setdefault("parser_bundle", self.bundle_version())
        failed: list[int] = []
        for job, result in zip(pages, results, strict=True):
            if isinstance(result, PageParseFailed):
                if not self.config.allow_partial:
                    raise result
                failed.append(job.page_number)
                page = PageIR(page_number=job.page_number, hard_page=True, metadata={"error": str(result)})
            else:
                page = result.page
                page.metadata.setdefault("served_by", result.backend)
                if result.from_cache:
                    page.metadata["from_cache"] = True
            doc.pages.append(page)
        if failed:
            doc.metadata["failed_pages"] = failed
        return doc


def build_cascade(
    config: dict[str, Any] | CascadeConfig | None,
    registry: ParserRegistry | None = None,
    *,
    cache: PageCache | None = None,
) -> ParserCascade:
    """Entry point: tenant/dataset config dict (or None for default) → cascade."""
    if config is None:
        cfg = default_cascade_config()
    elif isinstance(config, CascadeConfig):
        cfg = config
    else:
        cfg = CascadeConfig.from_dict(config)
    return ParserCascade(cfg, registry=registry, cache=cache)
