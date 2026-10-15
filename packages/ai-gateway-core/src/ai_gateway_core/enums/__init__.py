"""Shared enum types. ``StreamEventType`` is the SSE event-type contract
consumed by the frontend at ``web/src/lib/sse.ts``. Do not rename members
without updating both backend emitters and the frontend parser.
"""

from ._core import (
    ConnectorType,
    ContentType,
    DatasetPermission,
    DatasetVisibility,
    DataSourceType,
    DocumentStatus,
    InvocationMode,
    ModelAccessLevel,
    ModelProvider,
    RAGMode,
    RetrievalMethod,
    SegmentStatus,
    ServiceType,
    StreamEventType,
    StylePreset,
    ToolCategory,
    TransportType,
)

__all__ = [
    "ConnectorType",
    "TransportType",
    "ContentType",
    "DataSourceType",
    "DatasetPermission",
    "DatasetVisibility",
    "DocumentStatus",
    "InvocationMode",
    "ModelAccessLevel",
    "ModelProvider",
    "RAGMode",
    "RetrievalMethod",
    "SegmentStatus",
    "ServiceType",
    "StreamEventType",
    "StylePreset",
    "ToolCategory",
]
