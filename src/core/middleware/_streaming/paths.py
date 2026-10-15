"""Streaming-path detection shared by the pure ASGI middleware stack."""

from __future__ import annotations

STREAMING_PATHS: set[str] = {
    "/api/v1/stream",
    "/api/v1/assistant/chat/stream",
    "/v1/responses",
}

STREAMING_PATH_PREFIXES: list[str] = [
    "/api/v1/conversations/",  # /api/v1/conversations/{id}/stream
    "/api/v1/langgraph/",  # LangGraph SSE endpoints
    "/api/v1/proxy/",  # 透明代理 SSE endpoints
    "/proxy/",  # 透明代理（无版本前缀）
]

# Streaming suffixes to detect (LangGraph 兼容)
STREAMING_SUFFIXES: list[str] = [
    "/stream",  # 通用流式后缀
    "/runs/stream",  # LangGraph runs stream
    "/sse",  # SSE endpoint
]

# 额外的流式路径关键词检测
STREAMING_KEYWORDS: list[str] = [
    "/stream",  # 包含 stream 的路径
    "/events",  # SSE events
]


def is_streaming_path(path: str) -> bool:
    """检查是否是流式路径

    Critical for latency: paths detected as streaming will bypass
    response buffering in middleware, enabling true streaming.
    """
    # Exact match
    if path in STREAMING_PATHS:
        return True

    # Check suffixes (handles /runs/stream, /conversations/xxx/stream, etc.)
    for suffix in STREAMING_SUFFIXES:
        if path.endswith(suffix):
            return True

    # Check prefixes (streaming API areas)
    for prefix in STREAMING_PATH_PREFIXES:
        if path.startswith(prefix):
            # Check if this specific path is streaming-related
            for keyword in STREAMING_KEYWORDS:
                if keyword in path:
                    return True
            # Also check suffixes within prefixed paths
            for suffix in STREAMING_SUFFIXES:
                if path.endswith(suffix):
                    return True

    # Check keywords anywhere in path (but exclude false positives like "upstream")
    path_lower = path.lower()
    return bool("/stream" in path_lower and "upstream" not in path_lower)
