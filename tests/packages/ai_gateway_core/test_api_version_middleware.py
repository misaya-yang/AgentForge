from __future__ import annotations

import httpx
import pytest
from ai_gateway_core.proxy.version_middleware import APIVersionMiddleware
from starlette.applications import Starlette
from starlette.responses import JSONResponse
from starlette.routing import Route


@pytest.mark.asyncio
async def test_api_version_middleware_parses_vendor_accept_header() -> None:
    async def endpoint(request):
        return JSONResponse({"version": request.scope["api_version"]})

    app = Starlette(routes=[Route("/api/v1/ping", endpoint)])
    app.add_middleware(APIVersionMiddleware)

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app),
        base_url="http://test",
    ) as client:
        response = await client.get(
            "/api/v1/ping",
            headers={"Accept": "application/vnd.ai-gateway.v2+json"},
        )

    assert response.json() == {"version": "v2"}


@pytest.mark.asyncio
async def test_api_version_middleware_adds_deprecation_headers() -> None:
    async def endpoint(_request):
        return JSONResponse({"ok": True})

    app = Starlette(routes=[Route("/api/v1/legacy", endpoint)])
    app.add_middleware(
        APIVersionMiddleware,
        deprecated_routes={"/api/v1/legacy": "2027-01-01"},
    )

    async with httpx.AsyncClient(
        transport=httpx.ASGITransport(app=app),
        base_url="http://test",
    ) as client:
        response = await client.get("/api/v1/legacy")

    assert response.headers["Deprecation"] == "true"
    assert response.headers["Sunset"] == "2027-01-01"
