"""
Async Text Reranker with Multilingual Support.

Optimized for low latency with:
- True async HTTP calls (httpx)
- Connection pooling
- Result caching

Supported Providers:
- DashScope (gte-rerank-v2 / qwen3-rerank)
- BGE Reranker (BAAI/bge-reranker-v2-m3)
- Cohere (rerank-multilingual-v3.0)
- Local Cross-Encoder (sentence-transformers)

For Arabic-English RAG, recommended models:
- bge-reranker-v2-m3: Best multilingual, handles mixed AR-EN
- rerank-multilingual-v3.0: Good Arabic support
"""

from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import re
import threading
import time
from collections import OrderedDict
from dataclasses import dataclass

import httpx

from ...config import get_settings


# =============================================================================
# Sensitive Data Filtering for Logging
# =============================================================================
class SensitiveDataFilter(logging.Filter):
    """Filter that redacts sensitive information like API keys from log records."""

    # Patterns to redact
    _PATTERNS = [
        (re.compile(r"Bearer\s+[a-zA-Z0-9_-]+"), "Bearer ***"),
        (re.compile(r"api_key[=:]\s*[a-zA-Z0-9_-]+"), "api_key=***"),
        (re.compile(r"key[=:]\s*[a-zA-Z0-9_-]+"), "key=***"),
    ]

    def filter(self, record: logging.LogRecord) -> bool:
        """Redact sensitive data from log message."""
        if isinstance(record.msg, str):
            record.msg = self._redact(record.msg)
        if record.args:
            record.args = tuple(self._redact(str(arg)) for arg in record.args)
        return True

    def _redact(self, text: str) -> str:
        """Apply all redaction patterns."""
        for pattern, replacement in self._PATTERNS:
            text = pattern.sub(replacement, text)
        return text


logger = logging.getLogger(__name__)
logger.addFilter(SensitiveDataFilter())

# =============================================================================
# Rerank Result Cache
# =============================================================================
_rerank_cache: OrderedDict[str, list[tuple[int, float]]] = OrderedDict()
_rerank_cache_lock = threading.Lock()
_RERANK_CACHE_MAX_SIZE = 200
_provider_failure_state: dict[str, tuple[float, str]] = {}
_provider_failure_lock = threading.Lock()
_provider_success_state: dict[str, float] = {}
_provider_probe_locks: dict[str, asyncio.Lock] = {}
_provider_probe_lock = threading.Lock()
_PERMANENT_FAILURE_COOLDOWN_SECONDS = 300.0
_TRANSIENT_FAILURE_COOLDOWN_SECONDS = 30.0
_SUCCESS_HEALTH_WINDOW_SECONDS = 60.0


def _make_rerank_cache_key(
    model: str,
    query: str,
    docs: list[str],
    *,
    profile: str = "",
    top_n: int | None = None,
) -> str:
    """Generate cache key for rerank result."""
    docs_payload = json.dumps(docs, ensure_ascii=False, separators=(",", ":"))
    docs_hash = hashlib.sha256(docs_payload.encode()).hexdigest()[:32]
    query_hash = hashlib.sha256(query.encode()).hexdigest()[:32]
    profile_hash = hashlib.sha256(profile.encode()).hexdigest()[:16]
    top_n_key = "all" if top_n is None else str(int(top_n))
    return f"{model}:{profile_hash}:{top_n_key}:{query_hash}:{docs_hash}"


def _get_cached_rerank(
    model: str,
    query: str,
    docs: list[str],
    *,
    profile: str = "",
    top_n: int | None = None,
) -> list[tuple[int, float]] | None:
    """Get cached rerank result."""
    key = _make_rerank_cache_key(model, query, docs, profile=profile, top_n=top_n)
    with _rerank_cache_lock:
        if key in _rerank_cache:
            _rerank_cache.move_to_end(key)
            return _rerank_cache[key]
    return None


def _set_cached_rerank(
    model: str,
    query: str,
    docs: list[str],
    result: list[tuple[int, float]],
    *,
    profile: str = "",
    top_n: int | None = None,
) -> None:
    """Cache rerank result."""
    key = _make_rerank_cache_key(model, query, docs, profile=profile, top_n=top_n)
    with _rerank_cache_lock:
        if key in _rerank_cache:
            _rerank_cache.move_to_end(key)
        else:
            _rerank_cache[key] = result
            while len(_rerank_cache) > _RERANK_CACHE_MAX_SIZE:
                _rerank_cache.popitem(last=False)


def _credential_fingerprint(credential: str | None) -> str:
    if not credential:
        return "anonymous"
    return hashlib.sha256(credential.encode()).hexdigest()[:16]


def _make_provider_failure_key(
    provider: str,
    model: str,
    base_url: str | None = None,
    *,
    credential: str | None = None,
) -> str:
    normalized_url = (base_url or "").strip().lower()
    return f"{provider}:{model}:{normalized_url}:{_credential_fingerprint(credential)}"


def _get_provider_failure_reason(key: str) -> str | None:
    now = time.monotonic()
    with _provider_failure_lock:
        state = _provider_failure_state.get(key)
        if not state:
            return None
        reopen_at, reason = state
        if reopen_at <= now:
            _provider_failure_state.pop(key, None)
            return None
        return reason


def _mark_provider_failure(key: str, reason: str, *, cooldown_seconds: float) -> None:
    reopen_at = time.monotonic() + max(1.0, cooldown_seconds)
    with _provider_failure_lock:
        _provider_failure_state[key] = (reopen_at, reason)
        _provider_success_state.pop(key, None)


def _has_recent_provider_success(key: str) -> bool:
    now = time.monotonic()
    with _provider_failure_lock:
        deadline = _provider_success_state.get(key)
        if deadline is None:
            return False
        if deadline <= now:
            _provider_success_state.pop(key, None)
            return False
        return True


def _mark_provider_success(key: str, *, window_seconds: float = _SUCCESS_HEALTH_WINDOW_SECONDS) -> None:
    deadline = time.monotonic() + max(1.0, window_seconds)
    with _provider_failure_lock:
        _provider_success_state[key] = deadline


def _get_provider_probe_guard(key: str) -> asyncio.Lock:
    with _provider_probe_lock:
        lock = _provider_probe_locks.get(key)
        if lock is None:
            lock = asyncio.Lock()
            _provider_probe_locks[key] = lock
        return lock


def _is_permanent_http_error(exc: httpx.HTTPStatusError) -> bool:
    status = exc.response.status_code
    if status in {400, 401, 402, 403}:
        return True
    text = ""
    try:
        text = exc.response.text or ""
    except Exception:
        text = ""
    lowered = text.lower()
    permanent_markers = (
        "arrearage",
        "account is in good standing",
        "access denied",
        "invalid api key",
        "api key",
        "unauthorized",
        "forbidden",
    )
    return any(marker in lowered for marker in permanent_markers)


# =============================================================================
# HTTP Client Pool
# =============================================================================
_http_client: httpx.AsyncClient | None = None
_http_client_lock = asyncio.Lock()


async def _get_http_client() -> httpx.AsyncClient:
    """Get or create shared async HTTP client with connection pooling."""
    global _http_client
    if _http_client is None or _http_client.is_closed:
        async with _http_client_lock:
            if _http_client is None or _http_client.is_closed:
                _http_client = httpx.AsyncClient(
                    timeout=httpx.Timeout(30.0, connect=5.0),
                    limits=httpx.Limits(max_connections=20, max_keepalive_connections=10),
                )
    return _http_client


def _resolve_dashscope_rerank_url(model: str | None = None) -> str:
    """Resolve the model-compatible regional rerank endpoint.

    ``DASHSCOPE_RERANK_BASE_URL`` is an exact endpoint override. Otherwise a
    regional ``DASHSCOPE_BASE_URL`` is normalized by model. ``qwen3-rerank``
    supports both the shared Singapore native endpoint (legacy schema) and a
    regional Model Studio workspace endpoint (flat schema). It fails closed
    when neither is configured instead of silently selecting the China host.
    """

    env = get_settings()
    explicit = env.dashscope_rerank_base_url.strip()
    if explicit:
        return explicit.rstrip("/")

    normalized_model = str(model or "").strip().lower()
    qwen3_text_rerank = normalized_model == "qwen3-rerank"
    general = env.dashscope_base_url.strip()
    if not general:
        if qwen3_text_rerank:
            raise ValueError(
                "qwen3-rerank requires DASHSCOPE_RERANK_BASE_URL or a "
                "regional DASHSCOPE_BASE_URL for its configured region"
            )
        return AsyncTextReranker.DASHSCOPE_RERANK_URL

    host = general.rstrip("/")
    for suffix in (
        "/compatible-mode/v1/chat/completions",
        "/compatible-api/v1/reranks",
        "/compatible-mode/v1/reranks",
        "/compatible-mode/v1",
        "/compatible-api/v1",
        "/compatible-mode",
        "/api/v1",
    ):
        if host.endswith(suffix):
            host = host[: -len(suffix)]
            break
    if qwen3_text_rerank:
        normalized_host = host.lower()
        if ".maas.aliyuncs.com" in normalized_host:
            return f"{host}/compatible-api/v1/reranks"
        if normalized_host == "https://dashscope-intl.aliyuncs.com":
            return f"{host}/api/v1/services/rerank/text-rerank/text-rerank"
        raise ValueError(
            "qwen3-rerank requires the shared Singapore or a regional Model Studio "
            "workspace endpoint; set DASHSCOPE_RERANK_BASE_URL explicitly"
        )
    return f"{host}/api/v1/services/rerank/text-rerank/text-rerank"


@dataclass
class RerankResult:
    """Result from reranking."""

    index: int
    relevance_score: float


class AsyncTextReranker:
    """
    Async text reranker using DashScope API.

    Features:
    - True async HTTP calls (no thread blocking)
    - Connection pooling for lower latency
    - Result caching for repeated queries
    """

    # DashScope Rerank API endpoint
    DASHSCOPE_RERANK_URL = (
        "https://dashscope.aliyuncs.com/api/v1/services/rerank/text-rerank/text-rerank"
    )

    def __init__(
        self,
        api_key: str,
        model: str = "qwen3-rerank",
        base_url: str | None = None,
        request_schema: str | None = None,
        instruct: str | None = None,
    ):
        self.api_key = api_key
        self.model = model
        self.base_url = base_url or _resolve_dashscope_rerank_url(self.model)
        configured_schema = str(
            request_schema or get_settings().dashscope_rerank_request_schema or "auto"
        ).strip().lower()
        if configured_schema not in {"auto", "legacy", "flat"}:
            raise ValueError("DashScope rerank request_schema must be auto|legacy|flat")
        self.request_schema = (
            "flat"
            if configured_schema == "auto" and self.base_url.rstrip("/").endswith("/reranks")
            else "legacy"
            if configured_schema == "auto"
            else configured_schema
        )
        self.instruct = instruct or get_settings().dashscope_rerank_instruct or None
        if self.instruct and self.model not in {"qwen3-rerank", "qwen3-vl-rerank"}:
            raise ValueError(f"rerank instruct is not supported by model {self.model}")
        self._cache_profile = ":".join(
            (
                self.base_url.rstrip("/").lower(),
                self.request_schema,
                self.instruct or "",
                _credential_fingerprint(self.api_key),
            )
        )
        self._provider_failure_key = _make_provider_failure_key(
            "dashscope",
            f"{self.model}:{self.request_schema}",
            self.base_url,
            credential=self.api_key,
        )

    async def rerank(
        self,
        query: str,
        documents: list[str],
        top_n: int | None = None,
        return_documents: bool = False,
    ) -> list[RerankResult]:
        """
        Rerank documents by relevance to query.

        Args:
            query: The query text
            documents: List of document texts to rerank
            top_n: Number of top results to return (default: all)
            return_documents: Whether to return document text in results

        Returns:
            List of RerankResult sorted by relevance (highest first)
        """
        if not documents:
            return []

        # Check cache
        cached = _get_cached_rerank(
            self.model,
            query,
            documents,
            profile=self._cache_profile,
            top_n=top_n,
        )
        if cached is not None:
            logger.debug(f"Rerank cache hit for query: {query[:50]}...")
            results = [RerankResult(index=idx, relevance_score=score) for idx, score in cached]
            if top_n:
                results = results[:top_n]
            return results

        if failure_reason := _get_provider_failure_reason(self._provider_failure_key):
            raise RuntimeError(f"DashScope rerank temporarily unavailable: {failure_reason}")

        probe_lock: asyncio.Lock | None = None
        if not _has_recent_provider_success(self._provider_failure_key):
            probe_lock = _get_provider_probe_guard(self._provider_failure_key)
            await probe_lock.acquire()
            failure_reason = _get_provider_failure_reason(self._provider_failure_key)
            if failure_reason:
                probe_lock.release()
                raise RuntimeError(f"DashScope rerank temporarily unavailable: {failure_reason}")
            if _has_recent_provider_success(self._provider_failure_key):
                probe_lock.release()
                probe_lock = None

        # Build request
        if self.request_schema == "flat":
            payload = {
                "model": self.model,
                "query": query,
                "documents": documents,
            }
            if top_n:
                payload["top_n"] = top_n
            if self.instruct:
                payload["instruct"] = self.instruct
        else:
            payload = {
                "model": self.model,
                "input": {
                    "query": query,
                    "documents": documents,
                },
                "parameters": {
                    "return_documents": return_documents,
                },
            }
            if top_n:
                payload["parameters"]["top_n"] = top_n
            if self.instruct:
                payload["parameters"]["instruct"] = self.instruct

        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }

        try:
            client = await _get_http_client()
            response = await client.post(
                self.base_url,
                json=payload,
                headers=headers,
            )
            response.raise_for_status()
            data = response.json()
            _mark_provider_success(self._provider_failure_key)

            # Parse response
            results_data = data.get("results")
            if not isinstance(results_data, list):
                output = data.get("output", {})
                results_data = output.get("results", [])

            results: list[RerankResult] = []
            cache_data: list[tuple[int, float]] = []

            for item in results_data:
                idx = item.get("index", -1)
                score = item.get("relevance_score", 0.0)
                if idx >= 0:
                    results.append(RerankResult(index=idx, relevance_score=score))
                    cache_data.append((idx, score))

            # Sort by score descending
            results.sort(key=lambda x: x.relevance_score, reverse=True)
            cache_data.sort(key=lambda x: x[1], reverse=True)

            # Cache result
            _set_cached_rerank(
                self.model,
                query,
                documents,
                cache_data,
                profile=self._cache_profile,
                top_n=top_n,
            )

            logger.debug(f"Rerank completed: {len(results)} results for query: {query[:50]}...")
            return results

        except httpx.HTTPStatusError as e:
            cooldown = (
                _PERMANENT_FAILURE_COOLDOWN_SECONDS
                if _is_permanent_http_error(e)
                else _TRANSIENT_FAILURE_COOLDOWN_SECONDS
            )
            error_reason = f"HTTP {e.response.status_code}"
            try:
                response_text = (e.response.text or "").strip()
            except Exception:
                response_text = ""
            if response_text:
                error_reason = f"{error_reason}: {response_text[:240]}"
            _mark_provider_failure(
                self._provider_failure_key,
                error_reason,
                cooldown_seconds=cooldown,
            )
            logger.error(f"Rerank API error: {e.response.status_code} - {e.response.text}")
            raise
        except (httpx.TimeoutException, httpx.RequestError) as e:
            _mark_provider_failure(
                self._provider_failure_key,
                str(e),
                cooldown_seconds=_TRANSIENT_FAILURE_COOLDOWN_SECONDS,
            )
            logger.error(f"Rerank transport failure: {e}")
            raise
        except Exception as e:
            logger.error(f"Rerank failed: {e}")
            raise
        finally:
            if probe_lock is not None and probe_lock.locked():
                probe_lock.release()


# =============================================================================
# Singleton Reranker
# =============================================================================
_reranker_cache: dict[str, AsyncTextReranker] = {}
_reranker_cache_lock = threading.Lock()


def get_text_reranker(api_key: str, model: str = "qwen3-rerank") -> AsyncTextReranker:
    """Get or create cached reranker instance."""
    endpoint = _resolve_dashscope_rerank_url(model)
    credential_hash = hashlib.sha256(api_key.encode()).hexdigest()[:16]
    key = f"{credential_hash}:{model}:{endpoint}"
    with _reranker_cache_lock:
        if key not in _reranker_cache:
            _reranker_cache[key] = AsyncTextReranker(
                api_key=api_key,
                model=model,
                base_url=endpoint,
            )
        return _reranker_cache[key]


# =============================================================================
# BGE Reranker (Multilingual)
# =============================================================================


class BGEReranker:
    """
    BGE Reranker for multilingual cross-encoder reranking.

    Models:
    - BAAI/bge-reranker-v2-m3: Multilingual (100+ langs, 1024 max tokens)
    - BAAI/bge-reranker-v2-gemma: Longer context (8k tokens)

    For Arabic-English RAG, bge-reranker-v2-m3 is recommended.
    """

    MODEL_MAX_LENGTH = {
        "BAAI/bge-reranker-v2-m3": 1024,
        "BAAI/bge-reranker-v2-gemma": 8192,
        "BAAI/bge-reranker-large": 512,
    }

    def __init__(
        self,
        model: str = "BAAI/bge-reranker-v2-m3",
        device: str | None = None,
        use_fp16: bool = True,
    ):
        self.model_name = model
        self.device = device
        self.use_fp16 = use_fp16
        self._model = None
        self._initialized = False
        # Guards the model load: rerank() runs it inside asyncio.to_thread,
        # and concurrent callers must not each instantiate the model.
        self._init_lock = threading.Lock()

    def _ensure_initialized(self):
        """Lazy load the model (synchronous, call from thread context).

        Double-checked under ``_init_lock`` so concurrent worker threads load
        the model exactly once. Combined with the shared instance from
        ``get_bge_reranker`` this also keeps the cold-load cost off the
        interactive rerank budget: only the first request after process start
        pays it, instead of every request re-downloading/reloading the model
        in an orphaned worker thread.
        """
        if self._initialized:
            return

        with self._init_lock:
            if self._initialized:
                return

            try:
                from FlagEmbedding import FlagReranker

                logger.info(f"Loading BGE Reranker: {self.model_name}")
                self._model = FlagReranker(
                    self.model_name,
                    device=self.device,
                    use_fp16=self.use_fp16,
                )
                self._initialized = True
                logger.info("BGE Reranker loaded")

            except ImportError:
                raise RuntimeError(
                    "FlagEmbedding package required: pip install FlagEmbedding"
                ) from None

    async def rerank(
        self,
        query: str,
        documents: list[str],
        top_n: int | None = None,
        return_documents: bool = False,
    ) -> list[RerankResult]:
        """Rerank documents using BGE cross-encoder."""
        _ = return_documents
        if not documents:
            return []

        # Lazy-init + inference both in thread to avoid blocking the event loop
        def _init_and_compute():
            self._ensure_initialized()
            return self._model.compute_score(
                [[query, doc] for doc in documents], normalize=True
            )

        scores = await asyncio.to_thread(_init_and_compute)

        # Handle single document case
        if isinstance(scores, float):
            scores = [scores]

        # Create results
        results = [
            RerankResult(index=i, relevance_score=float(score)) for i, score in enumerate(scores)
        ]

        # Sort by score
        results.sort(key=lambda x: x.relevance_score, reverse=True)

        if top_n:
            results = results[:top_n]

        return results


_bge_reranker_cache: dict[str, BGEReranker] = {}
_bge_reranker_cache_lock = threading.Lock()


def get_bge_reranker(
    model: str = "BAAI/bge-reranker-v2-m3",
    device: str | None = None,
    use_fp16: bool = True,
) -> BGEReranker:
    """Get or create the shared BGE reranker for a model/device profile.

    The FlagEmbedding model is heavyweight and loads inside an
    ``asyncio.to_thread`` worker that a budget-cancelled await cannot stop.
    A fresh instance per retrieval therefore meant a fresh cold model load per
    request: the load latency permanently blew the interactive rerank budget
    (always degrade) and orphaned one loader thread per request. Sharing one
    instance per profile loads the model once and keeps it warm.
    """
    key = f"{model}:{device}:{use_fp16}"
    with _bge_reranker_cache_lock:
        cached = _bge_reranker_cache.get(key)
        if cached is None:
            cached = BGEReranker(model=model, device=device, use_fp16=use_fp16)
            _bge_reranker_cache[key] = cached
        return cached


# =============================================================================
# Cohere Reranker (Cloud)
# =============================================================================


class CohereReranker:
    """
    Cohere Reranker for multilingual reranking.

    Models:
    - rerank-multilingual-v3.0: Latest multilingual (100+ langs)
    - rerank-english-v3.0: English-optimized

    Note: Requires Cohere API key.
    """

    COHERE_RERANK_URL = "https://api.cohere.ai/v1/rerank"

    def __init__(
        self,
        api_key: str,
        model: str = "rerank-multilingual-v3.0",
    ):
        self.api_key = api_key
        self.model = model
        self._cache_profile = (
            f"{self.COHERE_RERANK_URL}:{_credential_fingerprint(self.api_key)}"
        )
        self._provider_failure_key = _make_provider_failure_key(
            "cohere",
            self.model,
            self.COHERE_RERANK_URL,
            credential=self.api_key,
        )

    async def rerank(
        self,
        query: str,
        documents: list[str],
        top_n: int | None = None,
        return_documents: bool = False,
    ) -> list[RerankResult]:
        """Rerank documents using Cohere API."""
        if not documents:
            return []

        # Check cache
        cached = _get_cached_rerank(
            self.model,
            query,
            documents,
            profile=self._cache_profile,
            top_n=top_n,
        )
        if cached is not None:
            results = [RerankResult(index=idx, relevance_score=score) for idx, score in cached]
            if top_n:
                results = results[:top_n]
            return results

        if failure_reason := _get_provider_failure_reason(self._provider_failure_key):
            raise RuntimeError(f"Cohere rerank temporarily unavailable: {failure_reason}")

        probe_lock: asyncio.Lock | None = None
        if not _has_recent_provider_success(self._provider_failure_key):
            probe_lock = _get_provider_probe_guard(self._provider_failure_key)
            await probe_lock.acquire()
            failure_reason = _get_provider_failure_reason(self._provider_failure_key)
            if failure_reason:
                probe_lock.release()
                raise RuntimeError(f"Cohere rerank temporarily unavailable: {failure_reason}")
            if _has_recent_provider_success(self._provider_failure_key):
                probe_lock.release()
                probe_lock = None

        payload = {
            "model": self.model,
            "query": query,
            "documents": documents,
            "return_documents": return_documents,
        }
        if top_n:
            payload["top_n"] = top_n

        headers = {
            "Authorization": f"Bearer {self.api_key}",
            "Content-Type": "application/json",
        }

        try:
            client = await _get_http_client()
            response = await client.post(
                self.COHERE_RERANK_URL,
                json=payload,
                headers=headers,
            )
            response.raise_for_status()
            data = response.json()
            _mark_provider_success(self._provider_failure_key)
        except httpx.HTTPStatusError as e:
            cooldown = (
                _PERMANENT_FAILURE_COOLDOWN_SECONDS
                if _is_permanent_http_error(e)
                else _TRANSIENT_FAILURE_COOLDOWN_SECONDS
            )
            error_reason = f"HTTP {e.response.status_code}"
            try:
                response_text = (e.response.text or "").strip()
            except Exception:
                response_text = ""
            if response_text:
                error_reason = f"{error_reason}: {response_text[:240]}"
            _mark_provider_failure(
                self._provider_failure_key,
                error_reason,
                cooldown_seconds=cooldown,
            )
            raise
        except (httpx.TimeoutException, httpx.RequestError) as e:
            _mark_provider_failure(
                self._provider_failure_key,
                str(e),
                cooldown_seconds=_TRANSIENT_FAILURE_COOLDOWN_SECONDS,
            )
            raise
        finally:
            if probe_lock is not None and probe_lock.locked():
                probe_lock.release()

        results = []
        cache_data = []

        for item in data.get("results", []):
            idx = item.get("index", -1)
            score = item.get("relevance_score", 0.0)
            if idx >= 0:
                results.append(RerankResult(index=idx, relevance_score=score))
                cache_data.append((idx, score))

        results.sort(key=lambda x: x.relevance_score, reverse=True)
        cache_data.sort(key=lambda x: x[1], reverse=True)

        _set_cached_rerank(
            self.model,
            query,
            documents,
            cache_data,
            profile=self._cache_profile,
            top_n=top_n,
        )

        return results


# =============================================================================
# Local Cross-Encoder Reranker
# =============================================================================


class LocalCrossEncoderReranker:
    """
    Local cross-encoder reranker using sentence-transformers.

    Recommended multilingual models:
    - cross-encoder/ms-marco-MiniLM-L-12-v2: Fast, English-optimized
    - jeffwan/mmarco-mMiniLMv2-L12-H384-v1: Multilingual mMARCO
    - amberoad/bert-multilingual-passage-reranking-msmarco: Arabic support

    For Arabic-English, use multilingual models.
    """

    def __init__(
        self,
        model: str = "cross-encoder/ms-marco-MiniLM-L-12-v2",
        device: str | None = None,
        max_length: int = 512,
    ):
        self.model_name = model
        self.device = device
        self.max_length = max_length
        self._model = None

    def _ensure_initialized(self):
        if self._model is not None:
            return

        try:
            from sentence_transformers import CrossEncoder

            logger.info(f"Loading CrossEncoder: {self.model_name}")
            self._model = CrossEncoder(
                self.model_name,
                device=self.device,
                max_length=self.max_length,
            )
            logger.info("CrossEncoder loaded")

        except ImportError:
            raise RuntimeError("sentence-transformers required: pip install sentence-transformers")

    async def rerank(
        self,
        query: str,
        documents: list[str],
        top_n: int | None = None,
        return_documents: bool = False,
    ) -> list[RerankResult]:
        """Rerank documents using local cross-encoder."""
        _ = return_documents
        if not documents:
            return []

        self._ensure_initialized()

        pairs = [(query, doc) for doc in documents]

        def _predict():
            return self._model.predict(pairs)

        scores = await asyncio.to_thread(_predict)

        results = [
            RerankResult(index=i, relevance_score=float(score)) for i, score in enumerate(scores)
        ]

        results.sort(key=lambda x: x.relevance_score, reverse=True)

        if top_n:
            results = results[:top_n]

        return results


# =============================================================================
# Factory Function for Rerankers
# =============================================================================


_DASHSCOPE_DEFAULT_MODEL = "qwen3-rerank"
_BGE_DEFAULT_MODEL = "BAAI/bge-reranker-v2-m3"
_COHERE_DEFAULT_MODEL = "rerank-multilingual-v3.0"
_LOCAL_DEFAULT_MODEL = "cross-encoder/ms-marco-MiniLM-L-12-v2"


def normalize_rerank_provider(provider: str | None, model: str | None = None) -> str:
    """Normalize rerank provider to a canonical value."""
    raw_provider = str(provider or "").strip().lower()
    if raw_provider in {"dashscope", "gte", "aliyun"}:
        return "dashscope"
    if raw_provider in {"bge", "flagembedding", "baai"}:
        return "bge"
    if raw_provider in {"cohere"}:
        return "cohere"
    if raw_provider in {"local", "cross-encoder", "sentence-transformers"}:
        return "local"
    if raw_provider:
        logger.warning(f"Unknown rerank provider '{provider}', infer from model")

    model_text = str(model or "").strip().lower()
    if model_text.startswith("baai/") or "bge-reranker" in model_text:
        return "bge"
    if model_text.startswith("rerank-multilingual"):
        return "cohere"
    if model_text.startswith("cross-encoder/"):
        return "local"
    return "dashscope"


def normalize_rerank_model(provider: str | None, model: str | None) -> str:
    """Normalize rerank model by provider with backward-compatible aliases."""
    normalized_provider = normalize_rerank_provider(provider, model)
    model_text = str(model or "").strip()
    model_text_lower = model_text.lower()

    if normalized_provider == "dashscope":
        if not model_text or model_text_lower == "default":
            return _DASHSCOPE_DEFAULT_MODEL
        if model_text_lower in {"gte-rerank", "gte-reranker"}:
            return "gte-rerank-v2"
        return model_text

    if normalized_provider == "bge":
        if not model_text:
            return _BGE_DEFAULT_MODEL
        if model_text_lower == "bge-reranker-v2-m3":
            return _BGE_DEFAULT_MODEL
        return model_text

    if normalized_provider == "cohere":
        return model_text or _COHERE_DEFAULT_MODEL

    if normalized_provider == "local":
        return model_text or _LOCAL_DEFAULT_MODEL

    return model_text or _DASHSCOPE_DEFAULT_MODEL


def create_reranker(
    provider: str = "dashscope",
    api_key: str | None = None,
    model: str | None = None,
    **kwargs,
):
    """
    Factory function to create reranker instances.

    Args:
        provider: Reranker provider ("dashscope", "bge", "cohere", "local")
        api_key: API key (required for cloud providers)
        model: Model name override
        **kwargs: Additional arguments for specific providers

    Returns:
        Reranker instance

    Example:
        # DashScope (default)
        reranker = create_reranker(provider="dashscope", api_key="...")

        # BGE (local)
        reranker = create_reranker(provider="bge")

        # Cohere
        reranker = create_reranker(provider="cohere", api_key="...")
    """
    normalized_provider = normalize_rerank_provider(provider, model)
    normalized_model = normalize_rerank_model(normalized_provider, model)

    if normalized_provider == "dashscope":
        if not api_key:
            raise ValueError("API key required for DashScope reranker")
        return AsyncTextReranker(
            api_key=api_key,
            model=normalized_model,
            **kwargs,
        )

    if normalized_provider == "bge":
        return get_bge_reranker(
            model=normalized_model,
            **kwargs,
        )

    if normalized_provider == "cohere":
        if not api_key:
            raise ValueError("API key required for Cohere reranker")
        return CohereReranker(
            api_key=api_key,
            model=normalized_model,
        )

    if normalized_provider == "local":
        return LocalCrossEncoderReranker(
            model=normalized_model,
            **kwargs,
        )

    if not api_key:
        raise ValueError("API key required for default DashScope reranker")
    return AsyncTextReranker(api_key=api_key, model=normalized_model)


# =============================================================================
# Multilingual Rerank Helper
# =============================================================================


async def rerank_multilingual(
    query: str,
    documents: list[str],
    top_n: int = 10,
    provider: str = "bge",
    api_key: str | None = None,
    model: str | None = None,
) -> list[tuple[int, float]]:
    """
    Convenience function for multilingual reranking.

    For Arabic-English RAG, uses BGE-Reranker-v2-m3 by default.

    Args:
        query: Query text
        documents: List of document texts
        top_n: Number of top results
        provider: Reranker provider
        api_key: API key (for cloud providers)
        model: Model override

    Returns:
        List of (index, score) tuples sorted by score descending
    """
    reranker = create_reranker(provider=provider, api_key=api_key, model=model)
    results = await reranker.rerank(query, documents, top_n=top_n)
    return [(r.index, r.relevance_score) for r in results]
