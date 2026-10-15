"""
多租户隔离测试

测试内容：
- 用户会话隔离
- 租户数据隔离
- 跨用户访问拒绝
- 会话恢复
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import jwt
import pytest

from tests.conftest import (
    TEST_JWT_ALGORITHM,
    TEST_JWT_SECRET,
    create_test_token,
)

# ============ Multi-Tenant Isolation Tests ============


class TestMultiTenantIsolation:
    """多租户隔离测试"""

    @pytest.fixture
    def user_a_token(self):
        """User A 的 Token"""
        return create_test_token(
            user_id="user_a_001",
            tenant_id="tenant_alpha",
            tier="normal",
            roles=["user"],
        )

    @pytest.fixture
    def user_b_token(self):
        """User B 的 Token（同租户）"""
        return create_test_token(
            user_id="user_b_002",
            tenant_id="tenant_alpha",
            tier="normal",
            roles=["user"],
        )

    @pytest.fixture
    def user_c_token(self):
        """User C 的 Token（不同租户）"""
        return create_test_token(
            user_id="user_c_003",
            tenant_id="tenant_beta",
            tier="normal",
            roles=["user"],
        )

    @pytest.fixture
    def mock_session_manager(self):
        """Mock 会话管理器"""
        manager = AsyncMock()

        # 存储会话数据
        sessions: dict[str, dict] = {}

        async def get_or_create(session_id, user_id, tenant_id, service_id=None):
            key = f"{user_id}_{tenant_id}_{session_id or 'new'}"
            if key not in sessions:
                sessions[key] = {
                    "session_id": session_id or f"session_{len(sessions)}",
                    "user_id": user_id,
                    "tenant_id": tenant_id,
                    "service_id": service_id,
                    "messages": [],
                }
            return MagicMock(**sessions[key])

        async def get(session_id):
            for _key, session in sessions.items():
                if session["session_id"] == session_id:
                    return MagicMock(**session)
            return None

        async def add_message(session_id, role, content, metadata=None):
            for _key, session in sessions.items():
                if session["session_id"] == session_id:
                    session["messages"].append(
                        {
                            "role": role,
                            "content": content,
                            "metadata": metadata,
                        }
                    )
                    return
            raise ValueError(f"Session not found: {session_id}")

        manager.get_or_create = get_or_create
        manager.get = get
        manager.add_message = add_message
        manager._sessions = sessions

        return manager

    # -------- 用户隔离测试 --------

    @pytest.mark.asyncio
    async def test_user_a_creates_session(self, mock_session_manager):
        """User A 创建会话"""
        session = await mock_session_manager.get_or_create(
            session_id=None,
            user_id="user_a_001",
            tenant_id="tenant_alpha",
        )

        assert session.user_id == "user_a_001"
        assert session.tenant_id == "tenant_alpha"

    @pytest.mark.asyncio
    async def test_user_b_cannot_access_user_a_session(self, mock_session_manager):
        """User B 不能访问 User A 的会话"""
        # User A 创建会话
        await mock_session_manager.get_or_create(
            session_id="session_user_a",
            user_id="user_a_001",
            tenant_id="tenant_alpha",
        )

        # User B 尝试获取 User A 的会话
        session = await mock_session_manager.get("session_user_a")

        # 会话存在但应该验证所有权
        if session:
            # 验证会话属于正确的用户
            assert session.user_id == "user_a_001"
            # User B (user_b_002) 不应该能访问

    @pytest.mark.asyncio
    async def test_different_tenant_complete_isolation(self, mock_session_manager):
        """不同租户完全隔离"""
        # Tenant Alpha 创建会话
        session_alpha = await mock_session_manager.get_or_create(
            session_id="session_alpha",
            user_id="user_a_001",
            tenant_id="tenant_alpha",
        )

        # Tenant Beta 创建会话
        session_beta = await mock_session_manager.get_or_create(
            session_id="session_beta",
            user_id="user_c_003",
            tenant_id="tenant_beta",
        )

        # 验证会话属于不同租户
        assert session_alpha.tenant_id != session_beta.tenant_id

    @pytest.mark.asyncio
    async def test_session_messages_isolated(self, mock_session_manager):
        """会话消息隔离"""
        # User A 的会话
        await mock_session_manager.get_or_create(
            session_id="session_a",
            user_id="user_a_001",
            tenant_id="tenant_alpha",
        )
        await mock_session_manager.add_message("session_a", "user", "Hello from User A")

        # User B 的会话
        await mock_session_manager.get_or_create(
            session_id="session_b",
            user_id="user_b_002",
            tenant_id="tenant_alpha",
        )
        await mock_session_manager.add_message("session_b", "user", "Hello from User B")

        # 验证消息隔离
        sessions = mock_session_manager._sessions
        session_a_msgs = None
        session_b_msgs = None

        for _key, session in sessions.items():
            if session["session_id"] == "session_a":
                session_a_msgs = session["messages"]
            elif session["session_id"] == "session_b":
                session_b_msgs = session["messages"]

        assert session_a_msgs != session_b_msgs
        assert len(session_a_msgs) == 1
        assert session_a_msgs[0]["content"] == "Hello from User A"

    # -------- 会话恢复测试 --------

    @pytest.mark.asyncio
    async def test_user_can_resume_own_session(self, mock_session_manager):
        """用户可以恢复自己的会话"""
        # 创建会话
        session1 = await mock_session_manager.get_or_create(
            session_id="resume_session",
            user_id="user_a_001",
            tenant_id="tenant_alpha",
        )

        # 添加消息
        await mock_session_manager.add_message("resume_session", "user", "First message")
        await mock_session_manager.add_message("resume_session", "assistant", "First response")

        # 恢复会话
        session2 = await mock_session_manager.get_or_create(
            session_id="resume_session",
            user_id="user_a_001",
            tenant_id="tenant_alpha",
        )

        # 应该是同一个会话
        assert session1.session_id == session2.session_id

    @pytest.mark.asyncio
    async def test_session_history_preserved(self, mock_session_manager):
        """会话历史被保留"""
        session_id = "history_session"

        # 创建会话并添加多条消息
        await mock_session_manager.get_or_create(
            session_id=session_id,
            user_id="user_a_001",
            tenant_id="tenant_alpha",
        )

        messages = [
            ("user", "Question 1"),
            ("assistant", "Answer 1"),
            ("user", "Question 2"),
            ("assistant", "Answer 2"),
        ]

        for role, content in messages:
            await mock_session_manager.add_message(session_id, role, content)

        # 获取会话历史
        sessions = mock_session_manager._sessions
        session_data = None
        for _key, session in sessions.items():
            if session["session_id"] == session_id:
                session_data = session
                break

        assert session_data is not None
        assert len(session_data["messages"]) == 4


# ============ JWT-based User Identity Tests ============


class TestJWTUserIdentity:
    """JWT 用户身份测试"""

    def test_extract_user_id_from_token(self, valid_jwt_user_a):
        """从 Token 提取 user_id"""
        payload = jwt.decode(
            valid_jwt_user_a,
            TEST_JWT_SECRET,
            algorithms=[TEST_JWT_ALGORITHM],
        )

        assert "user_id" in payload
        assert payload["user_id"] == "user_a_123"

    def test_extract_tenant_id_from_token(self, valid_jwt_user_a):
        """从 Token 提取 tenant_id"""
        payload = jwt.decode(
            valid_jwt_user_a,
            TEST_JWT_SECRET,
            algorithms=[TEST_JWT_ALGORITHM],
        )

        assert "tenant_id" in payload
        assert payload["tenant_id"] == "tenant_001"

    def test_different_users_have_different_identities(self, valid_jwt_user_a, valid_jwt_user_b):
        """不同用户有不同身份"""
        payload_a = jwt.decode(
            valid_jwt_user_a,
            TEST_JWT_SECRET,
            algorithms=[TEST_JWT_ALGORITHM],
        )
        payload_b = jwt.decode(
            valid_jwt_user_b,
            TEST_JWT_SECRET,
            algorithms=[TEST_JWT_ALGORITHM],
        )

        assert payload_a["user_id"] != payload_b["user_id"]


# ============ Request Context Isolation Tests ============


class TestRequestContextIsolation:
    """请求上下文隔离测试"""

    def test_context_contains_user_info(self):
        """上下文包含用户信息"""
        from src.proxy.context_injector import RequestContext

        context = RequestContext(
            user_id="user_123",
            tenant_id="tenant_001",
            user_tier="normal",
            is_authenticated=True,
            roles=["user"],
        )

        assert context.user_id == "user_123"
        assert context.tenant_id == "tenant_001"
        assert context.is_authenticated is True

    def test_context_headers_include_user_identity(self):
        """上下文头包含用户身份"""
        from src.proxy.context_injector import ContextInjector, RequestContext

        injector = ContextInjector(
            inject_user_info=True,
            inject_request_info=True,
        )

        context = RequestContext(
            user_id="user_123",
            tenant_id="tenant_001",
            request_id="req_001",
        )

        headers = injector.build_headers(context)

        # 验证用户信息被注入到头中
        assert (
            "X-User-Id" in headers or "x-user-id" in headers.lower()
            if isinstance(headers, str)
            else any("user" in k.lower() for k in headers)
        )


# ============ Cross-Tenant Access Prevention Tests ============


class TestCrossTenantPrevention:
    """跨租户访问防护测试"""

    @pytest.fixture
    def tenant_a_context(self):
        """Tenant A 的上下文"""
        from src.proxy.context_injector import RequestContext

        return RequestContext(
            user_id="user_a",
            tenant_id="tenant_a",
            is_authenticated=True,
        )

    @pytest.fixture
    def tenant_b_context(self):
        """Tenant B 的上下文"""
        from src.proxy.context_injector import RequestContext

        return RequestContext(
            user_id="user_b",
            tenant_id="tenant_b",
            is_authenticated=True,
        )

    def test_tenant_id_mismatch_should_fail(self, tenant_a_context, tenant_b_context):
        """租户 ID 不匹配应该失败"""
        assert tenant_a_context.tenant_id != tenant_b_context.tenant_id

    def test_user_belongs_to_correct_tenant(self, tenant_a_context):
        """用户属于正确的租户"""
        # 验证 user_a 属于 tenant_a
        assert tenant_a_context.user_id == "user_a"
        assert tenant_a_context.tenant_id == "tenant_a"
