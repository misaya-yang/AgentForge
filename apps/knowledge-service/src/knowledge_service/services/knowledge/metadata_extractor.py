"""Generic LLM-based metadata extractor for knowledge base chunks.

Uses Qwen 3.6-Plus (free preview via DashScope) to extract structured
metadata from arbitrary document chunks.

Fields extracted: topic, entities, keywords, summary.
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
from dataclasses import dataclass, field
from typing import Any

import httpx
from ai_gateway_core.config import resolve_dashscope

from ...config import get_settings

logger = logging.getLogger(__name__)

# Keep the exported module value compatible with the previous environment-read
# contract. Runtime instances resolve again in ``__init__`` so workers and tests
# use the current per-domain precedence and URL normalization.
DASHSCOPE_BASE_URL = f"{resolve_dashscope('chat')[1].rstrip('/')}/v1"
DEFAULT_MODEL = "qwen3.6-plus"

EXTRACTION_PROMPT = """Extract structured metadata from the following text chunk. Return ONLY a JSON object with:
- "topic": primary topic (1-5 words, e.g. "deployment rollback steps")
- "entities": list of named entities (people, places, concepts, terms) — max 10
- "keywords": list of search keywords/phrases — max 8
- "summary": one-sentence summary (under 50 words)

Rules:
- Be precise and specific, not generic
- Keywords should help search retrieval
- For multilingual text, include source-language and English terms where applicable
- Return ONLY the JSON object, no other text

Text:
---
{text}
---"""

BATCH_PROMPT = """Extract structured metadata from each text chunk below. Return a JSON array where each element has:
- "topic": primary topic (1-5 words)
- "entities": list of named entities — max 10
- "keywords": list of search keywords — max 8
- "summary": one-sentence summary (under 50 words)

Return ONLY a JSON array with one object per chunk, in the same order. No other text.

{chunks}"""


@dataclass
class ExtractionResult:
    """Result of metadata extraction for a single chunk."""

    topic: str = ""
    entities: list[str] = field(default_factory=list)
    keywords: list[str] = field(default_factory=list)
    summary: str = ""
    success: bool = False


class MetadataExtractor:
    """Generic LLM-based metadata extractor.

    Supports:
      - "dashscope" (default): Qwen 3.6-Plus via DashScope OpenAI-compatible API
      - "gemini": Gemini Flash via Google Generative Language API
    """

    def __init__(
        self,
        api_key: str = "",
        model: str = DEFAULT_MODEL,
        base_url: str | None = None,
        provider: str = "dashscope",
        max_concurrent: int = 5,
        timeout: float = 30.0,
    ):
        self.provider = provider
        if provider == "gemini":
            self.api_key = api_key or get_settings().google_api_key
            self.model = model if model != DEFAULT_MODEL else "gemini-2.0-flash"
            self.base_url = base_url or DASHSCOPE_BASE_URL
        else:
            resolved_key, resolved_base_url = resolve_dashscope("chat")
            self.api_key = api_key or resolved_key
            self.model = model
            self.base_url = base_url or f"{resolved_base_url.rstrip('/')}/v1"
        self.max_concurrent = max_concurrent
        self.timeout = timeout
        self._semaphore = asyncio.Semaphore(max_concurrent)
        self._client: httpx.AsyncClient | None = None

    async def _get_client(self) -> httpx.AsyncClient:
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(timeout=self.timeout)
        return self._client

    async def close(self) -> None:
        if self._client and not self._client.is_closed:
            await self._client.aclose()
            self._client = None

    async def extract_single(self, text: str) -> ExtractionResult:
        """Extract metadata from a single text chunk."""
        if not text or not self.api_key:
            return ExtractionResult()

        async with self._semaphore:
            return await self._call_llm_single(text)

    async def extract_batch(
        self,
        texts: list[str],
        batch_size: int = 10,
    ) -> list[ExtractionResult]:
        """Extract metadata from multiple chunks, batching LLM calls.

        Args:
            texts: List of text chunks to process.
            batch_size: Number of chunks per LLM call (max 20).
        """
        if not texts or not self.api_key:
            return [ExtractionResult() for _ in texts]

        batch_size = min(batch_size, 20)
        results: list[ExtractionResult] = []

        # Process in batches — track actual batch lengths for error padding
        tasks = []
        batch_lengths = []
        for i in range(0, len(texts), batch_size):
            batch = texts[i : i + batch_size]
            batch_lengths.append(len(batch))
            tasks.append(self._call_llm_batch(batch))

        batch_results = await asyncio.gather(*tasks, return_exceptions=True)

        for br, actual_len in zip(batch_results, batch_lengths, strict=True):
            if isinstance(br, Exception):
                logger.warning(f"Batch extraction failed: {br}")
                results.extend([ExtractionResult() for _ in range(actual_len)])
            else:
                results.extend(br)

        return results[: len(texts)]

    async def _call_llm_single(self, text: str) -> ExtractionResult:
        """Call LLM for single chunk extraction."""
        prompt = EXTRACTION_PROMPT.format(text=text[:2000])
        raw = await self._call_api(prompt, max_tokens=300)
        if raw is None:
            return ExtractionResult()
        return self._parse_single(raw)

    async def _call_llm_batch(self, texts: list[str]) -> list[ExtractionResult]:
        """Call LLM for batch extraction."""
        chunks_text = "\n".join(
            f"--- Chunk {i + 1} ---\n{t[:1500]}\n"
            for i, t in enumerate(texts)
        )
        prompt = BATCH_PROMPT.format(chunks=chunks_text)
        raw = await self._call_api(prompt, max_tokens=300 * len(texts))
        if raw is None:
            return [ExtractionResult() for _ in texts]

        parsed = self._parse_batch(raw, len(texts))
        return parsed

    async def _call_api(self, prompt: str, max_tokens: int = 300) -> str | None:
        """Make the actual API call (supports dashscope and gemini).

        Note: caller (extract_single/extract_batch) manages the semaphore.
        """
        client = await self._get_client()
        try:
            if self.provider == "gemini":
                return await self._call_gemini(client, prompt, max_tokens)
            else:
                return await self._call_openai_compat(client, prompt, max_tokens)
        except Exception as e:
            logger.warning(f"LLM metadata extraction API error: {e}")
            return None

    async def _call_openai_compat(
        self, client: httpx.AsyncClient, prompt: str, max_tokens: int
    ) -> str:
        """Call DashScope / OpenAI-compatible API."""
        resp = await client.post(
            f"{self.base_url}/chat/completions",
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
            json={
                "model": self.model,
                "messages": [{"role": "user", "content": prompt}],
                "temperature": 0.0,
                "max_tokens": min(max_tokens, 4000),
            },
            timeout=self.timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        return data["choices"][0]["message"]["content"]

    async def _call_gemini(
        self, client: httpx.AsyncClient, prompt: str, max_tokens: int
    ) -> str:
        """Call Google Gemini API."""
        url = (
            f"https://generativelanguage.googleapis.com/v1beta/"
            f"models/{self.model}:generateContent?key={self.api_key}"
        )
        resp = await client.post(
            url,
            json={
                "contents": [{"parts": [{"text": prompt}]}],
                "generationConfig": {
                    "temperature": 0,
                    "maxOutputTokens": min(max_tokens, 4000),
                },
            },
            timeout=self.timeout,
        )
        resp.raise_for_status()
        data = resp.json()
        return data["candidates"][0]["content"]["parts"][0]["text"]

    def _parse_single(self, raw: str) -> ExtractionResult:
        """Parse single JSON result from LLM response."""
        obj = self._extract_json_object(raw)
        if obj is None:
            return ExtractionResult()
        return self._obj_to_result(obj)

    def _parse_batch(self, raw: str, expected: int) -> list[ExtractionResult]:
        """Parse JSON array from batch LLM response."""
        arr = self._extract_json_array(raw)
        if arr is None:
            # Fallback: try as single object
            obj = self._extract_json_object(raw)
            if obj:
                return [self._obj_to_result(obj)] + [
                    ExtractionResult() for _ in range(expected - 1)
                ]
            return [ExtractionResult() for _ in range(expected)]

        results = []
        for item in arr:
            results.append(self._obj_to_result(item) if isinstance(item, dict) else ExtractionResult())

        # Pad if fewer results than expected
        while len(results) < expected:
            results.append(ExtractionResult())

        return results[:expected]

    @staticmethod
    def _obj_to_result(obj: dict[str, Any]) -> ExtractionResult:
        """Convert parsed JSON object to ExtractionResult."""
        return ExtractionResult(
            topic=str(obj.get("topic", ""))[:200],
            entities=[str(e)[:100] for e in obj.get("entities", []) if e][:10],
            keywords=[str(k)[:100] for k in obj.get("keywords", []) if k][:8],
            summary=str(obj.get("summary", ""))[:500],
            success=True,
        )

    @staticmethod
    def _extract_json_object(text: str) -> dict[str, Any] | None:
        """Extract JSON object from LLM response, handling markdown fences."""
        text = text.strip()
        if "```" in text:
            text = re.sub(r"```\w*\n?", "", text).strip()
        try:
            result = json.loads(text)
            if isinstance(result, dict):
                return result
            if isinstance(result, list) and result and isinstance(result[0], dict):
                return result[0]
        except json.JSONDecodeError:
            # Try to find JSON in the text
            match = re.search(r"\{[^{}]*\}", text, re.DOTALL)
            if match:
                try:
                    return json.loads(match.group())
                except json.JSONDecodeError:
                    pass
        return None

    @staticmethod
    def _extract_json_array(text: str) -> list[dict[str, Any]] | None:
        """Extract JSON array from LLM response."""
        text = text.strip()
        if "```" in text:
            text = re.sub(r"```\w*\n?", "", text).strip()
        try:
            result = json.loads(text)
            if isinstance(result, list):
                return result
        except json.JSONDecodeError:
            match = re.search(r"\[.*\]", text, re.DOTALL)
            if match:
                try:
                    return json.loads(match.group())
                except json.JSONDecodeError:
                    pass
        return None
