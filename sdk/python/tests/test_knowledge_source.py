"""Public citation-to-source SDK workflow over the actual HTTP transport."""
from __future__ import annotations

import hashlib
import json
from types import SimpleNamespace
from unittest.mock import AsyncMock

import httpx
import pytest
from ai_assistant import AssistantClient, AssistantError

CONTENT = "Full source: 中文🙂 e\u0301\nFinal line."
HASH = hashlib.sha256(CONTENT.encode("utf-8")).hexdigest()


@pytest.fixture
async def host():
    client = AssistantClient("synthetic-key", base_url="https://gateway.example",
                             tenant_id="tenant-one", user_id="owner-one", max_retries=0)
    await client._transport._client.aclose()
    state = SimpleNamespace(client=client, requests=[], responses=[])

    def handle(request):
        state.requests.append(request)
        assert state.responses, "Unexpected extra request"
        response = state.responses.pop(0)
        if isinstance(response, BaseException):
            raise response
        return response

    client._transport._client = httpx.AsyncClient(
        base_url="https://gateway.example", transport=httpx.MockTransport(handle),
    )

    async def read(**changes):
        return await client.knowledge.get_source(**{
            "dataset_id": "dataset-one", "document_id": "document-one",
            "source_version": 3, "source_hash": HASH, **changes,
        })

    state.read = read
    try:
        yield state
    finally:
        await client.close()


def source(**changes):
    return {"document_id": "document-one", "version_number": 3,
            "source_hash": HASH, "title": "Saved title", "content": CONTENT, **changes}


def respond(host, value, status=200):
    host.responses.append(httpx.Response(status, content=json.dumps(value).encode(),
                                         headers={"content-type": "application/json"}))


@pytest.mark.asyncio
async def test_public_source_reader_exists(host):
    reader = getattr(host.client.knowledge, "get_source", None)
    assert callable(reader), "public citation source reader missing"


@pytest.mark.asyncio
async def test_existing_ask_contract_unchanged(host):
    respond(host, {"content": "answer", "session_id": "session-one", "contexts": []})
    answer = await host.client.knowledge.ask("question", ["dataset-one"])
    assert answer.content == "answer" and answer.contexts == []
    assert [(r.method, r.url.path) for r in host.requests] == [("POST", "/api/v1/assistant/chat")]


@pytest.mark.asyncio
async def test_ask_then_open_exact_citation_publicly(host):
    citation = {"dataset_id": "dataset-one", "chunks": [{"content": "excerpt", "metadata": {
        "document_id": "document-one", "source_version": 3, "source_hash": HASH}}]}
    respond(host, {"content": "answer", "session_id": "session-one", "contexts": [citation]})
    respond(host, source())
    answer = await host.client.knowledge.ask("question", ["dataset-one"])
    identity = answer.contexts[0]["chunks"][0]["metadata"]
    result = await host.client.knowledge.get_source(
        answer.contexts[0]["dataset_id"], identity["document_id"],
        source_version=identity["source_version"], source_hash=identity["source_hash"],
    )
    assert result == source()
    assert [(r.method, r.url.path) for r in host.requests] == [
        ("POST", "/api/v1/assistant/chat"),
        ("GET", "/api/v1/knowledge/dataset-one/documents/document-one/versions/3/source")]
    assert dict(host.requests[-1].url.params) == {"hash": HASH}
    assert all(r.headers["x-api-key"] == "synthetic-key"
               and r.headers["x-tenant-id"] == "tenant-one"
               and r.headers["x-user-id"] == "owner-one" for r in host.requests)


@pytest.mark.asyncio
async def test_unicode_ids_are_encoded_and_full_source_is_hashed(host):
    document = "文档 α:one"
    respond(host, source(document_id=document))
    assert (await host.read(dataset_id="资料 库", document_id=document))["content"] == CONTENT
    request = host.requests[0]
    prefix = b"/api/v1/knowledge/%E8%B5%84%E6%96%99%20%E5%BA%93/documents/"
    assert request.url.raw_path.startswith(prefix)
    assert b"%3Aone/versions/3/source" in request.url.raw_path
    assert request.url.host == "gateway.example" and request.method == "GET"


@pytest.mark.asyncio
async def test_uppercase_input_hash_is_normalized(host):
    respond(host, source())
    assert await host.read(source_hash=HASH.upper()) == source()
    assert host.requests[0].url.params["hash"] == HASH


@pytest.mark.asyncio
async def test_unknown_response_fields_are_not_exposed(host):
    respond(host, source(private_token="internal-secret", metadata={"credential": "secret"}))
    assert await host.read() == source()


@pytest.mark.asyncio
@pytest.mark.parametrize("value", ["", ".", "..", "../other", "a/b", "a\\b", "a?b", "a#b",
                                   "%2e%2e", " a", "a\n", "a\x80", "\ud800", "a" * 1025, None],
                         ids=["empty", "dot", "dotdot", "traversal", "slash", "backslash",
                              "query", "fragment", "encoded", "space", "newline", "control",
                              "surrogate", "oversize", "null"])
async def test_unsafe_identifiers_fail_before_http(host, value):
    for field in ("dataset_id", "document_id"):
        with pytest.raises(ValueError):
            await host.read(**{field: value})
    assert host.requests == []


@pytest.mark.asyncio
@pytest.mark.parametrize("value", [0, -1, True, "3", 3.0, 2**31, None],
                         ids=["zero", "negative", "bool", "string", "float", "overflow", "null"])
async def test_invalid_version_fails_before_http(host, value):
    with pytest.raises(ValueError):
        await host.read(source_version=value)
    assert host.requests == []


@pytest.mark.asyncio
@pytest.mark.parametrize("value", ["", "a" * 63, "a" * 65, "z" * 64, " " + HASH, None],
                         ids=["empty", "short", "long", "nonhex", "space", "null"])
async def test_invalid_hash_fails_before_http(host, value):
    with pytest.raises(ValueError):
        await host.read(source_hash=value)
    assert host.requests == []


@pytest.mark.asyncio
@pytest.mark.parametrize("change", [
    {"document_id": "other"}, {"version_number": 4}, {"version_number": True},
    {"version_number": "3"}, {"source_hash": "b" * 64}, {"source_hash": HASH.upper()},
    {"content": "changed"}, {"content": None}, {"content": "\ud800"}, {"title": None},
], ids=["document", "version", "bool-version", "string-version", "hash", "noncanonical-hash",
        "content", "null-content", "invalid-unicode", "title"])
async def test_response_identity_and_content_mismatch_is_safe(host, change):
    respond(host, source(**change))
    with pytest.raises(AssistantError, match="Invalid immutable source response"):
        await host.read()
    assert len(host.requests) == 1 and host.requests[0].method == "GET"


@pytest.mark.asyncio
@pytest.mark.parametrize("value", [[], None, {"document_id": "document-one"}],
                         ids=["array", "null", "missing-fields"])
async def test_invalid_response_shape(host, value):
    respond(host, value)
    with pytest.raises(AssistantError, match="Invalid immutable source response"):
        await host.read()
    assert len(host.requests) == 1


@pytest.mark.asyncio
async def test_invalid_json_is_safe(host):
    host.responses.append(httpx.Response(200, content=b"internal-secret-not-json"))
    with pytest.raises(AssistantError, match="^Invalid immutable source response$"):
        await host.read()
    assert len(host.requests) == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("status", [401, 403, 404],
                         ids=["unauthenticated", "denied", "unavailable"])
async def test_http_denial_preserved_without_fallback(host, status):
    respond(host, {"detail": {"code": "SOURCE_VERSION_UNAVAILABLE"}}, status)
    with pytest.raises(AssistantError) as error:
        await host.read()
    assert error.value.status_code == status
    assert len(host.requests) == 1 and host.requests[0].method == "GET"


@pytest.mark.asyncio
async def test_existing_get_retry_only_repeats_exact_read(host):
    host.client._auth.config.max_retries = 1
    host.client._transport._backoff = AsyncMock()
    host.responses.append(httpx.ConnectError("synthetic read interruption"))
    respond(host, source())
    assert await host.read() == source()
    assert len(host.requests) == 2
    assert all(r.method == "GET" and r.url == host.requests[0].url for r in host.requests)


@pytest.mark.asyncio
async def test_redirect_is_not_followed(host):
    host.responses.append(httpx.Response(302, headers={"location": "https://other.example/source"}))
    with pytest.raises(AssistantError):
        await host.read()
    assert len(host.requests) == 1


class LazyBody(httpx.AsyncByteStream):
    def __init__(self, chunks, *, close_error=False, block=False):
        import asyncio

        self.chunks = chunks
        self.close_error = close_error
        self.block = block
        self.entered = asyncio.Event()
        self.reads = 0
        self.closed = 0

    async def __aiter__(self):
        import asyncio

        for chunk in self.chunks:
            self.reads += 1
            yield chunk
        if self.block:
            self.entered.set()
            await asyncio.Event().wait()

    async def aclose(self):
        self.closed += 1
        if self.close_error:
            raise ValueError("internal-secret-cleanup")


@pytest.mark.asyncio
async def test_utf8_id_byte_boundary_and_max_version(host):
    document = "é" * 512
    respond(host, source(document_id=document, version_number=2**31 - 1))
    result = await host.read(document_id=document, source_version=2**31 - 1)
    assert result["document_id"] == document and result["version_number"] == 2**31 - 1
    with pytest.raises(ValueError):
        await host.read(document_id="é" * 513)
    assert len(host.requests) == 1


@pytest.mark.asyncio
async def test_declared_oversize_closes_before_read_without_retry(host):
    host.client._auth.config.max_retries = 3
    body = LazyBody([b"must not be read"])
    host.responses.append(httpx.Response(200, headers={"content-length": str(2**21 + 1)},
                                         stream=body))
    with pytest.raises(AssistantError, match="Response exceeds byte limit"):
        await host.read()
    assert body.reads == 0 and body.closed == 1 and len(host.requests) == 1
    assert host.requests[0].headers["accept-encoding"] == "identity"


@pytest.mark.asyncio
@pytest.mark.parametrize("declared", [None, "2"], ids=["chunked", "dishonest-length"])
async def test_stream_overflow_closes_without_retry(host, declared):
    host.client._auth.config.max_retries = 3
    body = LazyBody([b"a" * 2**20, b"b" * 2**20, b"overflow", b"never-read"])
    headers = {} if declared is None else {"content-length": declared}
    host.responses.append(httpx.Response(200, headers=headers, stream=body))
    with pytest.raises(AssistantError, match="Response exceeds byte limit"):
        await host.read()
    assert body.reads == 3 and body.closed == 1 and len(host.requests) == 1


@pytest.mark.asyncio
async def test_compressed_response_refused_before_body_read(host):
    body = LazyBody([b"not-even-valid-gzip"])
    host.responses.append(httpx.Response(200, headers={"content-encoding": "gzip"}, stream=body))
    with pytest.raises(AssistantError, match="Compressed bounded response is unsupported"):
        await host.read()
    assert body.reads == 0 and body.closed == 1 and len(host.requests) == 1


@pytest.mark.asyncio
async def test_exact_two_mib_json_boundary_passes(host):
    cap = 2**21
    empty = json.dumps(source(content=""), separators=(",", ":")).encode()
    content = "a" * (cap - len(empty))
    digest = hashlib.sha256(content.encode()).hexdigest()
    value = source(content=content, source_hash=digest)
    payload = json.dumps(value, separators=(",", ":")).encode()
    assert len(payload) == cap
    body = LazyBody([payload[i:i + 65536] for i in range(0, len(payload), 65536)])
    host.responses.append(httpx.Response(200, headers={"content-length": str(cap)}, stream=body))
    result = await host.read(source_hash=digest)
    assert result["content"] == content and result["source_hash"] == digest
    assert body.closed == 1 and len(host.requests) == 1


@pytest.mark.asyncio
async def test_size_failure_wins_cleanup_failure(host):
    body = LazyBody([], close_error=True)
    host.responses.append(httpx.Response(200, headers={"content-length": str(2**21 + 1)},
                                         stream=body))
    with pytest.raises(AssistantError, match="^Response exceeds byte limit$"):
        await host.read()
    assert body.closed == 1 and len(host.requests) == 1


@pytest.mark.asyncio
async def test_cancellation_wins_cleanup_failure(host):
    import asyncio

    body = LazyBody([], block=True, close_error=True)
    host.responses.append(httpx.Response(200, stream=body))
    task = asyncio.create_task(host.read())
    await body.entered.wait()
    task.cancel()
    with pytest.raises(asyncio.CancelledError):
        await task
    assert body.closed == 1 and len(host.requests) == 1


@pytest.mark.asyncio
async def test_bounded_normal_cleanup_failure_is_safe(host):
    body = LazyBody([json.dumps(source()).encode()], close_error=True)
    host.responses.append(httpx.Response(200, stream=body))
    with pytest.raises(AssistantError, match="^Bounded response read failed$"):
        await host.read()
    assert body.closed == 1


@pytest.mark.asyncio
@pytest.mark.parametrize("value", [0, -1, True, "2"], ids=["zero", "negative", "bool", "string"])
async def test_invalid_transport_limit_is_local(host, value):
    with pytest.raises(ValueError):
        await host.client._transport.request("GET", "/anything", max_response_bytes=value)
    assert host.requests == []


@pytest.mark.asyncio
async def test_transport_limit_cannot_change_mutation_behavior(host):
    with pytest.raises(ValueError):
        await host.client._transport.request("POST", "/anything", max_response_bytes=100)
    assert host.requests == []
