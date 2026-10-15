"""Assistant session listing compatibility tests."""

import base64
from datetime import datetime, timedelta
from types import SimpleNamespace
from unittest.mock import AsyncMock

import asyncpg
import pytest
from fastapi import FastAPI, HTTPException, Request

from src.api.schemas.artifacts import ArtifactCreateRequest
from src.api.v1._artifact_headers import attachment_content_disposition
from src.api.v1._assistant_routes import artifacts as artifact_routes
from src.api.v1._assistant_routes import runs as run_routes
from src.api.v1._assistant_routes import sessions as session_routes
from src.api.v1._assistant_routes.artifacts import _browser_artifact_download_url
from src.api.v1._assistant_routes.schemas import TaskCancelRequest
from src.api.v1._assistant_routes.sessions import _list_assistant_sessions
from src.core.auth.user_resolver import UserContext
from src.models.session import Session, SessionMessage
from src.services.assistant_entry.session_binding import session_runtime_assignment


@pytest.mark.asyncio
async def test_list_assistant_sessions_includes_legacy_service_id():
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)

    now = datetime.utcnow()
    legacy_session = Session(
        session_id="legacy-1",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id="assistant",
        updated_at=now - timedelta(minutes=10),
    )
    builtin_session = Session(
        session_id="builtin-1",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id="__builtin_assistant__",
        updated_at=now,
    )

    async def list_sessions(*, user_id, tenant_id, service_id, limit, status="active"):
        if service_id == "__builtin_assistant__":
            return [builtin_session]
        if service_id == "assistant":
            return [legacy_session]
        return []

    session_manager = AsyncMock()
    session_manager.list_sessions.side_effect = list_sessions

    sessions = await _list_assistant_sessions(session_manager, user, limit=50)

    assert [s.session_id for s in sessions] == ["builtin-1", "legacy-1"]


def _build_request(
    session_manager: AsyncMock,
    *,
    method: str = "GET",
    path: str = "/api/v1/assistant/sessions/test/artifacts",
) -> Request:
    app = FastAPI()
    app.state.session_manager = session_manager
    app.state.database = SimpleNamespace(
        fetch=AsyncMock(return_value=[]), fetchrow=AsyncMock(return_value=None),
    )
    return Request(
        {
            "type": "http",
            "app": app,
            "headers": [],
            "method": method,
            "path": path,
        }
    )


@pytest.mark.asyncio
async def test_create_session_persists_runtime_assignment() -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session = Session(
        session_id="session-1",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id="__builtin_assistant__",
    )
    session_manager = AsyncMock()
    session_manager.create.return_value = session
    assignment_store = AsyncMock()
    request = _build_request(session_manager, method="POST")
    request.app.state.assistant_runtime_assignments = assignment_store
    request.app.state.assistant_runtime_default_owner = "agent_runtime"
    request.app.state.assistant_runtime_kernel_revision = "fork-sha"

    response = await session_routes.create_session(None, user, request)

    assert response.session_id == session.session_id
    assignment_store.bind.assert_awaited_once_with(
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        session_id=session.session_id,
        runtime_owner="agent_runtime",
        kernel_revision="fork-sha",
    )


@pytest.mark.asyncio
async def test_create_session_removes_row_when_runtime_assignment_fails() -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session = Session(
        session_id="session-1",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id="__builtin_assistant__",
    )
    session_manager = AsyncMock()
    session_manager.create.return_value = session
    assignment_store = AsyncMock()
    assignment_store.bind.side_effect = RuntimeError("assignment failed")
    request = _build_request(session_manager, method="POST")
    request.app.state.assistant_runtime_assignments = assignment_store
    request.app.state.assistant_runtime_default_owner = "python_control"
    request.app.state.assistant_runtime_kernel_revision = None

    with pytest.raises(HTTPException) as exc_info:
        await session_routes.create_session(None, user, request)

    assert exc_info.value.status_code == 500
    session_manager.delete.assert_awaited_once_with(session.session_id)


@pytest.mark.asyncio
async def test_create_session_sanitizes_internal_failure_detail() -> None:
    """ARC-01 deliverable 6: 500 detail must not echo the exception."""
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    session_manager.create.side_effect = RuntimeError("db password=hunter2 at 10.0.0.7")
    request = _build_request(session_manager, method="POST")

    with pytest.raises(HTTPException) as exc_info:
        await session_routes.create_session(None, user, request)

    assert exc_info.value.status_code == 500
    assert exc_info.value.detail == "Failed to create session"
    assert "hunter2" not in str(exc_info.value.detail)


@pytest.mark.asyncio
async def test_agent_runtime_assignment_is_accepted_without_python_fallback() -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    request = _build_request(AsyncMock(), method="POST")
    assignment_store = AsyncMock()
    assignment_store.resolve.return_value = SimpleNamespace(runtime_owner="agent_runtime")
    request.app.state.assistant_runtime_assignments = assignment_store

    resolved = await session_runtime_assignment(request, user, "session-1")
    assert resolved.runtime_owner == "agent_runtime"


@pytest.mark.asyncio
async def test_agent_assignment_is_returned_only_when_control_plane_is_ready() -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    request = _build_request(AsyncMock(), method="POST")
    assignment = SimpleNamespace(runtime_owner="agent_runtime")
    assignment_store = AsyncMock()
    assignment_store.resolve.return_value = assignment
    request.app.state.assistant_runtime_assignments = assignment_store
    request.app.state.agent_runtime_control = SimpleNamespace()

    resolved = await session_runtime_assignment(
        request,
        user,
        "session-1",
    )

    assert resolved is assignment


@pytest.mark.asyncio
async def test_session_history_appends_runtime_item_projection() -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session = Session(
        session_id="session-1",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        history=[SessionMessage(role="user", content="legacy")],
    )
    session_manager = AsyncMock()
    session_manager.get.return_value = session
    request = _build_request(session_manager)
    request.app.state.database = AsyncMock()
    store = AsyncMock()
    store.get_for_session.return_value = SimpleNamespace(runtime_thread_id="runtime-1")
    store.history_messages.return_value = (
        [
            {
                "role": "assistant",
                "content": "runtime answer",
                "timestamp": None,
                "metadata": {"runtime_sequence": 2},
            }
        ],
        1,
    )
    request.app.state.agent_thread_store = store

    response = await session_routes.get_session_history(
        "session-1",
        limit=100,
        user=user,
        request=request,
    )

    assert response.total == 2
    assert [(message.role, message.content) for message in response.messages] == [
        ("user", "legacy"),
        ("assistant", "runtime answer"),
    ]


@pytest.mark.asyncio
async def test_session_history_sanitizes_internal_failure_detail() -> None:
    """History must stay a read projection and never leak storage internals."""
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    session_manager.get.side_effect = RuntimeError("connection string leaked")
    request = _build_request(session_manager)

    with pytest.raises(HTTPException) as exc_info:
        await session_routes.get_session_history(
            "session-1",
            limit=100,
            user=user,
            request=request,
        )

    assert exc_info.value.status_code == 500
    assert exc_info.value.detail == "Failed to get session history"
    assert "connection string" not in str(exc_info.value.detail)


@pytest.mark.asyncio
async def test_delete_session_tombstones_gateway_runtime_before_deleting_row() -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    request = _build_request(
        session_manager,
        method="DELETE",
        path="/api/v1/assistant/sessions/session-1",
    )
    session_manager.get.side_effect = [
        SimpleNamespace(user_id=user.user_id, tenant_id=user.tenant_id),
        None,
    ]
    control = AsyncMock()
    control.cleanup_session.return_value = True
    request.app.state.agent_runtime_control = control

    response = await session_routes.delete_session("session-1", user, request)

    assert response == {"session_id": "session-1", "status": "deleted"}
    control.cleanup_session.assert_awaited_once_with(
        tenant_id="tenant_1",
        user_id="user_1",
        session_id="session-1",
    )
    session_manager.delete.assert_awaited_once_with("session-1")
    assert session_manager.get.await_count == 2


@pytest.mark.asyncio
async def test_delete_session_preserves_gateway_row_when_runtime_cleanup_fails(
) -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    session_manager.get.return_value = SimpleNamespace(
        user_id=user.user_id,
        tenant_id=user.tenant_id,
    )
    request = _build_request(
        session_manager,
        method="DELETE",
        path="/api/v1/assistant/sessions/session-1",
    )

    control = AsyncMock()
    control.cleanup_session.side_effect = RuntimeError("runtime unavailable")
    request.app.state.agent_runtime_control = control

    with pytest.raises(HTTPException) as error:
        await session_routes.delete_session("session-1", user, request)

    assert error.value.status_code == 500
    session_manager.delete.assert_not_awaited()


@pytest.mark.asyncio
async def test_list_session_artifacts_returns_empty_when_schema_missing(
    monkeypatch: pytest.MonkeyPatch,
):
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session = Session(
        session_id="session-1",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id="__builtin_assistant__",
    )
    session_manager = AsyncMock()
    session_manager.get.return_value = session

    class BrokenArtifactStorage:
        async def get_session_artifacts(self, session_id: str, tenant_id: str):
            del session_id, tenant_id
            raise asyncpg.UndefinedTableError('relation "assistant.artifacts" does not exist')

    monkeypatch.setattr(
        artifact_routes, "get_artifact_storage", lambda: BrokenArtifactStorage()
    )

    response = await artifact_routes.list_session_artifacts(
        "session-1",
        _build_request(session_manager),
        user,
    )

    assert response.total == 0
    assert response.artifacts == []


@pytest.mark.asyncio
async def test_session_artifact_list_excludes_input_and_image_variants_and_marks_empty(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    session_manager.get.return_value = Session(
        session_id="session-1", user_id=user.user_id, tenant_id=user.tenant_id,
        service_id="__builtin_assistant__",
    )

    def artifact(artifact_id: str, *, source: str = "image_generation", variant: str = "raw", size: int = 10):
        return SimpleNamespace(
            artifact_id=artifact_id, session_id="session-1", type="image", format="png",
            title=artifact_id, filename=f"{artifact_id}.png", size_bytes=size,
            mime_type="image/png", source=source, message_id=None, metadata={},
            created_at=None, variant=variant, parent_artifact_id=None,
        )

    class Storage:
        async def get_session_artifacts(self, *_args):
            return [
                artifact("raw"), artifact("thumbnail", variant="thumbnail"),
                artifact("upload", source="user"), artifact("empty", size=0),
            ]

        async def get_presigned_download_url(self, _artifact):
            return "file:///private/file.png"

    monkeypatch.setattr(artifact_routes, "get_artifact_storage", lambda: Storage())
    response = await artifact_routes.list_session_artifacts(
        "session-1", _build_request(session_manager), user,
    )
    assert response.total == 2
    assert [item.artifact_id for item in response.artifacts] == ["raw", "empty"]
    assert response.artifacts[0].ready is True
    assert response.artifacts[1].ready is False


@pytest.mark.asyncio
async def test_zero_byte_artifact_is_unready_on_create_and_read_and_cannot_download(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    session_manager.get.return_value = Session(
        session_id="session-1", user_id=user.user_id, tenant_id=user.tenant_id,
        service_id="__builtin_assistant__",
    )
    artifact = SimpleNamespace(
        artifact_id="artifact-empty", session_id="session-1", tenant_id=user.tenant_id,
        user_id=user.user_id, type="image", format="png", title="Empty",
        filename="empty.png", size_bytes=0, mime_type="image/png", source="ai",
        message_id=None, metadata={}, created_at=None,
    )

    class Storage:
        async def create_artifact(self, **kwargs):
            assert kwargs["content"] == b""
            return artifact

        async def get_artifact(self, _artifact_id):
            return artifact

        async def get_presigned_download_url(self, _artifact):
            return "file:///private/empty.png"

        async def download_artifact(self, _artifact_id):
            return b""

    monkeypatch.setattr(artifact_routes, "get_artifact_storage", lambda: Storage())
    request = _build_request(session_manager)
    created = await artifact_routes.create_artifact(
        ArtifactCreateRequest(session_id="session-1", type="image", format="png",
                              title="Empty", filename="empty.png", content_base64=""),
        request, user,
    )
    read = await artifact_routes.get_artifact("artifact-empty", request, user)
    assert created.ready is False and read.ready is False
    with pytest.raises(HTTPException) as caught:
        await artifact_routes.download_artifact("artifact-empty", request, user)
    assert caught.value.status_code == 409


@pytest.mark.asyncio
async def test_list_session_artifacts_sanitizes_unexpected_storage_errors(
    monkeypatch: pytest.MonkeyPatch,
):
    """ARC-01 deliverable 6: storage failures surface as 500 with a stable detail."""
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session = Session(
        session_id="session-1",
        user_id=user.user_id,
        tenant_id=user.tenant_id,
        service_id="__builtin_assistant__",
    )
    session_manager = AsyncMock()
    session_manager.get.return_value = session

    class BrokenArtifactStorage:
        async def get_session_artifacts(self, session_id: str, tenant_id: str):
            del session_id, tenant_id
            raise RuntimeError("storage offline")

    monkeypatch.setattr(
        artifact_routes, "get_artifact_storage", lambda: BrokenArtifactStorage()
    )

    with pytest.raises(HTTPException) as exc_info:
        await artifact_routes.list_session_artifacts(
            "session-1",
            _build_request(session_manager),
            user,
        )

    assert exc_info.value.status_code == 500
    assert exc_info.value.detail == "Failed to list artifacts"
    assert "storage offline" not in str(exc_info.value.detail)


@pytest.mark.asyncio
async def test_create_artifact_rejects_foreign_session_before_writing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()
    session_manager.get.return_value = Session(
        session_id="foreign-session",
        user_id="other-user",
        tenant_id=user.tenant_id,
        service_id="__builtin_assistant__",
    )
    artifact_storage = AsyncMock()
    monkeypatch.setattr(
        artifact_routes,
        "get_artifact_storage",
        lambda: artifact_storage,
    )
    body = ArtifactCreateRequest(
        session_id="foreign-session",
        type="document",
        format="txt",
        title="Injected",
        filename="injected.txt",
        content_base64=base64.b64encode(b"not allowed").decode(),
    )

    with pytest.raises(HTTPException) as exc_info:
        await artifact_routes.create_artifact(
            body,
            _build_request(
                session_manager,
                method="POST",
                path="/api/v1/assistant/artifacts",
            ),
            user,
        )

    assert exc_info.value.status_code == 404
    assert exc_info.value.detail == "Session not found"
    artifact_storage.create_artifact.assert_not_awaited()


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("handler", "args"),
    [
        (artifact_routes.get_artifact, ("artifact-1",)),
        (artifact_routes.download_artifact, ("artifact-1",)),
        (artifact_routes.delete_artifact, ("artifact-1",)),
    ],
)
async def test_artifact_lookup_returns_404_when_schema_missing(
    handler,
    args: tuple[str],
    monkeypatch: pytest.MonkeyPatch,
):
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()

    class BrokenArtifactStorage:
        async def get_artifact(self, artifact_id: str):
            del artifact_id
            raise asyncpg.UndefinedTableError('relation "assistant.artifacts" does not exist')

    monkeypatch.setattr(
        artifact_routes, "get_artifact_storage", lambda: BrokenArtifactStorage()
    )

    with pytest.raises(HTTPException) as exc_info:
        await handler(*args, _build_request(session_manager), user)

    assert exc_info.value.status_code == 404
    assert exc_info.value.detail == "Artifact not found"


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("handler", "expected_detail"),
    [
        (artifact_routes.get_artifact, "Failed to get artifact"),
        (artifact_routes.download_artifact, "Failed to download artifact"),
        (artifact_routes.delete_artifact, "Failed to delete artifact"),
    ],
)
async def test_artifact_storage_errors_are_sanitized_to_stable_detail(
    handler,
    expected_detail: str,
    monkeypatch: pytest.MonkeyPatch,
):
    """ARC-01 deliverable 6: 500 detail never echoes the raw exception."""
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    session_manager = AsyncMock()

    class BrokenArtifactStorage:
        async def get_artifact(self, artifact_id: str):
            del artifact_id
            raise RuntimeError("s3 bucket secret endpoint leaked")

    monkeypatch.setattr(
        artifact_routes, "get_artifact_storage", lambda: BrokenArtifactStorage()
    )

    with pytest.raises(HTTPException) as exc_info:
        await handler("artifact-1", _build_request(session_manager), user)

    assert exc_info.value.status_code == 500
    assert exc_info.value.detail == expected_detail
    assert "secret endpoint" not in str(exc_info.value.detail)


def test_local_artifact_url_stays_behind_authenticated_http_route() -> None:
    assert _browser_artifact_download_url("file:///private/result.txt", "artifact-1") == (
        "/api/v1/assistant/artifacts/artifact-1/download"
    )
    assert (
        _browser_artifact_download_url(
            "https://storage.example/result.txt?signature=one",
            "artifact-1",
        )
        == "/api/v1/assistant/artifacts/artifact-1/download"
    )


@pytest.mark.parametrize(
    ("filename", "expected"),
    [
        ("result.txt", 'attachment; filename="result.txt"'),
        ("报告.txt", "attachment; filename*=UTF-8''%E6%8A%A5%E5%91%8A.txt"),
        (
            'report.txt\r\nX-Injected: yes"',
            "attachment; filename*=UTF-8''report.txt%0D%0AX-Injected%3A%20yes%22",
        ),
    ],
)
def test_artifact_attachment_header_is_browser_safe(filename: str, expected: str) -> None:
    assert attachment_content_disposition(filename) == expected


@pytest.mark.asyncio
async def test_download_artifact_streams_local_content_instead_of_file_redirect(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    user = UserContext(user_id="user_1", tenant_id="tenant_1", is_authenticated=True)
    artifact = SimpleNamespace(
        artifact_id="artifact-1",
        session_id="session-1",
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        mime_type="text/plain",
        filename="result.txt",
    )

    class LocalArtifactStorage:
        async def get_artifact(self, artifact_id: str):
            assert artifact_id == artifact.artifact_id
            return artifact

        async def get_presigned_download_url(self, artifact_arg):
            assert artifact_arg is artifact
            return "file:///private/result.txt"

        async def download_artifact(self, artifact_id: str):
            assert artifact_id == artifact.artifact_id
            return b"verified-result"

    monkeypatch.setattr(
        artifact_routes,
        "get_artifact_storage",
        lambda: LocalArtifactStorage(),
    )

    response = await artifact_routes.download_artifact(
        artifact.artifact_id,
        _build_request(AsyncMock()),
        user,
    )
    body = b"".join([chunk async for chunk in response.body_iterator])

    assert response.status_code == 200
    assert body == b"verified-result"
    assert response.headers["content-disposition"] == 'attachment; filename="result.txt"'


@pytest.mark.asyncio
@pytest.mark.parametrize("run_status", ["running", "pending", "queued", "awaiting_approval"])
async def test_cancel_task_interrupts_owning_agent_runtime_turn(run_status: str) -> None:
    task_id = "task-private-identifier"
    user_id = "user-private-identifier"
    reason = "client requested cancellation"
    user = UserContext(user_id=user_id, tenant_id="tenant_1", is_authenticated=True)
    request = _build_request(
        AsyncMock(),
        method="POST",
        path=f"/api/v1/assistant/tasks/{task_id}/cancel",
    )
    database = AsyncMock()
    database.fetchrow.return_value = {
        "run_id": "00000000-0000-0000-0000-000000000001",
        "session_id": "session-private-identifier",
        "harness_thread_id": "thread-private-identifier",
        "harness_turn_id": task_id,
        "status": run_status,
    }
    control = AsyncMock()
    request.app.state.database = database
    request.app.state.agent_runtime_control = control
    response = await run_routes.cancel_task(
        task_id=task_id,
        request=request,
        body=TaskCancelRequest(reason=reason),
        user=user,
    )

    assert response.cancelled is True
    assert response.session_id == "session-private-identifier"
    control.interrupt_turn.assert_awaited_once_with(
        runtime_thread_id="thread-private-identifier",
        turn_id=task_id,
        tenant_id=user.tenant_id,
        user_id=user.user_id,
        session_id="session-private-identifier",
        reason=reason,
    )
