"""
Confluence 客户端单元测试

测试 ConfluenceClient 类的核心功能，特别是分页支持。
"""

from __future__ import annotations

from unittest.mock import AsyncMock, patch

import pytest
from knowledge_service.services.knowledge.confluence.client import ConfluenceClient
from knowledge_service.services.knowledge.confluence.models import ConfluenceCredentials


class TestGetPageChildrenPagination:
    """测试 get_page_children 方法的分页功能"""

    def setup_method(self):
        """每个测试前创建 ConfluenceClient 实例"""
        self.credentials = ConfluenceCredentials(
            domain="test.atlassian.net",
            email="test@example.com",
            api_token="test-token",
        )
        self.client = ConfluenceClient(self.credentials)

    @pytest.mark.asyncio
    async def test_fetch_all_children_single_page(self):
        """测试单页结果（无需分页）"""
        children = [
            {"id": "1", "title": "Page 1"},
            {"id": "2", "title": "Page 2"},
        ]

        with patch.object(self.client, "_request", new_callable=AsyncMock) as mock_request:
            mock_request.return_value = {
                "results": children,
                "_links": {},  # 无 next 链接
            }

            result = await self.client.get_page_children("parent-123")

            assert len(result) == 2
            assert result[0]["id"] == "1"
            assert result[1]["title"] == "Page 2"
            mock_request.assert_called_once()

    @pytest.mark.asyncio
    async def test_fetch_all_children_multiple_pages(self):
        """测试多页结果（自动分页）"""
        # 第一页返回 100 条记录和 next 链接
        page1_children = [{"id": str(i), "title": f"Page {i}"} for i in range(1, 101)]
        # 第二页返回 50 条记录，无 next 链接
        page2_children = [{"id": str(i), "title": f"Page {i}"} for i in range(101, 151)]

        call_count = 0

        async def mock_request_side_effect(method, path, params=None):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                return {
                    "results": page1_children,
                    "_links": {"next": "/pages/parent-123/children?cursor=abc123"},
                }
            else:
                return {
                    "results": page2_children,
                    "_links": {},
                }

        with patch.object(self.client, "_request", new_callable=AsyncMock) as mock_request:
            mock_request.side_effect = mock_request_side_effect

            result = await self.client.get_page_children("parent-123")

            assert len(result) == 150
            assert result[0]["id"] == "1"
            assert result[99]["id"] == "100"
            assert result[100]["id"] == "101"
            assert result[149]["id"] == "150"
            assert mock_request.call_count == 2

    @pytest.mark.asyncio
    async def test_fetch_all_false_returns_first_page_only(self):
        """测试 fetch_all=False 只返回第一页"""
        children = [{"id": str(i), "title": f"Page {i}"} for i in range(1, 101)]

        with patch.object(self.client, "_request", new_callable=AsyncMock) as mock_request:
            mock_request.return_value = {
                "results": children,
                "_links": {"next": "/pages/parent-123/children?cursor=abc123"},
            }

            result = await self.client.get_page_children("parent-123", fetch_all=False)

            assert len(result) == 100
            mock_request.assert_called_once()

    @pytest.mark.asyncio
    async def test_pagination_with_custom_limit(self):
        """测试自定义 limit 参数"""
        children = [{"id": "1", "title": "Page 1"}]

        with patch.object(self.client, "_request", new_callable=AsyncMock) as mock_request:
            mock_request.return_value = {"results": children, "_links": {}}

            await self.client.get_page_children("parent-123", limit=50)

            call_args = mock_request.call_args
            assert call_args[1]["params"]["limit"] == 50

    @pytest.mark.asyncio
    async def test_cursor_extracted_from_next_link(self):
        """测试从 next 链接正确提取 cursor"""
        page1 = [{"id": "1"}]
        page2 = [{"id": "2"}]

        call_count = 0

        async def mock_request_side_effect(method, path, params=None):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                return {
                    "results": page1,
                    "_links": {
                        "next": "https://test.atlassian.net/wiki/api/v2/pages/123/children?cursor=xyz789&limit=100"
                    },
                }
            else:
                # 验证第二次调用时使用了正确的 cursor
                assert params.get("cursor") == "xyz789"
                return {"results": page2, "_links": {}}

        with patch.object(self.client, "_request", new_callable=AsyncMock) as mock_request:
            mock_request.side_effect = mock_request_side_effect

            result = await self.client.get_page_children("123", limit=1)

            assert len(result) == 2

    @pytest.mark.asyncio
    async def test_empty_results(self):
        """测试空结果"""
        with patch.object(self.client, "_request", new_callable=AsyncMock) as mock_request:
            mock_request.return_value = {"results": [], "_links": {}}

            result = await self.client.get_page_children("parent-empty")

            assert result == []

    @pytest.mark.asyncio
    async def test_handles_missing_links(self):
        """测试处理缺失的 _links 字段"""
        children = [{"id": "1", "title": "Page 1"}]

        with patch.object(self.client, "_request", new_callable=AsyncMock) as mock_request:
            mock_request.return_value = {"results": children}  # 无 _links 字段

            result = await self.client.get_page_children("parent-123")

            assert len(result) == 1

    @pytest.mark.asyncio
    async def test_follows_next_when_results_less_than_limit(self):
        """测试短页仍按 next 链接继续分页"""
        # 第一页返回 100 条
        page1 = [{"id": str(i)} for i in range(100)]
        # 第二页只返回 30 条（少于 limit）
        page2 = [{"id": str(i)} for i in range(100, 130)]

        call_count = 0

        async def mock_request_side_effect(method, path, params=None):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                return {
                    "results": page1,
                    "_links": {"next": "/children?cursor=abc"},
                }
            elif call_count == 2:
                return {
                    "results": page2,
                    "_links": {"next": "/children?cursor=def"},
                }
            return {"results": [{"id": "130"}], "_links": {}}

        with patch.object(self.client, "_request", new_callable=AsyncMock) as mock_request:
            mock_request.side_effect = mock_request_side_effect

            result = await self.client.get_page_children("parent-123", limit=100)

            # next 是继续分页的依据，短页不能表示发现已完整。
            assert len(result) == 131
            assert mock_request.call_count == 3
