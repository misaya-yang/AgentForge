"""
LangGraph-compatible retrieval tool interface.

Provides tools that can be used directly by LangGraph agents
for knowledge base retrieval operations.

Compatible with both LangChain tools format and direct function calls.

Usage Examples:

1. Create a simple tool function:
    ```python
    from agent_gateway.services.knowledge import create_kb_tool

    kb_tool = create_kb_tool(kb_service, "my_dataset", user_context)
    result = await kb_tool("What is our refund policy?")
    ```

2. Create a LangChain-compatible tool:
    ```python
    from agent_gateway.services.knowledge import create_langchain_kb_tool

    tool = create_langchain_kb_tool(kb_service, "my_dataset", user_context)
    # Use with LangGraph
    from langgraph.prebuilt import ToolNode
    tool_node = ToolNode([tool])
    ```

3. Create a multi-dataset tool:
    ```python
    from agent_gateway.services.knowledge import create_multi_kb_tool

    tool = create_multi_kb_tool(kb_service, ["docs", "wiki"], user_context)
    ```
"""

from __future__ import annotations

import asyncio
import logging
import math
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from ...core.auth.user_resolver import UserContext
from ...core.exceptions import AuthenticationRequiredError, ValidationFailedError
from .knowledge_service import KnowledgeService

logger = logging.getLogger(__name__)


def _require_tool_query(query: Any) -> str:
    if not isinstance(query, str) or not 1 <= len(query.strip()) <= 4096:
        raise ValidationFailedError("knowledge tool query must contain 1-4096 characters")
    return query.strip()


def _require_tool_top_k(top_k: Any) -> int:
    if isinstance(top_k, bool) or not isinstance(top_k, int) or not 1 <= top_k <= 20:
        raise ValidationFailedError("knowledge tool top_k must be an integer between 1 and 20")
    return top_k


def _require_tool_score_threshold(score_threshold: Any) -> float | None:
    if score_threshold is None:
        return None
    if isinstance(score_threshold, bool) or not isinstance(score_threshold, (int, float)):
        raise ValidationFailedError(
            "knowledge tool score_threshold must be finite and between 0 and 1"
        )
    numeric = float(score_threshold)
    if not math.isfinite(numeric) or not 0.0 <= numeric <= 1.0:
        raise ValidationFailedError(
            "knowledge tool score_threshold must be finite and between 0 and 1"
        )
    return numeric


def _require_text_only_intent(intent: Any) -> str:
    normalized = str(intent or "general").strip().lower()
    if normalized not in {"general", "find_document"}:
        raise ValidationFailedError(
            "knowledge tool intent must be general or find_document"
        )
    return normalized


def _require_tool_dataset_ids(dataset_ids: Any) -> list[str]:
    if not isinstance(dataset_ids, list) or not 1 <= len(dataset_ids) <= 8:
        raise ValidationFailedError(
            "knowledge tool dataset_ids must be a list containing 1-8 entries"
        )
    normalized = [str(dataset_id or "").strip() for dataset_id in dataset_ids]
    if any(not dataset_id for dataset_id in normalized) or len(set(normalized)) != len(
        normalized
    ):
        raise ValidationFailedError(
            "knowledge tool dataset_ids must be non-empty and unique"
        )
    return normalized


@dataclass
class KBRetrievalInput:
    """Input schema for knowledge base retrieval."""

    query: str
    dataset_id: str
    top_k: int = 5
    mode: str = "hybrid"  # hybrid | vector | keyword
    intent: str = "general"  # general | find_document
    document_id: str | None = None
    rerank: bool = False
    mmr: bool = False


@dataclass
class KBRetrievalOutput:
    """Output schema for knowledge base retrieval."""

    segments: list[dict[str, Any]]
    metadata: dict[str, Any]
    query: str
    dataset_id: str


@dataclass
class KBSearchResult:
    """A single search result for LangGraph consumption."""

    content: str
    score: float
    segment_id: str
    document_id: str
    metadata: dict[str, Any]

    def to_dict(self) -> dict[str, Any]:
        return {
            "content": self.content,
            "score": self.score,
            "segment_id": self.segment_id,
            "document_id": self.document_id,
            "metadata": self.metadata,
        }


class KnowledgeRetriever:
    """
    Knowledge base retriever for LangGraph agents.

    This class provides multiple interfaces for knowledge retrieval:
    - Direct async method calls
    - LangChain tool-compatible interface
    - Structured input/output for type safety

    Example usage in LangGraph:

    ```python
    # Create retriever
    retriever = KnowledgeRetriever(kb_service, dataset_id="my_dataset")

    # Use as a tool in LangGraph
    tools = [retriever.as_tool()]

    # Or call directly
    results = await retriever.retrieve("What is X?")
    ```
    """

    def __init__(
        self,
        knowledge_service: KnowledgeService,
        dataset_id: str,
        user_context: UserContext,
        default_top_k: int = 5,
        default_mode: str = "hybrid",
        default_rerank: bool = False,
        default_mmr: bool = False,
    ):
        """
        Initialize the retriever.

        Args:
            knowledge_service: The knowledge base service
            dataset_id: Default dataset to search
            user_context: User context for authorization (REQUIRED)
            default_top_k: Default number of results to return
            default_mode: Default retrieval mode
            default_rerank: Whether to use reranking by default
            default_mmr: Whether to use MMR by default

        Raises:
            AuthenticationRequiredError: If user_context is None or not authenticated
        """
        if user_context is None:
            raise AuthenticationRequiredError("User context is required for knowledge retrieval")
        if not user_context.is_authenticated:
            raise AuthenticationRequiredError(
                "Authenticated user context is required for knowledge retrieval"
            )
        resolved_top_k = _require_tool_top_k(default_top_k)

        self.kb = knowledge_service
        self.dataset_id = dataset_id
        self.user_context = user_context
        self.default_top_k = resolved_top_k
        self.default_mode = default_mode
        self.default_rerank = default_rerank
        self.default_mmr = default_mmr

    @staticmethod
    def _create_system_context() -> UserContext:
        """
        Create a system-level user context.

        WARNING: Only use this for background tasks where no user context is available
        (e.g., scheduled sync jobs, admin operations). NEVER use in request handlers.
        For request handlers, always pass the actual user context.
        """
        return UserContext(
            user_id="system",
            tenant_id="system",
            is_authenticated=True,
            tier="admin",
            roles=["admin"],
        )

    async def retrieve(
        self,
        query: str,
        top_k: int | None = None,
        mode: str | None = None,
        document_id: str | None = None,
        rerank: bool | None = None,
        mmr: bool | None = None,
        dataset_id: str | None = None,
        intent: str | None = None,
        **kwargs,
    ) -> list[KBSearchResult]:
        """
        Retrieve relevant segments from the knowledge base.

        Args:
            query: The search query
            top_k: Number of results to return
            mode: Retrieval mode (hybrid/vector/keyword)
            document_id: Optional filter to specific document
            rerank: Whether to use reranking
            mmr: Whether to use MMR diversity
            dataset_id: Override the default dataset
            intent: Retrieval intent (general/find_document)
            **kwargs: Additional parameters passed to KB retrieve

        Returns:
            List of KBSearchResult objects
        """
        resolved_query = _require_tool_query(query)
        resolved_top_k = _require_tool_top_k(
            self.default_top_k if top_k is None else top_k
        )
        _require_text_only_intent(intent)
        if "score_threshold" in kwargs:
            kwargs["score_threshold"] = _require_tool_score_threshold(
                kwargs.get("score_threshold")
            )

        # Build retrieve kwargs
        retrieve_kwargs = {
            "user": self.user_context,
            "dataset_id": dataset_id or self.dataset_id,
            "query": resolved_query,
            "top_k": resolved_top_k,
            "mode": mode or self.default_mode,
            "document_id": document_id,
            "rerank": rerank if rerank is not None else self.default_rerank,
            "mmr": mmr if mmr is not None else self.default_mmr,
        }

        retrieve_kwargs.update(kwargs)
        results, _meta = await self.kb.retrieve(**retrieve_kwargs)

        return [
            KBSearchResult(
                content=r.text,
                score=r.score,
                segment_id=r.segment_id,
                document_id=r.document_id,
                metadata=r.metadata,
            )
            for r in results
        ]

    async def retrieve_texts(
        self,
        query: str,
        top_k: int | None = None,
        **kwargs,
    ) -> list[str]:
        """
        Retrieve relevant text segments as a simple list of strings.

        This is a convenience method for simple RAG pipelines.
        """
        results = await self.retrieve(query, top_k=top_k, **kwargs)
        return [r.content for r in results]

    async def retrieve_with_metadata(
        self,
        query: str,
        top_k: int | None = None,
        **kwargs,
    ) -> KBRetrievalOutput:
        """
        Retrieve with full metadata including retrieval statistics.
        """
        resolved_query = _require_tool_query(query)
        resolved_top_k = _require_tool_top_k(
            self.default_top_k if top_k is None else top_k
        )
        if "score_threshold" in kwargs:
            kwargs["score_threshold"] = _require_tool_score_threshold(
                kwargs.get("score_threshold")
            )
        results, meta = await self.kb.retrieve(
            user=self.user_context,
            dataset_id=self.dataset_id,
            query=resolved_query,
            top_k=resolved_top_k,
            mode=self.default_mode,
            rerank=self.default_rerank,
            mmr=self.default_mmr,
            **kwargs,
        )

        return KBRetrievalOutput(
            segments=[
                {
                    "content": r.text,
                    "score": r.score,
                    "segment_id": r.segment_id,
                    "document_id": r.document_id,
                    "metadata": r.metadata,
                }
                for r in results
            ],
            metadata=meta,
            query=resolved_query,
            dataset_id=self.dataset_id,
        )

    def as_langchain_tool(self) -> dict[str, Any]:
        """
        Return a LangChain-compatible tool definition.

        This can be used with LangChain's tool decorator or added to a toolkit.

        Note: This returns a sync wrapper that should be used in async context.
        """

        async def _retrieve(query: str, intent: str = "general", top_k: int = 5) -> str:
            """Retrieve relevant information from the knowledge base."""
            results = await self.retrieve(query, top_k=top_k, intent=intent)

            if not results:
                return "No relevant information found."

            # Format results as text
            formatted = []
            for i, r in enumerate(results, 1):
                formatted.append(f"[{i}] {r.content}")

            return "\n\n".join(formatted)

        # Return tool definition
        return {
            "name": f"kb_search_{self.dataset_id}",
            "description": f"Search the knowledge base '{self.dataset_id}' for relevant information. Use this tool to find answers to questions or gather context.",
            "func": _retrieve,
            "coroutine": _retrieve,
            "args_schema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query to find relevant information",
                        "minLength": 1,
                        "maxLength": 4096,
                    },
                    "intent": {
                        "type": "string",
                        "enum": ["general", "find_document"],
                        "description": "Text retrieval intent",
                        "default": "general",
                    },
                    "top_k": {
                        "type": "integer",
                        "description": "Number of results to return",
                        "default": 5,
                        "minimum": 1,
                        "maximum": 20,
                    },
                },
                "required": ["query"],
            },
        }

    def as_openai_function(self) -> dict[str, Any]:
        """
        Return an OpenAI function calling compatible definition.
        """
        return {
            "type": "function",
            "function": {
                "name": "search_knowledge_base",
                "description": f"Search the knowledge base for relevant text information. Dataset: {self.dataset_id}",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query describing what information you're looking for",
                            "minLength": 1,
                            "maxLength": 4096,
                        },
                        "intent": {
                            "type": "string",
                            "enum": ["general", "find_document"],
                            "description": "Text retrieval intent",
                        },
                        "top_k": {
                            "type": "integer",
                            "description": "Number of results to return",
                            "default": 5,
                            "minimum": 1,
                            "maximum": 20,
                        },
                    },
                    "required": ["query"],
                },
            },
        }


class MultiDatasetRetriever:
    """
    Retriever that can search across multiple datasets.

    Useful for LangGraph agents that need to access multiple knowledge bases.
    """

    def __init__(
        self,
        knowledge_service: KnowledgeService,
        dataset_ids: list[str],
        user_context: UserContext,
        default_top_k: int = 5,
        default_mode: str = "hybrid",
    ):
        """
        Initialize multi-dataset retriever.

        Args:
            knowledge_service: The knowledge base service
            dataset_ids: List of dataset IDs to search
            user_context: User context for authorization (REQUIRED)
            default_top_k: Default number of results to return
            default_mode: Default retrieval mode

        Raises:
            AuthenticationRequiredError: If user_context is None or not authenticated
        """
        if user_context is None:
            raise AuthenticationRequiredError("User context is required for knowledge retrieval")
        if not user_context.is_authenticated:
            raise AuthenticationRequiredError(
                "Authenticated user context is required for knowledge retrieval"
            )
        resolved_dataset_ids = _require_tool_dataset_ids(dataset_ids)
        resolved_top_k = _require_tool_top_k(default_top_k)

        self.kb = knowledge_service
        self.dataset_ids = resolved_dataset_ids
        self.user_context = user_context
        self.default_top_k = resolved_top_k
        self.default_mode = default_mode

        # Create individual retrievers
        self.retrievers = {
            ds_id: KnowledgeRetriever(
                knowledge_service=knowledge_service,
                dataset_id=ds_id,
                user_context=self.user_context,
                default_top_k=default_top_k,
                default_mode=default_mode,
            )
            for ds_id in resolved_dataset_ids
        }

    async def retrieve(
        self,
        query: str,
        top_k: int | None = None,
        dataset_id: str | None = None,
        merge_results: bool = True,
        intent: str | None = None,
        **kwargs,
    ) -> list[KBSearchResult]:
        """
        Retrieve from one or all datasets.

        Args:
            query: The search query
            top_k: Number of results to return
            dataset_id: Specific dataset to search (if None, search all)
            merge_results: If searching all, whether to merge and rank results
            intent: Retrieval intent (general/find_document)
            **kwargs: Additional parameters

        Returns:
            List of search results
        """
        resolved_query = _require_tool_query(query)
        top_k = _require_tool_top_k(self.default_top_k if top_k is None else top_k)
        resolved_intent = _require_text_only_intent(intent)
        if "score_threshold" in kwargs:
            kwargs["score_threshold"] = _require_tool_score_threshold(
                kwargs.get("score_threshold")
            )

        if dataset_id:
            # Search single dataset
            if dataset_id not in self.retrievers:
                raise ValueError(f"Dataset {dataset_id} not configured")
            return await self.retrievers[dataset_id].retrieve(
                resolved_query, top_k=top_k, intent=resolved_intent, **kwargs
            )

        # Search all datasets
        import asyncio

        tasks = [
            retriever.retrieve(
                resolved_query,
                top_k=top_k,
                intent=resolved_intent,
                **kwargs,
            )
            for retriever in self.retrievers.values()
        ]

        all_results = await asyncio.gather(*tasks, return_exceptions=True)

        # Flatten results
        merged: list[KBSearchResult] = []
        for result_list in all_results:
            if isinstance(result_list, list):
                merged.extend(result_list)

        if merge_results and merged:
            # Sort by score and take top_k
            merged.sort(key=lambda x: x.score, reverse=True)
            merged = merged[:top_k]

        return merged

    def as_openai_function(self) -> dict[str, Any]:
        """Return OpenAI function calling definition."""
        dataset_desc = ", ".join(self.dataset_ids)

        return {
            "type": "function",
            "function": {
                "name": "search_knowledge_bases",
                "description": f"Search knowledge bases for relevant text information. Available datasets: {dataset_desc}",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query describing what information you're looking for",
                            "minLength": 1,
                            "maxLength": 4096,
                        },
                        "intent": {
                            "type": "string",
                            "enum": ["general", "find_document"],
                            "description": "Text retrieval intent",
                        },
                        "dataset_id": {
                            "type": "string",
                            "description": f"Optional: specific dataset to search. Options: {dataset_desc}. If not provided, searches all datasets.",
                            "enum": self.dataset_ids,
                        },
                        "top_k": {
                            "type": "integer",
                            "description": "Number of results to return",
                            "default": 5,
                            "minimum": 1,
                            "maximum": 20,
                        },
                    },
                    "required": ["query"],
                },
            },
        }


def create_retrieval_tool(
    knowledge_service: KnowledgeService,
    dataset_id: str,
    user_context: UserContext,
    **kwargs,
) -> Callable:
    """
    Create a simple retrieval tool function for LangGraph.

    Args:
        knowledge_service: The knowledge base service
        dataset_id: Dataset ID to search
        user_context: User context for authorization (REQUIRED)
        **kwargs: Additional options for KnowledgeRetriever

    Returns:
        Async function for searching the knowledge base

    Raises:
        AuthenticationRequiredError: If user_context is None or not authenticated

    Usage in LangGraph:
    ```python
    search_tool = create_retrieval_tool(kb_service, "my_dataset", user_context)

    # In your graph node
    results = await search_tool("What is X?")
    ```
    """
    # Validation happens in KnowledgeRetriever constructor
    retriever = KnowledgeRetriever(
        knowledge_service=knowledge_service,
        dataset_id=dataset_id,
        user_context=user_context,
        **kwargs,
    )

    async def search(query: str, top_k: int = 5) -> str:
        """Search the knowledge base for relevant information."""
        results = await retriever.retrieve(query, top_k=top_k)

        if not results:
            return "No relevant information found in the knowledge base."

        # Format as numbered list
        formatted = []
        for i, r in enumerate(results, 1):
            formatted.append(f"[{i}] (score: {r.score:.3f})\n{r.content}")

        return "\n\n---\n\n".join(formatted)

    return search


# Dify-compatible external knowledge base interface


@dataclass
class DifyExternalDatasetConfig:
    """Configuration for Dify external dataset API."""

    dataset_id: str
    retrieval_model: dict[str, Any]

    def to_dict(self) -> dict[str, Any]:
        return {
            "dataset_id": self.dataset_id,
            "retrieval_model": self.retrieval_model,
        }


class DifyCompatibleKBAPI:
    """
    Provides a Dify-compatible API interface for the knowledge base.

    This allows the KB to be used as an external knowledge base in Dify applications.
    """

    def __init__(
        self,
        knowledge_service: KnowledgeService,
        user_context: UserContext,
    ):
        """
        Initialize Dify-compatible KB API.

        Args:
            knowledge_service: The knowledge base service
            user_context: User context for authorization (REQUIRED)

        Raises:
            AuthenticationRequiredError: If user_context is None or not authenticated
        """
        if user_context is None:
            raise AuthenticationRequiredError("User context is required for knowledge retrieval")
        if not user_context.is_authenticated:
            raise AuthenticationRequiredError(
                "Authenticated user context is required for knowledge retrieval"
            )

        self.kb = knowledge_service
        self.user_context = user_context

    async def retrieve(
        self,
        dataset_id: str,
        query: str,
        retrieval_model: dict[str, Any] | None = None,
    ) -> dict[str, Any]:
        """
        Dify-compatible retrieval endpoint.

        Returns results in Dify's expected format.
        """
        # Parse retrieval model config
        model = retrieval_model or {}

        resolved_query = _require_tool_query(query)
        top_k = _require_tool_top_k(model.get("top_k", 5))
        score_threshold = _require_tool_score_threshold(
            model.get("score_threshold")
        )
        rerank = model.get("reranking_enable", False)

        # Map Dify search method to our mode
        search_method = str(model.get("search_method", "hybrid_search")).lower()
        mode_mapping = {
            "semantic_search": "vector",
            "full_text_search": "keyword",
            "hybrid_search": "hybrid",
        }
        mode = mode_mapping.get(search_method, "hybrid")

        # Execute retrieval
        results, meta = await self.kb.retrieve(
            user=self.user_context,
            dataset_id=dataset_id,
            query=resolved_query,
            top_k=top_k,
            mode=mode,
            rerank=rerank,
        )

        # Filter by score threshold
        if score_threshold is not None:
            results = [r for r in results if r.score >= score_threshold]

        # Format as Dify response
        return {
            "records": [
                {
                    "segment": {
                        "id": r.segment_id,
                        "document_id": r.document_id,
                        "content": r.text,
                        "position": r.metadata.get("position", 0),
                        "word_count": len(r.text.split()),
                        "tokens": r.metadata.get("token_count", 0),
                        "keywords": r.metadata.get("keywords", []),
                        "hit_count": r.metadata.get("hit_count", 0),
                    },
                    "score": r.score,
                    "tsne_position": None,  # Not implemented
                }
                for r in results
            ],
            "metadata": {
                "total": len(results),
                "query": resolved_query,
                "dataset_id": dataset_id,
            },
        }


# ============================================================
# LangChain-Compatible Tool Interface
# ============================================================

# Try to import LangChain types for better integration
# These are optional - tools work without LangChain installed
try:
    from langchain_core.callbacks import AsyncCallbackManagerForToolRun, CallbackManagerForToolRun
    from langchain_core.tools import BaseTool, StructuredTool
    from pydantic import BaseModel as PydanticBaseModel
    from pydantic import Field as PydanticField

    LANGCHAIN_AVAILABLE = True
except ImportError:
    LANGCHAIN_AVAILABLE = False
    BaseTool = object
    StructuredTool = None
    CallbackManagerForToolRun = None
    AsyncCallbackManagerForToolRun = None
    PydanticBaseModel = None
    PydanticField = None


@dataclass
class KBToolConfig:
    """Configuration for KB tool creation."""

    top_k: int = 5
    mode: str = "hybrid"
    rerank: bool = False
    mmr: bool = False
    include_scores: bool = True
    max_content_length: int = 2000


def _format_results_for_llm(
    results: list[KBSearchResult],
    include_scores: bool = True,
    max_content_length: int = 2000,
) -> str:
    """Format search results for LLM consumption."""
    if not results:
        return "No relevant information found in the knowledge base."

    formatted_parts = []
    for i, r in enumerate(results, 1):
        content = r.content
        if len(content) > max_content_length:
            content = content[:max_content_length] + "..."

        if include_scores:
            formatted_parts.append(f"[{i}] (score: {r.score:.3f})\n{content}")
        else:
            formatted_parts.append(f"[{i}]\n{content}")

    return "\n\n---\n\n".join(formatted_parts)


class KnowledgeBaseTool:
    """
    LangChain-compatible Knowledge Base search tool.

    This class implements the interface expected by LangChain/LangGraph
    for tool integration. It can be used directly with ToolNode.

    Attributes:
        name: Tool name (used in function calling)
        description: Tool description (shown to LLM)
        args_schema: Pydantic model for input validation
    """

    def __init__(
        self,
        knowledge_service: KnowledgeService,
        dataset_id: str,
        user_context: UserContext,
        name: str | None = None,
        description: str | None = None,
        config: KBToolConfig | None = None,
    ):
        """
        Initialize the KB tool.

        Args:
            knowledge_service: The KB service instance
            dataset_id: Dataset to search
            user_context: User context for auth
            name: Custom tool name (default: search_{dataset_id})
            description: Custom description
            config: Tool configuration
        """
        if user_context is None:
            raise AuthenticationRequiredError("User context is required")
        if not user_context.is_authenticated:
            raise AuthenticationRequiredError("Authenticated user context required")

        self.kb = knowledge_service
        self.dataset_id = dataset_id
        self.user_context = user_context
        self.config = config or KBToolConfig()
        self.config.top_k = _require_tool_top_k(self.config.top_k)

        # LangChain tool properties
        self.name = name or f"search_{dataset_id.replace('-', '_')}"
        self.description = description or (
            f"Search the '{dataset_id}' knowledge base for relevant information. "
            f"Use this text retrieval tool when you need to find specific facts, documentation, or context."
        )

        # Build args schema as dict (compatible with OpenAI function calling)
        self.args_schema = {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query describing what information you're looking for",
                    "minLength": 1,
                    "maxLength": 4096,
                },
                "intent": {
                    "type": "string",
                    "enum": ["general", "find_document"],
                    "description": "Text retrieval intent",
                },
                "top_k": {
                    "type": "integer",
                    "description": "Number of results to return (default: 5)",
                    "default": self.config.top_k,
                    "minimum": 1,
                    "maximum": 20,
                },
            },
            "required": ["query"],
        }

    async def _arun(
        self,
        query: str,
        intent: str | None = None,
        top_k: int | None = None,
        **kwargs,
    ) -> str:
        """Async execution (primary method for LangGraph)."""
        resolved_query = _require_tool_query(query)
        resolved_top_k = _require_tool_top_k(
            self.config.top_k if top_k is None else top_k
        )
        _require_text_only_intent(intent)
        if "score_threshold" in kwargs:
            kwargs["score_threshold"] = _require_tool_score_threshold(
                kwargs.get("score_threshold")
            )
        try:
            # Build retrieve kwargs
            retrieve_kwargs = {
                "user": self.user_context,
                "dataset_id": self.dataset_id,
                "query": resolved_query,
                "top_k": resolved_top_k,
                "mode": self.config.mode,
                "rerank": self.config.rerank,
                "mmr": self.config.mmr,
            }

            retrieve_kwargs.update(kwargs)
            results, _meta = await self.kb.retrieve(**retrieve_kwargs)

            search_results = [
                KBSearchResult(
                    content=r.text,
                    score=r.score,
                    segment_id=r.segment_id,
                    document_id=r.document_id,
                    metadata=r.metadata,
                )
                for r in results
            ]

            return _format_results_for_llm(
                search_results,
                include_scores=self.config.include_scores,
                max_content_length=self.config.max_content_length,
            )

        except Exception as e:
            logger.exception(f"KB search failed: {e}")
            return f"Error searching knowledge base: {str(e)}"

    def _run(
        self,
        query: str,
        intent: str | None = None,
        top_k: int | None = None,
        **kwargs,
    ) -> str:
        """Sync execution — safely bridges async from sync context."""
        import concurrent.futures

        def _run_in_new_loop():
            loop = asyncio.new_event_loop()
            try:
                return loop.run_until_complete(
                    self._arun(query, intent=intent, top_k=top_k, **kwargs)
                )
            finally:
                loop.close()

        try:
            asyncio.get_running_loop()
            # Inside an async context — run in a thread with a fresh event loop
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                return pool.submit(_run_in_new_loop).result()
        except RuntimeError:
            # No running loop — safe to use asyncio.run()
            return asyncio.run(self._arun(query, intent=intent, top_k=top_k, **kwargs))

    async def ainvoke(self, input: str | dict[str, Any], **kwargs) -> str:
        """LangChain ainvoke interface."""
        if isinstance(input, str):
            return await self._arun(input, **kwargs)
        return await self._arun(**input, **kwargs)

    def invoke(self, input: str | dict[str, Any], **kwargs) -> str:
        """LangChain invoke interface."""
        if isinstance(input, str):
            return self._run(input, **kwargs)
        return self._run(**input, **kwargs)

    def __call__(self, query: str, **kwargs) -> str:
        """Direct call interface."""
        return self._run(query, **kwargs)

    def to_openai_function(self) -> dict[str, Any]:
        """Convert to OpenAI function calling format."""
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.args_schema,
            },
        }

    def to_langchain_tool(self) -> dict[str, Any]:
        """Convert to LangChain tool dict format."""
        return {
            "name": self.name,
            "description": self.description,
            "func": self._run,
            "coroutine": self._arun,
            "args_schema": self.args_schema,
        }


class MultiKnowledgeBaseTool:
    """
    LangChain-compatible tool for searching multiple knowledge bases.
    """

    def __init__(
        self,
        knowledge_service: KnowledgeService,
        dataset_ids: list[str],
        user_context: UserContext,
        name: str = "search_knowledge_bases",
        description: str | None = None,
        config: KBToolConfig | None = None,
    ):
        if user_context is None:
            raise AuthenticationRequiredError("User context is required")
        if not user_context.is_authenticated:
            raise AuthenticationRequiredError("Authenticated user context required")

        self.kb = knowledge_service
        self.dataset_ids = _require_tool_dataset_ids(dataset_ids)
        self.user_context = user_context
        self.config = config or KBToolConfig()
        self.config.top_k = _require_tool_top_k(self.config.top_k)

        self.name = name
        dataset_list = ", ".join(f"'{d}'" for d in self.dataset_ids)
        self.description = description or (
            f"Search across multiple knowledge bases for relevant information. "
            f"Available text datasets: {dataset_list}. "
            f"Optionally specify a dataset_id to search only that dataset."
        )

        self.args_schema = {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query describing what information you're looking for",
                    "minLength": 1,
                    "maxLength": 4096,
                },
                "intent": {
                    "type": "string",
                    "enum": ["general", "find_document"],
                    "description": "Text retrieval intent",
                },
                "dataset_id": {
                    "type": "string",
                    "description": f"Optional: specific dataset to search ({', '.join(self.dataset_ids)})",
                    "enum": self.dataset_ids,
                },
                "top_k": {
                    "type": "integer",
                    "description": "Number of results (default: 5)",
                    "default": self.config.top_k,
                    "minimum": 1,
                    "maximum": 20,
                },
            },
            "required": ["query"],
        }

    async def _arun(
        self,
        query: str,
        intent: str | None = None,
        dataset_id: str | None = None,
        top_k: int | None = None,
        **kwargs,
    ) -> str:
        """Async execution."""
        resolved_query = _require_tool_query(query)
        top_k = _require_tool_top_k(self.config.top_k if top_k is None else top_k)
        _require_text_only_intent(intent)
        if "score_threshold" in kwargs:
            kwargs["score_threshold"] = _require_tool_score_threshold(
                kwargs.get("score_threshold")
            )
        if dataset_id is not None and dataset_id not in self.dataset_ids:
            raise ValidationFailedError("knowledge tool dataset_id is not configured")
        datasets_to_search = [dataset_id] if dataset_id else self.dataset_ids

        all_results: list[KBSearchResult] = []

        async def search_dataset(ds_id: str):
            try:
                # Build retrieve kwargs
                retrieve_kwargs = {
                    "user": self.user_context,
                    "dataset_id": ds_id,
                    "query": resolved_query,
                    "top_k": top_k,
                    "mode": self.config.mode,
                    "rerank": self.config.rerank,
                    "mmr": self.config.mmr,
                }

                retrieve_kwargs.update(kwargs)
                results, _ = await self.kb.retrieve(**retrieve_kwargs)
                return [
                    KBSearchResult(
                        content=r.text,
                        score=r.score,
                        segment_id=r.segment_id,
                        document_id=r.document_id,
                        metadata={**r.metadata, "dataset_id": ds_id},
                    )
                    for r in results
                ]
            except Exception as e:
                logger.warning(f"Search failed for dataset {ds_id}: {e}")
                return []

        # Search datasets in parallel
        tasks = [search_dataset(ds_id) for ds_id in datasets_to_search]
        results_lists = await asyncio.gather(*tasks)

        for results in results_lists:
            all_results.extend(results)

        # Sort by score and take top_k
        all_results.sort(key=lambda x: x.score, reverse=True)
        all_results = all_results[:top_k]

        return _format_results_for_llm(
            all_results,
            include_scores=self.config.include_scores,
            max_content_length=self.config.max_content_length,
        )

    def _run(self, query: str, **kwargs) -> str:
        """Sync execution — safely bridges async from sync context."""
        import concurrent.futures

        def _run_in_new_loop():
            loop = asyncio.new_event_loop()
            try:
                return loop.run_until_complete(self._arun(query, **kwargs))
            finally:
                loop.close()

        try:
            asyncio.get_running_loop()
            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                return pool.submit(_run_in_new_loop).result()
        except RuntimeError:
            return asyncio.run(self._arun(query, **kwargs))

    async def ainvoke(self, input: str | dict[str, Any], **kwargs) -> str:
        if isinstance(input, str):
            return await self._arun(input, **kwargs)
        return await self._arun(**input, **kwargs)

    def invoke(self, input: str | dict[str, Any], **kwargs) -> str:
        if isinstance(input, str):
            return self._run(input, **kwargs)
        return self._run(**input, **kwargs)

    def __call__(self, query: str, **kwargs) -> str:
        return self._run(query, **kwargs)

    def to_openai_function(self) -> dict[str, Any]:
        return {
            "type": "function",
            "function": {
                "name": self.name,
                "description": self.description,
                "parameters": self.args_schema,
            },
        }


# ============================================================
# Factory Functions (Recommended API)
# ============================================================


def create_kb_tool(
    knowledge_service: KnowledgeService,
    dataset_id: str,
    user_context: UserContext,
    name: str | None = None,
    description: str | None = None,
    top_k: int = 5,
    mode: str = "hybrid",
    rerank: bool = False,
    mmr: bool = False,
) -> KnowledgeBaseTool:
    """
    Create a knowledge base search tool for LangGraph agents.

    This is the primary factory function for creating KB tools.
    Returns a tool that works with LangGraph's ToolNode.

    Args:
        knowledge_service: The KB service instance
        dataset_id: Dataset to search
        user_context: User context for authorization
        name: Custom tool name (default: search_{dataset_id})
        description: Custom tool description
        top_k: Default number of results
        mode: Retrieval mode (hybrid/dense/bm25)
        rerank: Enable reranking
        mmr: Enable MMR diversity

    Returns:
        KnowledgeBaseTool instance

    Example:
        ```python
        from agent_gateway.services.knowledge import create_kb_tool
        from langgraph.prebuilt import ToolNode

        # Create tool
        kb_tool = create_kb_tool(
            kb_service,
            "company-docs",
            user_context,
            top_k=5,
        )

        # Use with LangGraph
        tool_node = ToolNode([kb_tool])
        ```
    """
    config = KBToolConfig(
        top_k=top_k,
        mode=mode,
        rerank=rerank,
        mmr=mmr,
    )

    return KnowledgeBaseTool(
        knowledge_service=knowledge_service,
        dataset_id=dataset_id,
        user_context=user_context,
        name=name,
        description=description,
        config=config,
    )


def create_multi_kb_tool(
    knowledge_service: KnowledgeService,
    dataset_ids: list[str],
    user_context: UserContext,
    name: str = "search_knowledge_bases",
    description: str | None = None,
    top_k: int = 5,
    mode: str = "hybrid",
    rerank: bool = False,
    mmr: bool = False,
) -> MultiKnowledgeBaseTool:
    """
    Create a multi-dataset knowledge base search tool.

    Args:
        knowledge_service: The KB service instance
        dataset_ids: List of datasets to search
        user_context: User context for authorization
        name: Tool name
        description: Custom description
        top_k: Default number of results
        mode: Retrieval mode
        rerank: Enable reranking
        mmr: Enable MMR diversity

    Returns:
        MultiKnowledgeBaseTool instance

    Example:
        ```python
        kb_tool = create_multi_kb_tool(
            kb_service,
            ["docs", "wiki", "faq"],
            user_context,
        )
        ```
    """
    config = KBToolConfig(
        top_k=top_k,
        mode=mode,
        rerank=rerank,
        mmr=mmr,
    )

    return MultiKnowledgeBaseTool(
        knowledge_service=knowledge_service,
        dataset_ids=dataset_ids,
        user_context=user_context,
        name=name,
        description=description,
        config=config,
    )


def create_langchain_kb_tool(
    knowledge_service: KnowledgeService,
    dataset_id: str,
    user_context: UserContext,
    **kwargs,
) -> Any:
    """
    Create a LangChain StructuredTool for KB search.

    This requires langchain-core to be installed.
    Falls back to KnowledgeBaseTool if LangChain is not available.

    Returns:
        StructuredTool if LangChain available, else KnowledgeBaseTool
    """
    kb_tool = create_kb_tool(knowledge_service, dataset_id, user_context, **kwargs)

    if LANGCHAIN_AVAILABLE and StructuredTool is not None:
        # Create a proper LangChain StructuredTool
        return StructuredTool.from_function(
            func=kb_tool._run,
            coroutine=kb_tool._arun,
            name=kb_tool.name,
            description=kb_tool.description,
        )

    return kb_tool


# ============================================================
# Exports
# ============================================================

__all__ = [
    # Dataclasses
    "KBRetrievalInput",
    "KBRetrievalOutput",
    "KBSearchResult",
    "KBToolConfig",
    # Classes
    "KnowledgeRetriever",
    "MultiDatasetRetriever",
    "KnowledgeBaseTool",
    "MultiKnowledgeBaseTool",
    "DifyCompatibleKBAPI",
    "DifyExternalDatasetConfig",
    # Factory functions (recommended)
    "create_kb_tool",
    "create_multi_kb_tool",
    "create_langchain_kb_tool",
    "create_retrieval_tool",
]
