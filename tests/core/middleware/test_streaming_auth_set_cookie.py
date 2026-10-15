from __future__ import annotations

import pytest

from src.core.middleware._streaming.auth import StreamingAuthConfig, StreamingAuthMiddleware


@pytest.mark.asyncio
async def test_auth_response_headers_preserve_duplicate_set_cookie() -> None:
    middleware = StreamingAuthMiddleware(app=None, config=StreamingAuthConfig())
    message = await middleware.process_response_start(
        {
            "state": {
                "user_info": {
                    "user_id": "anon:1",
                    "user_type": "anonymous",
                }
            }
        },
        {
            "type": "http.response.start",
            "headers": [
                (b"set-cookie", b"ag_embed_session=nonce; Path=/; HttpOnly"),
                (b"set-cookie", b"ag_anon_id=11111111-1111-4111-8111-111111111111; Path=/; HttpOnly"),
            ],
        },
    )
    cookies = [value.decode() for name, value in message["headers"] if name == b"set-cookie"]
    assert any(item.startswith("ag_embed_session=") for item in cookies)
    assert any(item.startswith("ag_anon_id=") for item in cookies)
    assert any(name == b"x-user-id" for name, _ in message["headers"])
