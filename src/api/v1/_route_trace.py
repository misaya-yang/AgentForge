"""Shared request trace helpers for API route modules."""

from __future__ import annotations

from fastapi import Request


def current_trace_id(request: Request) -> str:
    request_id = getattr(request.state, "request_id", "")
    if isinstance(request_id, str) and request_id:
        return request_id
    trace_id = getattr(request.state, "trace_id", "")
    if isinstance(trace_id, str):
        return trace_id
    return ""