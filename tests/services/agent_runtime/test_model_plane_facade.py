from __future__ import annotations

import ast
import inspect
import textwrap
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest

from src.services.agent_runtime import model_plane
from src.services.agent_runtime.model import (
    authorization,
    chat_completions,
    native_responses,
    stream_projection,
)


def test_model_facade_ast_surface_is_stable() -> None:
    tree = ast.parse(textwrap.dedent(inspect.getsource(model_plane.AgentModelPlane)))
    facade = next(node for node in tree.body if isinstance(node, ast.ClassDef))
    methods = {
        node.name: (type(node).__name__, ast.unparse(node.args), ast.unparse(node.returns))
        for node in facade.body
        if isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef)
    }

    assert methods == {
        "__init__": (
            "FunctionDef",
            "self, *, database: _Database, provider_service: Any, "
            "lease_signer: RuntimeModelLeaseSigner, "
            "http_client: httpx.AsyncClient | None=None, "
            "clock: Callable[[], float]=time.perf_counter, "
            "admission_controller: Any | None=None, capacity_resolver: Any | None=None, "
            "source_access_checker: Any | None=None",
            "None",
        ),
        "close": ("AsyncFunctionDef", "self", "None"),
        "_validate_turn_thread_scope": (
            "AsyncFunctionDef",
            "self, *, claims: RuntimeModelLeaseClaims, turn_metadata: Mapping[str, Any]",
            "None",
        ),
        "authorize_and_reserve": (
            "AsyncFunctionDef",
            "self, *, body: dict[str, Any], turn_metadata: dict[str, Any]",
            "_AuthorizedCall",
        ),
        "stream": (
            "AsyncFunctionDef",
            "self, *, body: dict[str, Any], turn_metadata: dict[str, Any], "
            "authorized_call: _AuthorizedCall | None=None",
            "AsyncIterator[bytes]",
        ),
        "_stream_native_responses": (
            "AsyncFunctionDef",
            "self, *, call: _AuthorizedCall, timing: ModelPlaneTiming, "
            "body: dict[str, Any], profile: Mapping[str, Any], "
            "reasoning: Mapping[str, Any], api_key: str, base_url: str, "
            "allowed_tool_names: set[str] | None=None, "
            "tool_choice: str | dict[str, str]='auto', parallel_tool_calls: bool=True",
            "AsyncIterator[bytes]",
        ),
        "_log_model_plane_timing": (
            "FunctionDef",
            "self, wire: str, call: _AuthorizedCall, timing: ModelPlaneTiming",
            "None",
        ),
        "_complete_call": (
            "AsyncFunctionDef",
            "self, *, call: _AuthorizedCall, input_tokens: int, output_tokens: int, "
            "provider_request_id: str | None",
            "None",
        ),
        "_fail_call": (
            "AsyncFunctionDef",
            "self, call_id: uuid.UUID, code: str, *, dispatched: bool",
            "None",
        ),
        "_mark_unknown_if_dispatched": (
            "AsyncFunctionDef",
            "self, call_id: uuid.UUID",
            "None",
        ),
    }


def test_model_facade_preserves_identity_and_generator_descriptors() -> None:
    assert model_plane.AgentModelPlaneError is authorization.AgentModelPlaneError
    assert model_plane._AuthorizedCall is authorization._AuthorizedCall
    assert (
        model_plane._NativeResponsesStreamValidator
        is stream_projection._NativeResponsesStreamValidator
    )
    assert model_plane._ResponsesProjector is stream_projection._ResponsesProjector
    assert inspect.isasyncgenfunction(model_plane.AgentModelPlane.stream)
    assert inspect.isasyncgenfunction(
        model_plane.AgentModelPlane._stream_native_responses
    )


@pytest.mark.asyncio
async def test_authorize_wrapper_delegates_with_live_facade_helpers(monkeypatch) -> None:
    instance = object.__new__(model_plane.AgentModelPlane)
    sentinel = object()
    seen: dict[str, object] = {}

    async def fake_authorize(plane, **kwargs):
        seen.update(plane=plane, **kwargs)
        return sentinel

    monkeypatch.setattr(authorization, "authorize_and_reserve", fake_authorize)

    result = await instance.authorize_and_reserve(body={}, turn_metadata={})

    assert result is sentinel
    assert seen["plane"] is instance
    assert seen["_helpers"] is model_plane


def test_native_body_wrapper_forwards_reasoning_monkeypatch_seam(monkeypatch) -> None:
    sentinel = ({"wire": True}, {"alias": ("namespace", "tool")})
    seen: dict[str, object] = {}

    def fake_native(body, **kwargs):
        seen.update(body=body, **kwargs)
        return sentinel

    monkeypatch.setattr(native_responses, "_native_responses_body", fake_native)

    result = model_plane._native_responses_body(
        {"input": "hello"},
        model_id="model",
        max_output_tokens=32,
        profile={},
        reasoning_option="auto",
    )

    assert result is sentinel
    assert seen["_helpers"] is model_plane
    assert seen["_apply_reasoning_wire"] is model_plane.apply_reasoning_wire


def test_message_projection_resolves_live_content_helper(monkeypatch) -> None:
    monkeypatch.setattr(model_plane, "_content_text", lambda _value: "patched")

    messages = model_plane._responses_input_to_messages(
        {"input": [{"type": "message", "role": "user", "content": []}]}
    )

    assert messages == [{"role": "user", "content": "patched"}]


@pytest.mark.asyncio
async def test_stream_wrapper_delegates_and_closes_authority(monkeypatch) -> None:
    instance = object.__new__(model_plane.AgentModelPlane)
    instance.authorize_and_reserve = AsyncMock(return_value=SimpleNamespace(
        call_id="call", tenant_id="tenant", user_id="user", provider_id="provider",
    ))
    instance.database = SimpleNamespace(execute=AsyncMock())

    closed = False

    async def fake_stream(plane, **kwargs):
        nonlocal closed
        assert plane is instance
        assert kwargs["_helpers"] is model_plane
        try:
            yield b"frame"
            yield b"terminal"
        finally:
            closed = True

    monkeypatch.setattr(chat_completions, "stream", fake_stream)
    stream = instance.stream(body={}, turn_metadata={})

    assert await stream.__anext__() == b"frame"
    await stream.aclose()
    assert closed
