"""Vision/OCR service for document ingestion.

The default backend is Alibaba Cloud DashScope Qwen-OCR (``qwen-vl-ocr``).
The native DashScope API is used instead of the OpenAI-compatible endpoint so
the built-in OCR tasks (document parsing, tables, formulas, and positional
recognition) and regional native endpoint are available. Gemini and
SiliconFlow remain explicit compatibility backends.
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import logging

import httpx
from ai_gateway_core.config.endpoints import normalize_dashscope_base

logger = logging.getLogger(__name__)


# Prompt retained for the compatibility backends. Qwen-OCR native tasks carry
# their own optimized prompt and therefore intentionally do not receive this
# free-form prompt.
OCR_PROMPT = (
    "You are a highly accurate OCR engine. Extract ALL text from this scanned document image.\n"
    "Rules:\n"
    "1. Preserve the original language (Arabic and/or English) exactly as written.\n"
    "2. Maintain right-to-left (RTL) reading order for Arabic text.\n"
    "3. Preserve paragraph breaks and logical structure.\n"
    "4. If there are tables, reproduce them in a readable text format.\n"
    "5. If there are headers or titles, mark them clearly.\n"
    "6. Do NOT translate, summarize, or interpret — only transcribe.\n"
    "7. If text is partially illegible, transcribe what you can and mark unclear parts with [?].\n"
    "Output the extracted text only, with no additional commentary."
)


def _detect_media_type(image_bytes: bytes) -> str:
    """Detect image media type from magic bytes."""
    if len(image_bytes) >= 8 and image_bytes[:8] == b"\x89PNG\r\n\x1a\n":
        return "image/png"
    if len(image_bytes) >= 2 and image_bytes[:2] == b"\xff\xd8":
        return "image/jpeg"
    return "image/png"


# ============================================================================
# Gemini Backend
# ============================================================================


class _GeminiOCRBackend:
    """OCR backend using Google Gemini API (google-genai SDK)."""

    def __init__(self, api_key: str, model: str) -> None:
        from google import genai

        self._client = genai.Client(api_key=api_key)
        self._model = model

    async def call(self, image_bytes: bytes) -> str:
        from google.genai import types

        mime_type = _detect_media_type(image_bytes)
        image_part = types.Part.from_bytes(data=image_bytes, mime_type=mime_type)

        response = await asyncio.to_thread(
            self._client.models.generate_content,
            model=self._model,
            contents=[image_part, OCR_PROMPT],
        )

        if not response or not response.text:
            return ""
        return response.text.strip()


# ============================================================================
# DashScope Backend
# ============================================================================


_DASHSCOPE_DEFAULT_OCR_MODEL = "qwen-vl-ocr"
_DASHSCOPE_DEFAULT_NATIVE_BASE_URL = "https://dashscope.aliyuncs.com/api/v1"
_DASHSCOPE_OCR_PATH = "/services/aigc/multimodal-generation/generation"
_DASHSCOPE_OCR_TASKS = frozenset(
    {
        "text_recognition",
        "advanced_recognition",
        "key_information_extraction",
        "table_parsing",
        "document_parsing",
        "formula_recognition",
        "multi_lan",
    }
)


class _DashScopeOCRBackend:
    """OCR backend using the native DashScope Qwen-OCR API.

    The DashScope SDK exposes this API, but its global base-url setting makes
    it unsafe when chat and OCR use different regions in one process. A small
    dedicated async HTTP client keeps the endpoint scoped to this backend and
    also gives us bounded request timeouts and clean shutdown.
    """

    def __init__(
        self,
        api_key: str = "",
        model: str = _DASHSCOPE_DEFAULT_OCR_MODEL,
        *,
        api_keys: list[str] | None = None,
        base_url: str | None = None,
        task: str = "document_parsing",
        min_pixels: int = 3_072,
        max_pixels: int = 8_388_608,
        max_tokens: int = 8_192,
        enable_rotate: bool = True,
        timeout_seconds: float = 60.0,
        transport: httpx.AsyncBaseTransport | None = None,
    ) -> None:
        keys = [key.strip() for key in (api_keys or []) if key and key.strip()]
        if not keys and api_key.strip():
            keys = [api_key.strip()]
        if not keys:
            raise ValueError("At least one API key is required for DashScope OCR")
        if task not in _DASHSCOPE_OCR_TASKS:
            raise ValueError(f"Unsupported DashScope OCR task: {task}")
        if min_pixels <= 0 or max_pixels <= 0 or min_pixels > max_pixels:
            raise ValueError("DashScope OCR pixel bounds are invalid")
        if max_tokens <= 0 or max_tokens > 8_192:
            raise ValueError("DashScope OCR max_tokens must be between 1 and 8192")

        self._api_keys = keys
        self._key_index = 0
        self._key_lock = asyncio.Lock()
        self._model = model or _DASHSCOPE_DEFAULT_OCR_MODEL
        self._task = task
        self._min_pixels = int(min_pixels)
        self._max_pixels = int(max_pixels)
        self._max_tokens = int(max_tokens)
        self._enable_rotate = bool(enable_rotate)
        self._base_url = normalize_dashscope_base(
            base_url or _DASHSCOPE_DEFAULT_NATIVE_BASE_URL,
            "ocr",
        )
        self._endpoint = self._base_url.rstrip("/") + _DASHSCOPE_OCR_PATH
        timeout = max(float(timeout_seconds), 1.0)
        self._client = httpx.AsyncClient(
            timeout=httpx.Timeout(timeout, connect=min(10.0, timeout)),
            limits=httpx.Limits(max_connections=50, max_keepalive_connections=25),
            transport=transport,
        )

    async def _next_key(self) -> str:
        async with self._key_lock:
            key = self._api_keys[self._key_index % len(self._api_keys)]
            self._key_index += 1
            return key

    async def call(self, image_bytes: bytes) -> str:
        if not image_bytes:
            return ""

        media_type = _detect_media_type(image_bytes)
        b64_data = base64.b64encode(image_bytes).decode("utf-8")
        image_uri = f"data:{media_type};base64,{b64_data}"

        payload = {
            "model": self._model,
            "input": {
                "messages": [
                    {
                        "role": "user",
                        "content": [
                            {
                                "image": image_uri,
                                "min_pixels": self._min_pixels,
                                "max_pixels": self._max_pixels,
                                "enable_rotate": self._enable_rotate,
                            }
                        ],
                    }
                ]
            },
            "parameters": {
                "max_tokens": self._max_tokens,
                "ocr_options": {"task": self._task},
            },
        }
        key = await self._next_key()
        response = await self._client.post(
            self._endpoint,
            json=payload,
            headers={
                "Authorization": f"Bearer {key}",
                "Content-Type": "application/json",
            },
        )

        if response.status_code >= 400:
            code = ""
            message = ""
            with contextlib.suppress(ValueError, TypeError):
                error_body = response.json()
                if isinstance(error_body, dict):
                    code = str(error_body.get("code") or "")
                    message = str(error_body.get("message") or "")
            detail = " ".join(part for part in (code, message) if part)
            raise RuntimeError(
                f"DashScope OCR API error: HTTP {response.status_code}"
                + (f" {detail}" if detail else "")
            )

        try:
            body = response.json()
        except (ValueError, TypeError) as exc:
            raise RuntimeError("DashScope OCR API returned invalid JSON") from exc
        if not isinstance(body, dict):
            return ""

        output = body.get("output")
        if not output:
            return ""

        choices = output.get("choices", []) if isinstance(output, dict) else []
        if not choices:
            return ""

        first_choice = choices[0] if isinstance(choices[0], dict) else {}
        message = first_choice.get("message", {})
        message_content = message.get("content", []) if isinstance(message, dict) else []
        if isinstance(message_content, str):
            return message_content.strip()

        text_parts: list[str] = []
        for item in message_content:
            if isinstance(item, dict) and "text" in item:
                text_parts.append(str(item["text"]))
            elif isinstance(item, dict) and "ocr_result" in item:
                result = item["ocr_result"]
                text_parts.append(result if isinstance(result, str) else str(result))
            elif isinstance(item, str):
                text_parts.append(item)

        return "".join(text_parts).strip()

    async def close(self) -> None:
        await self._client.aclose()


# ============================================================================
# SiliconFlow Backend (DeepSeek-OCR with multi-key round-robin)
# ============================================================================

_SILICONFLOW_DEFAULT_URL = "https://api.siliconflow.cn/v1/chat/completions"
_SILICONFLOW_DEFAULT_MODEL = "deepseek-ai/DeepSeek-OCR"


class _SiliconFlowOCRBackend:
    """OCR backend using SiliconFlow API (OpenAI-compatible) with multi-key rotation.

    Supports round-robin across multiple API keys with automatic rotation
    on 429 rate limits. Concurrency is 5 requests per key.
    """

    def __init__(
        self,
        api_keys: list[str],
        model: str = _SILICONFLOW_DEFAULT_MODEL,
        base_url: str = _SILICONFLOW_DEFAULT_URL,
        max_retries: int = 3,
    ) -> None:
        if not api_keys:
            raise ValueError("At least one SiliconFlow API key is required")
        self._api_keys = api_keys
        self._model = model
        self._base_url = base_url
        self._max_retries = max_retries
        self._key_index = 0
        self._lock = asyncio.Lock()
        self._client = httpx.AsyncClient(
            timeout=httpx.Timeout(120.0, connect=10.0),
            limits=httpx.Limits(max_connections=50, max_keepalive_connections=25),
        )

    async def _next_key(self) -> str:
        """Round-robin key selection."""
        async with self._lock:
            key = self._api_keys[self._key_index % len(self._api_keys)]
            self._key_index += 1
            return key

    async def call(self, image_bytes: bytes) -> str:
        mime = _detect_media_type(image_bytes)
        b64 = base64.b64encode(image_bytes).decode()

        payload = {
            "model": self._model,
            "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": f"data:{mime};base64,{b64}"}},
                {"type": "text", "text": OCR_PROMPT},
            ]}],
            "max_tokens": 4096,
        }

        for attempt in range(self._max_retries):
            key = await self._next_key()
            try:
                resp = await self._client.post(
                    self._base_url,
                    json=payload,
                    headers={"Authorization": f"Bearer {key}", "Content-Type": "application/json"},
                )
                if resp.status_code == 429:
                    delay = 1 if len(self._api_keys) > 1 else 2 ** (attempt + 1)
                    logger.warning(
                        "SiliconFlow OCR 429 on key ..%s, rotating (attempt %d/%d)",
                        key[-6:], attempt + 1, self._max_retries,
                    )
                    await asyncio.sleep(delay)
                    continue
                if resp.status_code >= 400:
                    logger.warning("SiliconFlow OCR error %d: %s", resp.status_code, resp.text[:200])
                    if attempt < self._max_retries - 1:
                        await asyncio.sleep(2 ** attempt)
                    continue
                data = resp.json()
                text = data.get("choices", [{}])[0].get("message", {}).get("content", "").strip()
                return text
            except Exception as exc:
                if attempt < self._max_retries - 1:
                    await asyncio.sleep(2 ** attempt)
                else:
                    logger.error("SiliconFlow OCR failed after %d attempts: %s", self._max_retries, exc)
        return ""

    async def close(self) -> None:
        await self._client.aclose()


# ============================================================================
# Unified VLM OCR Service
# ============================================================================

# Known Gemini vision models
GEMINI_MODELS = frozenset({
    "gemini-3-flash-preview",
    "gemini-3.0-flash",
    "gemini-2.5-flash",
    "gemini-2.5-flash-preview-05-20",
    "gemini-2.0-flash",
    "gemini-2.0-flash-001",
    "gemini-1.5-flash",
    "gemini-1.5-pro",
})


class VLMOCRService:
    """High-accuracy OCR using vision language models.

    Supports Gemini, DashScope, and SiliconFlow backends. Automatically selects
    backend based on model name, or you can specify provider explicitly.

    SiliconFlow supports multi-key round-robin for higher throughput.
    Optimized for bilingual Arabic+English scanned documents with RTL layout.
    """

    # Keep OCR_PROMPT as class attribute for backward compatibility with tests
    OCR_PROMPT = OCR_PROMPT

    def __init__(
        self,
        api_key: str = "",
        model: str = _DASHSCOPE_DEFAULT_OCR_MODEL,
        provider: str = "auto",
        concurrency: int = 4,
        timeout_seconds: int = 30,
        max_retries: int = 2,
        api_keys: list[str] | None = None,
        base_url: str | None = None,
        task: str = "document_parsing",
        min_pixels: int = 3_072,
        max_pixels: int = 8_388_608,
        max_tokens: int = 8_192,
        enable_rotate: bool = True,
    ) -> None:
        """Initialize VLM OCR service.

        Args:
            api_key: API key for single-key providers (Gemini, DashScope).
            model: Model name.
            provider: "gemini", "dashscope", "siliconflow", or "auto".
            concurrency: Max concurrent OCR requests (overridden for siliconflow).
            timeout_seconds: Per-request timeout.
            max_retries: Retry count on failure.
            api_keys: Multiple API keys for round-robin (siliconflow).
            base_url: Custom API base URL override.
            task: Native DashScope OCR task (default: document_parsing).
            min_pixels: Minimum pixels sent to the OCR model.
            max_pixels: Maximum pixels sent to the OCR model.
            max_tokens: Maximum OCR output tokens per page.
            enable_rotate: Ask DashScope to auto-correct page rotation.
        """
        self.model = model or _DASHSCOPE_DEFAULT_OCR_MODEL
        self.task = task
        self.min_pixels = int(min_pixels)
        self.max_pixels = int(max_pixels)
        self.max_tokens = int(max_tokens)
        self.enable_rotate = bool(enable_rotate)
        self.timeout_seconds = timeout_seconds
        self.max_retries = max(1, max_retries)

        # Resolve provider
        if provider == "auto":
            if self.model.startswith("gemini-"):
                provider = "gemini"
            elif "deepseek" in self.model.lower():
                provider = "siliconflow"
            else:
                provider = "dashscope"

        self.provider = provider

        if provider == "siliconflow":
            keys = api_keys or ([api_key] if api_key else [])
            if not keys or not any(keys):
                raise ValueError("At least one API key is required for SiliconFlow OCR")
            self.concurrency = 5 * len(keys)
            self._semaphore = asyncio.Semaphore(self.concurrency)
            self._backend = _SiliconFlowOCRBackend(
                api_keys=keys,
                model=self.model or _SILICONFLOW_DEFAULT_MODEL,
                base_url=base_url or _SILICONFLOW_DEFAULT_URL,
                max_retries=self.max_retries,
            )
            logger.info(
                "VLMOCRService initialized: provider=siliconflow, model=%s, keys=%d, concurrency=%d",
                self.model, len(keys), self.concurrency,
            )
        else:
            keys = api_keys or ([api_key] if api_key else [])
            if provider == "dashscope" and not keys:
                raise ValueError("At least one API key is required for DashScope OCR")
            if provider != "dashscope" and not api_key:
                raise ValueError("API key is required for VLM OCR service")
            self.api_key = api_key or (keys[0] if provider == "dashscope" else "")
            self.concurrency = max(1, concurrency)
            self._semaphore = asyncio.Semaphore(self.concurrency)

            if provider == "gemini":
                self._backend = _GeminiOCRBackend(api_key=self.api_key, model=self.model)
            elif provider == "dashscope":
                self._backend = _DashScopeOCRBackend(
                    api_key=self.api_key,
                    api_keys=keys,
                    model=self.model,
                    base_url=base_url,
                    task=task,
                    min_pixels=self.min_pixels,
                    max_pixels=self.max_pixels,
                    max_tokens=self.max_tokens,
                    enable_rotate=self.enable_rotate,
                    timeout_seconds=timeout_seconds,
                )
            else:
                raise ValueError(
                    f"Unknown OCR provider: {provider}. Use 'gemini', 'dashscope', or 'siliconflow'."
                )
            logger.info(
                "VLMOCRService initialized: provider=%s, model=%s, concurrency=%d",
                provider, self.model, self.concurrency,
            )

    async def ocr_image(self, image_bytes: bytes) -> str:
        """Extract text from a single image using VLM.

        Args:
            image_bytes: PNG or JPEG image bytes.

        Returns:
            Extracted text, or empty string on failure.
        """
        async with self._semaphore:
            return await self._call_with_retry(image_bytes)

    async def ocr_pdf_pages(self, images: list[bytes]) -> list[str]:
        """Extract text from multiple page images concurrently.

        Args:
            images: List of page image bytes.

        Returns:
            List of extracted texts, one per page (empty string on failure).
        """
        if not images:
            return []

        tasks = [self.ocr_image(img) for img in images]
        return list(await asyncio.gather(*tasks))

    async def _call_with_retry(self, image_bytes: bytes) -> str:
        """Call OCR backend with retry logic."""
        last_error: Exception | None = None
        for attempt in range(self.max_retries):
            try:
                return await self._backend.call(image_bytes)
            except Exception as e:
                last_error = e
                if attempt < self.max_retries - 1:
                    wait = (attempt + 1) * 2
                    logger.warning(
                        f"VLM OCR attempt {attempt + 1} failed, retrying in {wait}s: {e}"
                    )
                    await asyncio.sleep(wait)

        logger.error(f"VLM OCR failed after {self.max_retries} attempts: {last_error}")
        return ""

    async def close(self) -> None:
        """Close any provider-owned HTTP resources."""
        close = getattr(self._backend, "close", None)
        if callable(close):
            await close()

    # Keep _detect_media_type as instance method for backward compatibility with tests
    def _detect_media_type(self, image_bytes: bytes) -> str:
        return _detect_media_type(image_bytes)
