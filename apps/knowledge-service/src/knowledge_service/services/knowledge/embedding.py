from __future__ import annotations

import asyncio
import hashlib
import json
import logging
import math
import re
from abc import ABC, abstractmethod
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from typing import Any
from urllib.parse import urlsplit, urlunsplit

import httpx
from ai_gateway_core.knowledge import (
    MULTIMODAL_EMBEDDING_MODELS as MULTIMODAL_EMBEDDING_MODELS,
)
from ai_gateway_core.knowledge import (
    is_multimodal_embedding_model as is_multimodal_embedding_model,
)
from cachetools import TTLCache


async def _await_bounded_thread(
    fn,
    /,
    *args,
    timeout: float,
    semaphore: asyncio.Semaphore,
    **kwargs,
):
    """Run ``fn`` in a worker thread with a timeout that does not leak slots.

    ``asyncio.wait_for`` cannot cancel the executor thread. Keep the semaphore
    until that thread actually finishes so a hung provider cannot spawn more
    workers than the concurrency cap.
    """
    loop = asyncio.get_running_loop()
    started_at = loop.time()
    await asyncio.wait_for(semaphore.acquire(), timeout=timeout)
    remaining = timeout - (loop.time() - started_at)
    if remaining <= 0:
        semaphore.release()
        raise asyncio.TimeoutError
    released = False

    def _release(_=None) -> None:
        nonlocal released
        if not released:
            released = True
            semaphore.release()

    try:
        fut = loop.run_in_executor(None, lambda: fn(*args, **kwargs))
    except BaseException:
        _release()
        raise

    # Register before awaiting so every terminal path, including caller
    # cancellation, releases exactly once when the executor work is truly done.
    # A timed-out or cancelled await must not make capacity available while its
    # non-cancellable provider thread is still running.
    fut.add_done_callback(_release)
    return await asyncio.wait_for(asyncio.shield(fut), timeout=remaining)


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


# Apply sensitive data filter to embedding logger
logger = logging.getLogger(__name__)
logger.addFilter(SensitiveDataFilter())


class EmbeddingError(RuntimeError):
    pass


@dataclass(frozen=True)
class EmbeddingConfig:
    provider: str
    model: str
    api_key: str | None = None
    base_url: str | None = None
    timeout_seconds: float = 30.0
    extra: dict[str, Any] = None

    def __post_init__(self):
        object.__setattr__(self, "extra", self.extra or {})


class BaseEmbedding(ABC):
    """Unified embedding interface (text-first, multimodal-ready)."""

    def __init__(
        self,
        provider: str,
        model: str,
        dimension: int | None = None,
    ):
        self.provider = provider
        self.model = model
        self._dimension: int | None = dimension
        self._configured_cache_profile: dict[str, Any] = {}

    def _configure_cache_profile(self, config: EmbeddingConfig) -> None:
        """Record the effective config fields that define this embedding space."""
        self._configured_cache_profile = _build_embedding_cache_profile(
            provider=self.provider,
            model=self.model,
            dimension=self._dimension,
            base_url=getattr(self, "base_url", None) or config.base_url,
            api_key=getattr(self, "api_key", None) or config.api_key,
        )

    def _query_cache_profile(self, text_type: str) -> dict[str, Any]:
        """Build the current cache profile for one embedding request."""
        profile = dict(self._configured_cache_profile)
        profile.update(
            {
                "provider": _canonical_provider(self.provider),
                "model": self.model or "",
                "dimension": self._dimension,
                "text_type": text_type,
            }
        )

        current_endpoint = _endpoint_fingerprint(getattr(self, "base_url", None))
        if current_endpoint or "endpoint" not in profile:
            profile["endpoint"] = current_endpoint

        current_credential = _credential_fingerprint(getattr(self, "api_key", None))
        if current_credential or "credential" not in profile:
            profile["credential"] = current_credential
        return profile

    @property
    def dimension(self) -> int:
        if self._dimension is None:
            raise EmbeddingError(
                "Embedding dimension is unknown. Provide it in config or call embed_texts() once."
            )
        return self._dimension

    @property
    def supports_multimodal(self) -> bool:
        return False

    async def close(self) -> None:
        return None

    def _validate_vectors(
        self,
        vectors: list[list[float]],
        expected_count: int,
    ) -> None:
        if len(vectors) != expected_count:
            raise EmbeddingError(
                f"{self.provider} embedding count mismatch: "
                f"expected {expected_count}, received {len(vectors)}"
            )
        if self._dimension is None and vectors:
            self._dimension = len(vectors[0])
        invalid_dimensions = {len(vector) for vector in vectors if len(vector) != self.dimension}
        if invalid_dimensions:
            raise EmbeddingError(
                f"{self.provider} embedding dimension mismatch: "
                f"expected {self.dimension}, received {sorted(invalid_dimensions)}"
            )

    async def embed_query(self, query: str) -> list[float]:
        cache_profile = self._query_cache_profile(text_type="query")
        # Check cache first
        cached = get_cached_query_embedding(
            self.provider,
            self.model,
            query,
            profile=cache_profile,
        )
        if cached is not None:
            return cached
        # Compute embedding
        vectors = await self.embed_texts([query], text_type="query")
        result = vectors[0]
        # Cache the result
        set_cached_query_embedding(
            self.provider,
            self.model,
            query,
            result,
            profile=self._query_cache_profile(text_type="query"),
        )
        return result

    async def embed_documents(self, texts: Sequence[str]) -> list[list[float]]:
        return await self.embed_texts(list(texts), text_type="document")

    @abstractmethod
    async def embed_texts(
        self, texts: list[str], text_type: str | None = None
    ) -> list[list[float]]:
        raise NotImplementedError

    async def embed_images(self, images: list[bytes]) -> list[list[float]]:  # pragma: no cover
        del images
        raise EmbeddingError(f"{self.provider}:{self.model} does not support image embedding")


class GeminiEmbedding(BaseEmbedding):
    """
    Google Gemini Embedding API adapter.

    Features:
    - Task type optimization (RETRIEVAL_DOCUMENT / RETRIEVAL_QUERY)
    - Matryoshka variable dimensions (768/1024/1536/3072)
    - Batch embedding support
    - Connection pooling via httpx

    API Reference: https://ai.google.dev/gemini-api/docs/embeddings

    Usage:
        embedder = GeminiEmbedding(api_key="your-key", dimension=1024)

        # For indexing documents
        doc_vectors = await embedder.embed_texts(texts, text_type="document")

        # For search queries
        query_vector = await embedder.embed_query("search query")
    """

    MODEL_DIMENSIONS: dict[str, int] = {
        "gemini-embedding-001": 768,  # Default, but configurable up to 3072
        "text-embedding-004": 768,  # Vertex AI model
    }

    # Task types for retrieval optimization
    TASK_TYPES: dict[str, str] = {
        "query": "RETRIEVAL_QUERY",
        "document": "RETRIEVAL_DOCUMENT",
        "similarity": "SEMANTIC_SIMILARITY",
        "classification": "CLASSIFICATION",
        "clustering": "CLUSTERING",
    }

    # Gemini API endpoint
    GEMINI_API_URL = "https://generativelanguage.googleapis.com/v1beta/models"

    # API limits
    MAX_BATCH_SIZE = 100  # Gemini supports up to 100 texts per batch
    MAX_TOKENS_PER_TEXT = 2048
    MAX_RETRIES = 3
    RETRY_BASE_DELAY = 1.0

    def __init__(
        self,
        api_key: str,
        model: str = "gemini-embedding-001",
        dimension: int = 1024,
        base_url: str | None = None,
        timeout_seconds: float = 30.0,
    ):
        """
        Initialize Gemini Embedding.

        Args:
            api_key: Google AI API key
            model: Model name (default: gemini-embedding-001)
            dimension: Output dimension (768, 1024, 1536, or 3072)
            base_url: Optional API base URL override
            timeout_seconds: Request timeout
        """
        super().__init__(provider="gemini", model=model, dimension=dimension)
        if not api_key:
            raise EmbeddingError("Gemini API key is required")

        self.api_key = api_key
        self.base_url = _effective_embedding_endpoint("gemini", base_url)
        self.output_dimension = dimension
        self._client = httpx.AsyncClient(
            timeout=httpx.Timeout(timeout_seconds, connect=5.0),
            limits=httpx.Limits(max_connections=20, max_keepalive_connections=10),
        )

    async def close(self) -> None:
        """Close the HTTP client."""
        await self._client.aclose()

    async def embed_query(self, query: str) -> list[float]:
        """Embed a query using RETRIEVAL_QUERY task type."""
        cache_profile = self._query_cache_profile(text_type="query")
        # Check cache first
        cached = get_cached_query_embedding(
            self.provider,
            self.model,
            query,
            profile=cache_profile,
        )
        if cached is not None:
            return cached

        vectors = await self.embed_texts([query], text_type="query")
        result = vectors[0]

        # Cache the result
        set_cached_query_embedding(
            self.provider,
            self.model,
            query,
            result,
            profile=self._query_cache_profile(text_type="query"),
        )
        return result

    async def embed_texts(
        self,
        texts: list[str],
        text_type: str | None = None,
    ) -> list[list[float]]:
        """
        Embed texts using Gemini API.

        Args:
            texts: List of text strings to embed
            text_type: "query" for RETRIEVAL_QUERY, "document" for RETRIEVAL_DOCUMENT

        Returns:
            List of embedding vectors
        """
        if not texts:
            return []

        # Determine task type
        task_type = self.TASK_TYPES.get(text_type or "document", "RETRIEVAL_DOCUMENT")

        # Process in batches
        all_vectors: list[list[float]] = []

        for i in range(0, len(texts), self.MAX_BATCH_SIZE):
            batch = texts[i : i + self.MAX_BATCH_SIZE]
            batch_info = f"batch {i // self.MAX_BATCH_SIZE + 1}"

            vectors = await self._embed_batch_with_retry(batch, task_type, batch_info)
            all_vectors.extend(vectors)

        return all_vectors

    async def _embed_batch_with_retry(
        self,
        texts: list[str],
        task_type: str,
        batch_info: str,
    ) -> list[list[float]]:
        """Embed a batch of texts with retry logic."""
        import logging
        import re

        logger = logging.getLogger(__name__)

        last_error: Exception | None = None

        for attempt in range(self.MAX_RETRIES):
            try:
                return await self._embed_batch(texts, task_type)

            except EmbeddingError as e:
                if "429" in str(e) or "500" in str(e) or "503" in str(e):
                    last_error = e
                    logger.warning(
                        f"Gemini embedding retryable error ({batch_info}) "
                        f"attempt {attempt + 1}/{self.MAX_RETRIES}: {e}"
                    )
                else:
                    raise

            except Exception as exc:
                last_error = EmbeddingError(f"Gemini embedding error ({batch_info}): {exc}")
                logger.warning(
                    f"Gemini embedding error ({batch_info}) "
                    f"attempt {attempt + 1}/{self.MAX_RETRIES}: {exc}"
                )

            # Exponential backoff
            if attempt < self.MAX_RETRIES - 1:
                import random

                delay = self.RETRY_BASE_DELAY * (2**attempt)

                # Respect server-provided retry delay if present (e.g., 429 quota errors)
                retry_match = re.search(r'"retryDelay"\s*:\s*"(\d+)s"', str(last_error or ""))
                if retry_match:
                    delay = max(delay, float(retry_match.group(1)))

                delay = delay + random.uniform(0.0, min(0.3, delay))
                await asyncio.sleep(delay)

        raise last_error or EmbeddingError(
            f"Gemini embedding failed after {self.MAX_RETRIES} attempts ({batch_info})"
        )

    async def _embed_batch(
        self,
        texts: list[str],
        task_type: str,
    ) -> list[list[float]]:
        """Call Gemini embedContent API for a batch of texts."""
        # Build request for batch embedding
        # Gemini API: POST /v1beta/models/{model}:batchEmbedContents
        url = f"{self.base_url}/{self.model}:batchEmbedContents"

        # Build requests array
        requests = []
        for text in texts:
            req = {
                "model": f"models/{self.model}",
                "content": {"parts": [{"text": text}]},
                "taskType": task_type,
            }
            if self.output_dimension:
                req["outputDimensionality"] = self.output_dimension
            requests.append(req)

        payload = {"requests": requests}

        response = await self._client.post(
            url,
            json=payload,
            params={"key": self.api_key},
            headers={"Content-Type": "application/json"},
        )

        if response.status_code >= 400:
            raise EmbeddingError(f"Gemini API error: {response.status_code} - {response.text}")

        data = response.json()

        # Parse response
        embeddings = data.get("embeddings", [])
        if not embeddings:
            raise EmbeddingError("Gemini API returned no embeddings")

        vectors: list[list[float]] = []
        for emb in embeddings:
            values = emb.get("values", [])
            if not values:
                raise EmbeddingError("Gemini embedding missing values")
            vectors.append(values)

        if self._dimension is None and vectors:
            self._dimension = len(vectors[0])

        return vectors


class DashScopeEmbedding(BaseEmbedding):
    """DashScope text embeddings adapter.

    Uses the official `dashscope` SDK when installed, and runs calls in a thread
    to avoid blocking the event loop.

    Features:
    - Retry with exponential backoff (3 attempts)
    - Configurable HTTP timeout
    - Graceful degradation on API errors

    API Limits:
    - Max 25 texts per batch
    - Max 2048 tokens per text for v1/v2, 8192 for v3/v4
    - Max ~6000 characters per text (safe estimate)
    """

    MODEL_DIMENSIONS: dict[str, int] = {
        "text-embedding-v1": 1536,
        "text-embedding-v2": 1536,
        "text-embedding-v3": 1024,
        "text-embedding-v4": 1024,  # DashScope v4 default dimension
    }

    DASHSCOPE_API_URL = "https://dashscope.aliyuncs.com/api/v1"

    # Max characters per text (conservative: ~2.5 chars/token)
    # DashScope v1/v2: max 2048 tokens, v3: max 8192 tokens
    # But in practice, shorter is safer to avoid InvalidParameter errors
    MODEL_MAX_CHARS: dict[str, int] = {
        "text-embedding-v1": 4000,
        "text-embedding-v2": 4000,
        "text-embedding-v3": 8000,  # More conservative for v3
        "text-embedding-v4": 8000,
    }

    # DashScope API limit: max 10 texts per batch for v3/v4 (observed 400 when >10)
    # Use 10 for safety across models to avoid InvalidParameter errors.

    # Retry configuration
    MAX_RETRIES = 5
    RETRY_BASE_DELAY = 0.5  # seconds (reduced from 1.0)
    REQUEST_TIMEOUT = 60  # seconds for HTTP request
    MAX_BATCH_SIZE = 10  # DashScope safe batch limit

    def __init__(
        self,
        model: str,
        api_key: str,
        dimension: int | None = None,
        base_url: str | None = None,
        request_timeout_s: float = 60.0,
        max_concurrent: int = 5,
    ):
        # Try to lookup dimension, fallback to 1024 if model not recognized
        dim = (
            dimension
            or self.MODEL_DIMENSIONS.get(model)
            or self.MODEL_DIMENSIONS.get(model.lower())
            or 1024
        )
        super().__init__(provider="dashscope", model=model, dimension=dim)
        self._requested_dimension = dimension
        if not api_key:
            raise EmbeddingError("DashScope api_key is required")
        self.api_key = api_key
        self.base_url = _effective_embedding_endpoint("dashscope", base_url)
        self.request_timeout_s = max(float(request_timeout_s), 1.0)
        self._semaphore = asyncio.Semaphore(max(int(max_concurrent), 1))
        self.max_chars = (
            self.MODEL_MAX_CHARS.get(model) or self.MODEL_MAX_CHARS.get(model.lower()) or 6000
        )
        try:
            from dashscope import TextEmbedding  # type: ignore

            self._TextEmbedding = TextEmbedding
        except Exception as exc:  # pragma: no cover
            raise EmbeddingError(
                "dashscope package is required for DashScopeEmbedding (pip install dashscope)"
            ) from exc

    def _truncate_text(self, text: str) -> str:
        """Truncate text to max allowed characters."""
        if not text:
            return ""
        text = text.strip()
        if len(text) > self.max_chars:
            # Truncate at word boundary if possible
            truncated = text[: self.max_chars]
            last_space = truncated.rfind(" ")
            if last_space > self.max_chars * 0.8:  # Keep at least 80% of content
                truncated = truncated[:last_space]
            return truncated
        return text

    def _sanitize_text(self, text: str) -> str:
        """Clean text to avoid API errors."""
        if not text:
            return "empty"  # DashScope doesn't accept empty strings

        # Remove NULL bytes and control characters
        text = text.replace("\x00", "")
        # Remove other control characters except newline/tab
        text = "".join(c if c.isprintable() or c in "\n\t" else " " for c in text)

        # Normalize line endings
        text = text.replace("\r\n", "\n").replace("\r", "\n")

        # Collapse multiple newlines and spaces
        text = re.sub(r"\n{3,}", "\n\n", text)
        text = re.sub(r"[ \t]{2,}", " ", text)
        text = re.sub(r"\n[ \t]+", "\n", text)  # Remove leading spaces on lines

        # Remove very long sequences of repeated characters (often from PDF extraction errors)
        text = re.sub(r"(.)\1{20,}", r"\1\1\1", text)

        text = text.strip()
        return text if text else "empty"

    async def _call_with_retry(
        self, batch: list[str], batch_info: str, **kwargs: Any
    ) -> list[list[float]]:
        """Call DashScope API with retry and exponential backoff."""
        import logging

        logger = logging.getLogger(__name__)

        last_error: Exception | None = None
        loop = asyncio.get_running_loop()
        deadline = loop.time() + self.request_timeout_s

        for attempt in range(self.MAX_RETRIES):
            remaining = deadline - loop.time()
            if remaining <= 0:
                break
            try:
                resp = await _await_bounded_thread(
                    self._TextEmbedding.call,
                    model=self.model,
                    input=batch,
                    api_key=self.api_key,
                    base_address=self.base_url,
                    dimension=self._requested_dimension,
                    **kwargs,
                    timeout=remaining,
                    semaphore=self._semaphore,
                )

                status_code = int(getattr(resp, "status_code", 0) or 0)
                if status_code and status_code >= 400:
                    code = getattr(resp, "code", "") or ""
                    message = getattr(resp, "message", "") or ""
                    # Check if retryable (rate limit or server error)
                    if status_code in (429, 500, 502, 503, 504):
                        raise EmbeddingError(
                            f"DashScope retryable error ({batch_info}): {status_code} {code} {message}"
                        )
                    # Non-retryable error
                    raise EmbeddingError(
                        f"DashScope embeddings failed ({batch_info}): {status_code} {code} {message}"
                    )

                output = getattr(resp, "output", None)
                vectors = _parse_dashscope_embeddings(output)
                return vectors

            except asyncio.TimeoutError:
                last_error = EmbeddingError(
                    f"DashScope embedding timeout ({batch_info}): "
                    f"exceeded {self.request_timeout_s}s total deadline"
                )
                logger.warning(
                    f"DashScope embedding timeout on attempt {attempt + 1}/{self.MAX_RETRIES} "
                    f"({batch_info}), retrying..."
                )
            except EmbeddingError as e:
                if "retryable" in str(e).lower() or "timeout" in str(e).lower():
                    last_error = e
                    logger.warning(
                        f"DashScope embedding error on attempt {attempt + 1}/{self.MAX_RETRIES} "
                        f"({batch_info}): {e}, retrying..."
                    )
                else:
                    # Non-retryable error, raise immediately
                    raise
            except Exception as exc:
                last_error = EmbeddingError(f"DashScope embedding error ({batch_info}): {exc}")
                logger.warning(
                    f"DashScope unexpected error on attempt {attempt + 1}/{self.MAX_RETRIES} "
                    f"({batch_info}): {exc}, retrying..."
                )

            # Exponential backoff before retry
            if attempt < self.MAX_RETRIES - 1:
                delay = min(
                    self.RETRY_BASE_DELAY * (2**attempt),
                    max(deadline - loop.time(), 0.0),
                )
                if delay <= 0:
                    break
                await asyncio.sleep(delay)

        # All retries exhausted
        raise last_error or EmbeddingError(
            f"DashScope embedding failed after {self.MAX_RETRIES} attempts ({batch_info})"
        )

    async def embed_texts(
        self, texts: list[str], text_type: str | None = None
    ) -> list[list[float]]:
        if not texts:
            return []

        # Sanitize and truncate all texts
        processed_texts = [self._truncate_text(self._sanitize_text(t)) for t in texts]

        kwargs: dict[str, Any] = {}
        if text_type in {"query", "document"}:
            kwargs["text_type"] = text_type

        # Process in batches of MAX_BATCH_SIZE
        all_vectors: list[list[float]] = []
        for i in range(0, len(processed_texts), self.MAX_BATCH_SIZE):
            batch = processed_texts[i : i + self.MAX_BATCH_SIZE]
            batch_info = f"batch {i // self.MAX_BATCH_SIZE + 1}, texts {i}-{i + len(batch) - 1}"

            try:
                vectors = await self._call_with_retry(batch, batch_info, **kwargs)
                self._validate_vectors(vectors, expected_count=len(batch))
                all_vectors.extend(vectors)
            except EmbeddingError as e:
                # On timeout, fall back to smaller batches to reduce request cost.
                if "timeout" in str(e).lower() and len(batch) > 1:
                    for j, text in enumerate(batch):
                        single_info = f"{batch_info} (fallback {j + 1}/{len(batch)})"
                        vectors = await self._call_with_retry([text], single_info, **kwargs)
                        self._validate_vectors(vectors, expected_count=1)
                        all_vectors.extend(vectors)
                else:
                    raise

        if self._dimension is None and all_vectors:
            self._dimension = len(all_vectors[0])
        return all_vectors


class DashScopeMultimodalEmbedding(BaseEmbedding):
    """DashScope multimodal embeddings adapter for images.

    Uses the official `dashscope` SDK for multimodal embedding.
    Supports image embedding via DashScope's multimodal-embedding API.

    Models:
    - multimodal-embedding-v1: 1024 dimensions
    - tongyi-embedding-vision-plus: 1024 dimensions (recommended)

    API Limits:
    - Max image size: 3MB (base64 encoded)
    - Supported formats: JPEG, PNG, GIF, BMP, WebP
    """

    MODEL_DIMENSIONS: dict[str, int] = {
        "multimodal-embedding-v1": 1024,
        "multimodal-embedding-one-peace": 1536,
        "tongyi-embedding-vision-plus": 1024,  # Unified to 1024
        "qwen2.5-vl-embedding": 1024,  # Latest Qwen VL embedding model
    }

    # Max 3MB for DashScope multimodal API
    MAX_IMAGE_SIZE_BYTES = 3 * 1024 * 1024

    # Supported image MIME types
    SUPPORTED_MEDIA_TYPES = {
        "image/jpeg",
        "image/jpg",
        "image/png",
        "image/gif",
        "image/bmp",
        "image/webp",
    }

    def __init__(
        self,
        model: str = "multimodal-embedding-v1",
        api_key: str = "",
        dimension: int | None = None,
        base_url: str | None = None,
        request_timeout_s: float = 60.0,
    ):
        dim = dimension or self.MODEL_DIMENSIONS.get(model) or 1024
        super().__init__(provider="dashscope_multimodal", model=model, dimension=dim)
        self._requested_dimension = dimension
        if not api_key:
            raise EmbeddingError("DashScope api_key is required for multimodal embedding")
        self.api_key = api_key
        self.base_url = _effective_embedding_endpoint("dashscope_multimodal", base_url)
        # SPO-04 / K4: per-RPC timeout + bounded concurrency for text too.
        self.request_timeout_s = max(float(request_timeout_s), 1.0)
        self._text_semaphore = asyncio.Semaphore(5)

        try:
            from dashscope import MultiModalEmbedding  # type: ignore

            self._MultiModalEmbedding = MultiModalEmbedding
        except ImportError as exc:
            raise EmbeddingError(
                "dashscope package is required for DashScopeMultimodalEmbedding "
                "(pip install dashscope>=1.24.6)"
            ) from exc

    async def _call_text_api(self, text: str) -> list[float]:
        """One bounded multimodal text RPC (timeout + concurrency cap)."""
        resp = await _await_bounded_thread(
            self._MultiModalEmbedding.call,
            model=self.model,
            input=[{"text": text}],
            api_key=self.api_key,
            base_address=getattr(self, "base_url", None),
            dimension=getattr(self, "_requested_dimension", None),
            timeout=getattr(self, "request_timeout_s", 60.0),
            semaphore=self._text_semaphore,
        )
        status_code = int(getattr(resp, "status_code", 0) or 0)
        if status_code and status_code >= 400:
            code = getattr(resp, "code", "") or ""
            message = getattr(resp, "message", "") or ""
            raise EmbeddingError(
                f"DashScope multimodal text embedding failed: {status_code} {code} {message}"
            )
        output = getattr(resp, "output", None)
        vectors = self._parse_multimodal_output(output)
        self._validate_vectors(vectors, expected_count=1)
        return vectors[0]

    @property
    def supports_multimodal(self) -> bool:
        return True

    def _image_to_base64_data_uri(self, image_bytes: bytes, media_type: str = "image/png") -> str:
        """Convert image bytes to base64 data URI format."""
        import base64

        b64_data = base64.b64encode(image_bytes).decode("utf-8")
        return f"data:{media_type};base64,{b64_data}"

    def _detect_media_type(self, image_bytes: bytes) -> str:
        """Detect image media type from magic bytes."""
        if len(image_bytes) < 8:
            return "image/png"  # Default

        # Check magic bytes
        if image_bytes[:8] == b"\x89PNG\r\n\x1a\n":
            return "image/png"
        elif image_bytes[:2] == b"\xff\xd8":
            return "image/jpeg"
        elif image_bytes[:6] in (b"GIF87a", b"GIF89a"):
            return "image/gif"
        elif image_bytes[:2] == b"BM":
            return "image/bmp"
        elif image_bytes[:4] == b"RIFF" and image_bytes[8:12] == b"WEBP":
            return "image/webp"
        else:
            return "image/png"  # Default fallback

    async def embed_texts(
        self, texts: list[str], text_type: str | None = None
    ) -> list[list[float]]:
        """Embed text using multimodal model.

        Note: While this model supports text, it's primarily designed for images.
        For text-only embedding, consider using DashScopeEmbedding instead.
        """
        del text_type
        if not texts:
            return []

        # SPO-04 / K4: bounded concurrency via the shared semaphore; each RPC
        # carries its own timeout.
        try:
            all_vectors = list(
                await asyncio.gather(*(self._call_text_api(text) for text in texts))
            )
        except EmbeddingError:
            raise
        except Exception as exc:
            raise EmbeddingError(f"DashScope multimodal text embedding error: {exc}") from exc

        if self._dimension is None and all_vectors:
            self._dimension = len(all_vectors[0])

        return all_vectors

    async def embed_images(
        self,
        images: list[bytes],
        max_concurrent: int = 5,
    ) -> list[list[float]]:
        """Embed images using DashScope multimodal embedding API.

        Supports concurrent processing with configurable parallelism.

        Args:
            images: List of image bytes (JPEG, PNG, GIF, BMP, WebP)
            max_concurrent: Maximum concurrent API calls (default: 5)

        Returns:
            List of embedding vectors (1024 dimensions each)

        Raises:
            EmbeddingError: If image is too large or API call fails
        """
        if not images:
            return []

        # Validate all image sizes first
        for i, image_bytes in enumerate(images):
            if len(image_bytes) > self.MAX_IMAGE_SIZE_BYTES:
                raise EmbeddingError(
                    f"Image {i} exceeds max size: {len(image_bytes)} bytes > {self.MAX_IMAGE_SIZE_BYTES} bytes (3MB)"
                )

        # Use semaphore for concurrent rate limiting
        semaphore = asyncio.Semaphore(max_concurrent)

        async def embed_single_image(idx: int, image_bytes: bytes) -> list[float]:
            """Embed a single image with semaphore-based rate limiting."""
            try:
                # Detect media type and convert to data URI
                media_type = self._detect_media_type(image_bytes)
                data_uri = self._image_to_base64_data_uri(image_bytes, media_type)

                # Call DashScope multimodal embedding API
                resp = await _await_bounded_thread(
                    self._MultiModalEmbedding.call,
                    model=self.model,
                    input=[{"image": data_uri}],
                    api_key=self.api_key,
                    base_address=getattr(self, "base_url", None),
                    dimension=getattr(self, "_requested_dimension", None),
                    timeout=getattr(self, "request_timeout_s", 60.0),
                    semaphore=semaphore,
                )

                status_code = int(getattr(resp, "status_code", 0) or 0)
                if status_code and status_code >= 400:
                    code = getattr(resp, "code", "") or ""
                    message = getattr(resp, "message", "") or ""
                    raise EmbeddingError(
                        f"DashScope multimodal image embedding failed for image {idx}: "
                        f"{status_code} {code} {message}"
                    )

                output = getattr(resp, "output", None)
                vectors = self._parse_multimodal_output(output)
                self._validate_vectors(vectors, expected_count=1)
                return vectors[0]

            except EmbeddingError:
                raise
            except Exception as exc:
                raise EmbeddingError(
                    f"DashScope multimodal image embedding error for image {idx}: {exc}"
                ) from exc

        # Launch all embedding tasks concurrently (limited by semaphore)
        tasks = [embed_single_image(i, img) for i, img in enumerate(images)]
        all_vectors = await asyncio.gather(*tasks)

        if self._dimension is None and all_vectors:
            self._dimension = len(all_vectors[0])

        return list(all_vectors)

    async def embed_image_and_text(
        self, image_bytes: bytes, text: str | None = None
    ) -> list[float]:
        """Embed an image with optional text context.

        This combines image and text into a single multimodal embedding,
        which can improve retrieval when images have associated captions.

        Args:
            image_bytes: Image content
            text: Optional text description or caption

        Returns:
            Single embedding vector combining image and text
        """
        if len(image_bytes) > self.MAX_IMAGE_SIZE_BYTES:
            raise EmbeddingError(
                f"Image exceeds max size: {len(image_bytes)} bytes > {self.MAX_IMAGE_SIZE_BYTES} bytes"
            )

        try:
            media_type = self._detect_media_type(image_bytes)
            data_uri = self._image_to_base64_data_uri(image_bytes, media_type)

            # Build input with image and optional text
            input_items: list[dict[str, str]] = [{"image": data_uri}]
            if text:
                input_items.append({"text": text})

            resp = await asyncio.to_thread(
                self._MultiModalEmbedding.call,
                model=self.model,
                input=input_items,
                api_key=self.api_key,
                base_address=getattr(self, "base_url", None),
                dimension=getattr(self, "_requested_dimension", None),
            )

            status_code = int(getattr(resp, "status_code", 0) or 0)
            if status_code and status_code >= 400:
                code = getattr(resp, "code", "") or ""
                message = getattr(resp, "message", "") or ""
                raise EmbeddingError(
                    f"DashScope multimodal embedding failed: {status_code} {code} {message}"
                )

            output = getattr(resp, "output", None)
            vectors = self._parse_multimodal_output(output)
            self._validate_vectors(vectors, expected_count=1)

            return vectors[0]

        except EmbeddingError:
            raise
        except Exception as exc:
            raise EmbeddingError(f"DashScope multimodal embedding error: {exc}") from exc

    def _parse_multimodal_output(self, output: Any) -> list[list[float]]:
        """Parse DashScope multimodal embedding output."""
        if output is None:
            raise EmbeddingError("DashScope multimodal response missing output")

        # Response format: {"embeddings": [{"embedding": [...], "type": "image|text"}]}
        if isinstance(output, dict):
            embeddings_list = output.get("embeddings")
            if embeddings_list is None:
                raise EmbeddingError(
                    f"Unexpected DashScope multimodal output keys: {list(output.keys())}"
                )
            output = embeddings_list

        if not isinstance(output, list):
            raise EmbeddingError(f"Unexpected DashScope multimodal output type: {type(output)}")

        vectors: list[list[float]] = []
        for entry in output:
            if isinstance(entry, dict):
                vec = entry.get("embedding") or entry.get("vector")
                if isinstance(vec, list):
                    vectors.append(vec)
            elif isinstance(entry, list):
                vectors.append(entry)

        if not vectors:
            raise EmbeddingError("DashScope multimodal response has no embeddings")

        return vectors


class LocalHashEmbedding(BaseEmbedding):
    """Lightweight local embedding via feature hashing (no external dependencies)."""

    DEFAULT_DIMENSION = 384

    def __init__(self, model: str, dimension: int | None = None):
        dim = dimension or _infer_local_dimension(model) or self.DEFAULT_DIMENSION
        super().__init__(provider="local", model=model, dimension=dim)

    async def embed_texts(
        self, texts: list[str], text_type: str | None = None
    ) -> list[list[float]]:
        del text_type
        if not texts:
            return []

        vectors: list[list[float]] = []
        dim = self.dimension
        token_re = re.compile(r"[A-Za-z0-9\u4e00-\u9fff]+")

        for text in texts:
            vec = [0.0] * dim
            for token in token_re.findall((text or "").lower()):
                digest = hashlib.blake2b(token.encode("utf-8"), digest_size=8).digest()
                idx = int.from_bytes(digest[:4], "little") % dim
                sign = 1.0 if (digest[4] & 1) == 0 else -1.0
                vec[idx] += sign

            norm = math.sqrt(sum(v * v for v in vec))
            if norm > 0:
                vec = [v / norm for v in vec]
            vectors.append(vec)

        return vectors


def _infer_local_dimension(model: str) -> int | None:
    if not model:
        return None
    match = re.search(r"(\d{2,5})", model)
    if not match:
        return None
    try:
        return int(match.group(1))
    except ValueError:
        return None


def _parse_dashscope_embeddings(output: Any) -> list[list[float]]:
    """Best-effort parser for DashScope embedding outputs."""
    if output is None:
        raise EmbeddingError("DashScope embeddings response missing output")

    # Common shape: {"embeddings": [{"embedding": [...]}, ...]}
    if isinstance(output, dict):
        candidates = (
            output.get("embeddings")
            or output.get("data")
            or output.get("results")
            or output.get("output")
        )
        if isinstance(candidates, dict):
            candidates = (
                candidates.get("embeddings") or candidates.get("data") or candidates.get("results")
            )
        if candidates is None:
            raise EmbeddingError(
                f"Unexpected DashScope embeddings output keys: {list(output.keys())}"
            )
        output = candidates

    if not isinstance(output, list):
        raise EmbeddingError(f"Unexpected DashScope embeddings output type: {type(output)}")

    items: list[dict[str, Any]] = []
    vectors: list[list[float]] = []
    for entry in output:
        if isinstance(entry, dict):
            items.append(entry)
        elif isinstance(entry, list):
            vectors.append(entry)

    # If dict items exist, sort by index if present.
    if items:

        def _idx(d: dict[str, Any]) -> int:
            for k in ("index", "text_index", "id"):
                v = d.get(k)
                if isinstance(v, int):
                    return v
            return 0

        items = sorted(items, key=_idx)
        for d in items:
            vec = d.get("embedding") or d.get("vector") or d.get("values")
            if not isinstance(vec, list):
                raise EmbeddingError("DashScope embeddings item missing vector")
            vectors.append(vec)

    if not vectors:
        raise EmbeddingError("DashScope embeddings response empty")
    return vectors


# =============================================================================
# SiliconFlow Embedding
# =============================================================================
def _embeddings_endpoint(endpoint: str | None) -> str:
    """Append the OpenAI-compatible embeddings path exactly once."""
    value = (endpoint or "").strip()
    if not value or value.rstrip("/").endswith("/embeddings"):
        return value
    return f"{value.rstrip('/')}/embeddings"


class SiliconFlowEmbedding(BaseEmbedding):
    """SiliconFlow embedding adapter.

    Uses SiliconFlow API for text embedding with OpenAI-compatible interface.

    API Reference: https://docs.siliconflow.cn/docs/embeddings

    Features:
    - OpenAI-compatible API
    - Support for BGE and other models
    - Configurable HTTP timeout
    - Batch embedding support

    Models:
    - BAAI/bge-m3: 1024 dimensions
    - Pro/BAAI/bge-m3: 1024 dimensions
    - BAAI/bge-large-zh-v1.5: 1024 dimensions
    - BAAI/bge-large-en-v1.5: 1024 dimensions
    - netease-youdao/bce-embedding-base_v1: 512 dimensions
    """

    MODEL_DIMENSIONS: dict[str, int] = {
        "BAAI/bge-m3": 1024,
        "Pro/BAAI/bge-m3": 1024,
        "BAAI/bge-large-zh-v1.5": 1024,
        "BAAI/bge-large-en-v1.5": 1024,
        "netease-youdao/bce-embedding-base_v1": 512,
    }

    # API endpoint (embeddings endpoint)
    SILICONFLOW_API_URL = "https://api.siliconflow.cn/v1/embeddings"

    # API limits
    MAX_BATCH_SIZE = 100
    MAX_RETRIES = 3
    RETRY_BASE_DELAY = 1.0

    def __init__(
        self,
        api_key: str,
        model: str = "BAAI/bge-large-zh-v1.5",
        dimension: int | None = None,
        base_url: str | None = None,
        timeout_seconds: float = 30.0,
    ):
        """Initialize SiliconFlow Embedding.

        Args:
            api_key: SiliconFlow API key
            model: Model name (default: BAAI/bge-large-zh-v1.5)
            dimension: Output dimension (auto-detected from model if not provided)
            base_url: Optional API base URL override
            timeout_seconds: Request timeout
        """
        dim = dimension or self.MODEL_DIMENSIONS.get(model) or 1024
        super().__init__(provider="siliconflow", model=model, dimension=dim)

        if not api_key:
            raise EmbeddingError("SiliconFlow API key is required")

        self.api_key = api_key
        self.base_url = _effective_embedding_endpoint("siliconflow", base_url)
        self.timeout_seconds = timeout_seconds
        self._client = httpx.AsyncClient(
            timeout=httpx.Timeout(timeout_seconds, connect=5.0),
            limits=httpx.Limits(max_connections=20, max_keepalive_connections=10),
        )

    async def close(self) -> None:
        """Close the HTTP client."""
        await self._client.aclose()

    async def embed_texts(
        self,
        texts: list[str],
        text_type: str | None = None,
    ) -> list[list[float]]:
        """Embed texts using SiliconFlow API.

        Args:
            texts: List of text strings to embed
            text_type: Not used by SiliconFlow (kept for interface compatibility)

        Returns:
            List of embedding vectors
        """
        del text_type
        if not texts:
            return []

        # Sanitize: replace empty/whitespace-only strings with placeholder
        # to avoid API errors from providers that reject empty input.
        sanitized = [t if t and t.strip() else "empty" for t in texts]

        # Process in batches
        all_vectors: list[list[float]] = []

        for i in range(0, len(sanitized), self.MAX_BATCH_SIZE):
            batch = sanitized[i : i + self.MAX_BATCH_SIZE]
            batch_info = f"batch {i // self.MAX_BATCH_SIZE + 1}"

            vectors = await self._embed_batch_with_retry(batch, batch_info)
            all_vectors.extend(vectors)

        return all_vectors

    async def _embed_batch_with_retry(
        self,
        texts: list[str],
        batch_info: str,
    ) -> list[list[float]]:
        """Embed a batch of texts with retry logic."""
        import logging

        logger = logging.getLogger(__name__)

        last_error: Exception | None = None

        for attempt in range(self.MAX_RETRIES):
            try:
                return await self._embed_batch(texts)

            except EmbeddingError as e:
                err_str = str(e)
                retryable = any(code in err_str for code in ("429", "500", "502", "503"))
                last_error = e
                if retryable:
                    logger.warning(
                        f"SiliconFlow embedding retryable error ({batch_info}) "
                        f"attempt {attempt + 1}/{self.MAX_RETRIES}: {e}"
                    )
                else:
                    raise

            except Exception as exc:
                last_error = EmbeddingError(f"SiliconFlow embedding error ({batch_info}): {exc}")
                logger.warning(
                    f"SiliconFlow embedding error ({batch_info}) "
                    f"attempt {attempt + 1}/{self.MAX_RETRIES}: {exc}"
                )

            # Exponential backoff
            if attempt < self.MAX_RETRIES - 1:
                import random

                delay = self.RETRY_BASE_DELAY * (2**attempt)
                delay = delay + random.uniform(0.0, min(0.3, delay))
                await asyncio.sleep(delay)

        raise last_error or EmbeddingError(
            f"SiliconFlow embedding failed after {self.MAX_RETRIES} attempts ({batch_info})"
        )

    async def _embed_batch(
        self,
        texts: list[str],
    ) -> list[list[float]]:
        """Call SiliconFlow embeddings API for a batch of texts."""
        payload = {
            "model": self.model,
            "input": texts,
        }

        response = await self._client.post(
            self.base_url,
            json=payload,
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
        )

        if response.status_code >= 400:
            raise EmbeddingError(f"SiliconFlow API error: {response.status_code} - {response.text}")

        data = response.json()

        # Parse response
        embeddings = data.get("data", [])
        if not embeddings:
            raise EmbeddingError("SiliconFlow API returned no embeddings")

        vectors: list[list[float]] = []
        for emb in embeddings:
            values = emb.get("embedding", [])
            if not values:
                raise EmbeddingError("SiliconFlow embedding missing values")
            vectors.append(values)

        if self._dimension is None and vectors:
            self._dimension = len(vectors[0])

        return vectors


# =============================================================================
# OpenAI-compatible self-hosted embedding server (vLLM / TEI)
# =============================================================================
class OpenAICompatibleEmbedding(SiliconFlowEmbedding):
    """Embeddings served by any OpenAI-compatible ``/v1/embeddings`` endpoint.

    Reuses the SiliconFlow wire protocol (POST ``{"model", "input"}`` →
    ``{"data": [{"embedding": [...]}]}``), which is what vLLM's OpenAI server
    and Hugging Face Text Embeddings Inference expose, with the defaults a
    self-hosted deployment needs:

    * ``base_url`` is REQUIRED — there is deliberately no public-cloud
      fallback, so a half-configured upgrade fails at startup instead of
      silently embedding against SiliconFlow;
    * ``api_key`` is optional — unauthenticated internal serving is the norm;
      when omitted a placeholder bearer token is sent for servers that still
      require the header to be present.

    This is the config hook for the Qwen3-Embedding default-model upgrade:
    set the provider to ``openai_compatible`` (aliases: ``vllm``, ``tei``),
    point the base URL at the deployment, and the blue-green migration
    machine can re-embed datasets into the new space with no code change.
    """

    MODEL_DIMENSIONS = {
        **SiliconFlowEmbedding.MODEL_DIMENSIONS,
        "Qwen/Qwen3-Embedding-0.6B": 1024,
        "Qwen/Qwen3-Embedding-4B": 2560,
        "Qwen/Qwen3-Embedding-8B": 4096,
    }

    DEFAULT_MODEL = "Qwen/Qwen3-Embedding-0.6B"

    def __init__(
        self,
        base_url: str,
        api_key: str = "",
        model: str = DEFAULT_MODEL,
        dimension: int | None = None,
        timeout_seconds: float = 30.0,
    ):
        endpoint = _embeddings_endpoint(base_url)
        if not endpoint:
            raise EmbeddingError(
                "openai_compatible embedding provider requires an explicit base_url "
                "(the vLLM / TEI server address); refusing to fall back to a cloud default"
            )
        super().__init__(
            api_key=(api_key or "").strip() or "unused",
            model=(model or "").strip() or self.DEFAULT_MODEL,
            dimension=dimension,
            base_url=endpoint,
            timeout_seconds=timeout_seconds,
        )
        self.provider = "openai_compatible"


# =============================================================================
# Embedder Cache - Singleton per config to reuse HTTP connections
# =============================================================================
_embedder_cache: dict[str, BaseEmbedding] = {}
_embedder_cache_lock = asyncio.Lock()

# =============================================================================
# Query Embedding Cache - TTLCache for query embeddings (lock-free, async-safe)
_query_embedding_cache: TTLCache[str, list[float]] = TTLCache(maxsize=1000, ttl=1800)


_PROVIDER_ALIASES: dict[str, str] = {
    "builtin": "local",
    "hash": "local",
    "aliyun": "dashscope",
    "aliyun_multimodal": "dashscope_multimodal",
    "multimodal": "dashscope_multimodal",
    "unified": "unified_multimodal",
    "cross_modal": "unified_multimodal",
    "google": "gemini",
    "silicon": "siliconflow",
    "sf": "siliconflow",
    "vllm": "openai_compatible",
    "tei": "openai_compatible",
    "openai-compatible": "openai_compatible",
}


def _canonical_provider(provider: str | None) -> str:
    normalized = (provider or "").strip().lower()
    return _PROVIDER_ALIASES.get(normalized, normalized)


def _effective_embedding_endpoint(provider: str | None, endpoint: str | None) -> str:
    """Resolve once so runtime requests and cache identity use the same endpoint."""
    provider = _canonical_provider(provider)
    if provider == "local":
        return ""
    if endpoint and endpoint.strip():
        return _normalize_endpoint(endpoint)
    if provider in {"dashscope", "dashscope_multimodal", "unified_multimodal"}:
        return DashScopeEmbedding.DASHSCOPE_API_URL
    if provider == "gemini":
        return GeminiEmbedding.GEMINI_API_URL
    if provider == "siliconflow":
        return SiliconFlowEmbedding.SILICONFLOW_API_URL
    return ""


def _normalize_endpoint(endpoint: str | None) -> str:
    """Normalize endpoint spelling without collapsing distinct regions or paths."""
    value = (endpoint or "").strip()
    if not value:
        return ""

    parsed = urlsplit(value)
    if not parsed.scheme or not parsed.netloc:
        return value.rstrip("/")

    host = (parsed.hostname or "").lower()
    if ":" in host and not host.startswith("["):
        host = f"[{host}]"
    try:
        port = parsed.port
    except ValueError:
        return value.rstrip("/")
    if port and not (
        (parsed.scheme.lower() == "http" and port == 80)
        or (parsed.scheme.lower() == "https" and port == 443)
    ):
        host = f"{host}:{port}"
    if "@" in parsed.netloc:
        userinfo = parsed.netloc.rsplit("@", 1)[0]
        host = f"{userinfo}@{host}"

    return urlunsplit(
        (
            parsed.scheme.lower(),
            host,
            parsed.path.rstrip("/"),
            parsed.query,
            "",
        )
    )


def _credential_fingerprint(api_key: str | None) -> str:
    """Return a short irreversible account discriminator without exposing secrets."""
    if not api_key:
        return ""
    return hashlib.sha256(api_key.encode("utf-8")).hexdigest()[:24]


def _endpoint_fingerprint(endpoint: str | None) -> str:
    """Hash the normalized endpoint so URL credentials never enter cache profiles."""
    normalized = _normalize_endpoint(endpoint)
    if not normalized:
        return ""
    return hashlib.sha256(normalized.encode("utf-8")).hexdigest()[:24]


def _normalize_cache_value(value: Any) -> Any:
    """Convert config values into a deterministic JSON-compatible structure."""
    if isinstance(value, Mapping):
        return {
            str(key): _normalize_cache_value(item)
            for key, item in sorted(value.items(), key=lambda pair: str(pair[0]))
        }
    if isinstance(value, (list, tuple)):
        return [_normalize_cache_value(item) for item in value]
    if isinstance(value, (set, frozenset)):
        normalized = [_normalize_cache_value(item) for item in value]
        return sorted(normalized, key=lambda item: json.dumps(item, sort_keys=True, default=str))
    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    return str(value)


def _stable_cache_digest(payload: Mapping[str, Any]) -> str:
    canonical = json.dumps(
        _normalize_cache_value(payload),
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    )
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _build_embedding_cache_profile(
    *,
    provider: str | None,
    model: str | None,
    dimension: int | None,
    base_url: str | None,
    api_key: str | None,
) -> dict[str, Any]:
    return {
        "provider": _canonical_provider(provider),
        "model": (model or "").strip(),
        "dimension": dimension,
        "endpoint": _endpoint_fingerprint(base_url),
        "credential": _credential_fingerprint(api_key),
    }


def _build_embedder_instance_profile(
    config: EmbeddingConfig,
    dimension: int | None,
) -> dict[str, Any]:
    """Build constructor identity without polluting semantic query cache keys."""
    extra = config.extra or {}
    effective_dimension = dimension
    if effective_dimension is None and extra.get("dimension") is not None:
        effective_dimension = extra["dimension"]
    profile = _build_embedding_cache_profile(
        provider=config.provider,
        model=config.model,
        dimension=effective_dimension,
        base_url=_effective_embedding_endpoint(config.provider, config.base_url),
        api_key=config.api_key,
    )
    provider = _canonical_provider(config.provider)
    if provider in {"gemini", "siliconflow", "openai_compatible"}:
        profile["timeout_seconds"] = config.timeout_seconds
    profile["instance_options"] = dict(extra)
    return profile


def _get_query_cache_key(
    provider: str,
    model: str,
    query: str,
    profile: Mapping[str, Any] | None = None,
) -> str:
    """Generate a non-sensitive cache key for one effective embedding space."""
    digest = _stable_cache_digest(
        {
            "provider": _canonical_provider(provider),
            "model": model or "",
            "query": query,
            "profile": profile or {},
        }
    )
    return f"query-embedding:v2:{digest}"


def get_cached_query_embedding(
    provider: str,
    model: str,
    query: str,
    profile: Mapping[str, Any] | None = None,
) -> list[float] | None:
    """Get cached query embedding if exists."""
    key = _get_query_cache_key(provider, model, query, profile)
    cached = _query_embedding_cache.get(key)
    return list(cached) if cached is not None else None


def set_cached_query_embedding(
    provider: str,
    model: str,
    query: str,
    embedding: list[float],
    profile: Mapping[str, Any] | None = None,
) -> None:
    """Cache query embedding."""
    key = _get_query_cache_key(provider, model, query, profile)
    _query_embedding_cache[key] = list(embedding)


def _make_cache_key(config: EmbeddingConfig, dimension: int | None) -> str:
    """Generate cache key for embedder config."""
    profile = _build_embedder_instance_profile(config, dimension)
    return f"embedder:v2:{_stable_cache_digest(profile)}"


async def get_cached_embedder(
    config: EmbeddingConfig, dimension: int | None = None
) -> BaseEmbedding:
    """Get or create cached embedder instance.

    This helps reduce first-call latency by reusing HTTP connections.
    """
    cache_key = _make_cache_key(config, dimension)

    async with _embedder_cache_lock:
        if cache_key not in _embedder_cache:
            _embedder_cache[cache_key] = create_embedding(config, dimension)
        return _embedder_cache[cache_key]


def create_embedding(config: EmbeddingConfig, dimension: int | None = None) -> BaseEmbedding:
    provider = _canonical_provider(config.provider)
    model = (config.model or "").strip()
    if provider == "local":
        embedding: BaseEmbedding = LocalHashEmbedding(
            model=model or "hash-384",
            dimension=dimension or (config.extra or {}).get("dimension"),
        )
    elif provider == "dashscope":
        embedding = DashScopeEmbedding(
            model=model,
            api_key=config.api_key or "",
            dimension=dimension,
            base_url=config.base_url,
            request_timeout_s=float(
                (config.extra or {}).get("request_timeout_s")
                or config.timeout_seconds
                or 60.0
            ),
            max_concurrent=int((config.extra or {}).get("max_concurrent") or 5),
        )
    elif provider == "dashscope_multimodal":
        embedding = DashScopeMultimodalEmbedding(
            model=model or "multimodal-embedding-v1",
            api_key=config.api_key or "",
            dimension=dimension,
            base_url=config.base_url,
            request_timeout_s=float(
                (config.extra or {}).get("request_timeout_s")
                or config.timeout_seconds
                or 60.0
            ),
        )
    elif provider == "unified_multimodal":
        embedding = UnifiedMultimodalEmbedding(
            model=model or "tongyi-embedding-vision-plus",
            api_key=config.api_key or "",
            dimension=dimension,
            base_url=config.base_url,
            max_concurrent=(config.extra or {}).get("max_concurrent", 5),
            request_timeout_s=float(
                (config.extra or {}).get("request_timeout_s")
                or config.timeout_seconds
                or 60.0
            ),
        )
    elif provider == "gemini":
        embedding = GeminiEmbedding(
            api_key=config.api_key or "",
            model=model or "gemini-embedding-001",
            dimension=dimension or 1024,
            base_url=config.base_url,
            timeout_seconds=config.timeout_seconds,
        )
    elif provider == "siliconflow":
        embedding = SiliconFlowEmbedding(
            api_key=config.api_key or "",
            model=model or "BAAI/bge-large-zh-v1.5",
            dimension=dimension,
            base_url=config.base_url,
            timeout_seconds=config.timeout_seconds,
        )
    elif provider == "openai_compatible":
        embedding = OpenAICompatibleEmbedding(
            api_key=config.api_key or "",
            model=model or OpenAICompatibleEmbedding.DEFAULT_MODEL,
            dimension=dimension or (config.extra or {}).get("dimension"),
            base_url=config.base_url or "",
            timeout_seconds=config.timeout_seconds,
        )
    elif provider == "openai":
        raise EmbeddingError(
            "OpenAI embedding provider has been removed. "
            "Please update your dataset to use 'gemini', 'dashscope', 'siliconflow', "
            "or 'openai_compatible' for a self-hosted OpenAI-compatible server."
        )
    else:
        raise EmbeddingError(f"Unsupported embedding provider: {config.provider}")

    embedding._configure_cache_profile(config)
    return embedding


def create_multimodal_embedding(
    api_key: str,
    model: str = "multimodal-embedding-v1",
    base_url: str | None = None,
) -> DashScopeMultimodalEmbedding:
    """Convenience factory for creating multimodal embedding instances.

    Args:
        api_key: DashScope API key
        model: Model name (default: multimodal-embedding-v1)
        base_url: Optional base URL override

    Returns:
        DashScopeMultimodalEmbedding instance
    """
    return DashScopeMultimodalEmbedding(
        model=model,
        api_key=api_key,
        base_url=base_url,
    )


# ============================================================
# Unified Multimodal Embedding (Phase 2: Cross-Modal Search)
# ============================================================


@dataclass
class UnifiedEmbeddingResult:
    """Result from unified multimodal embedding."""

    vector: list[float]
    content_type: str  # "text" | "image" | "mixed"
    dimension: int
    model: str


class UnifiedMultimodalEmbedding(BaseEmbedding):
    """Unified embedding for text and images in the SAME vector space.

    This is the key component for cross-modal retrieval (Dify 1.11 approach).
    Both text and images are embedded using the same multimodal model,
    ensuring they can be directly compared via cosine similarity.

    Key Features:
    - Text queries can find relevant images
    - Image queries can find relevant text
    - Mixed content (text + image) embedding supported
    - Consistent 1024D vector space

    Recommended Model:
    - tongyi-embedding-vision-plus: Best for unified cross-modal search

    Usage:
        # Create unified embedding instance
        unified = UnifiedMultimodalEmbedding(api_key="your-key")

        # Embed text
        text_vectors = await unified.embed_texts(["What is our refund policy?"])

        # Embed images
        image_vectors = await unified.embed_images([image_bytes])

        # Both vectors are in the same space - can compare directly!
        similarity = cosine_similarity(text_vectors[0], image_vectors[0])
    """

    # Recommended model for unified cross-modal embedding
    DEFAULT_MODEL = "tongyi-embedding-vision-plus"

    MODEL_DIMENSIONS: dict[str, int] = {
        "tongyi-embedding-vision-plus": 1024,  # Unified to 1024
        "multimodal-embedding-v1": 1024,
        "qwen2.5-vl-embedding": 1024,
    }

    MAX_IMAGE_SIZE_BYTES = 3 * 1024 * 1024  # 3MB
    MAX_TEXT_CHARS = 8000

    SUPPORTED_MEDIA_TYPES = {
        "image/jpeg",
        "image/jpg",
        "image/png",
        "image/gif",
        "image/bmp",
        "image/webp",
    }

    def __init__(
        self,
        api_key: str,
        model: str = "tongyi-embedding-vision-plus",
        dimension: int | None = None,
        base_url: str | None = None,
        max_concurrent: int = 5,
        request_timeout_s: float = 60.0,
    ):
        """Initialize unified multimodal embedding.

        Args:
            api_key: DashScope API key
            model: Model to use (recommended: tongyi-embedding-vision-plus)
            dimension: Vector dimension (auto-detected from model)
            base_url: Optional API base URL override
            max_concurrent: Max concurrent API calls
            request_timeout_s: Per-RPC timeout (SPO-04 / K4)
        """
        dim = dimension or self.MODEL_DIMENSIONS.get(model, 1024)
        super().__init__(provider="unified_multimodal", model=model, dimension=dim)

        if not api_key:
            raise EmbeddingError("API key is required for UnifiedMultimodalEmbedding")

        self.api_key = api_key
        self.base_url = _effective_embedding_endpoint("unified_multimodal", base_url)
        self._requested_dimension = dimension
        self.max_concurrent = max_concurrent
        self.request_timeout_s = max(float(request_timeout_s), 1.0)
        self._semaphore = asyncio.Semaphore(max_concurrent)

        try:
            from dashscope import MultiModalEmbedding

            self._MultiModalEmbedding = MultiModalEmbedding
        except ImportError as exc:
            raise EmbeddingError(
                "dashscope package required (pip install dashscope>=1.24.6)"
            ) from exc

    @property
    def supports_multimodal(self) -> bool:
        return True

    def _detect_media_type(self, image_bytes: bytes) -> str:
        """Detect image MIME type from magic bytes."""
        if len(image_bytes) < 8:
            return "image/png"

        if image_bytes[:8] == b"\x89PNG\r\n\x1a\n":
            return "image/png"
        elif image_bytes[:2] == b"\xff\xd8":
            return "image/jpeg"
        elif image_bytes[:6] in (b"GIF87a", b"GIF89a"):
            return "image/gif"
        elif image_bytes[:2] == b"BM":
            return "image/bmp"
        elif image_bytes[:4] == b"RIFF" and len(image_bytes) > 12 and image_bytes[8:12] == b"WEBP":
            return "image/webp"
        return "image/png"

    def _to_base64_data_uri(self, image_bytes: bytes, media_type: str) -> str:
        """Convert image bytes to base64 data URI."""
        import base64

        b64 = base64.b64encode(image_bytes).decode("utf-8")
        return f"data:{media_type};base64,{b64}"

    def _sanitize_text(self, text: str) -> str:
        """Clean and truncate text for embedding."""
        if not text:
            return "empty"

        # Remove control characters
        text = text.replace("\x00", "")
        text = "".join(c if c.isprintable() or c in "\n\t" else " " for c in text)

        # Normalize whitespace
        text = re.sub(r"\s+", " ", text).strip()

        # Truncate if needed
        if len(text) > self.MAX_TEXT_CHARS:
            text = text[: self.MAX_TEXT_CHARS]

        return text if text else "empty"

    async def _call_api(self, input_items: list[dict[str, str]]) -> list[float]:
        """Call DashScope multimodal embedding API."""
        try:
            resp = await _await_bounded_thread(
                self._MultiModalEmbedding.call,
                model=self.model,
                input=input_items,
                api_key=self.api_key,
                base_address=getattr(self, "base_url", None),
                dimension=self._requested_dimension,
                timeout=getattr(self, "request_timeout_s", 60.0),
                semaphore=self._semaphore,
            )

            status_code = int(getattr(resp, "status_code", 0) or 0)
            if status_code and status_code >= 400:
                code = getattr(resp, "code", "") or ""
                message = getattr(resp, "message", "") or ""
                raise EmbeddingError(
                    f"Unified embedding API failed: {status_code} {code} {message}"
                )

            output = getattr(resp, "output", None)
            vectors = self._parse_output(output)
            self._validate_vectors(vectors, expected_count=1)
            return vectors[0]

        except EmbeddingError:
            raise
        except TimeoutError as exc:
            raise EmbeddingError(f"Unified embedding error: {exc}") from exc
        except Exception as exc:
            raise EmbeddingError(f"Unified embedding error: {exc}") from exc

    def _parse_output(self, output: Any) -> list[list[float]]:
        """Parse API response to extract vectors."""
        if output is None:
            raise EmbeddingError("Response missing output")

        if isinstance(output, dict):
            embeddings_list = output.get("embeddings")
            if embeddings_list is None:
                raise EmbeddingError(f"Unexpected output keys: {list(output.keys())}")
            output = embeddings_list

        if not isinstance(output, list):
            raise EmbeddingError(f"Unexpected output type: {type(output)}")

        vectors: list[list[float]] = []
        for entry in output:
            if isinstance(entry, dict):
                vec = entry.get("embedding") or entry.get("vector")
                if isinstance(vec, list):
                    vectors.append(vec)
            elif isinstance(entry, list):
                vectors.append(entry)

        return vectors

    async def embed_texts(
        self,
        texts: list[str],
        text_type: str | None = None,
    ) -> list[list[float]]:
        """Embed text in the unified multimodal space.

        IMPORTANT: Use this instead of DashScopeEmbedding when you need
        cross-modal retrieval (text queries finding images).

        Args:
            texts: List of text strings
            text_type: Ignored (for API compatibility)

        Returns:
            List of 1024D vectors in the same space as image embeddings
        """
        del text_type
        if not texts:
            return []

        # SPO-04 / K4: bounded concurrency instead of a 1-chunk-1-RPC serial
        # loop — the semaphore caps in-flight RPCs, the per-RPC timeout
        # bounds the wall clock.
        vectors = await asyncio.gather(
            *(
                self._call_api([{"text": self._sanitize_text(text)}])
                for text in texts
            )
        )

        if self._dimension is None and vectors:
            self._dimension = len(vectors[0])

        return list(vectors)

    async def embed_query(self, query: str) -> list[float]:
        """Embed a query for cross-modal search.

        The resulting vector can find both relevant text AND images.
        """
        vectors = await self.embed_texts([query])
        return vectors[0]

    async def embed_images(
        self,
        images: list[bytes],
        max_concurrent: int | None = None,
    ) -> list[list[float]]:
        """Embed images in the unified multimodal space.

        Args:
            images: List of image bytes
            max_concurrent: Override max concurrent calls

        Returns:
            List of 1024D vectors in the same space as text embeddings
        """
        if not images:
            return []

        # Validate sizes
        for i, img in enumerate(images):
            if len(img) > self.MAX_IMAGE_SIZE_BYTES:
                raise EmbeddingError(f"Image {i} exceeds 3MB limit ({len(img)} bytes)")

        # Process concurrently. The optional per-call cap composes with the
        # instance-wide provider semaphore inside _call_api.
        call_semaphore = (
            asyncio.Semaphore(max(int(max_concurrent), 1))
            if max_concurrent is not None
            else None
        )

        async def embed_single(idx: int, img_bytes: bytes) -> list[float]:
            del idx
            media_type = self._detect_media_type(img_bytes)
            data_uri = self._to_base64_data_uri(img_bytes, media_type)
            if call_semaphore is not None:
                async with call_semaphore:
                    return await self._call_api([{"image": data_uri}])
            return await self._call_api([{"image": data_uri}])

        tasks = [embed_single(i, img) for i, img in enumerate(images)]
        vectors = await asyncio.gather(*tasks)

        if self._dimension is None and vectors:
            self._dimension = len(vectors[0])

        return list(vectors)

    async def embed_image_with_context(
        self,
        image_bytes: bytes,
        context_text: str | None = None,
    ) -> list[float]:
        """Embed image with optional text context.

        This creates a combined embedding that captures both visual
        content and textual context (e.g., captions, surrounding text).

        Args:
            image_bytes: Image content
            context_text: Optional text context (caption, description)

        Returns:
            Combined embedding vector
        """
        if len(image_bytes) > self.MAX_IMAGE_SIZE_BYTES:
            raise EmbeddingError("Image exceeds 3MB limit")

        media_type = self._detect_media_type(image_bytes)
        data_uri = self._to_base64_data_uri(image_bytes, media_type)

        input_items: list[dict[str, str]] = [{"image": data_uri}]
        if context_text:
            input_items.append({"text": self._sanitize_text(context_text)})

        return await self._call_api(input_items)

    async def embed_image_and_text(
        self,
        image_bytes: bytes,
        text: str | None = None,
    ) -> list[float]:
        """Embed image with optional text - alias for embed_image_with_context.

        Provides compatibility with DashScopeMultimodalEmbedding interface.
        """
        return await self.embed_image_with_context(image_bytes, text)

    async def embed_mixed_batch(
        self,
        items: list[dict[str, Any]],
    ) -> list[UnifiedEmbeddingResult]:
        """Embed a batch of mixed text and image content.

        Args:
            items: List of dicts with either:
                   {"type": "text", "content": "text string"}
                   {"type": "image", "content": image_bytes}
                   {"type": "mixed", "text": str, "image": bytes}

        Returns:
            List of UnifiedEmbeddingResult with vectors and metadata
        """
        results: list[UnifiedEmbeddingResult] = []

        for item in items:
            item_type = item.get("type", "text")

            if item_type == "text":
                text = item.get("content", "")
                vec = (await self.embed_texts([text]))[0]
                results.append(
                    UnifiedEmbeddingResult(
                        vector=vec,
                        content_type="text",
                        dimension=len(vec),
                        model=self.model,
                    )
                )

            elif item_type == "image":
                img = item.get("content", b"")
                vec = (await self.embed_images([img]))[0]
                results.append(
                    UnifiedEmbeddingResult(
                        vector=vec,
                        content_type="image",
                        dimension=len(vec),
                        model=self.model,
                    )
                )

            elif item_type == "mixed":
                text = item.get("text", "")
                img = item.get("image", b"")
                vec = await self.embed_image_with_context(img, text)
                results.append(
                    UnifiedEmbeddingResult(
                        vector=vec,
                        content_type="mixed",
                        dimension=len(vec),
                        model=self.model,
                    )
                )

        return results


def create_unified_embedding(
    api_key: str,
    model: str = "tongyi-embedding-vision-plus",
    base_url: str | None = None,
    max_concurrent: int = 5,
) -> UnifiedMultimodalEmbedding:
    """Create a unified multimodal embedding instance.

    This is the recommended way to create embeddings for cross-modal search.
    Both text and images will be in the same 1024D vector space.

    Args:
        api_key: DashScope API key
        model: Model name (default: tongyi-embedding-vision-plus)
        base_url: Optional API base URL
        max_concurrent: Max concurrent API calls

    Returns:
        UnifiedMultimodalEmbedding instance

    Example:
        ```python
        unified = create_unified_embedding(api_key=get_settings().dashscope_api_key)

        # Embed text query
        query_vec = await unified.embed_query("What does the architecture diagram show?")

        # This query can now find both:
        # 1. Text segments discussing architecture
        # 2. Image segments containing architecture diagrams
        ```
    """
    return UnifiedMultimodalEmbedding(
        api_key=api_key,
        model=model,
        base_url=base_url,
        max_concurrent=max_concurrent,
    )
