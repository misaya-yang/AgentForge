"""T4 cascade: ordered selection, confidence escalation, exception fallback,
version-keyed caching / resume, tenant config, document assembly."""

from __future__ import annotations

from typing import Any

import pytest
from knowledge_service.services.knowledge.parsing import (
    Block,
    BlockType,
    CascadeConfig,
    CascadeStage,
    DocIR,
    InMemoryPageCache,
    PageIR,
    PageJob,
    PageParseFailed,
    ParserCascade,
    ParserError,
    ParserRegistry,
    build_cascade,
    default_cascade_config,
    page_confidence,
)
from knowledge_service.services.knowledge.parsing.registry import UnknownBackend
from knowledge_service.services.knowledge.parsing.versioning import (
    cascade_bundle_version,
    needs_reparse,
    page_cache_key,
)


class FakeBackend:
    """Configurable stub backend registered per-test via a fresh registry."""

    def __init__(
        self,
        name: str = "fake",
        version: str = "1",
        *,
        confidence: float | None = 1.0,
        raises: Exception | None = None,
        available: bool = True,
        handle: bool = True,
        text: str = "hello",
    ) -> None:
        self.name = name
        self.version = version
        self.confidence = confidence
        self.raises = raises
        self.available = available
        self.handle = handle
        self.text = text
        self.calls = 0

    def is_available(self) -> bool:
        return self.available

    def can_handle(self, _job: PageJob) -> bool:
        return self.handle

    async def parse_page(self, job: PageJob) -> PageIR:
        self.calls += 1
        if self.raises is not None:
            raise self.raises
        page = PageIR(
            page_number=job.page_number,
            blocks=[Block(block_id=f"{self.name}-{job.page_number}", type=BlockType.TEXT, text=self.text, order=0)],
            parser=self.name,
            parser_version=self.version,
        )
        page.confidence = self.confidence
        return page


def _registry_with(**backends: FakeBackend) -> ParserRegistry:
    reg = ParserRegistry()
    for name, backend in backends.items():
        reg.register(name, lambda *, _b=backend, **_opts: _b)
    return reg


def _job(page: int = 1, text_layer: str = "", image: bool = False, **kw: Any) -> PageJob:
    return PageJob(
        doc_id="d1",
        page_number=page,
        content_hash=kw.pop("content_hash", "hash-1"),
        text_layer=text_layer,
        image_bytes=b"png" if image else None,
        **kw,
    )


def _stage(backend: str, **kw: Any) -> CascadeStage:
    return CascadeStage(backend=backend, **kw)


# ---------------------------------------------------------------- selection


async def test_first_acceptable_stage_wins_later_stages_untouched():
    a, b = FakeBackend(name="a"), FakeBackend(name="b")
    cascade = ParserCascade(
        CascadeConfig(stages=[_stage("a"), _stage("b")]),
        _registry_with(a=a, b=b),
    )
    outcome = await cascade.parse_page(_job())
    assert outcome.backend == "a"
    assert outcome.page.parser == "a"
    assert a.calls == 1 and b.calls == 0
    assert outcome.attempts == []


async def test_router_predicates_skip_stages():
    text_be = FakeBackend(name="text", confidence=1.0)
    ocr_be = FakeBackend(name="ocr", confidence=0.9)
    cascade = ParserCascade(
        CascadeConfig(
            stages=[
                _stage("text", require_text_layer=True),
                _stage("ocr", require_image=True),
            ]
        ),
        _registry_with(text=text_be, ocr=ocr_be),
    )
    # Page with text layer only: stage 2's require_image skips it.
    outcome = await cascade.parse_page(_job(text_layer="  real text  "))
    assert outcome.backend == "text" and ocr_be.calls == 0
    # Scanned page (image, no text layer): stage 1 skipped despite being first.
    outcome2 = await cascade.parse_page(_job(text_layer="", image=True))
    assert outcome2.backend == "ocr" and text_be.calls == 1


async def test_unavailable_or_unhandled_backends_are_skipped():
    down = FakeBackend(name="down", available=False)
    blind = FakeBackend(name="blind", handle=False)
    up = FakeBackend(name="up")
    cascade = ParserCascade(
        CascadeConfig(stages=[_stage("down"), _stage("blind"), _stage("up")]),
        _registry_with(down=down, blind=blind, up=up),
    )
    outcome = await cascade.parse_page(_job())
    assert outcome.backend == "up"
    assert down.calls == 0 and blind.calls == 0


async def test_confidence_below_bar_escalates_to_next_stage():
    weak = FakeBackend(name="weak", confidence=0.4)
    strong = FakeBackend(name="strong", confidence=0.95)
    cascade = ParserCascade(
        CascadeConfig(stages=[_stage("weak", min_confidence=0.8), _stage("strong", min_confidence=0.8)]),
        _registry_with(weak=weak, strong=strong),
    )
    outcome = await cascade.parse_page(_job())
    assert outcome.backend == "strong"
    assert outcome.attempts == ["weak"]  # the escalation trail is reportable


async def test_backend_exception_falls_through_with_reason():
    boom = FakeBackend(name="boom", raises=RuntimeError("gpu on fire"))
    good = FakeBackend(name="good")
    cascade = ParserCascade(
        CascadeConfig(stages=[_stage("boom"), _stage("good")]),
        _registry_with(boom=boom, good=good),
    )
    outcome = await cascade.parse_page(_job())
    assert outcome.backend == "good"
    assert outcome.attempts == ["boom"]


async def test_all_stages_fail_raises_page_parse_failed():
    a = FakeBackend(name="a", raises=RuntimeError("x"))
    b = FakeBackend(name="b", raises=ValueError("y"))
    cascade = ParserCascade(
        CascadeConfig(stages=[_stage("a"), _stage("b")]),
        _registry_with(a=a, b=b),
    )
    with pytest.raises(PageParseFailed) as excinfo:
        await cascade.parse_page(_job(page=7))
    err = excinfo.value
    assert err.page_number == 7
    assert err.attempts == ["a", "b"]
    assert "RuntimeError" in err.reasons["a"] and "ValueError" in err.reasons["b"]


async def test_all_low_confidence_returns_best_page_marked_hard():
    a = FakeBackend(name="a", confidence=0.2, text="garbled")
    b = FakeBackend(name="b", confidence=0.6, text="mostly ok")
    c = FakeBackend(name="c", confidence=0.3)
    cascade = ParserCascade(
        CascadeConfig(
            stages=[_stage("a", min_confidence=0.9), _stage("b", min_confidence=0.9), _stage("c", min_confidence=0.9)]
        ),
        _registry_with(a=a, b=b, c=c),
    )
    outcome = await cascade.parse_page(_job())
    assert outcome.page.hard_page is True
    assert outcome.backend == "b"  # best partial wins
    assert outcome.page.metadata["cascade_attempts"] == ["a", "b", "c"]


# ---------------------------------------------------------------- caching


async def test_page_cache_hit_skips_backend_and_flags():
    backend = FakeBackend(name="a")
    cache = InMemoryPageCache()
    config = CascadeConfig(stages=[_stage("a")])
    first = await ParserCascade(config, _registry_with(a=backend), cache=cache).parse_page(_job())
    assert not first.from_cache and len(cache) == 1
    second = await ParserCascade(config, _registry_with(a=backend), cache=cache).parse_page(_job())
    assert second.from_cache and second.backend == "a"
    assert backend.calls == 1  # resume: only unparsed pages are re-paid


async def test_backend_version_bump_invalidates_cache_key():
    old = FakeBackend(name="engine", version="1.0")
    new = FakeBackend(name="engine", version="2.0")
    cache = InMemoryPageCache()
    await ParserCascade(CascadeConfig(stages=[_stage("engine")]), _registry_with(engine=old), cache=cache).parse_page(_job())
    assert old.calls == 1
    outcome = await ParserCascade(CascadeConfig(stages=[_stage("engine")]), _registry_with(engine=new), cache=cache).parse_page(_job())
    assert new.calls == 1 and not outcome.from_cache  # parser upgrade ⇒ cache miss
    assert page_cache_key("h", 1, "engine", "1.0") != page_cache_key("h", 1, "engine", "2.0")


async def test_cache_hands_out_copies_not_shared_objects():
    backend = FakeBackend(name="a")
    cache = InMemoryPageCache()
    cascade = ParserCascade(CascadeConfig(stages=[_stage("a")]), _registry_with(a=backend), cache=cache)
    await cascade.parse_page(_job())
    hit = await cascade.parse_page(_job())
    hit.page.metadata["mutated"] = True
    again = await cascade.parse_page(_job())
    assert "mutated" not in again.page.metadata


# ---------------------------------------------------------------- document


async def test_parse_document_assembles_ordered_pages_and_bundle():
    a = FakeBackend(name="a")
    cascade = ParserCascade(CascadeConfig(stages=[_stage("a")]), _registry_with(a=a))
    doc = await cascade.parse_document(
        "doc-9",
        [_job(page=2), _job(page=1), _job(page=3)],
        content_hash="h",
        filename="x.pdf",
    )
    assert isinstance(doc, DocIR)
    assert [p.page_number for p in doc.pages] == [1, 2, 3]
    assert doc.metadata["parser_bundle"] == cascade.bundle_version()
    assert all(p.metadata.get("served_by") == "a" for p in doc.pages)


async def test_parse_document_allow_partial_marks_failed_pages():
    class _BoomOnSecond(FakeBackend):
        async def parse_page(self, job: PageJob) -> PageIR:
            if job.page_number == 2:
                raise RuntimeError("bad scan")
            return await super().parse_page(job)

    be = _BoomOnSecond(name="a")
    cascade = ParserCascade(CascadeConfig(stages=[_stage("a")], allow_partial=True), _registry_with(a=be))
    doc = await cascade.parse_document("d", [_job(page=1), _job(page=2), _job(page=3)], content_hash="h")
    assert doc.metadata["failed_pages"] == [2]
    assert doc.page(2).hard_page is True
    assert "bad scan" in doc.page(2).metadata["error"]
    assert doc.page(1).blocks and doc.page(3).blocks


async def test_parse_document_without_allow_partial_raises():
    be = FakeBackend(name="a", raises=RuntimeError("nope"))
    cascade = ParserCascade(CascadeConfig(stages=[_stage("a")]), _registry_with(a=be))
    with pytest.raises(PageParseFailed):
        await cascade.parse_document("d", [_job(page=1)], content_hash="h")


async def test_resume_after_failure_only_repays_failed_page():
    calls: dict[int, int] = {}

    class Flaky(FakeBackend):
        async def parse_page(self, job: PageJob) -> PageIR:
            calls[job.page_number] = calls.get(job.page_number, 0) + 1
            if job.page_number == 1 and calls[job.page_number] == 1:
                raise RuntimeError("crash before checkpoint")
            return await super().parse_page(job)

    backend = Flaky(name="a")
    cascade = ParserCascade(
        CascadeConfig(stages=[_stage("a")], allow_partial=True),
        _registry_with(a=backend),
        cache=InMemoryPageCache(),
    )
    jobs = [_job(page=n) for n in (1, 2, 3)]
    first = await cascade.parse_document("d", jobs, content_hash="h")
    assert first.metadata["failed_pages"] == [1]
    second = await cascade.parse_document("d", jobs, content_hash="h")
    assert "failed_pages" not in second.metadata
    assert calls == {1: 2, 2: 1, 3: 1}  # pages 2/3 served from cache on resume


# ---------------------------------------------------------------- config/registry


def test_default_config_json_round_trip():
    cfg = default_cascade_config()
    restored = CascadeConfig.from_dict(cfg.to_dict())
    assert restored.to_dict() == cfg.to_dict()
    assert [s.backend for s in restored.stages] == ["text_layer", "legacy_ocr_vlm", "general_vlm_fallback"]


def test_unknown_backend_in_config_rejects_at_build_time():
    with pytest.raises(UnknownBackend):
        build_cascade({"stages": [{"backend": "nonexistent_engine"}]})


def test_empty_config_rejected():
    with pytest.raises(ParserError):
        ParserCascade(CascadeConfig(stages=[]), _registry_with(a=FakeBackend()))


def test_backend_options_reach_factories():
    reg = ParserRegistry()
    reg.register("kw", lambda **opts: FakeBackend(name="kw", version=str(opts.get("version", "?"))))
    cascade = ParserCascade(
        CascadeConfig(stages=[_stage("kw")], backend_options={"kw": {"version": "42"}}),
        reg,
    )
    assert cascade.bundle_version() == cascade_bundle_version([("kw", "42")])


def test_bundle_version_changes_with_stage_set():
    v1 = cascade_bundle_version([("a", "1"), ("b", "1")])
    v2 = cascade_bundle_version([("a", "1"), ("b", "2")])
    assert v1 != v2
    assert needs_reparse(v1, v2)
    assert not needs_reparse(v1, v1)
    assert needs_reparse(None, v1)


def test_page_confidence_semantics():
    empty = PageIR(page_number=1)
    assert page_confidence(empty) == 0.0
    blocky = PageIR(
        page_number=1,
        blocks=[
            Block(block_id="x", type=BlockType.TEXT, text="t", confidence=0.6),
            Block(block_id="y", type=BlockType.TEXT, text="t", confidence=0.8),
        ],
    )
    assert page_confidence(blocky) == pytest.approx(0.7)
    blocky.confidence = 0.95
    assert page_confidence(blocky) == 0.95
    textless = PageIR(page_number=1, blocks=[Block(block_id="x", type=BlockType.TEXT, text="  ")])
    assert page_confidence(textless) == 0.0
