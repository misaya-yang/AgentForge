"""
ACL 权限管理单元测试

测试范围：
1. Agent/Assistant 权限隔离
2. 知识库 ACL 强制执行
3. Confluence 资源访问控制

测试账号：
- user_alice: 普通用户，资源创建者
- user_bob: 普通用户，非创建者
- admin_user: 管理员用户
"""

from datetime import datetime
from unittest.mock import AsyncMock, MagicMock

import pytest

# AuthenticationRequiredError is now raised by the kb-service modules under test
# (kb-service ships standalone and does not depend on ai_gateway_core, so it has
# its own copy of the exception class). See plans/kb-fork-merge-report.md.
from knowledge_service.core.exceptions import AuthenticationRequiredError

from src.core.auth.user_resolver import UserContext


def _rbac_has_permission(subjects, permission: str) -> bool:
    values = {str(item) for item in (subjects or []) if item is not None}
    if "admin:*" in values:
        return True
    if permission in values:
        return True
    parts = permission.split(":")
    for idx in range(len(parts) - 1, 0, -1):
        wildcard = ":".join(parts[:idx]) + ":*"
        if wildcard in values:
            return True
    return False


# ============ 测试账号 Fixtures ============


@pytest.fixture
def user_alice():
    """User Alice - 普通用户，资源创建者"""
    return UserContext(
        user_id="user_alice_001",
        tenant_id="tenant_test",
        tier="normal",
        is_authenticated=True,
        roles=["user", "developer"],
    )


@pytest.fixture
def user_bob():
    """User Bob - 普通用户，非创建者"""
    return UserContext(
        user_id="user_bob_002",
        tenant_id="tenant_test",
        tier="normal",
        is_authenticated=True,
        roles=["user"],
    )


@pytest.fixture
def admin_user():
    """Admin User - 管理员用户"""
    return UserContext(
        user_id="admin_001",
        tenant_id="tenant_test",
        tier="admin",
        is_authenticated=True,
        roles=["user", "admin"],
    )


@pytest.fixture
def guest_user():
    """Guest User - 未认证用户"""
    return UserContext(
        user_id="guest_anonymous",
        tenant_id="",
        tier="anonymous",
        is_authenticated=False,
        roles=[],
    )


# ============ Agent/Assistant 权限测试 ============


class TestAssistantOwnership:
    """Assistant 所有权验证测试"""

    @pytest.fixture
    def mock_proxy(self):
        """创建 Mock LangGraphProxy"""
        from src.adapters.langgraph_proxy import (
            LangGraphInstance,
            LangGraphLoadBalancer,
            LangGraphProxy,
            LoadBalancerConfig,
        )

        # 创建最小化的 LoadBalancer
        config = LoadBalancerConfig(
            strategy="round_robin",
            instances=[
                LangGraphInstance(
                    instance_id="test-1",
                    url="http://localhost:8123",
                )
            ],
        )
        lb = LangGraphLoadBalancer(config)
        proxy = LangGraphProxy(load_balancer=lb)
        return proxy

    def test_verify_assistant_ownership_creator_allowed(self, mock_proxy, user_alice):
        """测试：创建者可以访问自己的 Assistant"""
        assistant = {
            "assistant_id": "asst_001",
            "metadata": {
                "created_by": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        # 不应抛出异常
        mock_proxy._verify_assistant_ownership(assistant, user_alice, require_write=False)
        mock_proxy._verify_assistant_ownership(assistant, user_alice, require_write=True)

    def test_verify_assistant_ownership_non_creator_denied(self, mock_proxy, user_alice, user_bob):
        """测试：非创建者无法访问私有 Assistant"""
        from src.adapters.langgraph_proxy import AssistantAccessDeniedError

        assistant = {
            "assistant_id": "asst_001",
            "metadata": {
                "created_by": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        # Bob 尝试访问 Alice 的 Assistant - 应被拒绝
        with pytest.raises(AssistantAccessDeniedError):
            mock_proxy._verify_assistant_ownership(assistant, user_bob, require_write=False)

    def test_verify_assistant_ownership_admin_allowed(self, mock_proxy, user_alice, admin_user):
        """测试：管理员可以访问任何 Assistant"""
        assistant = {
            "assistant_id": "asst_001",
            "metadata": {
                "created_by": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        # Admin 应该可以访问
        mock_proxy._verify_assistant_ownership(assistant, admin_user, require_write=False)
        mock_proxy._verify_assistant_ownership(assistant, admin_user, require_write=True)

    def test_verify_assistant_ownership_shared_user_read_only(
        self, mock_proxy, user_alice, user_bob
    ):
        """测试：共享用户只能读取，不能写入"""
        from src.adapters.langgraph_proxy import AssistantAccessDeniedError

        assistant = {
            "assistant_id": "asst_001",
            "metadata": {
                "created_by": "user_alice_001",
                "tenant_id": "tenant_test",
                "shared_with": ["user_bob_002"],  # Bob 在共享列表中
            },
        }

        # Bob 可以读取
        mock_proxy._verify_assistant_ownership(assistant, user_bob, require_write=False)

        # Bob 不能写入
        with pytest.raises(AssistantAccessDeniedError):
            mock_proxy._verify_assistant_ownership(assistant, user_bob, require_write=True)

    def test_verify_assistant_ownership_public_assistant(self, mock_proxy, user_alice, user_bob):
        """测试：公开的 Assistant 所有人可读"""
        from src.adapters.langgraph_proxy import AssistantAccessDeniedError

        assistant = {
            "assistant_id": "asst_public",
            "metadata": {
                "created_by": "user_alice_001",
                "tenant_id": "tenant_test",
                "is_public": True,
            },
        }

        # 任何人都可以读取公开的 Assistant
        mock_proxy._verify_assistant_ownership(assistant, user_bob, require_write=False)

        # 但只有创建者可以写入
        with pytest.raises(AssistantAccessDeniedError):
            mock_proxy._verify_assistant_ownership(assistant, user_bob, require_write=True)


class TestThreadOwnership:
    """Thread 所有权验证测试"""

    @pytest.fixture
    def mock_proxy(self):
        """创建 Mock LangGraphProxy"""
        from src.adapters.langgraph_proxy import (
            LangGraphInstance,
            LangGraphLoadBalancer,
            LangGraphProxy,
            LoadBalancerConfig,
        )

        config = LoadBalancerConfig(
            strategy="round_robin",
            instances=[
                LangGraphInstance(
                    instance_id="test-1",
                    url="http://localhost:8123",
                )
            ],
        )
        lb = LangGraphLoadBalancer(config)
        proxy = LangGraphProxy(load_balancer=lb)
        return proxy

    def test_verify_thread_ownership_owner_allowed(self, mock_proxy, user_alice):
        """测试：Thread 所有者可以访问"""
        thread = {
            "thread_id": "thread_001",
            "metadata": {
                "owner_id": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        # 不应抛出异常
        mock_proxy._verify_ownership(thread, user_alice)

    def test_verify_thread_ownership_non_owner_denied(self, mock_proxy, user_alice, user_bob):
        """测试：非所有者无法访问 Thread"""
        from src.adapters.langgraph_proxy import ForbiddenError

        thread = {
            "thread_id": "thread_001",
            "metadata": {
                "owner_id": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        # Bob 尝试访问 Alice 的 Thread - 应被拒绝
        with pytest.raises(ForbiddenError):
            mock_proxy._verify_ownership(thread, user_bob)

    def test_verify_thread_ownership_admin_allowed(self, mock_proxy, user_alice, admin_user):
        """测试：管理员可以访问任何 Thread"""
        thread = {
            "thread_id": "thread_001",
            "metadata": {
                "owner_id": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        # Admin 应该可以访问
        mock_proxy._verify_ownership(thread, admin_user)


# ============ 知识库 ACL 测试 ============


class TestKnowledgeRetrieverACL:
    """知识库检索器 ACL 测试"""

    def test_retriever_requires_user_context(self):
        """测试：KnowledgeRetriever 必须提供 user_context"""
        from knowledge_service.services.knowledge.langgraph_tools import KnowledgeRetriever

        mock_kb_service = MagicMock()

        # None user_context 应抛出异常
        with pytest.raises(AuthenticationRequiredError) as exc_info:
            KnowledgeRetriever(
                knowledge_service=mock_kb_service,
                dataset_id="test_dataset",
                user_context=None,
            )
        assert "User context is required" in str(exc_info.value)

    def test_retriever_requires_authenticated_user(self, guest_user):
        """测试：KnowledgeRetriever 必须是已认证用户"""
        from knowledge_service.services.knowledge.langgraph_tools import KnowledgeRetriever

        mock_kb_service = MagicMock()

        # 未认证用户应抛出异常
        with pytest.raises(AuthenticationRequiredError) as exc_info:
            KnowledgeRetriever(
                knowledge_service=mock_kb_service,
                dataset_id="test_dataset",
                user_context=guest_user,
            )
        assert "Authenticated user context is required" in str(exc_info.value)

    def test_retriever_accepts_authenticated_user(self, user_alice):
        """测试：已认证用户可以创建 Retriever"""
        from knowledge_service.services.knowledge.langgraph_tools import KnowledgeRetriever

        mock_kb_service = MagicMock()

        # 已认证用户应成功
        retriever = KnowledgeRetriever(
            knowledge_service=mock_kb_service,
            dataset_id="test_dataset",
            user_context=user_alice,
        )
        assert retriever.user_context == user_alice
        assert retriever.dataset_id == "test_dataset"


class TestMultiDatasetRetrieverACL:
    """多数据集检索器 ACL 测试"""

    def test_multi_retriever_requires_user_context(self):
        """测试：MultiDatasetRetriever 必须提供 user_context"""
        from knowledge_service.services.knowledge.langgraph_tools import MultiDatasetRetriever

        mock_kb_service = MagicMock()

        with pytest.raises(AuthenticationRequiredError):
            MultiDatasetRetriever(
                knowledge_service=mock_kb_service,
                dataset_ids=["ds1", "ds2"],
                user_context=None,
            )

    def test_multi_retriever_requires_authenticated_user(self, guest_user):
        """测试：MultiDatasetRetriever 必须是已认证用户"""
        from knowledge_service.services.knowledge.langgraph_tools import MultiDatasetRetriever

        mock_kb_service = MagicMock()

        with pytest.raises(AuthenticationRequiredError):
            MultiDatasetRetriever(
                knowledge_service=mock_kb_service,
                dataset_ids=["ds1", "ds2"],
                user_context=guest_user,
            )

    def test_multi_retriever_accepts_authenticated_user(self, user_alice):
        """测试：已认证用户可以创建 MultiDatasetRetriever"""
        from knowledge_service.services.knowledge.langgraph_tools import MultiDatasetRetriever

        mock_kb_service = MagicMock()

        retriever = MultiDatasetRetriever(
            knowledge_service=mock_kb_service,
            dataset_ids=["ds1", "ds2"],
            user_context=user_alice,
        )
        assert retriever.user_context == user_alice
        assert len(retriever.dataset_ids) == 2


class TestDifyCompatibleKBAPIACL:
    """Dify 兼容 KB API ACL 测试"""

    def test_dify_api_requires_user_context(self):
        """测试：DifyCompatibleKBAPI 必须提供 user_context"""
        from knowledge_service.services.knowledge.langgraph_tools import DifyCompatibleKBAPI

        mock_kb_service = MagicMock()

        with pytest.raises(AuthenticationRequiredError):
            DifyCompatibleKBAPI(
                knowledge_service=mock_kb_service,
                user_context=None,
            )

    def test_dify_api_requires_authenticated_user(self, guest_user):
        """测试：DifyCompatibleKBAPI 必须是已认证用户"""
        from knowledge_service.services.knowledge.langgraph_tools import DifyCompatibleKBAPI

        mock_kb_service = MagicMock()

        with pytest.raises(AuthenticationRequiredError):
            DifyCompatibleKBAPI(
                knowledge_service=mock_kb_service,
                user_context=guest_user,
            )

    def test_dify_api_accepts_authenticated_user(self, user_alice):
        """测试：已认证用户可以创建 DifyCompatibleKBAPI"""
        from knowledge_service.services.knowledge.langgraph_tools import DifyCompatibleKBAPI

        mock_kb_service = MagicMock()

        api = DifyCompatibleKBAPI(
            knowledge_service=mock_kb_service,
            user_context=user_alice,
        )
        assert api.user_context == user_alice


class TestCreateRetrievalToolACL:
    """create_retrieval_tool 工厂函数 ACL 测试"""

    def test_create_tool_requires_user_context(self):
        """测试：create_retrieval_tool 必须提供 user_context"""
        from knowledge_service.services.knowledge.langgraph_tools import create_retrieval_tool

        mock_kb_service = MagicMock()

        with pytest.raises(AuthenticationRequiredError):
            create_retrieval_tool(
                knowledge_service=mock_kb_service,
                dataset_id="test_dataset",
                user_context=None,
            )

    def test_create_tool_accepts_authenticated_user(self, user_alice):
        """测试：已认证用户可以创建 retrieval tool"""
        from knowledge_service.services.knowledge.langgraph_tools import create_retrieval_tool

        mock_kb_service = MagicMock()

        tool = create_retrieval_tool(
            knowledge_service=mock_kb_service,
            dataset_id="test_dataset",
            user_context=user_alice,
        )
        assert callable(tool)


# ============ Confluence ACL 测试 ============


class TestConfluenceAccessControl:
    """Confluence 资源访问控制测试"""

    @pytest.fixture
    def mock_sync_service(self):
        """创建 Mock ConfluenceSyncService"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceSyncService,
        )

        mock_settings = MagicMock()
        mock_settings.confluence.request_timeout = 30
        mock_settings.confluence.max_retries = 3

        mock_db = AsyncMock()
        mock_kb_service = MagicMock()

        service = ConfluenceSyncService(
            settings=mock_settings,
            database=mock_db,
            knowledge_service=mock_kb_service,
        )
        return service

    def test_verify_confluence_access_owner_allowed(self, mock_sync_service, user_alice):
        """测试：资源所有者可以访问"""
        resource = {
            "connection_id": "conn_001",
            "owner_id": "user_alice_001",
            "created_by": "user_alice_001",
        }

        # 不应抛出异常
        mock_sync_service._verify_confluence_access(resource, "connection", user_alice)

    def test_verify_confluence_access_non_owner_denied(
        self, mock_sync_service, user_alice, user_bob
    ):
        """测试：非所有者无法访问"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceAccessDeniedError,
        )

        resource = {
            "connection_id": "conn_001",
            "owner_id": "user_alice_001",
            "created_by": "user_alice_001",
        }

        # Bob 尝试访问 Alice 的资源 - 应被拒绝
        with pytest.raises(ConfluenceAccessDeniedError) as exc_info:
            mock_sync_service._verify_confluence_access(resource, "connection", user_bob)
        assert "connection" in str(exc_info.value)

    def test_verify_confluence_access_admin_allowed(
        self, mock_sync_service, user_alice, admin_user
    ):
        """测试：管理员可以访问任何资源"""
        resource = {
            "connection_id": "conn_001",
            "owner_id": "user_alice_001",
            "created_by": "user_alice_001",
        }

        # Admin 应该可以访问
        mock_sync_service._verify_confluence_access(resource, "connection", admin_user)

    def test_verify_confluence_access_uses_created_by_fallback(self, mock_sync_service, user_alice):
        """测试：当 owner_id 为空时使用 created_by"""
        resource = {
            "connection_id": "conn_001",
            "owner_id": None,  # owner_id 为空
            "created_by": "user_alice_001",
        }

        # 应该使用 created_by 作为 fallback
        mock_sync_service._verify_confluence_access(resource, "connection", user_alice)


class TestConfluenceBindingAccess:
    """Confluence Binding 访问控制测试"""

    @pytest.fixture
    def mock_sync_service(self):
        """创建 Mock ConfluenceSyncService"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceSyncService,
        )

        mock_settings = MagicMock()
        mock_settings.confluence.request_timeout = 30
        mock_settings.confluence.max_retries = 3

        mock_db = AsyncMock()
        mock_kb_service = MagicMock()

        service = ConfluenceSyncService(
            settings=mock_settings,
            database=mock_db,
            knowledge_service=mock_kb_service,
        )
        return service

    @pytest.mark.asyncio
    async def test_verify_binding_access_owner_allowed(self, mock_sync_service, user_alice):
        """测试：Binding 所有者可以访问"""
        mock_sync_service.db.get_confluence_binding = AsyncMock(
            return_value={
                "binding_id": "bind_001",
                "owner_id": "user_alice_001",
                "created_by": "user_alice_001",
                "space_key": "TEST",
            }
        )

        # 不应抛出异常
        binding = await mock_sync_service._verify_binding_access("bind_001", user_alice)
        assert binding["binding_id"] == "bind_001"

    @pytest.mark.asyncio
    async def test_verify_binding_access_non_owner_denied(
        self, mock_sync_service, user_alice, user_bob
    ):
        """测试：非所有者无法访问 Binding"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceAccessDeniedError,
        )

        mock_sync_service.db.get_confluence_binding = AsyncMock(
            return_value={
                "binding_id": "bind_001",
                "owner_id": "user_alice_001",
                "created_by": "user_alice_001",
                "space_key": "TEST",
            }
        )

        with pytest.raises(ConfluenceAccessDeniedError):
            await mock_sync_service._verify_binding_access("bind_001", user_bob)

    @pytest.mark.asyncio
    async def test_verify_binding_access_not_found(self, mock_sync_service, user_alice):
        """测试：Binding 不存在时抛出错误"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceSyncError,
        )

        mock_sync_service.db.get_confluence_binding = AsyncMock(return_value=None)

        with pytest.raises(ConfluenceSyncError) as exc_info:
            await mock_sync_service._verify_binding_access("bind_nonexistent", user_alice)
        assert "not found" in str(exc_info.value)


class TestConfluenceConnectionAccess:
    """Confluence Connection 访问控制测试"""

    @pytest.fixture
    def mock_sync_service(self):
        """创建 Mock ConfluenceSyncService"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceSyncService,
        )

        mock_settings = MagicMock()
        mock_settings.confluence.request_timeout = 30
        mock_settings.confluence.max_retries = 3

        mock_db = AsyncMock()
        mock_kb_service = MagicMock()

        service = ConfluenceSyncService(
            settings=mock_settings,
            database=mock_db,
            knowledge_service=mock_kb_service,
        )
        return service

    @pytest.mark.asyncio
    async def test_verify_connection_access_owner_allowed(self, mock_sync_service, user_alice):
        """测试：Connection 所有者可以访问"""
        mock_sync_service.db.get_confluence_connection = AsyncMock(
            return_value={
                "connection_id": "conn_001",
                "owner_id": "user_alice_001",
                "created_by": "user_alice_001",
                "domain": "test.atlassian.net",
            }
        )

        connection = await mock_sync_service._verify_connection_access("conn_001", user_alice)
        assert connection["connection_id"] == "conn_001"

    @pytest.mark.asyncio
    async def test_verify_connection_access_non_owner_denied(
        self, mock_sync_service, user_alice, user_bob
    ):
        """测试：非所有者无法访问 Connection"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceAccessDeniedError,
        )

        mock_sync_service.db.get_confluence_connection = AsyncMock(
            return_value={
                "connection_id": "conn_001",
                "owner_id": "user_alice_001",
                "created_by": "user_alice_001",
                "domain": "test.atlassian.net",
            }
        )

        with pytest.raises(ConfluenceAccessDeniedError):
            await mock_sync_service._verify_connection_access("conn_001", user_bob)

    @pytest.mark.asyncio
    async def test_verify_connection_access_admin_allowed(
        self, mock_sync_service, user_alice, admin_user
    ):
        """测试：管理员可以访问任何 Connection"""
        mock_sync_service.db.get_confluence_connection = AsyncMock(
            return_value={
                "connection_id": "conn_001",
                "owner_id": "user_alice_001",
                "created_by": "user_alice_001",
                "domain": "test.atlassian.net",
            }
        )

        connection = await mock_sync_service._verify_connection_access("conn_001", admin_user)
        assert connection["connection_id"] == "conn_001"


class TestConfluencePageAccess:
    """Confluence Page 访问控制测试"""

    @pytest.fixture
    def mock_sync_service(self):
        """创建 Mock ConfluenceSyncService"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceSyncService,
        )

        mock_settings = MagicMock()
        mock_settings.confluence.request_timeout = 30
        mock_settings.confluence.max_retries = 3

        mock_db = AsyncMock()
        mock_kb_service = MagicMock()

        service = ConfluenceSyncService(
            settings=mock_settings,
            database=mock_db,
            knowledge_service=mock_kb_service,
        )
        return service

    @pytest.mark.asyncio
    async def test_verify_page_access_via_binding(self, mock_sync_service, user_alice):
        """测试：通过 Binding 验证 Page 访问权限"""
        # Mock page record
        mock_sync_service.db.get_confluence_page = AsyncMock(
            return_value={
                "id": "page_001",
                "binding_id": "bind_001",
                "page_id": "12345",
                "title": "Test Page",
            }
        )

        # Mock binding (owned by Alice)
        mock_sync_service.db.get_confluence_binding = AsyncMock(
            return_value={
                "binding_id": "bind_001",
                "owner_id": "user_alice_001",
                "created_by": "user_alice_001",
                "space_key": "TEST",
            }
        )

        page = await mock_sync_service._verify_page_access("page_001", user_alice)
        assert page["id"] == "page_001"

    @pytest.mark.asyncio
    async def test_verify_page_access_non_owner_denied(
        self, mock_sync_service, user_alice, user_bob
    ):
        """测试：非所有者无法访问 Page"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceAccessDeniedError,
        )

        mock_sync_service.db.get_confluence_page = AsyncMock(
            return_value={
                "id": "page_001",
                "binding_id": "bind_001",
                "page_id": "12345",
                "title": "Test Page",
            }
        )

        mock_sync_service.db.get_confluence_binding = AsyncMock(
            return_value={
                "binding_id": "bind_001",
                "owner_id": "user_alice_001",
                "created_by": "user_alice_001",
                "space_key": "TEST",
            }
        )

        with pytest.raises(ConfluenceAccessDeniedError):
            await mock_sync_service._verify_page_access("page_001", user_bob)


# ============ 跨模块权限隔离测试 ============


class TestCrossUserIsolation:
    """跨用户隔离测试"""

    @pytest.fixture
    def mock_proxy(self):
        """创建 Mock LangGraphProxy"""
        from src.adapters.langgraph_proxy import (
            LangGraphInstance,
            LangGraphLoadBalancer,
            LangGraphProxy,
            LoadBalancerConfig,
        )

        config = LoadBalancerConfig(
            strategy="round_robin",
            instances=[
                LangGraphInstance(
                    instance_id="test-1",
                    url="http://localhost:8123",
                )
            ],
        )
        lb = LangGraphLoadBalancer(config)
        proxy = LangGraphProxy(load_balancer=lb)
        return proxy

    def test_user_cannot_access_other_user_assistant(self, mock_proxy, user_alice, user_bob):
        """测试：用户无法访问其他用户的 Assistant"""
        from src.adapters.langgraph_proxy import AssistantAccessDeniedError

        alice_assistant = {
            "assistant_id": "asst_alice",
            "metadata": {
                "created_by": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        # Alice 可以访问自己的 Assistant
        mock_proxy._verify_assistant_ownership(alice_assistant, user_alice)

        # Bob 无法访问 Alice 的 Assistant
        with pytest.raises(AssistantAccessDeniedError):
            mock_proxy._verify_assistant_ownership(alice_assistant, user_bob)

    def test_user_cannot_access_other_user_thread(self, mock_proxy, user_alice, user_bob):
        """测试：用户无法访问其他用户的 Thread"""
        from src.adapters.langgraph_proxy import ForbiddenError

        alice_thread = {
            "thread_id": "thread_alice",
            "metadata": {
                "owner_id": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        # Alice 可以访问自己的 Thread
        mock_proxy._verify_ownership(alice_thread, user_alice)

        # Bob 无法访问 Alice 的 Thread
        with pytest.raises(ForbiddenError):
            mock_proxy._verify_ownership(alice_thread, user_bob)

    def test_admin_can_access_any_resource(self, mock_proxy, user_alice, admin_user):
        """测试：管理员可以访问任何资源"""
        alice_assistant = {
            "assistant_id": "asst_alice",
            "metadata": {
                "created_by": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        alice_thread = {
            "thread_id": "thread_alice",
            "metadata": {
                "owner_id": "user_alice_001",
                "tenant_id": "tenant_test",
            },
        }

        # Admin 可以访问 Alice 的 Assistant 和 Thread
        mock_proxy._verify_assistant_ownership(alice_assistant, admin_user)
        mock_proxy._verify_ownership(alice_thread, admin_user)


# ============ 缓存键隔离测试 ============


class TestCacheKeyIsolation:
    """缓存键隔离测试"""

    @pytest.fixture
    def mock_proxy(self):
        """创建 Mock LangGraphProxy"""
        from src.adapters.langgraph_proxy import (
            LangGraphInstance,
            LangGraphLoadBalancer,
            LangGraphProxy,
            LoadBalancerConfig,
        )

        config = LoadBalancerConfig(
            strategy="round_robin",
            instances=[
                LangGraphInstance(
                    instance_id="test-1",
                    url="http://localhost:8123",
                )
            ],
        )
        lb = LangGraphLoadBalancer(config)
        proxy = LangGraphProxy(load_balancer=lb)
        return proxy

    def test_cache_key_includes_user_id(self, mock_proxy, user_alice, user_bob):
        """测试：缓存键包含用户 ID，防止跨用户数据泄露"""
        assistant_id = "asst_001"

        # Alice 的缓存键
        alice_cache_key = f"{assistant_id}:{user_alice.user_id}"
        # Bob 的缓存键
        bob_cache_key = f"{assistant_id}:{user_bob.user_id}"

        # 缓存键应该不同
        assert alice_cache_key != bob_cache_key
        assert user_alice.user_id in alice_cache_key
        assert user_bob.user_id in bob_cache_key

    def test_invalidate_assistant_cache_removes_all_user_keys(self, mock_proxy):
        """测试：失效 Assistant 缓存时删除所有用户特定的键"""
        import time

        assistant_id = "asst_to_invalidate"

        # 模拟多个用户的缓存
        mock_proxy._assistant_cache[f"{assistant_id}:user_1"] = ({"data": "test"}, time.time())
        mock_proxy._assistant_cache[f"{assistant_id}:user_2"] = ({"data": "test"}, time.time())
        mock_proxy._assistant_cache["other_asst:user_1"] = ({"data": "other"}, time.time())

        # 失效 assistant_id 的缓存
        mock_proxy._invalidate_assistant_cache(assistant_id)

        # 该 assistant 的所有用户缓存都应被删除
        assert f"{assistant_id}:user_1" not in mock_proxy._assistant_cache
        assert f"{assistant_id}:user_2" not in mock_proxy._assistant_cache
        # 其他 assistant 的缓存不受影响
        assert "other_asst:user_1" in mock_proxy._assistant_cache


# ============ 透明代理服务级授权测试 ============


class TestProxyServiceAuthorization:
    """透明代理服务级授权测试

    测试 check_service_authorization 函数的各种授权场景：
    1. Capability 权限检查 (AgentInvoke)
    2. 服务级配置 (require_auth, allowed_roles, allowed_api_keys)
    3. 用户/API Key 级别的 allowed_services
    """

    @pytest.fixture
    def mock_request(self, user_alice):
        """创建模拟请求对象"""
        request = MagicMock()
        request.app.state.settings = MagicMock()
        request.app.state.settings.authentication.api_key.enabled = True
        request.app.state.settings.authentication.api_key.header_name = "X-API-Key"
        request.app.state.dispatcher = MagicMock()
        request.app.state.dispatcher.rbac = MagicMock()
        request.app.state.dispatcher.rbac.has_permission.side_effect = _rbac_has_permission
        request.app.state.registry = MagicMock()
        request.app.state.database = MagicMock()
        request.app.state.database.enabled = True
        request.state = MagicMock()
        request.headers = {}
        return request

    @pytest.fixture
    def mock_auth_context(self):
        """创建模拟 AuthContext"""
        from src.api.deps import AuthContext

        return AuthContext(
            user_id="user_alice_001",
            tenant_id="tenant_test",
            roles=["user", "service:invoke"],
            permissions=[],
        )

    @pytest.fixture
    def mock_guest_auth_context(self):
        """创建模拟 guest AuthContext"""
        from src.api.deps import AuthContext

        return AuthContext(user_id="", tenant_id="", roles=["guest"], permissions=[])

    @pytest.mark.asyncio
    async def test_rbac_permission_required(
        self, mock_request, guest_user, mock_guest_auth_context
    ):
        """测试：缺少 AgentInvoke capability 权限应被拒绝"""
        from fastapi import HTTPException

        from src.api.v1.proxy import check_service_authorization

        with pytest.raises(HTTPException) as exc_info:
            await check_service_authorization(
                mock_request, "test_service", guest_user, mock_guest_auth_context
            )
        assert exc_info.value.status_code == 403
        assert exc_info.value.detail["required_capability"] == "AgentInvoke"
        assert exc_info.value.detail["required_permission"] == "conversation:playground:access"

    @pytest.mark.asyncio
    async def test_service_require_auth_unauthenticated_denied(
        self, mock_request, guest_user, mock_guest_auth_context
    ):
        """测试：require_auth=True 时未认证用户被拒绝"""
        from fastapi import HTTPException

        from src.api.v1.proxy import check_service_authorization

        capability_guest_auth = mock_guest_auth_context.model_copy(
            update={"roles": ["guest", "service:invoke"]}
        )

        # 模拟服务配置：require_auth=True
        mock_service = MagicMock()
        mock_service.service_id = "test_service"
        mock_service.name = "test_service"

        service_config = MagicMock()
        service_config.auth = MagicMock()
        service_config.auth.enabled = True
        service_config.auth.public = False
        service_config.auth.require_auth = True
        service_config.auth.allowed_roles = []
        service_config.auth.allowed_api_keys = []

        mock_service.get_service_config.return_value = service_config
        mock_request.app.state.registry.get = AsyncMock(return_value=mock_service)

        with pytest.raises(HTTPException) as exc_info:
            await check_service_authorization(
                mock_request, "test_service", guest_user, capability_guest_auth
            )
        assert exc_info.value.status_code == 403
        assert "authentication required" in exc_info.value.detail

    @pytest.mark.asyncio
    async def test_service_allowed_roles_denied(self, mock_request, user_alice, mock_auth_context):
        """测试：用户角色不在 allowed_roles 中被拒绝"""
        from fastapi import HTTPException

        from src.api.v1.proxy import check_service_authorization

        # 模拟服务配置：allowed_roles=["premium", "enterprise"]
        mock_service = MagicMock()
        mock_service.service_id = "test_service"
        mock_service.name = "test_service"

        service_config = MagicMock()
        service_config.auth = MagicMock()
        service_config.auth.enabled = True
        service_config.auth.public = False
        service_config.auth.require_auth = False
        service_config.auth.allowed_roles = [
            "premium",
            "enterprise",
        ]  # Alice 只有 user, service:invoke
        service_config.auth.allowed_api_keys = []

        mock_service.get_service_config.return_value = service_config
        mock_request.app.state.registry.get = AsyncMock(return_value=mock_service)

        with pytest.raises(HTTPException) as exc_info:
            await check_service_authorization(
                mock_request, "test_service", user_alice, mock_auth_context
            )
        assert exc_info.value.status_code == 403
        assert "not authorized" in exc_info.value.detail

    @pytest.mark.asyncio
    async def test_service_allowed_roles_admin_bypass(self, mock_request, admin_user):
        """测试：admin 角色可绕过 allowed_roles 限制"""
        from src.api.deps import AuthContext
        from src.api.v1.proxy import check_service_authorization

        admin_auth = AuthContext(
            user_id="admin_001",
            tenant_id="tenant_test",
            roles=["user", "admin", "service:invoke"],
            permissions=[],
        )

        # 模拟服务配置：allowed_roles=["premium"]（admin 不在列表中）
        mock_service = MagicMock()
        mock_service.service_id = "test_service"
        mock_service.name = "test_service"

        service_config = MagicMock()
        service_config.auth = MagicMock()
        service_config.auth.enabled = True
        service_config.auth.public = False
        service_config.auth.require_auth = False
        service_config.auth.allowed_roles = ["premium"]
        service_config.auth.allowed_api_keys = []

        mock_service.get_service_config.return_value = service_config
        mock_request.app.state.registry.get = AsyncMock(return_value=mock_service)
        mock_request.app.state.registry.list = AsyncMock(return_value=[])
        mock_request.app.state.database.get_tenant = AsyncMock(return_value=None)

        # Admin 应该可以访问
        await check_service_authorization(mock_request, "test_service", admin_user, admin_auth)

    @pytest.mark.asyncio
    async def test_allowed_services_restriction(self, mock_request, user_alice, mock_auth_context):
        """测试：allowed_services 限制用户可访问的服务"""
        from fastapi import HTTPException

        from src.api.v1.proxy import check_service_authorization

        # 无服务级配置
        mock_request.app.state.registry.get = AsyncMock(return_value=None)

        # 模拟 API Key 的 allowed_services 限制
        mock_request.state.api_key_info = {
            "allowed_services": ["service_a", "service_b"]  # 不包含 test_service
        }
        mock_request.app.state.database.get_tenant = AsyncMock(return_value=None)

        with pytest.raises(HTTPException) as exc_info:
            await check_service_authorization(
                mock_request, "test_service", user_alice, mock_auth_context
            )
        assert exc_info.value.status_code == 403
        assert "not in allowed services" in exc_info.value.detail

    @pytest.mark.asyncio
    async def test_allowed_services_wildcard_allowed(
        self, mock_request, user_alice, mock_auth_context
    ):
        """测试：allowed_services=["*"] 允许所有服务"""
        from src.api.v1.proxy import check_service_authorization

        # 无服务级配置
        mock_request.app.state.registry.get = AsyncMock(return_value=None)

        # 模拟 API Key 的 allowed_services=["*"]
        mock_request.state.api_key_info = {"allowed_services": ["*"]}
        mock_request.app.state.database.get_tenant = AsyncMock(return_value=None)

        # 应该成功
        await check_service_authorization(
            mock_request, "any_service", user_alice, mock_auth_context
        )

    @pytest.mark.asyncio
    async def test_public_service_accessible(self, mock_request, user_alice, mock_auth_context):
        """测试：public=True 的服务可被具备基础 capability 的用户访问"""
        from src.api.v1.proxy import check_service_authorization

        # 模拟服务配置：public=True
        mock_service = MagicMock()
        mock_service.service_id = "public_service"
        mock_service.name = "public_service"

        service_config = MagicMock()
        service_config.auth = MagicMock()
        service_config.auth.enabled = True
        service_config.auth.public = True  # 公开服务

        mock_service.get_service_config.return_value = service_config
        mock_request.app.state.registry.get = AsyncMock(return_value=mock_service)
        mock_request.app.state.database.get_tenant = AsyncMock(return_value=None)
        mock_request.state.api_key_info = None

        # 普通用户应当可以访问公开服务
        await check_service_authorization(
            mock_request, "public_service", user_alice, mock_auth_context
        )

    @pytest.mark.asyncio
    async def test_tenant_allowed_services_restriction(
        self, mock_request, user_alice, mock_auth_context
    ):
        """测试：租户级别的 allowed_services 限制"""
        from fastapi import HTTPException

        from src.api.v1.proxy import check_service_authorization

        # 无服务级配置
        mock_request.app.state.registry.get = AsyncMock(return_value=None)
        mock_request.state.api_key_info = None

        # 模拟租户的 allowed_services
        mock_request.app.state.database.get_tenant = AsyncMock(
            return_value={
                "tenant_id": "tenant_test",
                "allowed_services": ["service_x", "service_y"],  # 不包含 test_service
            }
        )

        with pytest.raises(HTTPException) as exc_info:
            await check_service_authorization(
                mock_request, "test_service", user_alice, mock_auth_context
            )
        assert exc_info.value.status_code == 403


class TestProxyRateLimitHeaders:
    """透明代理限流响应头测试"""

    def test_rate_limit_headers_build(self):
        """测试：限流响应头正确构建"""
        from datetime import timedelta

        from src.core.gateway.multi_dimension_rate_limiter import RateLimitHeaders, RateLimitResult

        reset_time = datetime.now() + timedelta(seconds=60)
        result = RateLimitResult(
            allowed=True,
            remaining=95,
            limit=100,
            reset_at=reset_time,
            retry_after=0,
            dimension="user",
        )

        headers = RateLimitHeaders.build(result)

        assert "X-RateLimit-Limit" in headers
        assert headers["X-RateLimit-Limit"] == "100"
        assert "X-RateLimit-Remaining" in headers
        assert headers["X-RateLimit-Remaining"] == "95"
        assert "X-RateLimit-Reset" in headers

    def test_rate_limit_exceeded_response(self):
        """测试：限流超限响应正确构建"""
        from datetime import timedelta

        from src.core.gateway.multi_dimension_rate_limiter import RateLimitHeaders, RateLimitResult

        reset_time = datetime.now() + timedelta(seconds=60)
        result = RateLimitResult(
            allowed=False,
            remaining=0,
            limit=100,
            reset_at=reset_time,
            retry_after=60,
            dimension="user",
        )

        response = RateLimitHeaders.build_exceeded_response(result)

        assert "error" in response
        # error is a dict with code, message, retry_after, etc.
        assert response["error"]["code"] == "RATE_LIMIT_EXCEEDED"
        assert response["error"]["retry_after"] == 60
