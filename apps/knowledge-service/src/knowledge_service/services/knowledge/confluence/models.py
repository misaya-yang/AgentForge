"""
Confluence Data Models.

Defines data structures for Confluence entities including pages,
spaces, and synchronization results.
"""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from datetime import datetime
from typing import Any


@dataclass
class ConfluenceCredentials:
    """Confluence 认证凭据"""

    domain: str  # e.g., 'yourcompany.atlassian.net'
    email: str = field(repr=False)
    api_token: str = field(repr=False)

    @property
    def base_url(self) -> str:
        """Wiki base URL"""
        return f"https://{self.domain}/wiki"

    @property
    def api_v2_url(self) -> str:
        """REST API v2 base URL"""
        return f"{self.base_url}/api/v2"

    @property
    def api_v1_url(self) -> str:
        """REST API v1 base URL (for some legacy endpoints)"""
        return f"{self.base_url}/rest/api"


@dataclass
class ConfluencePage:
    """Confluence 页面数据"""

    page_id: str
    space_key: str
    title: str
    version: int
    body_storage: str  # Storage format (XHTML-like)
    body_text: str | None = None  # Plain text (if converted)
    parent_id: str | None = None
    web_url: str | None = None
    author_id: str | None = None
    created_at: str | None = None
    updated_at: str | None = None
    labels: list[str] = field(default_factory=list)
    space_id: str | None = None

    @property
    def content_hash(self) -> str:
        """计算内容哈希用于变更检测"""
        content = f"{self.title}:{self.version}:{self.body_storage}"
        return hashlib.sha256(content.encode()).hexdigest()

    def to_dict(self) -> dict[str, Any]:
        """转换为字典"""
        return {
            "page_id": self.page_id,
            "space_key": self.space_key,
            "title": self.title,
            "version": self.version,
            "body_storage": self.body_storage,
            "body_text": self.body_text,
            "parent_id": self.parent_id,
            "web_url": self.web_url,
            "author_id": self.author_id,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "labels": self.labels,
            "space_id": self.space_id,
            "content_hash": self.content_hash,
        }


@dataclass
class ConfluenceSpace:
    """Confluence 空间数据"""

    space_id: str
    space_key: str
    name: str
    type: str  # global, personal
    status: str  # current, archived
    homepage_id: str | None = None
    description: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """转换为字典"""
        return {
            "space_id": self.space_id,
            "space_key": self.space_key,
            "name": self.name,
            "type": self.type,
            "status": self.status,
            "homepage_id": self.homepage_id,
            "description": self.description,
        }


@dataclass
class SyncResult:
    """同步结果"""

    total_pages: int = 0
    synced_pages: int = 0
    skipped_pages: int = 0
    failed_pages: int = 0
    created_documents: list[str] = field(default_factory=list)
    updated_documents: list[str] = field(default_factory=list)
    deleted_documents: list[str] = field(default_factory=list)
    errors: list[dict[str, Any]] = field(default_factory=list)
    started_at: datetime | None = None
    completed_at: datetime | None = None
    task_id: str | None = None

    @property
    def success_rate(self) -> float:
        """成功率"""
        if self.total_pages == 0:
            return 0.0
        return (self.synced_pages + self.skipped_pages) / self.total_pages

    @property
    def has_errors(self) -> bool:
        """是否有错误"""
        return len(self.errors) > 0 or self.failed_pages > 0

    def to_dict(self) -> dict[str, Any]:
        """转换为字典"""
        return {
            "task_id": self.task_id,
            "total_pages": self.total_pages,
            "synced_pages": self.synced_pages,
            "skipped_pages": self.skipped_pages,
            "failed_pages": self.failed_pages,
            "created_documents": self.created_documents,
            "updated_documents": self.updated_documents,
            "deleted_documents": self.deleted_documents,
            "errors": self.errors,
            "success_rate": self.success_rate,
            "has_errors": self.has_errors,
            "started_at": self.started_at.isoformat() if self.started_at else None,
            "completed_at": self.completed_at.isoformat() if self.completed_at else None,
        }


@dataclass
class ConfluenceConnection:
    """Confluence 连接配置"""

    connection_id: str
    tenant_id: str
    name: str
    domain: str
    email: str = field(repr=False)
    api_token: str = field(repr=False)
    sync_mode: str = "manual"  # manual | polling
    polling_interval_minutes: int = 60
    status: str = "active"  # active | disabled | error
    last_sync_at: datetime | None = None
    last_error: str | None = None
    created_by: str | None = None
    created_at: datetime | None = None
    updated_at: datetime | None = None

    def to_credentials(self, encryption_key: str = "") -> ConfluenceCredentials:
        """Decode at consumption; an envelope must never become a Basic-auth token."""
        from .credentials import connection_credentials

        return connection_credentials(vars(self), encryption_key)


@dataclass
class ConfluenceSpaceBinding:
    """Confluence 空间绑定"""

    binding_id: str
    connection_id: str
    dataset_id: str
    space_key: str
    space_id: str | None = None
    space_name: str | None = None
    include_patterns: list[str] = field(default_factory=list)
    exclude_patterns: list[str] = field(default_factory=list)
    max_depth: int = 10
    include_attachments: bool = False
    include_comments: bool = False
    status: str = "pending"  # pending | syncing | completed | error
    last_sync_at: datetime | None = None
    synced_page_count: int = 0
    total_page_count: int = 0
    last_error: str | None = None
    created_by: str | None = None
    created_at: datetime | None = None
    updated_at: datetime | None = None


@dataclass
class ConfluencePageRecord:
    """Confluence 页面同步记录"""

    id: str
    binding_id: str
    document_id: str | None
    page_id: str
    space_key: str
    title: str
    version: int
    content_hash: str | None = None
    parent_page_id: str | None = None
    depth: int = 0
    status: str = "pending"  # pending | synced | error | deleted
    last_synced_at: datetime | None = None
    confluence_updated_at: datetime | None = None
    error: str | None = None
    labels: list[str] = field(default_factory=list)
    web_url: str | None = None
    author: str | None = None
    created_at: datetime | None = None
    updated_at: datetime | None = None


@dataclass
class ConfluenceSyncTask:
    """Confluence 同步任务"""

    task_id: str
    binding_id: str | None
    page_id: str | None
    task_type: str  # full_sync | incremental_sync | page_sync | page_delete
    priority: int = 0
    status: str = "pending"  # pending | processing | completed | failed
    retry_count: int = 0
    max_retries: int = 3
    progress: float = 0.0
    total_items: int = 0
    processed_items: int = 0
    error: str | None = None
    result: dict[str, Any] | None = None
    created_at: datetime | None = None
    started_at: datetime | None = None
    completed_at: datetime | None = None


@dataclass
class ConfluenceAttachment:
    """Confluence 附件数据"""

    attachment_id: str
    page_id: str
    filename: str
    media_type: str
    file_size: int
    download_link: str
    title: str | None = None
    created_at: str | None = None
    updated_at: str | None = None
    comment: str | None = None

    @property
    def is_image(self) -> bool:
        """判断是否为图片类型"""
        image_types = {
            "image/jpeg",
            "image/png",
            "image/gif",
            "image/bmp",
            "image/webp",
            "image/svg+xml",
        }
        return self.media_type.lower() in image_types

    @property
    def is_embeddable_image(self) -> bool:
        """判断是否可用于嵌入（支持的图片格式且大小合适）"""
        # DashScope multimodal API limits: ≤3 MB
        max_size = 3 * 1024 * 1024  # 3 MB
        embeddable_types = {"image/jpeg", "image/png", "image/bmp", "image/webp"}
        return self.media_type.lower() in embeddable_types and self.file_size <= max_size

    def to_dict(self) -> dict[str, Any]:
        """转换为字典"""
        return {
            "attachment_id": self.attachment_id,
            "page_id": self.page_id,
            "filename": self.filename,
            "media_type": self.media_type,
            "file_size": self.file_size,
            "download_link": self.download_link,
            "title": self.title,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "comment": self.comment,
            "is_image": self.is_image,
            "is_embeddable_image": self.is_embeddable_image,
        }


@dataclass
class ImageSegment:
    """图片段数据（用于向量化存储）"""

    segment_id: str
    document_id: str
    attachment_id: str
    filename: str
    media_type: str
    file_size: int
    storage_url: str  # S3/OSS URL
    vector_id: str | None = None
    alt_text: str | None = None
    ocr_text: str | None = None
    context_text: str | None = None  # Surrounding text from page
    vlm_description: str | None = None  # VLM-generated image description for RAG
    embedding: list[float] | None = None  # Multimodal embedding vector
    metadata: dict[str, Any] = field(default_factory=dict)
    created_at: datetime | None = None

    @property
    def has_embedding(self) -> bool:
        """Check if this segment has an embedding vector"""
        return self.embedding is not None and len(self.embedding) > 0

    @property
    def embedding_dimension(self) -> int:
        """Get embedding dimension"""
        if self.embedding:
            return len(self.embedding)
        return 0

    def to_dict(self) -> dict[str, Any]:
        """转换为字典"""
        return {
            "segment_id": self.segment_id,
            "document_id": self.document_id,
            "attachment_id": self.attachment_id,
            "filename": self.filename,
            "media_type": self.media_type,
            "file_size": self.file_size,
            "storage_url": self.storage_url,
            "vector_id": self.vector_id,
            "alt_text": self.alt_text,
            "ocr_text": self.ocr_text,
            "context_text": self.context_text,
            "has_embedding": self.has_embedding,
            "embedding_dimension": self.embedding_dimension,
            "metadata": self.metadata,
            "created_at": self.created_at.isoformat() if self.created_at else None,
        }
