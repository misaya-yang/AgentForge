"""
Live Assistant API E2E tests.

Run only when a real backend is up:
    RUN_ASSISTANT_API_E2E=1 pytest \
      tests/integration/test_assistant_api_e2e_live.py -q -s
"""

from __future__ import annotations

import contextlib
import io
import json
import os
import time
import zipfile
from typing import Any
from urllib.parse import urlsplit

import httpx
import pytest

API_BASE_URL = os.getenv("ASSISTANT_E2E_BASE_URL", "http://127.0.0.1:8080").rstrip("/")
API_PREFIX = f"{API_BASE_URL}/api/v1"
RUN_LIVE = os.getenv("RUN_ASSISTANT_API_E2E", "0") == "1"
API_HOST = (urlsplit(API_BASE_URL).hostname or "").lower()
HTTPX_TRUST_ENV = API_HOST not in {"127.0.0.1", "::1", "localhost"}

AUTH_EMAIL_DOMAIN = os.getenv("ASSISTANT_E2E_AUTH_EMAIL_DOMAIN", "example.com")
USER1_EMAIL = os.getenv("ASSISTANT_E2E_USER1_EMAIL", f"assistant.e2e.user1@{AUTH_EMAIL_DOMAIN}")
USER2_EMAIL = os.getenv("ASSISTANT_E2E_USER2_EMAIL", f"assistant.e2e.user2@{AUTH_EMAIL_DOMAIN}")
LOGIN_PASSWORD = os.getenv("ASSISTANT_E2E_PASSWORD", "")
USER2_PASSWORD = os.getenv("ASSISTANT_E2E_USER2_PASSWORD", LOGIN_PASSWORD)
MODEL_ID = os.getenv("ASSISTANT_E2E_MODEL_ID", "qwen3.7-plus")
REPETITIONS = int(os.getenv("ASSISTANT_CAPABILITY_REPETITIONS", "3"))
MAX_APPROVAL_ROUNDS = int(os.getenv("ASSISTANT_E2E_MAX_APPROVAL_ROUNDS", "8"))


def _require_live() -> None:
    if not RUN_LIVE:
        pytest.skip("Set RUN_ASSISTANT_API_E2E=1 to run live API validation")
    if not LOGIN_PASSWORD:
        pytest.skip("Set ASSISTANT_E2E_PASSWORD to run live API validation")


def _headers(token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}"}


def _assert_service_ready(client: httpx.Client) -> None:
    resp = client.get(f"{API_BASE_URL}/health")
    assert resp.status_code == 200, resp.text


def _login(client: httpx.Client, email: str, password: str) -> str:
    resp = client.post(
        f"{API_PREFIX}/auth/login",
        json={"email": email, "password": password},
    )
    assert resp.status_code == 200, resp.text
    payload = resp.json()
    token = payload.get("access_token")
    assert token, payload
    return token


def _create_session(client: httpx.Client, token: str) -> str:
    resp = client.post(
        f"{API_PREFIX}/assistant/sessions",
        headers=_headers(token),
        json={},
    )
    assert resp.status_code == 200, resp.text
    payload = resp.json()
    session_id = payload.get("session_id")
    assert session_id, payload
    return session_id


def _visible_tool_names(client: httpx.Client, token: str) -> set[str]:
    response = client.get(
        f"{API_PREFIX}/assistant/tools",
        headers=_headers(token),
    )
    assert response.status_code == 200, response.text
    return {
        str(tool.get("name") or "")
        for tool in response.json().get("tools", [])
        if isinstance(tool, dict)
    }


def _chat(
    client: httpx.Client,
    token: str,
    session_id: str,
    message: str,
    *,
    execution_profile: str = "safe",
    memory_mode: str = "auto",
    os_agent_enabled: bool = False,
    require_non_empty: bool = True,
    max_attempts: int = 2,
) -> dict[str, Any]:
    last_payload: dict[str, Any] | None = None
    for attempt in range(1, max_attempts + 1):
        resp = client.post(
            f"{API_PREFIX}/assistant/chat",
            headers=_headers(token),
            json={
                "session_id": session_id,
                "message": message,
                "model_id": MODEL_ID,
                "execution_profile": execution_profile,
                "memory_mode": memory_mode,
                "os_agent_enabled": os_agent_enabled,
            },
            timeout=180.0,
        )
        assert resp.status_code == 200, resp.text
        payload = resp.json()
        last_payload = payload
        content = payload.get("content")
        if not require_non_empty:
            return payload
        if isinstance(content, str) and content.strip():
            return payload
        if attempt < max_attempts:
            time.sleep(1.2 * attempt)

    assert isinstance(last_payload, dict)
    assert isinstance(last_payload.get("content"), str) and last_payload["content"].strip(), (
        last_payload
    )
    return last_payload


def _stream_chat(
    client: httpx.Client,
    token: str,
    session_id: str,
    message: str,
    *,
    execution_profile: str = "safe",
    memory_mode: str = "auto",
    os_agent_enabled: bool = False,
) -> list[dict[str, Any]]:
    events: list[dict[str, Any]] = []
    decided_approval_ids: set[str] = set()
    with client.stream(
        "POST",
        f"{API_PREFIX}/assistant/chat/stream",
        headers=_headers(token),
        json={
            "session_id": session_id,
            "message": message,
            "model_id": MODEL_ID,
            "execution_profile": execution_profile,
            "memory_mode": memory_mode,
            "os_agent_enabled": os_agent_enabled,
        },
        timeout=240.0,
    ) as resp:
        assert resp.status_code == 200, resp.text
        for line in resp.iter_lines():
            if not line or not line.startswith("data: "):
                continue
            raw = line[6:].strip()
            if not raw:
                continue
            with contextlib.suppress(json.JSONDecodeError):
                evt = json.loads(raw)
                events.append(evt)
                if evt.get("event_type") == "approval_required":
                    approval_data = evt.get("data")
                    assert isinstance(approval_data, dict), evt
                    approval_id = str(approval_data.get("approval_id") or "")
                    run_id = str(approval_data.get("run_id") or "")
                    assert approval_id and run_id, approval_data
                    if approval_id not in decided_approval_ids:
                        if len(decided_approval_ids) >= MAX_APPROVAL_ROUNDS:
                            raise AssertionError(
                                "approval rounds exceeded "
                                "ASSISTANT_E2E_MAX_APPROVAL_ROUNDS="
                                f"{MAX_APPROVAL_ROUNDS}"
                            )
                        _approve_runtime_event(client, token, evt)
                        decided_approval_ids.add(approval_id)
                # ``done`` closes model transport output, but the canonical
                # run still has persistence/finalization work to perform.  Do
                # not disconnect until the authoritative run terminal arrives.
                if evt.get("event_type") in {"run_finished", "run_error"}:
                    break
    return events


def _approve_runtime_event(
    client: httpx.Client,
    token: str,
    event: dict[str, Any],
) -> None:
    approval_data = event.get("data")
    assert isinstance(approval_data, dict), event
    approval_id = str(approval_data.get("approval_id") or "")
    run_id = str(approval_data.get("run_id") or "")
    thread_id = str(approval_data.get("thread_id") or "")
    assert approval_id and run_id, approval_data

    if thread_id:
        decision_url = (
            f"{API_BASE_URL}/api/v2/agent/threads/{thread_id}/approvals/{approval_id}/decision"
        )
    else:
        decision_url = f"{API_PREFIX}/assistant/approvals/{approval_id}"
    approval_response = client.post(
        decision_url,
        headers=_headers(token),
        json={"approved": True, "reason": "result-level live capability check"},
    )
    assert approval_response.status_code == 200, approval_response.text
    approval = approval_response.json().get("approval")
    assert isinstance(approval, dict), approval_response.json()
    assert str(approval.get("approval_id") or approval_id) == approval_id, approval
    assert approval.get("approved") is True, approval
    assert approval.get("status") in {"approved", "consumed"}, approval


def _extract_run_id(events: list[dict[str, Any]]) -> str | None:
    for evt in events:
        if evt.get("event_type") == "run_started":
            data = evt.get("data") or {}
            run_id = data.get("run_id")
            if run_id:
                return str(run_id)
    return None


def _stream_text(events: list[dict[str, Any]]) -> str:
    chunks: list[str] = []
    for event in events:
        if event.get("event_type") not in {"text_delta", "text_message_content"}:
            continue
        data = event.get("data")
        if isinstance(data, str):
            chunks.append(data)
            continue
        if not isinstance(data, dict):
            continue
        for key in ("content", "message", "delta"):
            value = data.get(key)
            if isinstance(value, str):
                chunks.append(value)
                break
    return "".join(chunks)


def _session_artifacts(
    client: httpx.Client,
    token: str,
    session_id: str,
) -> list[dict[str, Any]]:
    response = client.get(
        f"{API_PREFIX}/assistant/sessions/{session_id}/artifacts",
        headers=_headers(token),
    )
    assert response.status_code == 200, response.text
    payload = response.json()
    artifacts = payload.get("artifacts")
    assert isinstance(artifacts, list), payload
    return [artifact for artifact in artifacts if isinstance(artifact, dict)]


def _download_artifact(
    client: httpx.Client,
    token: str,
    artifact_id: str,
) -> bytes:
    response = client.get(
        f"{API_PREFIX}/assistant/artifacts/{artifact_id}/download",
        headers=_headers(token),
        timeout=120.0,
        follow_redirects=True,
    )
    assert response.status_code == 200, response.text
    assert response.content, response.headers
    return response.content


def _new_artifacts(
    before: list[dict[str, Any]],
    after: list[dict[str, Any]],
) -> list[dict[str, Any]]:
    previous_ids = {str(artifact.get("artifact_id")) for artifact in before}
    return [
        artifact
        for artifact in after
        if artifact.get("artifact_id") and str(artifact["artifact_id"]) not in previous_ids
    ]


def _wait_for_new_artifacts(
    client: httpx.Client,
    token: str,
    session_id: str,
    before: list[dict[str, Any]],
    *,
    timeout_seconds: float = 10.0,
) -> list[dict[str, Any]]:
    deadline = time.monotonic() + timeout_seconds
    while time.monotonic() < deadline:
        artifacts = _new_artifacts(
            before,
            _session_artifacts(client, token, session_id),
        )
        if artifacts:
            return artifacts
        time.sleep(0.25)
    return []


def _assert_valid_document(data: bytes, artifact: dict[str, Any]) -> None:
    filename = str(artifact.get("filename") or "").lower()
    mime_type = str(artifact.get("mime_type") or "").lower()
    if filename.endswith(".pdf") or mime_type == "application/pdf":
        assert data.startswith(b"%PDF-"), artifact
        assert b"%%EOF" in data[-2048:], artifact
        return
    if filename.endswith(".docx") or "wordprocessingml" in mime_type:
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            names = set(archive.namelist())
            assert "[Content_Types].xml" in names, artifact
            assert "word/document.xml" in names, artifact
        return
    raise AssertionError(f"Expected a PDF or DOCX artifact, got {artifact!r}")


def _assert_valid_image(data: bytes, artifact: dict[str, Any]) -> None:
    filename = str(artifact.get("filename") or "").lower()
    mime_type = str(artifact.get("mime_type") or "").lower()
    if filename.endswith(".png") or mime_type == "image/png":
        assert data.startswith(b"\x89PNG\r\n\x1a\n"), artifact
        return
    if filename.endswith((".jpg", ".jpeg")) or mime_type == "image/jpeg":
        assert data.startswith(b"\xff\xd8") and data.endswith(b"\xff\xd9"), artifact
        return
    if filename.endswith(".webp") or mime_type == "image/webp":
        assert data.startswith(b"RIFF") and data[8:12] == b"WEBP", artifact
        return
    raise AssertionError(f"Expected a PNG, JPEG, or WebP artifact, got {artifact!r}")


def _has_any_event(events: list[dict[str, Any]], *event_types: str) -> bool:
    names = {evt.get("event_type") for evt in events}
    return any(name in names for name in event_types)


def _event_data(events: list[dict[str, Any]], event_type: str) -> dict[str, Any]:
    event = next((item for item in events if item.get("event_type") == event_type), None)
    assert event is not None, events
    data = event.get("data")
    assert isinstance(data, dict), event
    return data


def _assert_no_event(events: list[dict[str, Any]], event_type: str) -> None:
    assert all(evt.get("event_type") != event_type for evt in events), events


def _stream_chat_until_success(
    client: httpx.Client,
    token: str,
    session_id: str,
    message: str,
    *,
    execution_profile: str = "safe",
    memory_mode: str = "auto",
    os_agent_enabled: bool = False,
    max_attempts: int = 3,
) -> list[dict[str, Any]]:
    """
    Retry streaming when provider/network returns transient run_error.

    This keeps live E2E strict on behavior but resilient to sporadic upstream failures.
    """
    last_events: list[dict[str, Any]] = []
    for attempt in range(1, max_attempts + 1):
        last_events = _stream_chat(
            client=client,
            token=token,
            session_id=session_id,
            message=message,
            execution_profile=execution_profile,
            memory_mode=memory_mode,
            os_agent_enabled=os_agent_enabled,
        )
        has_content_or_tool = _has_any_event(last_events, "text_delta", "tool_call_result")
        has_run_error = _has_any_event(last_events, "run_error")
        has_run_finished = _has_any_event(last_events, "run_finished")
        if has_content_or_tool and has_run_finished and not has_run_error:
            return last_events
        if attempt < max_attempts:
            time.sleep(1.5 * attempt)
    return last_events


@pytest.mark.integration
def test_assistant_code_executor_live():
    """Prove the Assistant tool loop executes code and persists its exact artifact."""
    _require_live()
    assert 1 <= MAX_APPROVAL_ROUNDS <= 32

    with httpx.Client(timeout=60.0, trust_env=HTTPX_TRUST_ENV) as client:
        _assert_service_ready(client)
        token = _login(client, USER1_EMAIL, LOGIN_PASSWORD)
        session_id = _create_session(client, token)
        assert "execute_python_code" in _visible_tool_names(client, token)

        marker = f"CODE-EXEC-LIVE-{time.time_ns()}"
        artifacts_before = _session_artifacts(client, token, session_id)
        events = _stream_chat_until_success(
            client,
            token,
            session_id,
            (
                "必须实际调用 execute_python_code 工具运行 Python，不要只给代码。"
                f"把唯一一行 {marker} 打印到 stdout，并将同一内容写入 "
                "/workspace/output/capability-result.txt。"
            ),
            execution_profile="balanced",
            os_agent_enabled=True,
            max_attempts=1,
        )

        _assert_no_event(events, "run_error")
        assert _has_any_event(events, "tool_call_start", "tool_call_result")
        execution_result = _event_data(events, "code_execution_result")
        assert execution_result.get("success") is True, execution_result
        assert execution_result.get("exit_code") == 0, execution_result
        assert marker in str(execution_result.get("result") or ""), execution_result
        artifacts = _wait_for_new_artifacts(
            client,
            token,
            session_id,
            artifacts_before,
        )
        artifact = next(
            (
                item
                for item in artifacts
                if str(item.get("filename") or "").endswith("capability-result.txt")
            ),
            None,
        )
        assert artifact is not None, artifacts
        content = _download_artifact(client, token, str(artifact["artifact_id"]))
        assert content.decode("utf-8").strip() == marker


@pytest.mark.integration
def test_assistant_docgen_plugin_live():
    """Verify the bundled Agent Plugin creates a downloadable document."""

    _require_live()
    with httpx.Client(timeout=60.0, trust_env=HTTPX_TRUST_ENV) as client:
        _assert_service_ready(client)
        token = _login(client, USER1_EMAIL, LOGIN_PASSWORD)
        session_id = _create_session(client, token)

        tool_names = _visible_tool_names(client, token)
        assert "mcp_docgen__generate_document" in tool_names, tool_names

        artifacts_before = _session_artifacts(client, token, session_id)
        events = _stream_chat_until_success(
            client,
            token,
            session_id,
            (
                "请实际调用文档生成工具创建一份两节的项目周报 PDF 或 DOCX，"
                "标题为 Capability Weekly Report；必须生成可下载文件，不要只返回正文草稿。"
            ),
            execution_profile="balanced",
            max_attempts=1,
        )
        _assert_no_event(events, "run_error")
        assert _has_any_event(events, "tool_call_start", "tool_call_result")
        assert _has_any_event(events, "document_generation_result", "artifact_created")

        artifacts = _wait_for_new_artifacts(
            client,
            token,
            session_id,
            artifacts_before,
        )
        document = next(
            (
                artifact
                for artifact in artifacts
                if str(artifact.get("filename") or "").lower().endswith((".pdf", ".docx"))
            ),
            None,
        )
        assert document is not None, artifacts
        document_bytes = _download_artifact(client, token, str(document["artifact_id"]))
        _assert_valid_document(document_bytes, document)


@pytest.mark.integration
def test_assistant_image_generation_live():
    """Verify image generation returns a persisted, decodable image artifact."""

    _require_live()
    with httpx.Client(timeout=60.0, trust_env=HTTPX_TRUST_ENV) as client:
        _assert_service_ready(client)
        token = _login(client, USER1_EMAIL, LOGIN_PASSWORD)
        session_id = _create_session(client, token)

        assert "generate_image" in _visible_tool_names(client, token)
        artifacts_before = _session_artifacts(client, token, session_id)
        events = _stream_chat_until_success(
            client,
            token,
            session_id,
            (
                "请实际调用图像生成工具生成一张 1024x1024 的极简蓝色圆形图标，"
                "纯白背景；必须生成可下载图片，不要只描述画面。"
            ),
            execution_profile="balanced",
            max_attempts=1,
        )
        _assert_no_event(events, "run_error")
        assert _has_any_event(events, "tool_call_start", "tool_call_result")
        assert _has_any_event(events, "image_generation_result", "artifact_created")

        artifacts = _wait_for_new_artifacts(
            client,
            token,
            session_id,
            artifacts_before,
            timeout_seconds=30.0,
        )
        image = next(
            (
                artifact
                for artifact in artifacts
                if str(artifact.get("mime_type") or "").startswith("image/")
                or str(artifact.get("filename") or "")
                .lower()
                .endswith((".png", ".jpg", ".jpeg", ".webp"))
            ),
            None,
        )
        assert image is not None, artifacts
        image_bytes = _download_artifact(client, token, str(image["artifact_id"]))
        _assert_valid_image(image_bytes, image)


@pytest.mark.integration
def test_assistant_quiz_generation_live():
    """Verify the model invokes Quiz and emits its structured ready receipt."""

    _require_live()
    with httpx.Client(timeout=60.0, trust_env=HTTPX_TRUST_ENV) as client:
        _assert_service_ready(client)
        token = _login(client, USER1_EMAIL, LOGIN_PASSWORD)
        session_id = _create_session(client, token)

        assert "generate_quiz" in _visible_tool_names(client, token)
        events = _stream_chat_until_success(
            client,
            token,
            session_id,
            (
                "请实际调用 Quiz 工具，创建一份两题的 Python 基础单选测验；"
                "每题四个选项，必须返回可交互 quiz，不要只输出题目正文。"
            ),
            execution_profile="balanced",
            max_attempts=1,
        )
        _assert_no_event(events, "run_error")
        assert _has_any_event(events, "tool_call_start", "tool_call_result")
        quiz = _event_data(events, "quiz:ready")
        assert quiz.get("quiz_id"), quiz
        assert len(quiz.get("questions") or []) == 2, quiz


@pytest.mark.integration
@pytest.mark.parametrize("trial", range(1, REPETITIONS + 1))
def test_assistant_api_e2e_live_dialogues(trial: int):
    """
    Comprehensive API-level dialogue validation.

    Covers:
    - code generation/execution path
    - image generation path
    - document/pdf generation path
    - context continuity across turns
    - multi-user isolation
    - gateway policy + run tracking endpoints
    """
    _require_live()
    assert 1 <= REPETITIONS <= 10

    with httpx.Client(timeout=60.0, trust_env=HTTPX_TRUST_ENV) as client:
        _assert_service_ready(client)

        # Login/create two users
        token1 = _login(client, USER1_EMAIL, LOGIN_PASSWORD)
        token2 = _login(client, USER2_EMAIL, USER2_PASSWORD)

        session1 = _create_session(client, token1)
        session2 = _create_session(client, token2)
        context_marker = f"CTX-{trial}-{time.time_ns()}"

        # Multi-user isolation: user2 cannot read user1 session
        isolation_resp = client.get(
            f"{API_PREFIX}/assistant/sessions/{session1}",
            headers=_headers(token2),
        )
        assert isolation_resp.status_code == 404, isolation_resp.text

        # Policy endpoint
        policies_resp = client.get(
            f"{API_PREFIX}/assistant/policies",
            headers=_headers(token1),
        )
        assert policies_resp.status_code == 200, policies_resp.text
        policies = policies_resp.json().get("policies", {})
        assert "default_execution_profile" in policies

        tool_names = _visible_tool_names(client, token1)
        if "execute_python_code" not in tool_names:
            pytest.skip(
                "ASSISTANT_CODE_EXECUTOR_ENABLED is off; run the dedicated docgen "
                "live test for the default quickstart capability"
            )

        # Stream: execute code, persist an exact output artifact, and retain a
        # cross-turn marker. A non-empty prose response is not capability proof.
        code_marker = f"CODE-OK-{trial}-{time.time_ns()}"
        artifacts_before_code = _session_artifacts(client, token1, session1)
        events = _stream_chat_until_success(
            client,
            token1,
            session1,
            (
                "请实际调用代码执行工具运行 Python：把唯一一行 "
                f"{code_marker} 打印到 stdout，并写入 "
                "/workspace/output/capability-result.txt。"
                f"同时记住一个项目事实：我的项目代号是 {context_marker}。"
                "不要只给代码示例。"
            ),
            execution_profile="balanced",
            os_agent_enabled=True,
            max_attempts=1,
        )
        assert _has_any_event(events, "tool_call_start", "tool_call_result")
        if not _has_any_event(events, "code_execution_result", "artifact_created"):
            raise AssertionError(
                "event_types=" + ",".join(str(event.get("event_type") or "") for event in events)
            )
        assert _has_any_event(events, "context_budget", "gateway_decision")
        assert _has_any_event(events, "run_started")
        assert _has_any_event(events, "done", "run_finished")
        _assert_no_event(events, "run_error")
        execution_result = _event_data(events, "code_execution_result")
        assert execution_result.get("success") is True, execution_result
        assert execution_result.get("exit_code") == 0, execution_result
        assert code_marker in str(execution_result.get("result") or ""), execution_result
        code_artifacts = _wait_for_new_artifacts(
            client,
            token1,
            session1,
            artifacts_before_code,
        )
        code_artifact = next(
            (
                artifact
                for artifact in code_artifacts
                if str(artifact.get("filename") or "").endswith("capability-result.txt")
            ),
            None,
        )
        assert code_artifact is not None, code_artifacts
        code_bytes = _download_artifact(client, token1, str(code_artifact["artifact_id"]))
        assert code_bytes.decode("utf-8").strip() == code_marker

        run_id = _extract_run_id(events)
        if run_id:
            run_resp = client.get(
                f"{API_PREFIX}/assistant/runs/{run_id}",
                headers=_headers(token1),
            )
            if policies.get("gateway_enabled", False):
                assert run_resp.status_code == 200, run_resp.text
                run_payload = run_resp.json().get("run", {})
                assert str(run_payload.get("run_id")) == run_id
            else:
                # Gateway disabled: run lifecycle persistence API can return 404.
                assert run_resp.status_code in {200, 404}, run_resp.text

        # Generate a real PDF/DOCX artifact and verify its container bytes.
        artifacts_before_doc = _session_artifacts(client, token1, session1)
        doc_events = _stream_chat_until_success(
            client,
            token1,
            session1,
            (
                "请实际调用文档生成工具创建一份两节的项目周报 PDF 或 DOCX，"
                "标题为 Capability Weekly Report；必须生成可下载文件，不要只返回正文草稿。"
            ),
            execution_profile="balanced",
            max_attempts=1,
        )
        _assert_no_event(doc_events, "run_error")
        assert _has_any_event(doc_events, "document_generation_result", "artifact_created")
        document_artifacts = _wait_for_new_artifacts(
            client,
            token1,
            session1,
            artifacts_before_doc,
        )
        document_artifact = next(
            (
                artifact
                for artifact in document_artifacts
                if str(artifact.get("filename") or "").lower().endswith((".pdf", ".docx"))
            ),
            None,
        )
        assert document_artifact is not None, document_artifacts
        document_bytes = _download_artifact(
            client,
            token1,
            str(document_artifact["artifact_id"]),
        )
        _assert_valid_document(document_bytes, document_artifact)

        # Second same-session distractor: marker recall must not depend on
        # immediate adjacency to the turn that introduced it.
        distractor = _chat(
            client,
            token1,
            session1,
            "计算 17×19，只回复十进制整数。",
            execution_profile="balanced",
            memory_mode="strict",
        )
        assert str(distractor.get("content") or "").strip() == "323", distractor

        # Context continuity validation (same session follow-up)
        follow_events = _stream_chat_until_success(
            client,
            token1,
            session1,
            "请只回复我最早告诉你的项目代号，不要引号、代码块或其他文字。",
            execution_profile="balanced",
            memory_mode="strict",
        )
        assert _has_any_event(follow_events, "text_delta", "done", "run_finished")
        _assert_no_event(follow_events, "run_error")
        assert _stream_text(follow_events).strip() == context_marker, follow_events

        history_resp = client.get(
            f"{API_PREFIX}/assistant/sessions/{session1}/history?limit=50",
            headers=_headers(token1),
        )
        assert history_resp.status_code == 200, history_resp.text
        history_payload = history_resp.json()
        messages = history_payload.get("messages", [])
        assert history_payload.get("total", 0) >= 4
        assert any(context_marker in (m.get("content") or "") for m in messages), messages

        # A corrected project fact supersedes the original value after another
        # unrelated turn; stale-value reuse is a failure.
        latest_marker = f"{context_marker}-LATEST"
        update_result = _chat(
            client,
            token1,
            session1,
            (
                f"更正一个项目事实：我的项目代号已经改为 {latest_marker}；"
                "此前告诉你的旧项目代号已作废。只回复 OK。"
            ),
            execution_profile="balanced",
            memory_mode="strict",
        )
        assert str(update_result.get("content") or "").strip().upper() == "OK", update_result
        second_distractor = _chat(
            client,
            token1,
            session1,
            "法国首都是什么？只回复城市英文名。",
            execution_profile="balanced",
            memory_mode="strict",
        )
        assert str(second_distractor.get("content") or "").strip().casefold() == "paris", (
            second_distractor
        )
        latest_events = _stream_chat_until_success(
            client,
            token1,
            session1,
            "只回复我当前的项目代号，不要引号、代码块或其他文字。",
            execution_profile="balanced",
            memory_mode="strict",
        )
        _assert_no_event(latest_events, "run_error")
        assert _stream_text(latest_events).strip() == latest_marker, latest_events

        # User2 independent session should still work
        user2_result = _chat(
            client,
            token2,
            session2,
            "请给我三条今天可执行的学习计划。",
        )
        user2_text = str(user2_result.get("content") or "")
        assert all(str(index) in user2_text for index in (1, 2, 3)), user2_result

        # Approval endpoint contract (non-existing id should return 404)
        approval_resp = client.post(
            f"{API_PREFIX}/assistant/approvals/{'00000000-0000-0000-0000-000000000000'}",
            headers=_headers(token1),
            json={"approved": True, "reason": "integration contract check"},
        )
        assert approval_resp.status_code == 404, approval_resp.text


if __name__ == "__main__":
    # Manual debug run support:
    # RUN_ASSISTANT_API_E2E=1 pytest \
    #   tests/integration/test_assistant_api_e2e_live.py -q -s
    start = time.time()
    code = pytest.main([__file__, "-q", "-s"])
    print(f"Finished in {(time.time() - start):.1f}s, code={code}")
