import asyncio
import sys
import time
import types

import httpx
import pytest
from knowledge_service.services.knowledge import text_reranker as tr
from knowledge_service.services.knowledge.text_reranker import (
    AsyncTextReranker,
    BGEReranker,
    _make_rerank_cache_key,
    _provider_failure_state,
    _provider_success_state,
    _rerank_cache,
    _resolve_dashscope_rerank_url,
    create_reranker,
    get_bge_reranker,
    normalize_rerank_model,
    normalize_rerank_provider,
)


def test_normalize_provider_infers_bge_from_model():
    assert normalize_rerank_provider(None, "bge-reranker-v2-m3") == "bge"
    assert normalize_rerank_provider("", "BAAI/bge-reranker-v2-m3") == "bge"


def test_normalize_dashscope_model_alias():
    assert normalize_rerank_model("dashscope", "gte-rerank") == "gte-rerank-v2"
    assert normalize_rerank_model("dashscope", None) == "qwen3-rerank"


def test_create_reranker_uses_normalized_dashscope_model():
    reranker = create_reranker(provider="dashscope", api_key="test-key", model="gte-rerank")
    assert isinstance(reranker, AsyncTextReranker)
    assert reranker.model == "gte-rerank-v2"


def test_dashscope_rerank_url_follows_general_region_host(monkeypatch):
    monkeypatch.delenv("DASHSCOPE_RERANK_BASE_URL", raising=False)
    monkeypatch.setenv(
        "DASHSCOPE_BASE_URL",
        "https://workspace-123.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
    )

    assert _resolve_dashscope_rerank_url("qwen3-rerank") == (
        "https://workspace-123.ap-southeast-1.maas.aliyuncs.com/"
        "compatible-api/v1/reranks"
    )


def test_qwen3_rerank_accepts_shared_singapore_general_host(monkeypatch):
    monkeypatch.delenv("DASHSCOPE_RERANK_BASE_URL", raising=False)
    monkeypatch.setenv(
        "DASHSCOPE_BASE_URL",
        "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
    )

    reranker = AsyncTextReranker(api_key="test-key", model="qwen3-rerank")

    assert reranker.base_url == (
        "https://dashscope-intl.aliyuncs.com/"
        "api/v1/services/rerank/text-rerank/text-rerank"
    )
    assert reranker.request_schema == "legacy"


def test_qwen3_rerank_fails_closed_without_regional_workspace_endpoint(monkeypatch):
    monkeypatch.delenv("DASHSCOPE_RERANK_BASE_URL", raising=False)
    monkeypatch.delenv("DASHSCOPE_BASE_URL", raising=False)

    with pytest.raises(ValueError, match="regional"):
        _resolve_dashscope_rerank_url("qwen3-rerank")


def test_rerank_cache_key_preserves_document_boundaries():
    joined_left = _make_rerank_cache_key("model", "query", ["a|||b", "c"])
    joined_right = _make_rerank_cache_key("model", "query", ["a", "b|||c"])

    assert joined_left != joined_right


@pytest.mark.asyncio
async def test_qwen3_flat_schema_parses_top_level_results(monkeypatch):
    captured = {}
    _rerank_cache.clear()
    _provider_failure_state.clear()
    _provider_success_state.clear()

    class _Client:
        async def post(self, url, **kwargs):
            captured.update({"url": url, **kwargs})
            return httpx.Response(
                200,
                request=httpx.Request("POST", url),
                json={
                    "model": "qwen3-rerank",
                    "results": [
                        {"index": 1, "relevance_score": 0.9},
                        {"index": 0, "relevance_score": 0.2},
                    ],
                },
            )

    async def _client():
        return _Client()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker._get_http_client",
        _client,
    )
    endpoint = "https://workspace.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1/reranks"
    reranker = AsyncTextReranker(
        api_key="test-key",
        model="qwen3-rerank",
        base_url=endpoint,
        instruct="Retrieve semantically similar text.",
    )

    results = await reranker.rerank("query", ["first", "second"], top_n=2)

    assert reranker.request_schema == "flat"
    assert captured["json"] == {
        "model": "qwen3-rerank",
        "query": "query",
        "documents": ["first", "second"],
        "top_n": 2,
        "instruct": "Retrieve semantically similar text.",
    }
    assert [result.index for result in results] == [1, 0]


@pytest.mark.asyncio
async def test_rerank_cache_does_not_reuse_smaller_top_n_response(monkeypatch):
    call_top_n = []
    _rerank_cache.clear()
    _provider_failure_state.clear()
    _provider_success_state.clear()
    all_results = [
        {"index": 0, "relevance_score": 0.9},
        {"index": 1, "relevance_score": 0.8},
    ]

    class _Client:
        async def post(self, url, **kwargs):
            top_n = kwargs["json"].get("top_n")
            call_top_n.append(top_n)
            return httpx.Response(
                200,
                request=httpx.Request("POST", url),
                json={"results": all_results[:top_n]},
            )

    async def _client():
        return _Client()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker._get_http_client",
        _client,
    )
    reranker = AsyncTextReranker(
        api_key="cache-key",
        model="qwen3-rerank",
        base_url=(
            "https://workspace.ap-southeast-1.maas.aliyuncs.com/"
            "compatible-api/v1/reranks"
        ),
    )

    first = await reranker.rerank("query", ["first", "second"], top_n=1)
    second = await reranker.rerank("query", ["first", "second"], top_n=2)

    assert [result.index for result in first] == [0]
    assert [result.index for result in second] == [0, 1]
    assert call_top_n == [1, 2]


@pytest.mark.asyncio
async def test_shared_endpoint_keeps_legacy_schema_for_qwen3(monkeypatch):
    captured = {}
    _rerank_cache.clear()
    _provider_failure_state.clear()
    _provider_success_state.clear()

    class _Client:
        async def post(self, url, **kwargs):
            captured.update({"url": url, **kwargs})
            return httpx.Response(
                200,
                request=httpx.Request("POST", url),
                json={
                    "output": {
                        "results": [
                            {"index": 0, "relevance_score": 0.8},
                            {"index": 1, "relevance_score": 0.1},
                        ]
                    }
                },
            )

    async def _client():
        return _Client()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker._get_http_client",
        _client,
    )
    endpoint = (
        "https://dashscope-intl.aliyuncs.com/"
        "api/v1/services/rerank/text-rerank/text-rerank"
    )
    reranker = AsyncTextReranker(
        api_key="test-key",
        model="qwen3-rerank",
        base_url=endpoint,
        instruct="Retrieve semantically similar text.",
    )

    results = await reranker.rerank("query", ["first", "second"], top_n=2)

    assert reranker.request_schema == "legacy"
    assert captured["json"]["input"] == {
        "query": "query",
        "documents": ["first", "second"],
    }
    assert captured["json"]["parameters"] == {
        "return_documents": False,
        "top_n": 2,
        "instruct": "Retrieve semantically similar text.",
    }
    assert [result.index for result in results] == [0, 1]


def test_gte_rerank_rejects_unsupported_instruct():
    with pytest.raises(ValueError, match="not supported"):
        AsyncTextReranker(
            api_key="test-key",
            model="gte-rerank-v2",
            instruct="Retrieve semantically similar text.",
        )


def test_create_reranker_infers_provider_from_model():
    reranker = create_reranker(provider="unknown-provider", model="bge-reranker-v2-m3")
    assert isinstance(reranker, BGEReranker)
    assert reranker.model_name == "BAAI/bge-reranker-v2-m3"


# ---------------------------------------------------------------------------
# BGE shared instance / single model load (review-rerank-budget MAJOR #2)
# ---------------------------------------------------------------------------


def _fake_flag_embedding(monkeypatch: pytest.MonkeyPatch, *, load_delay: float = 0.0):
    loads: list[str] = []

    class _FakeFlagReranker:
        def __init__(self, model_name, **_kwargs):
            if load_delay:
                time.sleep(load_delay)
            loads.append(model_name)

        def compute_score(self, pairs, **_kwargs):
            return [0.5 + 0.1 * i for i in range(len(pairs))]

    module = types.ModuleType("FlagEmbedding")
    module.FlagReranker = _FakeFlagReranker
    monkeypatch.setitem(sys.modules, "FlagEmbedding", module)
    return loads


def test_bge_reranker_is_shared_per_profile(monkeypatch: pytest.MonkeyPatch):
    monkeypatch.setattr(tr, "_bge_reranker_cache", {})
    first = get_bge_reranker(model="BAAI/fake-m3")
    second = get_bge_reranker(model="BAAI/fake-m3")
    other = get_bge_reranker(model="BAAI/fake-m3", device="cuda")
    assert first is second
    assert first is not other
    # The factory must not hand out a fresh instance per retrieval: a
    # per-request cold load permanently blew the rerank budget.
    assert create_reranker(provider="bge", model="BAAI/fake-m3") is first


@pytest.mark.asyncio
async def test_bge_model_loads_once_under_concurrent_rerank(
    monkeypatch: pytest.MonkeyPatch,
):
    loads = _fake_flag_embedding(monkeypatch, load_delay=0.05)
    reranker = BGEReranker(model="BAAI/fake-m3")

    results = await asyncio.gather(
        *(reranker.rerank("query", ["doc-a", "doc-b"]) for _ in range(4))
    )

    assert loads == ["BAAI/fake-m3"]  # one load, not one per worker thread
    assert all(len(r) == 2 for r in results)
    assert [r.index for r in results[0]] == [1, 0]  # sorted by score

    await reranker.rerank("query-2", ["doc-c"])
    assert loads == ["BAAI/fake-m3"]  # warm instance is reused, never reloaded


# ---------------------------------------------------------------------------
# Probe lock under cancellation (review-rerank-budget MAJOR #1 — disproven;
# this pins the contract that made it safe so refactors keep it)
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_cancelled_probe_waiter_leaves_the_probe_lock_consistent(
    monkeypatch: pytest.MonkeyPatch,
):
    _rerank_cache.clear()
    _provider_failure_state.clear()
    _provider_success_state.clear()
    monkeypatch.setattr(tr, "_provider_probe_locks", {})

    in_flight = 0
    max_in_flight = 0

    class _SlowClient:
        async def post(self, *_args, **_kwargs):
            nonlocal in_flight, max_in_flight
            in_flight += 1
            max_in_flight = max(max_in_flight, in_flight)
            try:
                await asyncio.sleep(0.1)
                return httpx.Response(
                    200,
                    request=httpx.Request(
                        "POST", "https://dashscope.aliyuncs.com/test"
                    ),
                    json={
                        "output": {"results": [{"index": 0, "relevance_score": 0.9}]}
                    },
                )
            finally:
                in_flight -= 1

    async def _client():
        return _SlowClient()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker._get_http_client",
        _client,
    )
    reranker = AsyncTextReranker(
        api_key="probe-key",
        model="gte-rerank-v2",
        base_url=(
            "https://dashscope.aliyuncs.com/"
            "api/v1/services/rerank/text-rerank/text-rerank"
        ),
    )

    holder = asyncio.create_task(reranker.rerank("probe-holder", ["doc"]))
    await asyncio.sleep(0.02)  # holder owns the probe lock, suspended in POST
    waiter = asyncio.create_task(reranker.rerank("probe-waiter", ["doc"]))
    await asyncio.sleep(0.02)  # waiter suspended inside probe_lock.acquire()

    waiter.cancel()
    with pytest.raises(asyncio.CancelledError):
        await waiter

    # If the cancelled waiter had released the holder's lock, this challenger
    # would acquire it immediately and probe *concurrently* with the holder.
    challenger = asyncio.create_task(reranker.rerank("probe-challenger", ["doc"]))
    await asyncio.sleep(0.02)

    results = await holder
    assert [result.index for result in results] == [0]
    await challenger

    # Probes stay serialized: never two POSTs at once.
    assert max_in_flight == 1

    guard = tr._get_provider_probe_guard(reranker._provider_failure_key)
    # The cancelled waiter must not have released the holder's lock mid-flight
    # (the holder's own finally did), and must not have leaked it locked.
    assert not guard.locked()


def test_create_reranker_requires_api_key_for_dashscope():
    with pytest.raises(ValueError):
        create_reranker(provider="dashscope", model="gte-rerank-v2")


@pytest.mark.asyncio
async def test_dashscope_reranker_opens_failure_circuit_on_permanent_error(monkeypatch):
    _provider_failure_state.clear()
    call_count = 0

    class _FailingClient:
        async def post(self, *_args, **_kwargs):
            nonlocal call_count
            call_count += 1
            request = httpx.Request("POST", "https://dashscope.aliyuncs.com/test")
            return httpx.Response(
                400,
                request=request,
                text='{"code":"Arrearage","message":"Access denied"}',
            )

    async def _fake_get_http_client():
        return _FailingClient()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker._get_http_client",
        _fake_get_http_client,
    )

    reranker = AsyncTextReranker(api_key="test-key", model="gte-rerank-v2")

    with pytest.raises(httpx.HTTPStatusError):
        await reranker.rerank("ramadan rules", ["doc1", "doc2"])

    with pytest.raises(RuntimeError, match="temporarily unavailable"):
        await reranker.rerank("ramadan rules", ["doc1", "doc2"])

    assert call_count == 1


@pytest.mark.asyncio
async def test_dashscope_failure_circuit_is_isolated_by_credential(monkeypatch):
    _rerank_cache.clear()
    _provider_failure_state.clear()
    _provider_success_state.clear()
    calls = []

    class _CredentialAwareClient:
        async def post(self, url, **kwargs):
            authorization = kwargs["headers"]["Authorization"]
            calls.append(authorization)
            request = httpx.Request("POST", url)
            if authorization == "Bearer key-a":
                return httpx.Response(400, request=request, text="invalid credential")
            return httpx.Response(
                200,
                request=request,
                json={
                    "output": {
                        "results": [{"index": 0, "relevance_score": 0.9}]
                    }
                },
            )

    async def _client():
        return _CredentialAwareClient()

    monkeypatch.setattr(
        "knowledge_service.services.knowledge.text_reranker._get_http_client",
        _client,
    )
    endpoint = "https://dashscope.aliyuncs.com/api/v1/services/rerank/text-rerank/text-rerank"
    reranker_a = AsyncTextReranker(
        api_key="key-a", model="gte-rerank-v2", base_url=endpoint
    )
    reranker_b = AsyncTextReranker(
        api_key="key-b", model="gte-rerank-v2", base_url=endpoint
    )

    with pytest.raises(httpx.HTTPStatusError):
        await reranker_a.rerank("query", ["document"])
    results_b = await reranker_b.rerank("query", ["document"])
    with pytest.raises(RuntimeError, match="temporarily unavailable"):
        await reranker_a.rerank("another query", ["document"])

    assert [result.index for result in results_b] == [0]
    assert calls == ["Bearer key-a", "Bearer key-b"]
