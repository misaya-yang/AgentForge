"""
CORS configuration tests.
"""

from fastapi.testclient import TestClient


def test_cors_rejects_unlisted_origin_when_credentials_enabled(monkeypatch):
    monkeypatch.setenv("GATEWAY_CORS__ALLOW_ORIGINS", '["http://localhost:3000"]')
    monkeypatch.setenv("GATEWAY_CORS__ALLOW_CREDENTIALS", "true")

    from src.main import create_app

    app = create_app()
    client = TestClient(app)

    response = client.options(
        "/health",
        headers={
            "Origin": "http://evil.example",
            "Access-Control-Request-Method": "GET",
        },
    )

    assert "access-control-allow-origin" not in response.headers
