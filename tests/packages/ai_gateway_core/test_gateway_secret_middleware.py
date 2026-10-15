from __future__ import annotations

import asyncio

import pytest
from ai_gateway_core.auth.gateway_secret import GatewaySecret
from ai_gateway_core.auth.gateway_secret_middleware import (
    GatewaySecretAuthMiddleware,
    validate_gateway_auth_configuration,
)
from fastapi import FastAPI
from fastapi.responses import StreamingResponse
from fastapi.testclient import TestClient
from pydantic import BaseModel


class Payload(BaseModel):
    query: str


def _app(
    *,
    allow_anonymous: bool = False,
    separately_authenticated_paths: frozenset[str] = frozenset(),
    separately_authenticated_prefixes: frozenset[str] = frozenset(),
) -> tuple[FastAPI, GatewaySecret]:
    secret = GatewaySecret(secret="shared-secret-for-tests")
    app = FastAPI()
    app.add_middleware(
        GatewaySecretAuthMiddleware,
        gateway_secret=secret,
        allow_anonymous=allow_anonymous,
        separately_authenticated_paths=separately_authenticated_paths,
        separately_authenticated_prefixes=separately_authenticated_prefixes,
    )

    @app.get("/health")
    async def health():
        return {"status": "ok"}

    @app.get("/protected")
    async def protected():
        return {"status": "accepted"}

    @app.get("/private-runtime")
    async def private_runtime():
        return {"status": "separately-authenticated"}

    @app.get("/internal/v2/capabilities/item")
    async def private_capability():
        return {"status": "separately-authenticated"}

    @app.get("/internal/v2/capabilities-evil/item")
    async def prefix_confusion():
        return {"status": "must-stay-protected"}

    return app, secret


def test_gateway_secret_and_anonymous_mode_are_mutually_exclusive() -> None:
    with pytest.raises(RuntimeError, match="cannot be combined"):
        validate_gateway_auth_configuration(
            secret="configured-shared-secret",
            allow_anonymous=True,
            allow_anonymous_setting="SERVICE_APP__ALLOW_ANONYMOUS",
        )


def test_anonymous_mode_is_restricted_to_local_environment() -> None:
    with pytest.raises(RuntimeError, match="local development/test"):
        validate_gateway_auth_configuration(
            secret="",
            allow_anonymous=True,
            allow_anonymous_setting="SERVICE_APP__ALLOW_ANONYMOUS",
            environment="production",
        )


@pytest.mark.parametrize(
    ("secret", "allow_anonymous"),
    [("configured-shared-secret", False), ("", True), ("", False)],
)
def test_gateway_auth_configuration_accepts_unambiguous_modes(
    secret: str,
    allow_anonymous: bool,
) -> None:
    validate_gateway_auth_configuration(
        secret=secret,
        allow_anonymous=allow_anonymous,
        allow_anonymous_setting="SERVICE_APP__ALLOW_ANONYMOUS",
    )


def test_gateway_secret_middleware_allows_health_without_signature() -> None:
    app, _secret = _app()

    response = TestClient(app).get("/health")

    assert response.status_code == 200
    assert response.json() == {"status": "ok"}


def test_gateway_secret_middleware_rejects_missing_signature() -> None:
    app, _secret = _app()

    response = TestClient(app).get("/protected")

    assert response.status_code == 401
    assert response.json() == {
        "code": "AUTH_DENIED",
        "message": "Missing or invalid X-Gateway-Secret",
    }


def test_gateway_secret_middleware_bypasses_only_explicit_separate_auth_path() -> None:
    app, _secret = _app(separately_authenticated_paths=frozenset({"/private-runtime"}))

    accepted = TestClient(app).get("/private-runtime")
    rejected = TestClient(app).get("/protected")

    assert accepted.status_code == 200
    assert accepted.json() == {"status": "separately-authenticated"}
    assert rejected.status_code == 401


def test_gateway_secret_middleware_bypasses_only_segment_scoped_prefix() -> None:
    app, _secret = _app(separately_authenticated_prefixes=frozenset({"/internal/v2/capabilities/"}))

    accepted = TestClient(app).get("/internal/v2/capabilities/item")
    rejected = TestClient(app).get("/internal/v2/capabilities-evil/item")

    assert accepted.status_code == 200
    assert rejected.status_code == 401


def test_gateway_secret_middleware_accepts_valid_signature() -> None:
    app, secret = _app()

    response = TestClient(app).get(
        "/protected",
        headers={
            secret.header_name: secret.sign(
                method="GET",
                path="/protected",
                query="",
                body=b"",
            )
        },
    )

    assert response.status_code == 200
    assert response.json() == {"status": "accepted"}


def test_gateway_secret_middleware_allows_unsigned_dev_when_enabled() -> None:
    app, _secret = _app(allow_anonymous=True)

    response = TestClient(app).get("/protected")

    assert response.status_code == 200
    assert response.json() == {"status": "accepted"}


def test_gateway_secret_middleware_v2_verifies_and_replays_request_body() -> None:
    secret = GatewaySecret(
        secret="shared-secret-for-tests",
        version="v2",
        key_id="local",
        keys={"local": "shared-secret-for-tests"},
    )
    verifier = GatewaySecret(
        secret="shared-secret-for-tests",
        version="v2",
        key_id="local",
        keys={"local": "shared-secret-for-tests"},
    )
    app = FastAPI()
    app.add_middleware(
        GatewaySecretAuthMiddleware,
        gateway_secret=verifier,
        allow_anonymous=False,
    )

    @app.post("/protected")
    async def protected(payload: Payload):
        return {"query": payload.query}

    body = b'{"query":"hello"}'
    header = secret.sign(
        request_id="middleware-v2",
        method="POST",
        path="/protected",
        query="",
        body=body,
    )

    response = TestClient(app).post(
        "/protected",
        content=body,
        headers={
            secret.header_name: header,
            "Content-Type": "application/json",
        },
    )

    assert response.status_code == 200
    assert response.json() == {"query": "hello"}


def test_gateway_secret_middleware_v2_preserves_stream_until_first_chunk() -> None:
    secret = GatewaySecret(
        secret="shared-secret-for-tests",
        version="v2",
        key_id="local",
        keys={"local": "shared-secret-for-tests"},
    )
    app = FastAPI()
    app.add_middleware(
        GatewaySecretAuthMiddleware,
        gateway_secret=secret,
        allow_anonymous=False,
    )

    @app.post("/stream")
    async def stream(payload: Payload):
        async def events():
            await asyncio.sleep(0.01)
            yield f"data: {payload.query}\n\n"

        return StreamingResponse(events(), media_type="text/event-stream")

    body = b'{"query":"hello"}'
    header = secret.sign(
        request_id="middleware-v2-stream",
        method="POST",
        path="/stream",
        query="",
        body=body,
    )

    response = TestClient(app).post(
        "/stream",
        content=body,
        headers={secret.header_name: header, "Content-Type": "application/json"},
    )

    assert response.status_code == 200
    assert response.text == "data: hello\n\n"
