"""
Knowledge Base (KBMS) service package.

Provides a production-grade RAG system with:
- Configurable document chunking strategies (9 modes)
- Hybrid retrieval (vector + keyword)
- Fusion strategies (RRF, weighted)
- Reranking (DashScope, Cohere, etc.)
- MMR diversity
- LLM-based QA testing
- LangGraph-compatible tools

Quick Start for LangGraph Agents:

    from agent_gateway.services.knowledge import create_kb_tool

    # Create a KB tool for your agent
    kb_tool = create_kb_tool(kb_service, "my_dataset", user_context)

    # Use with LangGraph ToolNode
    from langgraph.prebuilt import ToolNode
    tool_node = ToolNode([kb_tool])
"""

from typing import TYPE_CHECKING

from .chunking import (
    AutomaticChunker,
    BaseChunker,
    Chunk,
    ChunkingConfig,
    ChunkingMode,
    FixedSizeChunker,
    HeadingChunker,
    HierarchicalChunker,
    PageChunker,
    ParagraphChunker,
    RecursiveChunker,
    RegexChunker,
    SeparatorChunker,
    TextPreprocessor,
    chunk_text,
    create_chunker,
    flatten_chunks,
    process_document,
)
from .retrieval_config import (
    DEFAULT_CONFIGS,
    DatasetIndexConfig,
    FusionConfig,
    FusionStrategy,
    KeywordRetrievalConfig,
    MMRConfig,
    RerankConfig,
    RerankProvider,
    RetrievalConfig,
    RetrievalMode,
    VectorRetrievalConfig,
    get_preset_config,
)

if TYPE_CHECKING:
    from .langgraph_tools import (
        DifyCompatibleKBAPI,
        KBSearchResult,
        KBToolConfig,
        KnowledgeBaseTool,
        KnowledgeRetriever,
        MultiDatasetRetriever,
        MultiKnowledgeBaseTool,
        create_kb_tool,
        create_langchain_kb_tool,
        create_multi_kb_tool,
        create_retrieval_tool,
    )

__all__ = [
    # Chunking
    "ChunkingConfig",
    "ChunkingMode",
    "Chunk",
    "TextPreprocessor",
    "BaseChunker",
    "FixedSizeChunker",
    "ParagraphChunker",
    "PageChunker",
    "HeadingChunker",
    "RegexChunker",
    "SeparatorChunker",
    "RecursiveChunker",
    "HierarchicalChunker",
    "AutomaticChunker",
    "create_chunker",
    "process_document",
    "flatten_chunks",
    "chunk_text",
    # Retrieval Config
    "RetrievalConfig",
    "RetrievalMode",
    "FusionStrategy",
    "FusionConfig",
    "VectorRetrievalConfig",
    "KeywordRetrievalConfig",
    "RerankConfig",
    "RerankProvider",
    "MMRConfig",
    "DatasetIndexConfig",
    "DEFAULT_CONFIGS",
    "get_preset_config",
    # LangGraph Tools
    "create_kb_tool",
    "create_multi_kb_tool",
    "create_langchain_kb_tool",
    "create_retrieval_tool",
    "KnowledgeBaseTool",
    "MultiKnowledgeBaseTool",
    "KnowledgeRetriever",
    "MultiDatasetRetriever",
    "KBToolConfig",
    "KBSearchResult",
    "DifyCompatibleKBAPI",
]


# Optional agent integrations are loaded only when their public exports are used.
# Importing a Knowledge route must not initialize unrelated HTTP clients.
_LANGGRAPH_EXPORTS = frozenset(
    {
        "DifyCompatibleKBAPI",
        "KBSearchResult",
        "KBToolConfig",
        "KnowledgeBaseTool",
        "KnowledgeRetriever",
        "MultiDatasetRetriever",
        "MultiKnowledgeBaseTool",
        "create_kb_tool",
        "create_langchain_kb_tool",
        "create_multi_kb_tool",
        "create_retrieval_tool",
    }
)


def __getattr__(name: str):
    if name not in _LANGGRAPH_EXPORTS:
        raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
    from . import langgraph_tools

    value = getattr(langgraph_tools, name)
    globals()[name] = value
    return value


def __dir__():
    return sorted(set(globals()) | _LANGGRAPH_EXPORTS)
