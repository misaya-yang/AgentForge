from __future__ import annotations

import base64
import re
from types import SimpleNamespace

import pytest
from fastapi import HTTPException

from src.api.v1._assistant_routes import attachment_refs
from src.core.auth.user_resolver import UserContext


class _Files:
    def __init__(self) -> None:
        self.downloads: list[str] = []

    async def download_file(self, key: str) -> bytes:
        self.downloads.append(key)
        if key.endswith("missing.txt"):
            raise FileNotFoundError
        return b"Visible attachment content"


class _Artifacts:
    def __init__(self) -> None:
        self.rows: dict[str, SimpleNamespace] = {}
        self.creates: list[dict] = []

    async def get_artifact(self, artifact_id: str, **_scope):
        return self.rows.get(artifact_id)

    async def create_artifact(self, **kwargs):
        self.creates.append(kwargs)
        self.rows[kwargs["artifact_id"]] = SimpleNamespace(
            session_id=kwargs["session_id"], size_bytes=len(kwargs["content"]), mime_type="text/plain",
        )
        return self.rows[kwargs["artifact_id"]]

    async def download_artifact(self, _artifact_id: str) -> bytes:
        return b"\x89PNG\r\n\x1a\n" + b"image bytes"


def _owner() -> UserContext:
    return UserContext(
        user_id="user-1", tenant_id="tenant-1", tier="normal", roles=[], is_authenticated=True,
    )


@pytest.mark.asyncio
async def test_upload_is_bound_to_stable_session_artifact_not_path(monkeypatch: pytest.MonkeyPatch) -> None:
    files, artifacts = _Files(), _Artifacts()
    monkeypatch.setattr(attachment_refs, "get_file_storage", lambda: files)
    monkeypatch.setattr(attachment_refs, "get_artifact_storage", lambda: artifacts)
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace()))
    ref = "/uploads/tenant-1/user-1/abcdef01_20260923_150000.txt"

    first = await attachment_refs.bind_assistant_attachment_refs(
        request, _owner(), session_id="session-1", model_id="text-model", refs=[ref, ref],
    )
    again = await attachment_refs.bind_assistant_attachment_refs(
        request, _owner(), session_id="session-1", model_id="text-model", refs=[ref],
    )

    assert first == again
    assert len(first) == 1 and re.fullmatch(r"art_[0-9a-f]{16}", first[0])
    assert all(item["source"] == "user" and item["session_id"] == "session-1" for item in artifacts.creates)
    assert files.downloads == ["uploads/tenant-1/user-1/abcdef01_20260923_150000.txt"] * 3


@pytest.mark.asyncio
async def test_upload_read_uses_gateway_storage_config_when_global_service_is_unavailable(
    monkeypatch: pytest.MonkeyPatch, tmp_path,
) -> None:
    monkeypatch.setenv("GATEWAY_STORAGE__BACKEND", "local")
    monkeypatch.setenv("GATEWAY_STORAGE__LOCAL_BASE_PATH", str(tmp_path / "objects"))
    monkeypatch.setattr(
        attachment_refs, "get_file_storage",
        lambda: (_ for _ in ()).throw(RuntimeError("not initialized")),
    )
    uploaded = await attachment_refs.FileStorageService(
        attachment_refs.StorageConfig.from_env()
    ).upload_file(
        user_id="user-1", tenant_id="tenant-1", filename="check.txt",
        content=b"verified content", content_type="text/plain", file_id="abcdef01",
    )

    assert await attachment_refs._user_upload_bytes(
        uploaded.file_path, attachment_refs._upload_name(uploaded.file_path, _owner()), _owner()
    ) == b"verified content"


@pytest.mark.asyncio
async def test_local_streamed_upload_without_key_prefix_remains_readable(
    monkeypatch: pytest.MonkeyPatch, tmp_path,
) -> None:
    monkeypatch.setenv("GATEWAY_STORAGE__BACKEND", "local")
    monkeypatch.setenv("GATEWAY_STORAGE__LOCAL_BASE_PATH", str(tmp_path))
    monkeypatch.setenv("GATEWAY_STORAGE__KEY_PREFIX", "dev")
    storage = attachment_refs.FileStorageService(attachment_refs.StorageConfig.from_env())
    monkeypatch.setattr(attachment_refs, "get_file_storage", lambda: storage)
    key = "uploads/tenant-1/user-1/abcdef01_20260923_150000.txt"
    path = tmp_path / key
    path.parent.mkdir(parents=True)
    path.write_bytes(b"streamed upload")

    assert await attachment_refs._user_upload_bytes(
        f"/{key}", "abcdef01_20260923_150000.txt", _owner()
    ) == b"streamed upload"


@pytest.mark.asyncio
async def test_legacy_local_upload_fallback_remains_readable(
    monkeypatch: pytest.MonkeyPatch, tmp_path,
) -> None:
    monkeypatch.setenv("GATEWAY_STORAGE__BACKEND", "local")
    monkeypatch.setenv("GATEWAY_STORAGE__LOCAL_BASE_PATH", str(tmp_path / "gateway"))
    monkeypatch.setattr(
        attachment_refs, "get_file_storage",
        lambda: (_ for _ in ()).throw(RuntimeError("not initialized")),
    )
    legacy_root = tmp_path / "legacy" / "uploads" / "tenant-1" / "user-1"
    monkeypatch.setattr(
        attachment_refs, "get_user_uploads_path",
        lambda _user_id, _tenant_id: legacy_root,
    )
    legacy_root.mkdir(parents=True)
    name = "abcdef01_20260923_150000.txt"
    (legacy_root / name).write_bytes(b"legacy upload")

    assert await attachment_refs._user_upload_bytes(
        f"/uploads/tenant-1/user-1/{name}", name, _owner()
    ) == b"legacy upload"


@pytest.mark.asyncio
async def test_foreign_or_missing_upload_never_enters_runtime(monkeypatch: pytest.MonkeyPatch) -> None:
    files, artifacts = _Files(), _Artifacts()
    monkeypatch.setattr(attachment_refs, "get_file_storage", lambda: files)
    monkeypatch.setattr(attachment_refs, "get_artifact_storage", lambda: artifacts)
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace()))
    for ref in (
        "/uploads/tenant-1/other-user/abcdef01_20260923_150000.txt",
        "/uploads/tenant-1/user-1/../../other-user/file.txt",
        "/uploads/tenant-1/user-1/abcdef01_20260923_150000.missing.txt",
    ):
        with pytest.raises(HTTPException) as error:
            await attachment_refs.bind_assistant_attachment_refs(
                request, _owner(), session_id="session-1", model_id="text-model", refs=[ref],
            )
        assert error.value.status_code == 422
    assert not artifacts.creates


@pytest.mark.asyncio
async def test_image_upload_requires_current_model_vision_capability(monkeypatch: pytest.MonkeyPatch) -> None:
    files, artifacts = _Files(), _Artifacts()
    monkeypatch.setattr(attachment_refs, "get_file_storage", lambda: files)
    monkeypatch.setattr(attachment_refs, "get_artifact_storage", lambda: artifacts)

    class _ModelService:
        async def get_model(self, *_args):
            return {"is_enabled": True, "supports_vision": False}

    monkeypatch.setattr(attachment_refs, "assistant_model_service", lambda _request: _ModelService())
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace()))
    with pytest.raises(HTTPException) as error:
        await attachment_refs.bind_assistant_attachment_refs(
            request, _owner(), session_id="session-1", model_id="text-model",
            refs=["/uploads/tenant-1/user-1/abcdef01_20260923_150000.png"],
        )
    assert error.value.status_code == 409
    assert error.value.detail == {"code": "VISION_MODEL_REQUIRED"}
    assert not files.downloads and not artifacts.creates


@pytest.mark.asyncio
async def test_existing_artifact_must_belong_to_current_session(monkeypatch: pytest.MonkeyPatch) -> None:
    artifacts = _Artifacts()
    artifacts.rows["art_1111111111111111"] = SimpleNamespace(
        session_id="other-session", size_bytes=10, mime_type="text/plain",
    )
    monkeypatch.setattr(attachment_refs, "get_artifact_storage", lambda: artifacts)
    request = SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace()))
    with pytest.raises(HTTPException) as error:
        await attachment_refs.bind_assistant_attachment_refs(
            request, _owner(), session_id="session-1", model_id="text-model",
            refs=["art_1111111111111111"],
        )
    assert error.value.status_code == 422


@pytest.mark.asyncio
async def test_selected_image_bytes_are_transient_bounded_model_input(monkeypatch: pytest.MonkeyPatch) -> None:
    artifacts = _Artifacts()
    image_bytes = await artifacts.download_artifact("art_1111111111111111")
    artifacts.rows["art_1111111111111111"] = SimpleNamespace(
        session_id="session-1", size_bytes=len(image_bytes), mime_type="image/png",
    )
    monkeypatch.setattr(attachment_refs, "get_artifact_storage", lambda: artifacts)

    images = await attachment_refs.selected_image_inputs(
        _owner(), session_id="session-1", refs=["art_1111111111111111"],
    )

    assert len(images) == 1
    assert images[0].startswith("data:image/png;base64,")
    assert base64.b64decode(images[0].split(",", 1)[1]) == image_bytes


@pytest.mark.asyncio
@pytest.mark.parametrize("original, expected", [("说明.png", "说明.png"), ("../secret.png", None), ("bad\nname.png", None)])
async def test_binding_preserves_safe_original_upload_name_once(monkeypatch, original, expected) -> None:
    files, artifacts = _Files(), _Artifacts()
    class Backend:
        async def head(self, key):
            assert key.startswith("uploads/tenant-1/user-1/")
            return SimpleNamespace(metadata={"original_filename": original})
    files._backend = Backend()
    monkeypatch.setattr(attachment_refs, "get_file_storage", lambda: files)
    monkeypatch.setattr(attachment_refs, "get_artifact_storage", lambda: artifacts)
    await attachment_refs.bind_assistant_attachment_refs(
        SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace())), _owner(), session_id="session-1", model_id="text-model",
        refs=["/uploads/tenant-1/user-1/abcdef01_20260923_150000.txt"],
    )
    assert artifacts.creates[0]["metadata"].get("original_filename") == expected


@pytest.mark.asyncio
async def test_optional_upload_metadata_failure_keeps_verified_bytes_usable(monkeypatch) -> None:
    files, artifacts = _Files(), _Artifacts()
    class Backend:
        async def head(self, _key):
            raise RuntimeError("controlled metadata service unavailable")
    files._backend = Backend()
    monkeypatch.setattr(attachment_refs, "get_file_storage", lambda: files)
    monkeypatch.setattr(attachment_refs, "get_artifact_storage", lambda: artifacts)
    bound = await attachment_refs.bind_assistant_attachment_refs(
        SimpleNamespace(app=SimpleNamespace(state=SimpleNamespace())), _owner(),
        session_id="session-1", model_id="text-model",
        refs=["/uploads/tenant-1/user-1/abcdef01_20260923_150000.txt"],
    )
    assert len(bound) == 1
    assert artifacts.creates[0]["filename"] == "abcdef01_20260923_150000.txt"
    assert "original_filename" not in artifacts.creates[0]["metadata"]
