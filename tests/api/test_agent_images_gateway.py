"""Contract tests for the Gateway-owned image routes."""

import base64
import inspect
from datetime import datetime, timezone
from types import SimpleNamespace
from unittest.mock import AsyncMock

import pytest
from fastapi import HTTPException

from src.api.schemas.assistant import ImageBlobFetchUrlRequest, ImageGenerationRequest
from src.api.v1 import agent_images as image_routes
from src.api.v1.agent_images import (
    _sniff_mime,
    fetch_image_blob_from_url,
    get_image_task_status,
    router,
)
from src.services.images import service as image_service
from src.services.images.repository import reserve_scoped_image_task
from src.services.images.service import (
    ImageGenerationService,
    _decode_image,
    _deterministic_artifact_id,
    _owner,
    _provider_supports_reference_images,
    _resolve_reference,
    _sniff_reference,
    public_image_error,
)
from src.services.images.worker import ImageTaskWorker


def test_gateway_image_routes_keep_public_paths() -> None:
    paths = {route.path for route in router.routes}
    assert {
        "/assistant/generate-image",
        "/assistant/generate-image-async",
        "/assistant/image-task/{task_id}",
        "/assistant/image-sessions/{session_id}",
        "/assistant/artifacts/{artifact_id}/download-url",
        "/assistant/image-blobs/upload-url",
        "/assistant/image-blobs/complete",
        "/assistant/image-blobs/fetch-url",
    } <= paths


def test_provider_image_payload_is_validated_before_artifact_write() -> None:
    with pytest.raises(ValueError, match="invalid image"):
        _decode_image({"content_base64": base64.b64encode(b"not-an-image").decode()})


def test_artifact_id_is_stable_and_scoped() -> None:
    args = ("tenant", "user", "session", "turn", 1, "a" * 64)
    first = _deterministic_artifact_id(*args)
    assert first == _deterministic_artifact_id(*args)
    assert first.startswith("art_") and len(first) == 20
    assert first != _deterministic_artifact_id("other", *args[1:])


@pytest.mark.asyncio
async def test_remote_blob_fetch_rejects_private_destination(monkeypatch) -> None:
    monkeypatch.setattr(image_routes, "get_artifact_storage", lambda: object())
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(multi_rate_limiter=None)))
    with pytest.raises(Exception) as error:
        await fetch_image_blob_from_url(
            ImageBlobFetchUrlRequest(url="http://127.0.0.1/a.png"),
            request,
            SimpleNamespace(user_id="u", tenant_id="t"),
        )
    assert getattr(error.value, "status_code", None) == 422


def test_blob_magic_is_not_trusted_from_declared_mime() -> None:
    assert _sniff_mime(b"not-a-png") is None


def test_concurrent_reservation_contract_is_scoped_and_atomic() -> None:
    params = inspect.signature(reserve_scoped_image_task).parameters
    assert {"tenant_id", "user_id", "owner_scope", "client_request_id", "request_hash"} <= set(
        params
    )


def test_reference_magic_and_worker_are_bounded() -> None:
    assert _sniff_reference(b"\x89PNG\r\n\x1a\n") == "image/png"
    assert "return" in ImageTaskWorker.run_once.__annotations__


def test_request_body_cannot_forge_owner_scope() -> None:
    user = SimpleNamespace(user_id="gateway-user", tenant_id="tenant-a")
    body = SimpleNamespace(app_tenant_id="tenant-b", app_user_id="other-user")
    assert _owner(user, body) == image_routes._owner_scope(user)
    assert _owner(user, body) != image_routes._owner_scope(
        SimpleNamespace(user_id="gateway-user", tenant_id="tenant-b")
    )


def test_reference_capability_requires_explicit_declaration() -> None:
    assert not _provider_supports_reference_images(SimpleNamespace(config=SimpleNamespace()))
    assert _provider_supports_reference_images(
        SimpleNamespace(config=SimpleNamespace(supports_reference_images=True))
    )


def test_image_model_receipt_uses_provider_configuration_not_chat_selection(monkeypatch) -> None:
    monkeypatch.setattr("src.services.images.service.get_artifact_storage", lambda: None)
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace(
        database=None,
        image_generation_service=SimpleNamespace(config=SimpleNamespace(model="wan2.6-t2i")),
    )))
    service = ImageGenerationService(request, None)
    assert service.effective_model_id() == "wan2.6-t2i"


@pytest.mark.asyncio
async def test_reference_requires_valid_image_bytes_not_magic_only() -> None:
    body = SimpleNamespace(reference_image=base64.b64encode(b"not-an-image").decode())
    with pytest.raises(ValueError, match="reference image is invalid"):
        await _resolve_reference(
            body,
            session_row=None,
            storage=SimpleNamespace(),
            owner="owner",
            user=SimpleNamespace(tenant_id="tenant", user_id="user"),
            pool=None,
        )


@pytest.mark.asyncio
async def test_unknown_image_provider_result_preserves_safe_task_id(monkeypatch) -> None:
    user = SimpleNamespace(tenant_id="tenant", user_id="user")
    owner = _owner(user, None)
    monkeypatch.setattr(
        image_service, "get_image_session",
        AsyncMock(side_effect=[None, {"owner_scope": owner}]),
    )
    monkeypatch.setattr(image_service, "reserve_scoped_image_task", AsyncMock(return_value={"state": "reserved"}))
    monkeypatch.setattr(image_service, "upsert_image_session", AsyncMock())
    monkeypatch.setattr(image_service, "insert_turn", AsyncMock())
    update_turn = AsyncMock()
    monkeypatch.setattr(image_service, "update_turn_status", update_turn)
    update_task = AsyncMock()
    monkeypatch.setattr(image_service, "update_image_task", update_task)
    service = object.__new__(ImageGenerationService)
    service.pool = object()
    service.storage = object()
    service.user = user
    service.provider = SimpleNamespace(
        config=SimpleNamespace(model="image-model"),
        generate=AsyncMock(return_value=SimpleNamespace(outcome_unknown=True)),
    )

    with pytest.raises(HTTPException) as error:
        await service.generate(ImageGenerationRequest(
            prompt="a blue square", model_id="chat-model", session_id="session",
        ))

    assert error.value.status_code == 502
    detail = error.value.detail
    assert detail["error_code"] == "outcome_unknown"
    assert detail["task_id"].startswith("imt_")
    assert "a blue square" not in str(detail)
    assert update_turn.await_args.kwargs["status"] == "unknown"
    assert update_task.await_args.kwargs["error"] == "image generation outcome unknown"


@pytest.mark.asyncio
async def test_image_storage_failure_after_provider_success_is_unknown(monkeypatch) -> None:
    user = SimpleNamespace(tenant_id="tenant", user_id="user")
    owner = _owner(user, None)
    monkeypatch.setattr(
        image_service, "get_image_session",
        AsyncMock(side_effect=[None, {"owner_scope": owner}]),
    )
    monkeypatch.setattr(image_service, "reserve_scoped_image_task", AsyncMock(return_value={"state": "reserved"}))
    monkeypatch.setattr(image_service, "upsert_image_session", AsyncMock())
    monkeypatch.setattr(image_service, "insert_turn", AsyncMock())
    update_turn = AsyncMock()
    monkeypatch.setattr(image_service, "update_turn_status", update_turn)
    update_task = AsyncMock()
    monkeypatch.setattr(image_service, "update_image_task", update_task)
    monkeypatch.setattr(image_service, "_decode_image", lambda _item: (b"image", "png"))
    service = object.__new__(ImageGenerationService)
    service.pool = object()
    service.storage = SimpleNamespace(create_artifact=AsyncMock(side_effect=RuntimeError("internal storage detail")))
    service.user = user
    service.provider = SimpleNamespace(
        config=SimpleNamespace(model="image-model"),
        generate=AsyncMock(return_value=SimpleNamespace(
            success=True, images=[{"content_base64": "unused"}], provider="test",
        )),
    )

    with pytest.raises(HTTPException) as error:
        await service.generate(ImageGenerationRequest(
            prompt="a blue square", model_id="chat-model", session_id="session",
        ))

    assert error.value.status_code == 502
    detail = error.value.detail
    assert detail["error_code"] == "outcome_unknown"
    assert detail["task_id"].startswith("imt_")
    assert "internal storage detail" not in str(detail)
    assert update_turn.await_args.kwargs["status"] == "unknown"
    assert update_task.await_args.kwargs["error"] == "image generation outcome unknown"
    assert "internal storage detail" not in str(update_turn.await_args.kwargs)


@pytest.mark.asyncio
async def test_old_image_task_and_session_errors_are_sanitized_on_read(monkeypatch) -> None:
    raw_error = "internal storage detail and credential"
    now = datetime.now(timezone.utc)
    service = SimpleNamespace(
        task=AsyncMock(return_value={
            "status": "unknown", "error_code": "outcome_unknown", "error": raw_error,
            "created_at": now, "completed_at": now,
        }),
        effective_model_id=lambda: "image-model",
    )
    monkeypatch.setattr(image_routes, "_service", lambda *_args: service)
    task = await get_image_task_status("imt_aaaaaaaaaaaaaaaaaaaa", SimpleNamespace(), SimpleNamespace())
    assert task["error"] == "image generation outcome unknown"
    assert task["effective_model_id"] is None
    assert raw_error not in str(task)

    user = SimpleNamespace(tenant_id="tenant", user_id="user")
    owner = _owner(user, None)
    monkeypatch.setattr(image_service, "get_image_session", AsyncMock(return_value={
        "owner_scope": owner, "latest_artifact_id": None,
        "locked_style": None, "created_at": now, "updated_at": now,
    }))
    monkeypatch.setattr(image_service, "list_turns", AsyncMock(return_value=([{
        "status": "failed", "error_code": "provider_failed", "error": raw_error,
        "thought_signature": "private provider continuation token",
        "provider_text": "internal provider text",
        "state": {"internal": "not public"},
        "created_at": now, "completed_at": now,
    }], None)))
    image_session = object.__new__(ImageGenerationService)
    image_session.pool = object()
    image_session.request = SimpleNamespace()
    image_session.user = user
    image_session.storage = None
    result = await image_session.session("session", 50, None, False)
    assert result["turns"][0]["error"] == "image generation failed"
    assert raw_error not in str(result)
    assert "thought_signature" not in result["turns"][0]
    assert "provider_text" not in result["turns"][0]
    assert "state" not in result["turns"][0]
    assert public_image_error("completed", None) is None
