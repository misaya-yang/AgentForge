from __future__ import annotations

import pytest

from src.proxy.config_loader import ProxyServiceConfig
from src.proxy.context_injector import RequestContext
from src.proxy.response_cache import ResponseCache


class _FakeCacheDB:
    enabled = True

    def __init__(self):
        self._rows = {}

    async def get_cache(self, service_id: str, input_hash: str):
        return self._rows.get((service_id, input_hash))

    async def save_cache(
        self,
        service_id: str,
        input_hash: str,
        output_text: str,
        input_text: str = None,
        output_data=None,
        metadata=None,
        expires_at=None,
    ) -> None:
        self._rows[(service_id, input_hash)] = {
            "service_id": service_id,
            "input_hash": input_hash,
            "output_data": output_data,
            "metadata": metadata or {},
        }


@pytest.fixture
def proxy_service_config() -> ProxyServiceConfig:
    return ProxyServiceConfig(
        service_id="agent",
        service_name="Agent",
        upstream_url="http://localhost:2024",
        cache_enabled=True,
        cache_ttl=120,
    )


@pytest.mark.asyncio
async def test_response_cache_miss_then_hit(proxy_service_config: ProxyServiceConfig) -> None:
    db = _FakeCacheDB()
    cache = ResponseCache(database=db)
    context = RequestContext(user_id="user_1", tenant_id="tenant_1")
    body = b'{"input":{"messages":[{"role":"user","content":"hello"}]},"assistant_id":"flash"}'

    status, cache_hash, cached = await cache.get_cached_response(
        config=proxy_service_config,
        context=context,
        method="POST",
        path="/runs/wait",
        body=body,
        query_params={},
        stream=False,
    )
    assert status == "MISS"
    assert cache_hash
    assert cached is None

    await cache.save_response(
        cache_hash=cache_hash,
        config=proxy_service_config,
        context=context,
        method="POST",
        path="/runs/wait",
        body=body,
        query_params={},
        response_status=200,
        response_headers={"content-type": "application/json"},
        response_body=b'{"messages":[{"type":"ai","content":"ok"}]}',
        stream=False,
    )

    status, _, cached = await cache.get_cached_response(
        config=proxy_service_config,
        context=context,
        method="POST",
        path="/runs/wait",
        body=body,
        query_params={},
        stream=False,
    )
    assert status == "HIT"
    assert cached is not None
    assert cached.body == b'{"messages":[{"type":"ai","content":"ok"}]}'


@pytest.mark.asyncio
async def test_response_cache_bypass_for_unsupported_path(
    proxy_service_config: ProxyServiceConfig,
) -> None:
    db = _FakeCacheDB()
    cache = ResponseCache(database=db)
    context = RequestContext(user_id="user_1", tenant_id="tenant_1")

    status, cache_hash, cached = await cache.get_cached_response(
        config=proxy_service_config,
        context=context,
        method="POST",
        path="/assistants/search",
        body=b"{}",
        query_params={},
        stream=False,
    )
    assert status == "BYPASS"
    assert cache_hash is None
    assert cached is None
