"""
LLM-based QA testing service for knowledge base retrieval validation.

Provides a complete RAG flow:
1. Query → Retrieval → Context Assembly → LLM Answer

Supports DeepSeek, Gemini, and DashScope LLM APIs.
"""

from __future__ import annotations

import asyncio
import json
import re
import time
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

import httpx

from ...config import get_settings
from ...core.exceptions import ValidationFailedError
from ...core.observability.logging import get_logger
from .knowledge_service import KnowledgeService, RetrieveResult
from .qa_provider_stream import QaProviderStream
from .retrieval_service import dataset_retrieval_generation

logger = get_logger(__name__)


class LLMProvider(str, Enum):
    """Supported LLM providers."""

    DEEPSEEK = "deepseek"
    GEMINI = "gemini"
    DASHSCOPE = "dashscope"
    CUSTOM = "custom"


@dataclass
class LLMConfig:
    """LLM configuration for QA testing."""

    provider: LLMProvider = LLMProvider.GEMINI
    model: str = ""  # Will use LLM_MODEL env or "gemini-2.0-flash"
    api_key: str | None = None
    base_url: str | None = None
    temperature: float = 0.1
    max_tokens: int = 2048
    timeout_seconds: float = 60.0

    def __post_init__(self):
        # Use environment variable for model if not set
        if not self.model:
            self.model = get_settings().llm_model

    # System prompt for QA
    system_prompt: str = """You are a helpful assistant that answers questions based on the provided context.

Rules:
1. Only answer based on the provided context
2. If the context doesn't contain relevant information, say "I don't have enough information to answer this question based on the provided context."
3. Be concise and accurate
4. Cite the relevant parts of the context when appropriate

Context will be provided in the user message."""

    @classmethod
    def from_dict(cls, data: dict[str, Any] | None) -> LLMConfig:
        if not data:
            return cls()

        provider_str = str(data.get("provider", "gemini")).lower()
        try:
            provider = LLMProvider(provider_str)
        except ValueError:
            provider = LLMProvider.CUSTOM

        return cls(
            provider=provider,
            model=str(data.get("model", "gemini-2.0-flash")),
            api_key=data.get("api_key"),
            base_url=data.get("base_url"),
            temperature=float(data.get("temperature", 0.1)),
            max_tokens=int(data.get("max_tokens", 2048)),
            timeout_seconds=float(data.get("timeout_seconds", 60.0)),
            system_prompt=str(data.get("system_prompt", cls.system_prompt)),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "provider": self.provider.value,
            "model": self.model,
            "base_url": self.base_url,
            "temperature": self.temperature,
            "max_tokens": self.max_tokens,
            "timeout_seconds": self.timeout_seconds,
            "system_prompt": self.system_prompt,
        }

    def get_base_url(self) -> str:
        """Get the appropriate base URL for the provider."""
        if self.base_url:
            return self.base_url.rstrip("/")

        # Try environment variable first
        env_url = get_settings().llm_base_url
        if env_url:
            return env_url.rstrip("/")

        defaults = {
            LLMProvider.DEEPSEEK: "https://api.deepseek.com/v1",
            LLMProvider.GEMINI: "https://generativelanguage.googleapis.com/v1beta/openai",
            LLMProvider.DASHSCOPE: "https://dashscope.aliyuncs.com/compatible-mode/v1",
        }
        return defaults.get(self.provider, "https://api.deepseek.com/v1")

    def get_api_key(self) -> str | None:
        """Get API key from config or environment.

        Env fallback order (first truthy wins, resolved via
        ``Settings.qa_llm_api_key``): generic ``LLM_API_KEY`` first, then
        provider-specific keys (deepseek: ``DEEPSEEK_API_KEY`` →
        ``DEEPSEEK_KEY``; gemini: ``GEMINI_API_KEY`` → ``GOOGLE_API_KEY``;
        dashscope: ``DASHSCOPE_API_KEY`` → ``ALIYUN_KEY``/``Aliyun_KEY``).
        """
        if self.api_key:
            return self.api_key

        return get_settings().qa_llm_api_key(self.provider.value)


@dataclass
class QAResult:
    """Result of a QA query."""

    query: str
    answer: str
    context_segments: list[dict[str, Any]]
    retrieval_metadata: dict[str, Any]

    # Timing information
    retrieval_time_ms: int
    llm_time_ms: int
    total_time_ms: int

    # LLM metadata
    model: str
    tokens_used: int | None = None
    trace_id: str | None = None
    query_fingerprint: str | None = None

    # Optional evaluation
    relevance_score: float | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "query": self.query,
            "answer": self.answer,
            "context_segments": self.context_segments,
            "retrieval_metadata": self.retrieval_metadata,
            "timing": {
                "retrieval_ms": self.retrieval_time_ms,
                "llm_ms": self.llm_time_ms,
                "total_ms": self.total_time_ms,
            },
            "model": self.model,
            "tokens_used": self.tokens_used,
            "trace_id": self.trace_id,
            "query_fingerprint": self.query_fingerprint,
            "relevance_score": self.relevance_score,
        }


@dataclass
class QATestCase:
    """A test case for QA evaluation."""

    query: str
    expected_answer: str | None = None
    expected_segments: list[str] | None = None  # segment_ids that should be retrieved
    metadata: dict[str, Any] = field(default_factory=dict)


@dataclass
class QATestResult:
    """Result of running a QA test case."""

    test_case: QATestCase
    qa_result: QAResult

    # Evaluation metrics
    answer_correct: bool | None = None
    retrieval_recall: float | None = None  # How many expected segments were retrieved
    retrieval_precision: float | None = None

    def to_dict(self) -> dict[str, Any]:
        return {
            "test_case": {
                "query": self.test_case.query,
                "expected_answer": self.test_case.expected_answer,
                "expected_segments": self.test_case.expected_segments,
            },
            "result": self.qa_result.to_dict(),
            "evaluation": {
                "answer_correct": self.answer_correct,
                "retrieval_recall": self.retrieval_recall,
                "retrieval_precision": self.retrieval_precision,
            },
        }


class LLMClient:
    """Async client for LLM API calls."""

    def __init__(self, config: LLMConfig):
        self.config = config
        self._client: httpx.AsyncClient | None = None

    async def _get_client(self) -> httpx.AsyncClient:
        if self._client is None:
            api_key = self.config.get_api_key()
            headers = {"Content-Type": "application/json"}
            if api_key:
                headers["Authorization"] = f"Bearer {api_key}"

            self._client = httpx.AsyncClient(
                base_url=self.config.get_base_url(),
                headers=headers,
                timeout=self.config.timeout_seconds,
            )
        return self._client

    async def close(self) -> None:
        if self._client:
            await self._client.aclose()
            self._client = None

    async def chat_completion(
        self,
        messages: list[dict[str, str]],
        **kwargs,
    ) -> tuple[str, int | None]:
        """
        Send a chat completion request.

        Returns: (response_text, tokens_used)
        """
        client = await self._get_client()

        payload = {
            "model": self.config.model,
            "messages": messages,
            "temperature": kwargs.get("temperature", self.config.temperature),
            "max_tokens": kwargs.get("max_tokens", self.config.max_tokens),
        }

        try:
            response = await client.post("/chat/completions", json=payload)
            response.raise_for_status()

            data = response.json()

            # Extract response text
            choices = data.get("choices", [])
            if not choices:
                raise ValueError("No choices in response")

            text = choices[0].get("message", {}).get("content", "")

            # Extract token usage
            usage = data.get("usage", {})
            tokens = usage.get("total_tokens")

            return text, tokens

        except httpx.HTTPStatusError as e:
            logger.error(f"LLM API error: {e.response.status_code} - {e.response.text}")
            raise
        except Exception as e:
            logger.error(f"LLM request failed: {e}")
            raise

    async def chat_completion_stream(
        self,
        messages: list[dict[str, str]],
        **kwargs,
    ):
        """
        Stream chat completion deltas.

        Yields dict events:
        - {"type": "delta", "content": "..."}
        - {"type": "usage", "tokens": int}
        """
        client = await self._get_client()
        payload = {
            "model": self.config.model,
            "messages": messages,
            "temperature": kwargs.get("temperature", self.config.temperature),
            "max_tokens": kwargs.get("max_tokens", self.config.max_tokens),
            "stream": True,
        }

        try:
            async with client.stream("POST", "/chat/completions", json=payload) as response:
                response.raise_for_status()
                parser = QaProviderStream()
                async for line in response.aiter_lines():
                    event = parser.line(line)
                    if event is None:
                        continue
                    if event.tokens is not None:
                        yield {"type": "usage", "tokens": event.tokens}
                    if event.content:
                        yield {"type": "delta", "content": event.content}
                    if event.closed:
                        break
                # EOF may finish a final event, but never supplies a missing stop.
                event = parser.finish()
                if event is not None:
                    if event.tokens is not None:
                        yield {"type": "usage", "tokens": event.tokens}
                    if event.content:
                        yield {"type": "delta", "content": event.content}
        except httpx.HTTPStatusError as e:
            logger.error(f"LLM stream API error: {e.response.status_code} - {e.response.text}")
            raise
        except Exception as e:
            logger.error(f"LLM stream request failed: {e}")
            raise


class QAService:
    """
    QA testing service for knowledge base validation.

    Provides:
    - Single query QA with retrieval
    - Batch testing with evaluation
    - Answer quality assessment
    """

    def __init__(
        self,
        knowledge_service: KnowledgeService,
        llm_config: LLMConfig | None = None,
    ):
        self.kb = knowledge_service
        self.llm_config = llm_config or LLMConfig()
        self._llm_client: LLMClient | None = None

    def _get_llm_client(self) -> LLMClient:
        if self._llm_client is None:
            self._llm_client = LLMClient(self.llm_config)
        return self._llm_client

    async def close(self) -> None:
        if self._llm_client:
            await self._llm_client.close()
            self._llm_client = None

    async def _source_generation(self, user: Any, dataset_id: str) -> tuple[Any, ...] | None:
        lookup = getattr(getattr(self.kb, "db", None), "published_source_identities", None)
        require_access = getattr(self.kb, "require_dataset_access", None)
        if not callable(lookup) or not callable(require_access):
            return None
        dataset = await require_access(user, dataset_id, required="viewer")
        return dataset_retrieval_generation(dataset)

    async def _context_segments(
        self, user: Any, dataset_id: str, results: list[RetrieveResult],
        *, source_generation: tuple[Any, ...] | None,
        include_raw_results: bool,
    ) -> list[dict[str, Any]]:
        identities: dict[str, tuple[int, str]] = {}
        if source_generation is not None and results:
            require_access = self.kb.require_dataset_access

            async def require_same_generation() -> None:
                dataset = await require_access(user, dataset_id, required="viewer")
                if dataset_retrieval_generation(dataset) != source_generation:
                    raise ValidationFailedError(
                        "dataset index generation changed during QA; retry the request"
                    )

            await require_same_generation()
            document_ids = sorted({str(item.document_id) for item in results})
            lookup = self.kb.db.published_source_identities
            found = await lookup(dataset_id, user.tenant_id, document_ids)
            if not isinstance(found, dict) or not set(found) <= set(document_ids):
                raise ValidationFailedError("published QA source identities are invalid")
            await require_same_generation()
            identities = found

        context_segments: list[dict[str, Any]] = []
        for result in results:
            source_metadata = result.metadata if isinstance(result.metadata, dict) else {}
            nested = source_metadata.get("metadata")
            nested = nested if isinstance(nested, dict) else {}
            manual = (
                source_metadata.get("source_type") == "manual"
                or source_metadata.get("manual_source_override") is True
                or nested.get("manual_source_override") is True
            )
            metadata = dict(source_metadata) if include_raw_results else {}
            metadata.pop("source_version", None)
            metadata.pop("source_hash", None)
            item = {
                "segment_id": result.segment_id,
                "document_id": result.document_id,
                "text": result.text,
                "score": result.score,
                "metadata": metadata,
            }
            identity = identities.get(result.document_id) if not manual else None
            if (
                isinstance(identity, (tuple, list)) and len(identity) == 2
                and type(identity[0]) is int and identity[0] > 0
                and isinstance(identity[1], str)
                and re.fullmatch(r"[0-9a-f]{64}", identity[1])
            ):
                item["source_version"], item["source_hash"] = identity
                if include_raw_results:
                    metadata["source_version"], metadata["source_hash"] = identity
            context_segments.append(item)
        return context_segments

    def _format_context(
        self,
        results: list[RetrieveResult],
        max_tokens: int = 4000,
    ) -> str:
        """Format retrieval results as context for LLM."""
        if not results:
            return "No relevant context found."

        # Estimate ~4 chars per token
        max_chars = max_tokens * 4

        context_parts = []
        total_chars = 0

        for i, result in enumerate(results):
            segment_text = f"[{i + 1}] {result.text.strip()}"

            # Check if we have room
            if total_chars + len(segment_text) > max_chars:
                break

            context_parts.append(segment_text)
            total_chars += len(segment_text)

        return "\n\n".join(context_parts)

    def _build_messages(
        self,
        query: str,
        context: str,
        system_prompt: str | None = None,
    ) -> list[dict[str, str]]:
        """Build message list for LLM."""
        system = system_prompt or self.llm_config.system_prompt

        user_content = f"""Context:
{context}

Question: {query}

Please answer the question based on the context provided above."""

        return [
            {"role": "system", "content": system},
            {"role": "user", "content": user_content},
        ]

    async def query(
        self,
        user_context: Any,  # UserContext from auth
        dataset_id: str,
        query: str,
        top_k: int = 5,
        mode: str = "hybrid",
        rerank: bool = False,
        mmr: bool = False,
        document_id: str | None = None,
        llm_config: LLMConfig | None = None,
        include_raw_results: bool = False,
        **retrieval_kwargs,
    ) -> QAResult:
        """
        Execute a complete QA flow: query → retrieve → LLM answer.

        Args:
            user_context: User context for authorization
            dataset_id: Target dataset
            query: User's question
            top_k: Number of segments to retrieve
            mode: Retrieval mode (hybrid/vector/keyword)
            rerank: Whether to use reranking
            mmr: Whether to use MMR diversity
            document_id: Optional filter to specific document
            llm_config: Optional LLM config override
            include_raw_results: Include raw retrieval results in response
            **retrieval_kwargs: Additional retrieval parameters

        Returns:
            QAResult with answer and metadata
        """
        start_time = time.time()
        source_generation = await self._source_generation(user_context, dataset_id)

        # Step 1: Retrieve relevant segments
        retrieval_start = time.time()

        results, meta = await self.kb.retrieve(
            user=user_context,
            dataset_id=dataset_id,
            query=query,
            top_k=top_k,
            mode=mode,
            document_id=document_id,
            rerank=rerank,
            mmr=mmr,
            telemetry_source="qa",
            **retrieval_kwargs,
        )

        retrieval_time = int((time.time() - retrieval_start) * 1000)

        # Step 2: Format context
        context = self._format_context(results)

        # Step 3: Generate answer
        config = llm_config or self.llm_config
        client = self._get_llm_client() if llm_config is None else LLMClient(config)

        llm_start = time.time()

        try:
            messages = self._build_messages(query, context)
            answer, tokens = await client.chat_completion(messages)

            llm_time = int((time.time() - llm_start) * 1000)

        finally:
            if llm_config is not None:
                await client.close()

        total_time = int((time.time() - start_time) * 1000)

        # Build result
        context_segments = await self._context_segments(
            user_context, dataset_id, results,
            source_generation=source_generation,
            include_raw_results=include_raw_results,
        )

        return QAResult(
            query=query,
            answer=answer,
            context_segments=context_segments,
            retrieval_metadata=meta,
            retrieval_time_ms=retrieval_time,
            llm_time_ms=llm_time,
            total_time_ms=total_time,
            model=config.model,
            tokens_used=tokens,
            trace_id=str(meta.get("trace_id") or "") or None,
            query_fingerprint=str(meta.get("query_fingerprint") or "") or None,
        )

    async def query_stream(
        self,
        user_context: Any,  # UserContext from auth
        dataset_id: str,
        query: str,
        top_k: int = 5,
        mode: str = "hybrid",
        rerank: bool = False,
        mmr: bool = False,
        document_id: str | None = None,
        llm_config: LLMConfig | None = None,
        include_raw_results: bool = False,
        **retrieval_kwargs,
    ):
        """
        Stream QA flow: retrieve → stream LLM answer.

        Yields dict events:
        - {"event": "retrieval", "data": {...}}
        - {"event": "delta", "data": {"content": "..." }}
        - {"event": "done", "data": {"result": {...}}}
        """
        start_time = time.time()
        source_generation = await self._source_generation(user_context, dataset_id)

        retrieval_start = time.time()
        results, meta = await self.kb.retrieve(
            user=user_context,
            dataset_id=dataset_id,
            query=query,
            top_k=top_k,
            mode=mode,
            document_id=document_id,
            rerank=rerank,
            mmr=mmr,
            telemetry_source="qa",
            **retrieval_kwargs,
        )
        retrieval_time = int((time.time() - retrieval_start) * 1000)

        context_segments = await self._context_segments(
            user_context, dataset_id, results,
            source_generation=source_generation,
            include_raw_results=include_raw_results,
        )

        yield {
            "event": "retrieval",
            "data": {
                "query": query,
                "context_segments": context_segments,
                "retrieval_metadata": meta,
                "trace_id": meta.get("trace_id"),
                "query_fingerprint": meta.get("query_fingerprint"),
                "timing": {"retrieval_ms": retrieval_time},
            },
        }

        context = self._format_context(results)
        config = llm_config or self.llm_config
        client = self._get_llm_client() if llm_config is None else LLMClient(config)
        llm_start = time.time()
        tokens_used: int | None = None
        answer_parts: list[str] = []

        try:
            messages = self._build_messages(query, context)
            async for chunk in client.chat_completion_stream(messages):
                if chunk.get("type") == "usage":
                    tokens_used = chunk.get("tokens")
                    continue
                if chunk.get("type") == "delta":
                    delta = chunk.get("content") or ""
                    if delta:
                        answer_parts.append(delta)
                        yield {"event": "delta", "data": {"content": delta}}
        finally:
            if llm_config is not None:
                await client.close()

        llm_time = int((time.time() - llm_start) * 1000)
        total_time = int((time.time() - start_time) * 1000)
        answer = "".join(answer_parts)

        result = QAResult(
            query=query,
            answer=answer,
            context_segments=context_segments,
            retrieval_metadata=meta,
            retrieval_time_ms=retrieval_time,
            llm_time_ms=llm_time,
            total_time_ms=total_time,
            model=config.model,
            tokens_used=tokens_used,
            trace_id=str(meta.get("trace_id") or "") or None,
            query_fingerprint=str(meta.get("query_fingerprint") or "") or None,
        )

        yield {"event": "done", "data": {"result": result.to_dict()}}

    async def evaluate_answer(
        self,
        query: str,
        answer: str,
        expected_answer: str,
        llm_config: LLMConfig | None = None,
    ) -> tuple[bool, float, str]:
        """
        Use LLM to evaluate if an answer is correct.

        Returns: (is_correct, confidence_score, explanation)
        """
        config = llm_config or self.llm_config
        client = LLMClient(config)

        eval_prompt = f"""You are an answer evaluator. Compare the given answer with the expected answer and determine if they are semantically equivalent.

Question: {query}

Expected Answer: {expected_answer}

Given Answer: {answer}

Respond in JSON format:
{{
    "is_correct": true/false,
    "confidence": 0.0-1.0,
    "explanation": "brief explanation"
}}"""

        try:
            messages = [
                {
                    "role": "system",
                    "content": "You are a precise answer evaluator. Output only valid JSON.",
                },
                {"role": "user", "content": eval_prompt},
            ]

            response, _ = await client.chat_completion(messages, temperature=0)

            # Parse JSON response
            try:
                result = json.loads(response)
                return (
                    bool(result.get("is_correct", False)),
                    float(result.get("confidence", 0.5)),
                    str(result.get("explanation", "")),
                )
            except json.JSONDecodeError:
                # Try to extract from text
                is_correct = "true" in response.lower() and "is_correct" in response.lower()
                return is_correct, 0.5, response

        finally:
            await client.close()

    async def run_test_case(
        self,
        user_context: Any,
        dataset_id: str,
        test_case: QATestCase,
        **qa_kwargs,
    ) -> QATestResult:
        """Run a single test case and evaluate results."""

        # Execute QA
        qa_result = await self.query(
            user_context=user_context,
            dataset_id=dataset_id,
            query=test_case.query,
            include_raw_results=True,
            **qa_kwargs,
        )

        # Initialize test result
        test_result = QATestResult(
            test_case=test_case,
            qa_result=qa_result,
        )

        # Evaluate answer correctness
        if test_case.expected_answer:
            is_correct, confidence, _ = await self.evaluate_answer(
                query=test_case.query,
                answer=qa_result.answer,
                expected_answer=test_case.expected_answer,
            )
            test_result.answer_correct = is_correct

        # Evaluate retrieval quality
        if test_case.expected_segments:
            retrieved_ids = {s["segment_id"] for s in qa_result.context_segments}
            expected_ids = set(test_case.expected_segments)

            # Recall: how many expected segments were retrieved
            if expected_ids:
                test_result.retrieval_recall = len(retrieved_ids & expected_ids) / len(expected_ids)

            # Precision: how many retrieved segments were expected
            if retrieved_ids:
                test_result.retrieval_precision = len(retrieved_ids & expected_ids) / len(
                    retrieved_ids
                )

        return test_result

    async def run_test_batch(
        self,
        user_context: Any,
        dataset_id: str,
        test_cases: list[QATestCase],
        concurrency: int = 3,
        **qa_kwargs,
    ) -> list[QATestResult]:
        """Run multiple test cases with controlled concurrency."""

        semaphore = asyncio.Semaphore(concurrency)

        async def run_one(test_case: QATestCase) -> QATestResult:
            async with semaphore:
                return await self.run_test_case(
                    user_context=user_context,
                    dataset_id=dataset_id,
                    test_case=test_case,
                    **qa_kwargs,
                )

        tasks = [run_one(tc) for tc in test_cases]
        results = await asyncio.gather(*tasks, return_exceptions=True)
        failures = [result for result in results if isinstance(result, BaseException)]
        if failures:
            # A partial batch must never shrink the denominator and masquerade
            # as a complete evaluation. Preserve the first failure as the
            # causal exception while making incompleteness explicit.
            raise RuntimeError(
                f"QA batch incomplete: {len(failures)} of {len(test_cases)} cases failed"
            ) from failures[0]

        if len(results) != len(test_cases) or not all(
            isinstance(result, QATestResult) for result in results
        ):
            raise RuntimeError("QA batch incomplete: invalid result cardinality")
        return [result for result in results if isinstance(result, QATestResult)]

    def aggregate_test_results(
        self,
        results: list[QATestResult],
    ) -> dict[str, Any]:
        """Aggregate test results into summary statistics."""
        if not results:
            return {"total": 0}

        total = len(results)

        # Answer accuracy
        answer_results = [r for r in results if r.answer_correct is not None]
        answer_accuracy = (
            sum(1 for r in answer_results if r.answer_correct) / len(answer_results)
            if answer_results
            else None
        )

        # Retrieval metrics
        recall_results = [r for r in results if r.retrieval_recall is not None]
        avg_recall = (
            sum(r.retrieval_recall for r in recall_results) / len(recall_results)
            if recall_results
            else None
        )

        precision_results = [r for r in results if r.retrieval_precision is not None]
        avg_precision = (
            sum(r.retrieval_precision for r in precision_results) / len(precision_results)
            if precision_results
            else None
        )

        # Timing stats
        avg_retrieval_ms = sum(r.qa_result.retrieval_time_ms for r in results) / total
        avg_llm_ms = sum(r.qa_result.llm_time_ms for r in results) / total
        avg_total_ms = sum(r.qa_result.total_time_ms for r in results) / total

        return {
            "total": total,
            "answer_accuracy": answer_accuracy,
            "avg_retrieval_recall": avg_recall,
            "avg_retrieval_precision": avg_precision,
            "timing": {
                "avg_retrieval_ms": round(avg_retrieval_ms, 2),
                "avg_llm_ms": round(avg_llm_ms, 2),
                "avg_total_ms": round(avg_total_ms, 2),
            },
        }


# Convenience function to create default DeepSeek config
def get_deepseek_config(api_key: str | None = None) -> LLMConfig:
    """Create a DeepSeek LLM configuration."""
    return LLMConfig(
        provider=LLMProvider.DEEPSEEK,
        model="deepseek-chat",
        api_key=api_key,
        temperature=0.1,
        max_tokens=2048,
    )


def get_gemini_config(
    api_key: str | None = None,
    model: str = "gemini-2.0-flash",
) -> LLMConfig:
    """Create a Gemini LLM configuration."""
    return LLMConfig(
        provider=LLMProvider.GEMINI,
        model=model,
        api_key=api_key,
        temperature=0.1,
        max_tokens=2048,
    )
