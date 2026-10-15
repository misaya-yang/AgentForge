"""
Confluence REST API v2 Client.

Provides async HTTP client for interacting with Confluence Cloud REST API.
Supports authentication, page retrieval, space management, and pagination.
"""

from __future__ import annotations

import asyncio
import base64
import logging
import re
from collections.abc import AsyncIterator
from datetime import datetime
from typing import Any
from urllib.parse import parse_qs, urljoin, urlparse

import httpx

from .models import ConfluenceAttachment, ConfluenceCredentials, ConfluencePage, ConfluenceSpace

logger = logging.getLogger(__name__)


def _escape_cql_value(value: str) -> str:
    """
    Escape special characters in CQL query values to prevent injection.

    CQL special characters: + - && || ! ( ) { } [ ] ^ " ~ * ? : \\ /
    For string values used in space keys and other identifiers.
    """
    if not value:
        return value
    # Only allow alphanumeric, hyphens, and underscores for identifiers
    # This is stricter than full CQL escaping but safer for space keys
    if not re.match(r"^[A-Za-z0-9_-]+$", value):
        raise ValueError(f"Invalid CQL identifier: {value}")
    return value


def _escape_cql_string(value: str) -> str:
    """
    Escape special characters for CQL string literals (used in quotes).

    Escapes backslashes and double quotes.
    """
    if not value:
        return value
    # Escape backslashes first, then quotes
    return value.replace("\\", "\\\\").replace('"', '\\"')


class ConfluenceAPIError(Exception):
    """Confluence API 错误"""

    def __init__(
        self, message: str, status_code: int | None = None, response_body: str | None = None
    ):
        super().__init__(message)
        self.status_code = status_code
        self.response_body = response_body


class ConfluenceClient:
    """
    Confluence REST API v2 客户端

    使用 httpx 进行异步 HTTP 请求，支持：
    - Basic Auth 认证
    - 页面获取和列表
    - 空间管理
    - 游标分页
    - 自动重试
    """

    def __init__(
        self,
        credentials: ConfluenceCredentials,
        timeout: float = 30.0,
        max_retries: int = 3,
    ):
        self.credentials = credentials
        self.timeout = timeout
        self.max_retries = max_retries
        self._client: httpx.AsyncClient | None = None

    @property
    def _auth_header(self) -> str:
        """生成 Basic Auth header"""
        credentials = f"{self.credentials.email}:{self.credentials.api_token}"
        encoded = base64.b64encode(credentials.encode()).decode()
        return f"Basic {encoded}"

    async def _get_client(self) -> httpx.AsyncClient:
        """获取或创建 HTTP 客户端"""
        if self._client is None or self._client.is_closed:
            self._client = httpx.AsyncClient(
                timeout=httpx.Timeout(self.timeout),
                headers={
                    "Authorization": self._auth_header,
                    "Accept": "application/json",
                    "Content-Type": "application/json",
                },
            )
        return self._client

    async def close(self) -> None:
        """关闭客户端连接"""
        if self._client and not self._client.is_closed:
            await self._client.aclose()
            self._client = None

    async def __aenter__(self) -> ConfluenceClient:
        return self

    async def __aexit__(self, exc_type, exc_val, exc_tb) -> None:
        await self.close()

    async def _request(
        self,
        method: str,
        endpoint: str,
        params: dict[str, Any] | None = None,
        json_data: dict[str, Any] | None = None,
        use_v1: bool = False,
    ) -> dict[str, Any]:
        """
        发送 API 请求 (带重试)

        Args:
            method: HTTP 方法
            endpoint: API 端点路径
            params: 查询参数
            json_data: JSON 请求体
            use_v1: 是否使用 v1 API

        Returns:
            响应 JSON 数据

        Raises:
            ConfluenceAPIError: API 请求失败
        """
        client = await self._get_client()
        base = self.credentials.api_v1_url if use_v1 else self.credentials.api_v2_url
        url = urljoin(base + "/", endpoint.lstrip("/"))

        last_error = None
        for attempt in range(self.max_retries):
            try:
                resp = await client.request(
                    method,
                    url,
                    params=params,
                    json=json_data,
                )

                if resp.status_code == 429:
                    # Rate limited
                    retry_after = int(resp.headers.get("Retry-After", 5))
                    logger.warning(f"Rate limited, retrying after {retry_after}s")
                    await asyncio.sleep(retry_after)
                    continue

                if resp.status_code >= 500 and attempt < self.max_retries - 1:
                    # Server error, retry
                    await asyncio.sleep(2**attempt)
                    continue

                if resp.status_code >= 400:
                    raise ConfluenceAPIError(
                        f"API request failed: {resp.status_code}",
                        status_code=resp.status_code,
                        response_body=resp.text,
                    )

                return resp.json()

            except httpx.RequestError as e:
                last_error = e
                if attempt < self.max_retries - 1:
                    logger.warning(f"Request error, retrying: {e}")
                    await asyncio.sleep(2**attempt)
                else:
                    raise ConfluenceAPIError(
                        f"Request failed after {self.max_retries} retries: {e}"
                    )

        raise ConfluenceAPIError(f"Request failed: {last_error}")

    # ============ Connection Test ============

    async def test_connection(self) -> dict[str, Any]:
        """
        测试连接是否有效

        Returns:
            包含连接状态和可用空间数量的字典
        """
        try:
            await self._request("GET", "/spaces", params={"limit": 1})
            return {
                "status": "success",
                "message": "Connection successful",
                "spaces_available": True,
            }
        except ConfluenceAPIError as e:
            return {
                "status": "error",
                "message": str(e),
                "status_code": e.status_code,
            }

    # ============ Page Operations ============

    async def get_page(
        self,
        page_id: str,
        body_format: str = "storage",
        include_labels: bool = True,
    ) -> ConfluencePage:
        """
        获取单个页面

        Args:
            page_id: 页面 ID
            body_format: 内容格式 (storage | atlas_doc_format | view)
            include_labels: 是否获取标签

        Returns:
            ConfluencePage 对象
        """
        params = {"body-format": body_format}
        data = await self._request("GET", f"/pages/{page_id}", params=params)

        # 调试日志：检查 API 返回的数据结构
        logger.debug(f"Confluence API response for page {page_id}:")
        logger.debug(f"  - title: {data.get('title')}")
        logger.debug(
            f"  - body keys: {list(data.get('body', {}).keys()) if data.get('body') else 'NO BODY'}"
        )
        if data.get("body"):
            for key, val in data["body"].items():
                if isinstance(val, dict):
                    logger.debug(f"  - body.{key} keys: {list(val.keys())}")
                    if "value" in val:
                        logger.debug(f"  - body.{key}.value length: {len(val.get('value', ''))}")
                else:
                    logger.debug(f"  - body.{key} = {type(val)}")

        labels = []
        if include_labels:
            try:
                labels_data = await self._request("GET", f"/pages/{page_id}/labels")
                labels = [label.get("name", "") for label in labels_data.get("results", [])]
            except Exception as e:
                logger.warning(f"Failed to get labels for page {page_id}: {e}")

        # 获取 space key
        space_id = data.get("spaceId")
        space_key = ""
        if space_id:
            try:
                space_data = await self._request("GET", f"/spaces/{space_id}")
                space_key = space_data.get("key", "")
            except Exception:
                pass

        # 提取 body 内容 - 增强处理多种可能的结构
        body = data.get("body", {})
        body_content = ""

        if body:
            # 首选：使用请求的格式
            if body_format in body:
                format_data = body[body_format]
                if isinstance(format_data, dict):
                    body_content = format_data.get("value", "")
                elif isinstance(format_data, str):
                    body_content = format_data

            # 备选：尝试其他常见格式
            if not body_content:
                for fallback_format in ["storage", "view", "atlas_doc_format"]:
                    if fallback_format in body and fallback_format != body_format:
                        format_data = body[fallback_format]
                        if isinstance(format_data, dict):
                            body_content = format_data.get("value", "")
                        elif isinstance(format_data, str):
                            body_content = format_data
                        if body_content:
                            logger.info(
                                f"Used fallback format '{fallback_format}' for page {page_id}"
                            )
                            break

        # 警告：body 为空
        if not body_content:
            logger.warning(
                f"Empty body for page {page_id} (title: {data.get('title')}). "
                f"API returned body: {body}. Check Confluence permissions or page content."
            )
        else:
            logger.info(f"Page {page_id} body extracted successfully, length: {len(body_content)}")

        version_info = data.get("version", {})

        # 构建完整的 web_url（API 返回的是相对路径）
        webui_path = data.get("_links", {}).get("webui")
        web_url = f"https://{self.credentials.domain}{webui_path}" if webui_path else None

        return ConfluencePage(
            page_id=str(data.get("id")),
            space_key=space_key,
            space_id=space_id,
            title=data.get("title", ""),
            version=version_info.get("number", 1),
            body_storage=body_content,
            parent_id=data.get("parentId"),
            author_id=data.get("authorId"),
            created_at=data.get("createdAt"),
            updated_at=version_info.get("createdAt"),
            labels=labels,
            web_url=web_url,
        )

    async def get_page_by_url(self, url: str) -> ConfluencePage:
        """
        根据 URL 获取页面

        支持的 URL 格式：
        - https://domain.atlassian.net/wiki/spaces/SPACE/pages/12345/Title
        - https://domain.atlassian.net/wiki/x/AbCdEf (短链接需要额外处理)

        Args:
            url: Confluence 页面 URL

        Returns:
            ConfluencePage 对象
        """
        page_id = self.parse_page_id_from_url(url)
        return await self.get_page(page_id)

    async def get_page_children(
        self,
        page_id: str,
        limit: int = 100,
        fetch_all: bool = True,
    ) -> list[dict[str, Any]]:
        """
        获取页面的子页面（支持分页）

        Args:
            page_id: 父页面 ID
            limit: 每页返回数量限制
            fetch_all: 是否获取所有子页面（自动分页）

        Returns:
            子页面列表
        """
        all_children: list[dict[str, Any]] = []
        cursor: str | None = None
        seen_cursors: set[str] = set()

        while True:
            params: dict[str, Any] = {"limit": limit}
            if cursor:
                params["cursor"] = cursor

            data = await self._request(
                "GET",
                f"/pages/{page_id}/children",
                params=params,
            )

            results = data.get("results")
            if not isinstance(results, list) or any(
                not isinstance(child, dict)
                or isinstance(child.get("id"), bool)
                or not isinstance(child.get("id"), (str, int))
                or not str(child["id"]).strip()
                for child in results
            ):
                raise ConfluenceAPIError("Invalid Confluence child discovery response")
            all_children.extend(results)

            # 如果不需要获取所有或没有更多结果，退出循环
            if not fetch_all:
                break

            # 检查是否有下一页
            links = data.get("_links", {})
            if not isinstance(links, dict):
                raise ConfluenceAPIError("Invalid Confluence child pagination response")
            next_link = links.get("next")
            if next_link is None or next_link == "":
                break
            if not isinstance(next_link, str):
                raise ConfluenceAPIError("Invalid Confluence child pagination response")

            # 从 next link 提取 cursor
            parsed = urlparse(next_link)
            qs = parse_qs(parsed.query)
            cursor = qs.get("cursor", [None])[0]
            if not cursor or cursor in seen_cursors:
                raise ConfluenceAPIError("Incomplete Confluence child pagination")
            seen_cursors.add(cursor)

        return all_children

    async def list_space_pages(
        self,
        space_id: str,
        status: str = "current",
        limit: int = 25,
        cursor: str | None = None,
        body_format: str | None = None,
    ) -> tuple[list[dict[str, Any]], str | None]:
        """
        列出空间中的页面 (分页)

        Args:
            space_id: 空间 ID
            status: 页面状态 (current | archived | trashed)
            limit: 每页数量 (最大 250)
            cursor: 分页游标
            body_format: 是否获取内容 (storage | atlas_doc_format)

        Returns:
            (页面列表, 下一页游标)
        """
        params: dict[str, Any] = {
            "status": status,
            "limit": min(limit, 250),
        }
        if cursor:
            params["cursor"] = cursor
        if body_format:
            params["body-format"] = body_format

        data = await self._request("GET", f"/spaces/{space_id}/pages", params=params)
        pages = data.get("results", [])

        # 提取下一页游标
        next_cursor = None
        links = data.get("_links", {})
        if "next" in links:
            next_url = links["next"]
            parsed = urlparse(next_url)
            qs = parse_qs(parsed.query)
            next_cursor = qs.get("cursor", [None])[0]

        return pages, next_cursor

    async def iter_space_pages(
        self,
        space_id: str,
        status: str = "current",
        batch_size: int = 25,
        body_format: str | None = None,
    ) -> AsyncIterator[dict[str, Any]]:
        """
        迭代空间中的所有页面

        Args:
            space_id: 空间 ID
            status: 页面状态
            batch_size: 每批数量
            body_format: 是否获取内容

        Yields:
            页面数据字典
        """
        cursor = None
        while True:
            pages, cursor = await self.list_space_pages(
                space_id=space_id,
                status=status,
                limit=batch_size,
                cursor=cursor,
                body_format=body_format,
            )
            for page in pages:
                yield page
            if not cursor:
                break

    # ============ Space Operations ============

    async def get_space(self, space_id: str) -> ConfluenceSpace:
        """
        获取空间信息

        Args:
            space_id: 空间 ID

        Returns:
            ConfluenceSpace 对象
        """
        data = await self._request("GET", f"/spaces/{space_id}")
        return ConfluenceSpace(
            space_id=str(data.get("id")),
            space_key=data.get("key", ""),
            name=data.get("name", ""),
            type=data.get("type", "global"),
            status=data.get("status", "current"),
            homepage_id=data.get("homepageId"),
            description=data.get("description", {}).get("plain", {}).get("value"),
        )

    async def get_space_by_key(self, space_key: str) -> ConfluenceSpace:
        """
        根据 Space Key 获取空间

        Args:
            space_key: 空间 Key (如 "ENG")

        Returns:
            ConfluenceSpace 对象

        Raises:
            ConfluenceAPIError: 空间不存在
        """
        data = await self._request("GET", "/spaces", params={"keys": space_key})
        spaces = data.get("results", [])
        if not spaces:
            raise ConfluenceAPIError(f"Space not found: {space_key}")

        space_data = spaces[0]
        return ConfluenceSpace(
            space_id=str(space_data.get("id")),
            space_key=space_data.get("key", ""),
            name=space_data.get("name", ""),
            type=space_data.get("type", "global"),
            status=space_data.get("status", "current"),
            homepage_id=space_data.get("homepageId"),
        )

    async def list_spaces(
        self,
        type_filter: str | None = None,
        status: str = "current",
        limit: int = 25,
    ) -> list[ConfluenceSpace]:
        """
        列出所有可访问的空间

        Args:
            type_filter: 类型过滤 (global | personal)
            status: 状态过滤
            limit: 返回数量

        Returns:
            空间列表
        """
        params: dict[str, Any] = {"status": status, "limit": limit}
        if type_filter:
            params["type"] = type_filter

        data = await self._request("GET", "/spaces", params=params)
        return [
            ConfluenceSpace(
                space_id=str(s.get("id")),
                space_key=s.get("key", ""),
                name=s.get("name", ""),
                type=s.get("type", "global"),
                status=s.get("status", "current"),
                homepage_id=s.get("homepageId"),
            )
            for s in data.get("results", [])
        ]

    # ============ Search ============

    async def search_pages(
        self,
        cql: str,
        limit: int = 25,
        start: int = 0,
    ) -> list[dict[str, Any]]:
        """
        使用 CQL 搜索页面 (使用 v1 API)

        Args:
            cql: Confluence Query Language 查询
            limit: 返回数量
            start: 起始位置

        Returns:
            搜索结果列表

        Example CQL:
            - space=ENG AND type=page
            - lastModified > "2024-01-01"
            - title ~ "FAQ"
        """
        data = await self._request(
            "GET",
            "/content/search",
            params={"cql": cql, "limit": limit, "start": start},
            use_v1=True,
        )
        return data.get("results", [])

    # ============ URL Parsing ============

    @staticmethod
    def parse_page_id_from_url(url: str) -> str:
        """
        从 URL 中解析页面 ID

        支持格式：
        - /wiki/spaces/SPACE/pages/12345/Title
        - /wiki/spaces/SPACE/pages/12345

        Args:
            url: Confluence URL

        Returns:
            页面 ID

        Raises:
            ValueError: 无法解析 URL
        """
        parsed = urlparse(url)
        path = parsed.path

        # 格式: /wiki/spaces/SPACE/pages/12345/Title
        pattern = r"/wiki/spaces/[^/]+/pages/(\d+)"
        match = re.search(pattern, path)
        if match:
            return match.group(1)

        # 格式: /pages/12345
        pattern2 = r"/pages/(\d+)"
        match2 = re.search(pattern2, path)
        if match2:
            return match2.group(1)

        raise ValueError(f"Cannot parse page ID from URL: {url}")

    @staticmethod
    def parse_space_key_from_url(url: str) -> str | None:
        """
        从 URL 中解析 Space Key

        Args:
            url: Confluence URL

        Returns:
            Space Key 或 None
        """
        parsed = urlparse(url)
        path = parsed.path

        # 格式: /wiki/spaces/SPACE/...
        pattern = r"/wiki/spaces/([^/]+)"
        match = re.search(pattern, path)
        if match:
            return match.group(1)

        return None

    @staticmethod
    def parse_domain_from_url(url: str) -> str:
        """
        从 URL 中解析域名

        Args:
            url: Confluence URL

        Returns:
            域名 (如 'yourcompany.atlassian.net')
        """
        parsed = urlparse(url)
        return parsed.netloc

    # ============ Incremental Sync ============

    async def search_pages_modified_since(
        self,
        space_key: str,
        since: datetime,
        limit: int = 100,
    ) -> list[ConfluencePage]:
        """
        搜索指定日期之后修改的页面（用于增量同步）

        使用 CQL 查询：space=KEY AND type=page AND lastModified > "YYYY-MM-DD HH:mm"

        Args:
            space_key: 空间 Key
            since: 起始时间
            limit: 返回数量限制

        Returns:
            修改过的页面列表
        """
        # 格式化时间为 CQL 支持的格式
        since_str = since.strftime("%Y-%m-%d %H:%M")
        # Escape space_key to prevent CQL injection
        safe_space_key = _escape_cql_value(space_key)
        cql = f'space={safe_space_key} AND type=page AND lastModified > "{since_str}"'

        logger.info(f"Searching pages modified since {since_str} in space {space_key}")

        all_pages: list[ConfluencePage] = []
        start = 0

        while True:
            data = await self._request(
                "GET",
                "/content/search",
                params={"cql": cql, "limit": limit, "start": start, "expand": "version"},
                use_v1=True,
            )

            results = data.get("results", [])
            if not results:
                break

            for item in results:
                # 从 v1 API 结果解析页面基本信息
                version_info = item.get("_expandable", {}).get("version") or item.get("version", {})
                version_num = 1
                updated_at = None

                if isinstance(version_info, dict):
                    version_num = version_info.get("number", 1)
                    updated_at = version_info.get("when")

                page = ConfluencePage(
                    page_id=str(item.get("id")),
                    space_key=space_key,
                    title=item.get("title", ""),
                    version=version_num,
                    body_storage="",  # 需要单独获取
                    updated_at=updated_at,
                )
                all_pages.append(page)

            # 检查是否有更多结果
            total_size = data.get("totalSize", 0)
            start += len(results)
            if start >= total_size or len(results) < limit:
                break

        logger.info(f"Found {len(all_pages)} pages modified since {since_str}")
        return all_pages

    async def get_deleted_pages_in_space(
        self,
        space_key: str,
        limit: int = 100,
    ) -> list[dict[str, Any]]:
        """
        获取空间中已删除的页面（用于同步删除操作）

        Args:
            space_key: 空间 Key
            limit: 返回数量限制

        Returns:
            已删除页面的 ID 列表
        """
        # Escape space_key to prevent CQL injection
        safe_space_key = _escape_cql_value(space_key)
        cql = f"space={safe_space_key} AND type=page"

        data = await self._request(
            "GET",
            "/content/search",
            params={"cql": cql, "limit": limit, "status": "trashed"},
            use_v1=True,
        )

        return [
            {"page_id": str(item.get("id")), "title": item.get("title", "")}
            for item in data.get("results", [])
        ]

    # ============ Attachment Operations ============

    async def get_page_attachments(
        self,
        page_id: str,
        media_type_filter: str | None = None,
        limit: int = 50,
    ) -> list[ConfluenceAttachment]:
        """
        获取页面的所有附件

        Args:
            page_id: 页面 ID
            media_type_filter: 媒体类型过滤 (如 "image/")
            limit: 返回数量限制

        Returns:
            附件列表
        """
        all_attachments: list[ConfluenceAttachment] = []
        cursor: str | None = None

        while True:
            params: dict[str, Any] = {"limit": min(limit, 250)}
            if cursor:
                params["cursor"] = cursor
            if media_type_filter:
                params["mediaType"] = media_type_filter

            try:
                data = await self._request(
                    "GET",
                    f"/pages/{page_id}/attachments",
                    params=params,
                )
            except ConfluenceAPIError as e:
                if e.status_code == 404:
                    logger.warning(f"Page {page_id} not found or no attachments")
                    return []
                raise

            results = data.get("results", [])
            for item in results:
                attachment = ConfluenceAttachment(
                    attachment_id=str(item.get("id")),
                    page_id=page_id,
                    filename=item.get("title", ""),
                    media_type=item.get("mediaType", "application/octet-stream"),
                    file_size=item.get("fileSize", 0),
                    download_link=item.get("downloadLink", ""),
                    title=item.get("title"),
                    created_at=item.get("createdAt"),
                    updated_at=item.get("version", {}).get("createdAt"),
                    comment=item.get("comment"),
                )
                all_attachments.append(attachment)

            # 检查分页
            next_link = data.get("_links", {}).get("next")
            if not next_link or len(results) < limit:
                break

            parsed = urlparse(next_link)
            qs = parse_qs(parsed.query)
            cursor = qs.get("cursor", [None])[0]
            if not cursor:
                break

        return all_attachments

    async def get_page_image_attachments(
        self,
        page_id: str,
        embeddable_only: bool = True,
    ) -> list[ConfluenceAttachment]:
        """
        获取页面的图片附件

        Args:
            page_id: 页面 ID
            embeddable_only: 是否只返回可嵌入的图片（大小限制内）

        Returns:
            图片附件列表
        """
        attachments = await self.get_page_attachments(page_id)

        if embeddable_only:
            return [a for a in attachments if a.is_embeddable_image]
        else:
            return [a for a in attachments if a.is_image]

    async def download_attachment(
        self,
        attachment: ConfluenceAttachment,
    ) -> bytes:
        """
        下载附件内容

        Args:
            attachment: 附件对象

        Returns:
            附件二进制内容

        Raises:
            ConfluenceAPIError: 下载失败
        """
        if not attachment.download_link:
            raise ConfluenceAPIError(f"No download link for attachment {attachment.attachment_id}")

        client = await self._get_client()

        # download_link 可能是相对路径或完整 URL
        # Confluence Cloud API 返回的路径缺少 /wiki 前缀
        if attachment.download_link.startswith("http"):
            url = attachment.download_link
        elif attachment.download_link.startswith("/wiki"):
            url = f"https://{self.credentials.domain}{attachment.download_link}"
        elif attachment.download_link.startswith("/download"):
            # 添加 /wiki 前缀
            url = f"https://{self.credentials.domain}/wiki{attachment.download_link}"
        else:
            url = f"https://{self.credentials.domain}/wiki{attachment.download_link}"

        logger.info(f"Downloading attachment {attachment.filename} from {url}")

        try:
            # 需要跟随重定向
            resp = await client.get(url, follow_redirects=True)
            if resp.status_code >= 400:
                raise ConfluenceAPIError(
                    f"Failed to download attachment: {resp.status_code}",
                    status_code=resp.status_code,
                    response_body=resp.text[:500] if resp.text else None,
                )
            return resp.content
        except httpx.RequestError as e:
            raise ConfluenceAPIError(f"Download request failed: {e}")
