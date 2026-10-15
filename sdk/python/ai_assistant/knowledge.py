"""
Knowledge-base module — dataset listing and RAG-powered Q&A.

Wraps ``/api/v1/assistant/datasets`` and delegates KB-aware chat to the
``ChatModule`` with appropriate ``kb_*`` parameters.
"""

from __future__ import annotations

import hashlib
import re
import unicodedata
from typing import TYPE_CHECKING, Any
from urllib.parse import quote

from ai_assistant.exceptions import AssistantError
from ai_assistant.models.response import ChatResponse, Dataset

if TYPE_CHECKING:
    from collections.abc import AsyncIterator

    from ai_assistant.chat import ChatModule
    from ai_assistant.models.events import StreamEvent
    from ai_assistant.transport.http import HTTPTransport


class KnowledgeModule:
    """High-level knowledge-base operations."""

    _DATASETS_PATH = "/api/v1/assistant/datasets"

    def __init__(self, transport: HTTPTransport, chat: ChatModule) -> None:
        self._transport = transport
        self._chat = chat

    async def list_datasets(self) -> list[Dataset]:
        """List all knowledge-base datasets accessible to the current user."""
        resp = await self._transport.request("GET", self._DATASETS_PATH)
        data = resp.json()
        datasets_raw = data.get("datasets", data) if isinstance(data, dict) else data
        return [Dataset.from_dict(d) for d in datasets_raw]

    async def get_source(
        self,
        dataset_id: str,
        document_id: str,
        *,
        source_version: int,
        source_hash: str,
    ) -> dict[str, Any]:
        """Open the exact immutable source identified by a returned citation.

        Current owner/document/source access is rechecked by the Gateway. This
        GET uses the transport's ordinary read retry policy. It never submits
        work, restores a version, or falls back to current document content.
        The hash covers the full UTF-8 source, not the bounded citation excerpt.
        The JSON response is limited to 2 MiB and must use identity encoding.
        Only the five public source fields are returned after validation.
        """
        identifiers = []
        for value in (dataset_id, document_id):
            if (
                not isinstance(value, str) or not value or value != value.strip()
                or value in {".", ".."} or any(char in value for char in "/\\?#%")
                or any(unicodedata.category(char) in {"Cc", "Cs"} for char in value)
                or len(value.encode("utf-8")) > 1024
            ):
                raise ValueError("Source identifiers must be safe nonempty path segments")
            identifiers.append(quote(value, safe=""))
        if type(source_version) is not int or not 1 <= source_version <= 2**31 - 1:
            raise ValueError("Source version must be a positive 32-bit integer")
        if (
            not isinstance(source_hash, str)
            or re.fullmatch(r"[0-9a-fA-F]{64}", source_hash) is None
        ):
            raise ValueError("Source hash must be a 64-character SHA256 hexadecimal digest")
        expected_hash = source_hash.lower()
        response = await self._transport.request(
            "GET",
            f"/api/v1/knowledge/{identifiers[0]}/documents/{identifiers[1]}"
            f"/versions/{source_version}/source",
            params={"hash": expected_hash}, max_response_bytes=2 * 1024 * 1024,
        )
        try:
            value = response.json()
            if (
                not isinstance(value, dict) or value.get("document_id") != document_id
                or type(value.get("version_number")) is not int
                or value["version_number"] != source_version
                or value.get("source_hash") != expected_hash
                or not isinstance(value.get("title"), str)
                or not isinstance(value.get("content"), str)
                or hashlib.sha256(value["content"].encode("utf-8")).hexdigest() != expected_hash
            ):
                raise ValueError("Source identity or content mismatch")
            return {key: value[key] for key in (
                "document_id", "version_number", "source_hash", "title", "content",
            )}
        except ValueError:
            raise AssistantError("Invalid immutable source response") from None

    async def ask(
        self,
        question: str,
        dataset_ids: list[str],
        *,
        model_id: str | None = None,
        kb_mode: str = "auto",
        kb_top_k: int | None = None,
        kb_score_threshold: float | None = None,
        kb_include_images: bool | None = None,
        session_id: str | None = None,
        **kwargs: Any,
    ) -> ChatResponse:
        """Ask a question using knowledge-base retrieval (non-streaming).

        This is a convenience wrapper around ``ChatModule.send`` with the
        KB parameters pre-filled.
        """
        return await self._chat.send(
            question,
            session_id=session_id,
            model_id=model_id,
            kb_dataset_ids=dataset_ids,
            kb_mode=kb_mode,
            kb_top_k=kb_top_k,
            kb_score_threshold=kb_score_threshold,
            kb_include_images=kb_include_images,
            **kwargs,
        )

    async def ask_stream(
        self,
        question: str,
        dataset_ids: list[str],
        *,
        model_id: str | None = None,
        kb_mode: str = "auto",
        kb_top_k: int | None = None,
        kb_score_threshold: float | None = None,
        kb_include_images: bool | None = None,
        session_id: str | None = None,
        **kwargs: Any,
    ) -> AsyncIterator[StreamEvent]:
        """Ask a question using knowledge-base retrieval (streaming).

        Yields ``StreamEvent`` objects including ``context_retrieved`` events
        containing the retrieved chunks.
        """
        events = self._chat.stream(
            question,
            session_id=session_id,
            model_id=model_id,
            kb_dataset_ids=dataset_ids,
            kb_mode=kb_mode,
            kb_top_k=kb_top_k,
            kb_score_threshold=kb_score_threshold,
            kb_include_images=kb_include_images,
            **kwargs,
        )
        body_failed = False
        try:
            async for event in events:
                yield event
        except BaseException as exc:
            body_failed = not isinstance(exc, GeneratorExit)
            raise
        finally:
            try:
                close = getattr(events, "aclose", None)
                if close is not None:
                    await close()
            except BaseException:
                if not body_failed:
                    raise
