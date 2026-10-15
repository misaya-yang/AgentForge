from __future__ import annotations

import pytest
from fastapi import HTTPException

from src.api.v1._langgraph_route_utils import handle_langgraph_proxy_error
from src.adapters.langgraph_proxy import QuotaExceededError


def test_handle_langgraph_proxy_error_passthrough_http_exception() -> None:
    original = HTTPException(status_code=403, detail={"message": "quota blocked"})

    with pytest.raises(HTTPException) as exc_info:
        handle_langgraph_proxy_error(original)

    assert exc_info.value.status_code == 403
    assert exc_info.value.detail == {"message": "quota blocked"}


def test_handle_langgraph_proxy_error_maps_quota_exceeded() -> None:
    with pytest.raises(HTTPException) as exc_info:
        handle_langgraph_proxy_error(QuotaExceededError("thread limit"))

    assert exc_info.value.status_code == 429
    assert "thread limit" in str(exc_info.value.detail)