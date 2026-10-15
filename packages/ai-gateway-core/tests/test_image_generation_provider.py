from __future__ import annotations

import base64
import json

import httpx
import pytest
from ai_gateway_core.media.image_generation import (
    ImageGenerationConfig,
    ImageGenerationProvider,
)


def _client(handler):
    return httpx.AsyncClient(transport=httpx.MockTransport(handler))


@pytest.mark.asyncio
async def test_google_inline_image_uses_configured_provider_without_fallback():
    encoded = base64.b64encode(b"\x89PNG\r\n\x1a\n").decode()

    async def handler(request: httpx.Request) -> httpx.Response:
        assert request.url.host == "generativelanguage.googleapis.com"
        assert request.headers["x-goog-api-key"] == "secret-is-not-in-result"
        return httpx.Response(
            200,
            json={"candidates": [{"content": {"parts": [{"inlineData": {"data": encoded}}]}}]},
        )

    client = _client(handler)
    provider = ImageGenerationProvider(
        ImageGenerationConfig(
            "google",
            "secret-is-not-in-result",
            "https://generativelanguage.googleapis.com",
            "image-model",
        ),
        client=client,
    )
    result = await provider.generate(prompt="a red kite")
    await client.aclose()
    assert result.success is True
    assert result.provider == "google"
    assert result.error is None


@pytest.mark.asyncio
async def test_google_reference_uses_inline_data_part():
    encoded = base64.b64encode(b"\x89PNG\r\n\x1a\n").decode()
    seen = {}

    async def handler(request: httpx.Request) -> httpx.Response:
        seen.update(json.loads(request.content))
        return httpx.Response(
            200, json={"candidates": [{"content": {"parts": [{"inlineData": {"data": encoded}}]}}]}
        )

    client = _client(handler)
    provider = ImageGenerationProvider(
        ImageGenerationConfig(
            "google", "key", "https://google.example", "image-model", supports_reference_images=True
        ),
        client=client,
    )
    result = await provider.generate(
        prompt="edit", reference_image=b"\x89PNG\r\n\x1a\n", reference_mime="image/png"
    )
    await client.aclose()
    assert result.success is True
    assert seen["contents"][0]["parts"][0]["inlineData"]["mimeType"] == "image/png"


@pytest.mark.asyncio
async def test_reference_unsupported_fails_before_http_call():
    calls = 0

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        raise AssertionError("reference must be rejected locally")

    client = _client(handler)
    provider = ImageGenerationProvider(
        ImageGenerationConfig("doubao", "key", "https://ark.example", "image-model"), client=client
    )
    result = await provider.generate(
        prompt="edit", reference_image=b"\x89PNG\r\n\x1a\n", reference_mime="image/png"
    )
    await client.aclose()
    assert calls == 0
    assert result.error_code == "reference_unsupported"


@pytest.mark.asyncio
async def test_reference_mime_and_size_rejected_before_http_call():
    client = _client(lambda _request: httpx.Response(500))
    provider = ImageGenerationProvider(
        ImageGenerationConfig(
            "google", "key", "https://google.example", "image-model", supports_reference_images=True
        ),
        client=client,
    )
    result = await provider.generate(
        prompt="edit", reference_image=b"bad", reference_mime="image/png"
    )
    await client.aclose()
    assert result.error_code == "invalid_reference"


@pytest.mark.asyncio
async def test_url_only_response_is_rejected_without_download():
    calls = 0

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal calls
        calls += 1
        return httpx.Response(200, json={"data": [{"url": "https://cdn.example/image.png"}]})

    client = _client(handler)
    provider = ImageGenerationProvider(
        ImageGenerationConfig("doubao", "key", "https://ark.example", "image-model"), client=client
    )
    result = await provider.generate(prompt="a red kite")
    await client.aclose()
    assert calls == 1
    assert result.error_code == "url_fetch_failed"
    assert result.images == []


@pytest.mark.asyncio
async def test_no_cross_provider_fallback_on_http_error():
    async def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(500, json={"error": {"message": "secret"}})

    client = _client(handler)
    provider = ImageGenerationProvider(
        ImageGenerationConfig("doubao", "key", "https://ark.example", "image-model"), client=client
    )
    result = await provider.generate(prompt="a red kite")
    await client.aclose()
    assert result.provider == "doubao"
    assert result.error_code == "provider_http_error"
    assert result.outcome_unknown is True
    assert "secret" not in (result.error or "")


def test_environment_defaults_to_configured_dashscope_provider(monkeypatch):
    for name in (
        "IMAGE_GENERATION_PROVIDER",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "VERTEX_API_KEY",
        "VERTEX_IMAGE_API_KEY",
        "ARK_API_KEY",
    ):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv("DASHSCOPE_IMAGE_API_KEY", "dashscope-image-key")
    monkeypatch.setenv("DASHSCOPE_IMAGE_MODEL", "wan2.6-t2i")

    config = ImageGenerationConfig.from_environment()

    assert config.provider == "dashscope"
    assert config.api_key == "dashscope-image-key"
    assert config.dashscope_protocol == "wan26"


@pytest.mark.asyncio
async def test_invalid_endpoint_is_blocked_before_http_call():
    async def handler(request: httpx.Request) -> httpx.Response:
        raise AssertionError("request must not be sent")

    client = _client(handler)
    provider = ImageGenerationProvider(
        ImageGenerationConfig("google", "key", "http://127.0.0.1:8080", "image-model"),
        client=client,
    )
    result = await provider.generate(prompt="a red kite")
    await client.aclose()
    assert result.error_code == "provider_endpoint_invalid"


@pytest.mark.asyncio
async def test_owned_client_is_reused_and_closed():
    requests = 0

    async def handler(request: httpx.Request) -> httpx.Response:
        nonlocal requests
        requests += 1
        encoded = base64.b64encode(b"\x89PNG\r\n\x1a\n").decode()
        return httpx.Response(
            200, json={"candidates": [{"content": {"parts": [{"inlineData": {"data": encoded}}]}}]}
        )

    transport = httpx.MockTransport(handler)
    provider = ImageGenerationProvider(
        ImageGenerationConfig("google", "key", "https://google.example", "image-model")
    )
    provider._client = httpx.AsyncClient(transport=transport)
    result = await provider.generate(prompt="a")
    await provider.close()
    assert result.success is True
    assert requests == 1


@pytest.mark.asyncio
@pytest.mark.parametrize('async_task', [False, True])
async def test_wan26_choices_result_uses_guarded_image_download(monkeypatch, async_task):
    from types import SimpleNamespace

    from ai_gateway_core.media import image_generation as module

    url = 'https://dashscope-result-bj.oss-cn-beijing.aliyuncs.com/fixture.png'
    output = {'choices': [{'message': {'content': [{'type': 'text', 'text': 'ignored'}, {'type': 'image', 'image': url}]}}]}
    requests = []
    downloads = []
    async def handler(request):
        requests.append(request.method)
        if async_task and request.method == 'POST':
            return httpx.Response(200, json={'output': {'task_id': 'original-task', 'task_status': 'PENDING'}})
        return httpx.Response(200, json={'output': {**output, **({'task_status': 'SUCCEEDED'} if async_task else {})}})
    async def fetch(image_url, **kwargs):
        downloads.append(image_url)
        assert kwargs['allowed_hosts'] == module._DEFAULT_RESULT_HOST_SUFFIXES
        assert kwargs['max_redirects'] == 1 and kwargs['max_bytes'] == module._MAX_IMAGE_BYTES
        return SimpleNamespace(content_type='image/png', body=b'\x89PNG\r\n\x1a\n')
    monkeypatch.setattr(module, 'safe_fetch_with_response', fetch)
    client = _client(handler)
    provider = ImageGenerationProvider(ImageGenerationConfig('dashscope', 'fixture-key', 'https://dashscope.aliyuncs.com/api/v1', 'wan2.6-t2i'), client=client)
    result = await provider.generate(prompt='fixture')
    await client.aclose()
    assert result.success and result.error_code is None
    assert result.images[0]['mime_type'] == 'image/png' and result.images[0]['size_bytes'] == 8
    assert requests == (['POST', 'GET'] if async_task else ['POST'])
    assert downloads == [url]


@pytest.mark.asyncio
async def test_wan26_malformed_choices_remains_unknown_without_redispatch():
    calls = []
    async def handler(request):
        calls.append(request.method)
        return httpx.Response(200, json={'output': {'task_id': 'original-task', 'task_status': 'SUCCEEDED', 'choices': [None, {'message': {'content': [None, {'image': 12}]}}]}})
    client = _client(handler)
    provider = ImageGenerationProvider(ImageGenerationConfig('dashscope', 'fixture-key', 'https://dashscope.aliyuncs.com/api/v1', 'wan2.6-t2i'), client=client)
    result = await provider.generate(prompt='fixture')
    await client.aclose()
    assert not result.success and result.outcome_unknown and result.error_code == 'no_image'
    assert calls == ['POST', 'GET']
