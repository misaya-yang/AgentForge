from __future__ import annotations

from fastapi import FastAPI, Request
from fastapi.testclient import TestClient

from src.core.middleware.request_body_limit import RequestBodyLimitMiddleware


def _client() -> TestClient:
    app = FastAPI()
    app.add_middleware(
        RequestBodyLimitMiddleware,
        max_bytes=8,
        paths={"/chat"},
    )

    @app.post("/chat")
    async def chat(request: Request):
        return {"size": len(await request.body())}

    return TestClient(app)


def test_request_body_limit_rejects_oversized_content_length() -> None:
    response = _client().post("/chat", content=b"123456789")

    assert response.status_code == 413


def test_request_body_limit_preserves_legitimate_body() -> None:
    response = _client().post("/chat", content=b"12345678")

    assert response.status_code == 200
    assert response.json() == {"size": 8}
