"""
Confluence Image Processor.

Handles downloading, storing, and processing images from Confluence pages.
Integrates with storage service (S3/OSS) and VLM service for generating
image descriptions.

Key features:
- Download images from Confluence attachments
- Store images in S3/OSS/local storage
- Generate VLM descriptions for text-based RAG retrieval
- Return ImageSegment with VLM description (embedding generated separately by sync_service)

Architecture (Text-First RAG):
- Images are stored in S3, VLM descriptions are embedded as text using Gemini
- No multimodal embedding - all content uses the same text embedding model
- Image retrieval via VLM description, original images served via presigned URL
"""

from __future__ import annotations

import asyncio
import logging
import uuid
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from .models import ConfluenceAttachment, ImageSegment
from .parser import extract_embeddable_images

if TYPE_CHECKING:
    from ...storage.image_storage import ImageStorageService
    from ..vlm_service import DashScopeVLMService
    from .client import ConfluenceClient

logger = logging.getLogger(__name__)


# Max image size for DashScope multimodal API (3MB)
MAX_IMAGE_SIZE_BYTES = 3 * 1024 * 1024

# VLM call timeout in seconds (prevent hanging on slow/unresponsive VLM service)
VLM_TIMEOUT_SECONDS = 60


@dataclass
class ImageProcessingResult:
    """Result of processing images from a Confluence page."""

    page_id: str
    document_id: str
    total_images: int
    processed_images: int
    skipped_images: int
    failed_images: int
    segments: list[ImageSegment]
    errors: list[str]
    skipped_attachments: list[dict[str, Any]] | None = None

    @property
    def success_rate(self) -> float:
        if self.total_images == 0:
            return 1.0
        return self.processed_images / self.total_images


class ConfluenceImageProcessor:
    """
    Confluence 图片处理器

    负责从 Confluence 页面下载图片、存储到对象存储、生成 VLM 描述。

    Workflow:
    1. Get page attachments from Confluence API
    2. Filter for embeddable image types (JPEG, PNG, etc.)
    3. Download images (respecting size limits)
    4. Upload to storage (S3/OSS/local)
    5. Generate VLM descriptions for RAG retrieval (if VLM service enabled)
    6. Return ImageSegment objects (embedding generated separately by sync_service)

    Text-First RAG Strategy:
    - VLM descriptions are embedded as text using Gemini embedding
    - Images are stored in S3 and served via presigned URLs
    - Unified text embedding space for both text and image descriptions
    """

    def __init__(
        self,
        confluence_client: ConfluenceClient,
        storage_service: ImageStorageService,
        multimodal_embedding: Any | None = None,
        vlm_service: DashScopeVLMService | None = None,
        max_image_size: int = MAX_IMAGE_SIZE_BYTES,
        max_images_per_page: int = 50,
        generate_vlm_descriptions: bool = True,
        max_concurrent_vlm: int = 5,
        max_concurrent_upload: int = 10,
    ):
        """
        初始化图片处理器

        Args:
            confluence_client: Confluence API 客户端
            storage_service: 图片存储服务
            vlm_service: VLM 图片描述服务 (可选)
            max_image_size: 单张图片最大大小 (bytes)
            max_images_per_page: 每页最大处理图片数
            generate_vlm_descriptions: 是否生成 VLM 描述
            max_concurrent_vlm: VLM 最大并发数 (默认5)
            max_concurrent_upload: 存储上传最大并发数 (默认10)
        """
        self.client = confluence_client
        self.storage = storage_service
        self.storage_service = storage_service
        self.multimodal_embedding = multimodal_embedding
        self.vlm_service = vlm_service
        self.max_image_size = max_image_size
        self.max_images_per_page = max_images_per_page
        self.generate_vlm_descriptions = generate_vlm_descriptions
        self.max_concurrent_vlm = max_concurrent_vlm
        self.max_concurrent_upload = max_concurrent_upload

        if vlm_service:
            logger.info(
                f"VLM service enabled for image description generation "
                f"(max_concurrent={max_concurrent_vlm})"
            )

    async def process_page_images(
        self,
        page_id: str,
        document_id: str,
        tenant_id: str,
        page_content: str | None = None,
        page_title: str | None = None,
        generate_embeddings: bool = False,
    ) -> ImageProcessingResult:
        """
        处理页面中的图片

        Args:
            page_id: Confluence 页面 ID
            document_id: 知识库文档 ID
            tenant_id: 租户 ID
            page_content: 页面内容 (用于提取图片上下文)
            page_title: 页面标题 (用于 VLM 上下文)

        Returns:
            ImageProcessingResult 包含处理结果
            - segments: ImageSegment 列表，包含 VLM 描述但不包含 embedding
            - embedding 将由 sync_service 统一使用 Gemini 生成

        Notes:
            On critical failure, uploaded images are rolled back (deleted)
            to prevent orphan files in storage.
        """
        errors: list[str] = []
        segments: list[ImageSegment] = []
        skipped = 0
        failed = 0
        # Track uploaded URLs for rollback on critical failure
        uploaded_storage_urls: list[str] = []

        try:
            # 1. Get image attachments from Confluence
            attachments = await self.client.get_page_image_attachments(
                page_id=page_id, embeddable_only=True
            )

            # The source-generation coordinator fingerprints the same
            # deterministic policy before it takes the document owner lease.
            # Keep this selection order identical so configured size/count
            # limits do not look like attachment-manifest drift.
            attachments = sorted(
                (
                    attachment
                    for attachment in attachments
                    if int(attachment.file_size or 0) <= self.max_image_size
                ),
                key=lambda attachment: (
                    str(attachment.attachment_id),
                    str(attachment.filename),
                ),
            )

            # 2. Limit number of images
            if len(attachments) > self.max_images_per_page:
                logger.warning(
                    f"Page {page_id} has {len(attachments)} images, "
                    f"limiting to {self.max_images_per_page}"
                )
                attachments = attachments[: self.max_images_per_page]

            total_images = len(attachments)

            if total_images == 0:
                return ImageProcessingResult(
                    page_id=page_id,
                    document_id=document_id,
                    total_images=0,
                    processed_images=0,
                    skipped_images=0,
                    failed_images=0,
                    segments=[],
                    errors=[],
                )

            # 3. Extract image references from content for context (keep order)
            image_contexts: dict[str, list[dict[str, Any]]] = {}
            if page_content:
                image_refs = extract_embeddable_images(page_content)
                for idx, ref in enumerate(image_refs):
                    key = ref.attachment_id or ref.filename
                    if not key:
                        continue
                    image_contexts.setdefault(key, []).append(
                        {
                            "context_text": ref.context_text or "",
                            "context_index": idx,
                            "alt_text": ref.alt_text,
                            "title": ref.title,
                        }
                    )

            # 4. Process images concurrently with semaphore-based rate limiting
            vlm_semaphore = asyncio.Semaphore(self.max_concurrent_vlm)
            upload_semaphore = asyncio.Semaphore(self.max_concurrent_upload)

            async def process_with_error_handling(
                attachment: ConfluenceAttachment,
            ) -> tuple[ConfluenceAttachment, ImageSegment | None, str | None]:
                """Process single image with error handling."""
                try:
                    context_info: dict[str, Any] = {}
                    context_key = attachment.attachment_id or attachment.filename
                    info_list = image_contexts.get(context_key)
                    if not info_list and attachment.filename:
                        info_list = image_contexts.get(attachment.filename)
                    if info_list:
                        context_info = info_list.pop(0) or {}

                    segment = await self._process_single_image_concurrent(
                        attachment=attachment,
                        document_id=document_id,
                        tenant_id=tenant_id,
                        context=context_info.get("context_text", ""),
                        context_index=context_info.get("context_index"),
                        context_alt=context_info.get("alt_text"),
                        context_title=context_info.get("title"),
                        page_title=page_title,
                        generate_embedding=generate_embeddings,
                        vlm_semaphore=vlm_semaphore,
                        upload_semaphore=upload_semaphore,
                    )
                    # Track uploaded URL for potential rollback
                    if segment and segment.storage_url:
                        uploaded_storage_urls.append(segment.storage_url)
                    return attachment, segment, None
                except Exception as e:
                    error_msg = f"Failed to process image {attachment.filename}: {e}"
                    logger.error(error_msg)
                    return attachment, None, error_msg

            # Launch all image processing tasks concurrently
            tasks = [process_with_error_handling(att) for att in attachments]
            results = await asyncio.gather(*tasks)

            # Collect results
            skipped_attachments: list[dict[str, Any]] = []
            for attachment, segment, error in results:
                if error:
                    failed += 1
                    errors.append(error)
                elif segment:
                    segments.append(segment)
                else:
                    skipped += 1
                    skipped_attachments.append(
                        {
                            "attachment_id": attachment.attachment_id,
                            "filename": attachment.filename,
                            "media_type": attachment.media_type,
                            "file_size": attachment.file_size,
                            "updated_at": attachment.updated_at,
                            "reason": "empty_or_actual_size_exceeded",
                        }
                    )

            processed = len(segments)
            vlm_count = sum(1 for s in segments if s.vlm_description)
            logger.info(
                f"Processed {processed}/{total_images} images for page {page_id} "
                f"(skipped={skipped}, failed={failed}, vlm_descriptions={vlm_count})"
            )

            return ImageProcessingResult(
                page_id=page_id,
                document_id=document_id,
                total_images=total_images,
                processed_images=processed,
                skipped_images=skipped,
                failed_images=failed,
                segments=segments,
                errors=errors,
                skipped_attachments=skipped_attachments,
            )

        except Exception as e:
            error_msg = f"Failed to process images for page {page_id}: {e}"
            logger.error(error_msg)

            # Rollback: delete any images that were uploaded before the failure
            if uploaded_storage_urls and self.storage_service:
                rollback_errors = await self._rollback_uploaded_images(
                    uploaded_storage_urls, document_id, tenant_id
                )
                if rollback_errors:
                    errors.extend(rollback_errors)

            return ImageProcessingResult(
                page_id=page_id,
                document_id=document_id,
                total_images=0,
                processed_images=0,
                skipped_images=0,
                failed_images=1,
                segments=[],
                errors=[error_msg] + errors,
            )

    async def _rollback_uploaded_images(
        self,
        storage_urls: list[str],
        document_id: str,
        tenant_id: str,
    ) -> list[str]:
        """
        Rollback uploaded images by deleting them from storage.

        Args:
            storage_urls: List of storage URLs to delete
            document_id: Document ID (for logging)
            tenant_id: Tenant ID (for logging)

        Returns:
            List of error messages (empty if all deletions succeeded)
        """
        errors: list[str] = []
        deleted_count = 0

        logger.info(f"Rolling back {len(storage_urls)} uploaded images for document {document_id}")

        for url in storage_urls:
            try:
                # Extract storage key from URL
                # For file:// URLs, extract the path
                # For S3/OSS URLs, extract the key from the path
                if url.startswith("file://"):
                    from urllib.parse import unquote, urlparse

                    parsed = urlparse(url)
                    # Get the path and try to delete
                    file_path = unquote(parsed.path)
                    import os

                    if os.path.exists(file_path):
                        os.remove(file_path)
                        deleted_count += 1
                elif self.storage_service:
                    # For cloud storage, use the storage service
                    # Try to extract key from URL and delete
                    # This is a best-effort cleanup
                    from urllib.parse import urlparse

                    parsed = urlparse(url)
                    key = parsed.path.lstrip("/")
                    if key:
                        await self.storage_service._backend.delete(key)
                        deleted_count += 1
            except Exception as e:
                error_msg = f"Failed to rollback image {url}: {e}"
                logger.warning(error_msg)
                errors.append(error_msg)

        logger.info(
            f"Rollback complete: deleted {deleted_count}/{len(storage_urls)} images "
            f"(errors={len(errors)})"
        )

        return errors

    async def _process_single_image(
        self,
        attachment: ConfluenceAttachment,
        document_id: str,
        tenant_id: str,
        context: str = "",
        context_index: int | None = None,
        context_alt: str | None = None,
        context_title: str | None = None,
        page_title: str | None = None,
        generate_embedding: bool = False,
    ) -> ImageSegment | None:
        """
        处理单张图片

        Args:
            attachment: Confluence 附件信息
            document_id: 文档 ID
            tenant_id: 租户 ID
            context: 图片上下文文本
            page_title: 页面标题（用于 VLM 上下文）

        Returns:
            ImageSegment 或 None (如果跳过)

        Note:
            返回的 ImageSegment 包含 VLM 描述但不包含 embedding
            embedding 由 sync_service 统一使用 Gemini 生成
        """
        # Check file size
        if attachment.file_size > self.max_image_size:
            logger.info(
                f"Skipping image {attachment.filename}: "
                f"size {attachment.file_size} exceeds limit {self.max_image_size}"
            )
            return None

        # Download image
        logger.debug(f"Downloading image: {attachment.filename}")
        image_bytes = await self.client.download_attachment(attachment)

        if not image_bytes:
            logger.warning(f"Empty image data for {attachment.filename}")
            return None

        # Double-check actual size
        if len(image_bytes) > self.max_image_size:
            logger.info(
                f"Skipping image {attachment.filename}: "
                f"actual size {len(image_bytes)} exceeds limit"
            )
            return None

        # Generate segment ID
        segment_id = str(uuid.uuid4())

        # Upload to storage
        storage_url = await self.storage.upload_image(
            tenant_id=tenant_id,
            document_id=document_id,
            attachment_id=attachment.attachment_id,
            filename=attachment.filename,
            content=image_bytes,
            content_type=attachment.media_type,
            metadata={
                "confluence_attachment_id": attachment.attachment_id,
                "page_id": attachment.page_id,
                "original_filename": attachment.filename,
            },
        )

        # Generate VLM description if service is available
        vlm_description: str | None = None
        if self.generate_vlm_descriptions and self.vlm_service:
            try:
                # Detect image type for better prompts
                image_type = self._detect_image_type(attachment.filename)

                # Build context for VLM
                vlm_context = page_title or ""
                if context:
                    vlm_context = f"{vlm_context} - {context}" if vlm_context else context

                # Add timeout to prevent hanging on slow/unresponsive VLM service
                description_result = await asyncio.wait_for(
                    self.vlm_service.describe_image(
                        image_bytes=image_bytes,
                        image_type=image_type,
                        context=vlm_context,
                    ),
                    timeout=VLM_TIMEOUT_SECONDS,
                )
                vlm_description = description_result.description
                logger.debug(
                    f"Generated VLM description for {attachment.filename}: "
                    f"{len(vlm_description)} chars, {description_result.tokens_used} tokens"
                )
            except asyncio.TimeoutError:
                logger.warning(
                    f"VLM description timed out for {attachment.filename} "
                    f"after {VLM_TIMEOUT_SECONDS}s"
                )
                # Continue without VLM description - image still stored
            except Exception as e:
                logger.warning(f"Failed to generate VLM description for {attachment.filename}: {e}")
                # Continue without VLM description - image still stored

        metadata = {
            "confluence_attachment_id": attachment.attachment_id,
            "page_id": attachment.page_id,
            "attachment_updated_at": attachment.updated_at,  # For change detection
            "attachment_file_size": attachment.file_size,
        }
        if context_index is not None:
            metadata["context_index"] = context_index
        if context:
            metadata["context_text"] = context
        if context_alt:
            metadata["context_alt"] = context_alt
        if context_title:
            metadata["context_title"] = context_title

        embedding: list[float] | None = None
        if generate_embedding and self.multimodal_embedding:
            try:
                if context and hasattr(self.multimodal_embedding, "embed_image_and_text"):
                    embedding = await self.multimodal_embedding.embed_image_and_text(
                        image_bytes,
                        context,
                    )
                elif hasattr(self.multimodal_embedding, "embed_images"):
                    vectors = await self.multimodal_embedding.embed_images([image_bytes])
                    embedding = vectors[0] if vectors else None
            except Exception as e:
                logger.warning(f"Failed to generate embedding for {attachment.filename}: {e}")

        # Create segment with VLM description
        segment = ImageSegment(
            segment_id=segment_id,
            document_id=document_id,
            attachment_id=attachment.attachment_id,
            filename=attachment.filename,
            media_type=attachment.media_type,
            file_size=len(image_bytes),
            storage_url=storage_url,
            vector_id=None,  # Will be set after Gemini embedding in sync_service
            context_text=context,
            vlm_description=vlm_description,
            embedding=embedding,
            metadata=metadata,
        )

        logger.debug(f"Created image segment: {segment_id} for {attachment.filename}")
        return segment

    async def _process_single_image_concurrent(
        self,
        attachment: ConfluenceAttachment,
        document_id: str,
        tenant_id: str,
        context: str = "",
        context_index: int | None = None,
        context_alt: str | None = None,
        context_title: str | None = None,
        page_title: str | None = None,
        generate_embedding: bool = False,
        vlm_semaphore: asyncio.Semaphore | None = None,
        upload_semaphore: asyncio.Semaphore | None = None,
    ) -> ImageSegment | None:
        """
        处理单张图片（并发版本，带信号量控制）

        Args:
            attachment: Confluence 附件信息
            document_id: 文档 ID
            tenant_id: 租户 ID
            context: 图片上下文文本
            page_title: 页面标题（用于 VLM 上下文）
            vlm_semaphore: VLM 调用信号量
            upload_semaphore: 上传操作信号量

        Returns:
            ImageSegment 或 None (如果跳过)

        Note:
            返回的 ImageSegment 包含 VLM 描述但不包含 embedding
            embedding 由 sync_service 统一使用 Gemini 生成
        """
        # Check file size
        if attachment.file_size > self.max_image_size:
            logger.info(
                f"Skipping image {attachment.filename}: "
                f"size {attachment.file_size} exceeds limit {self.max_image_size}"
            )
            return None

        # Download image (no semaphore needed, usually fast)
        logger.debug(f"Downloading image: {attachment.filename}")
        image_bytes = await self.client.download_attachment(attachment)

        if not image_bytes:
            logger.warning(f"Empty image data for {attachment.filename}")
            return None

        # Double-check actual size
        if len(image_bytes) > self.max_image_size:
            logger.info(
                f"Skipping image {attachment.filename}: "
                f"actual size {len(image_bytes)} exceeds limit"
            )
            return None

        # Generate segment ID
        segment_id = str(uuid.uuid4())

        # Upload to storage with semaphore
        async def upload_with_limit():
            if upload_semaphore:
                async with upload_semaphore:
                    return await self.storage.upload_image(
                        tenant_id=tenant_id,
                        document_id=document_id,
                        attachment_id=attachment.attachment_id,
                        filename=attachment.filename,
                        content=image_bytes,
                        content_type=attachment.media_type,
                        metadata={
                            "confluence_attachment_id": attachment.attachment_id,
                            "page_id": attachment.page_id,
                            "original_filename": attachment.filename,
                        },
                    )
            else:
                return await self.storage.upload_image(
                    tenant_id=tenant_id,
                    document_id=document_id,
                    attachment_id=attachment.attachment_id,
                    filename=attachment.filename,
                    content=image_bytes,
                    content_type=attachment.media_type,
                    metadata={
                        "confluence_attachment_id": attachment.attachment_id,
                        "page_id": attachment.page_id,
                        "original_filename": attachment.filename,
                    },
                )

        storage_url = await upload_with_limit()

        # Generate VLM description with semaphore and timeout
        vlm_description: str | None = None
        if self.generate_vlm_descriptions and self.vlm_service:

            async def vlm_with_limit():
                image_type = self._detect_image_type(attachment.filename)
                vlm_context = page_title or ""
                if context:
                    vlm_context = f"{vlm_context} - {context}" if vlm_context else context

                # Add timeout to prevent hanging on slow/unresponsive VLM service
                vlm_call = self.vlm_service.describe_image(
                    image_bytes=image_bytes,
                    image_type=image_type,
                    context=vlm_context,
                )

                if vlm_semaphore:
                    async with vlm_semaphore:
                        return await asyncio.wait_for(vlm_call, timeout=VLM_TIMEOUT_SECONDS)
                else:
                    return await asyncio.wait_for(vlm_call, timeout=VLM_TIMEOUT_SECONDS)

            try:
                description_result = await vlm_with_limit()
                vlm_description = description_result.description
                logger.debug(
                    f"Generated VLM description for {attachment.filename}: "
                    f"{len(vlm_description)} chars, {description_result.tokens_used} tokens"
                )
            except asyncio.TimeoutError:
                logger.warning(
                    f"VLM description timed out for {attachment.filename} "
                    f"after {VLM_TIMEOUT_SECONDS}s"
                )
            except Exception as e:
                logger.warning(f"Failed to generate VLM description for {attachment.filename}: {e}")

        metadata = {
            "confluence_attachment_id": attachment.attachment_id,
            "page_id": attachment.page_id,
            "attachment_updated_at": attachment.updated_at,
            "attachment_file_size": attachment.file_size,
        }
        if context_index is not None:
            metadata["context_index"] = context_index
        if context:
            metadata["context_text"] = context
        if context_alt:
            metadata["context_alt"] = context_alt
        if context_title:
            metadata["context_title"] = context_title

        embedding: list[float] | None = None
        if generate_embedding and self.multimodal_embedding:
            try:
                if context and hasattr(self.multimodal_embedding, "embed_image_and_text"):
                    embedding = await self.multimodal_embedding.embed_image_and_text(
                        image_bytes,
                        context,
                    )
                elif hasattr(self.multimodal_embedding, "embed_images"):
                    vectors = await self.multimodal_embedding.embed_images([image_bytes])
                    embedding = vectors[0] if vectors else None
            except Exception as e:
                logger.warning(f"Failed to generate embedding for {attachment.filename}: {e}")

        # Create segment with VLM description
        segment = ImageSegment(
            segment_id=segment_id,
            document_id=document_id,
            attachment_id=attachment.attachment_id,
            filename=attachment.filename,
            media_type=attachment.media_type,
            file_size=len(image_bytes),
            storage_url=storage_url,
            vector_id=None,  # Will be set after Gemini embedding in sync_service
            context_text=context,
            vlm_description=vlm_description,
            embedding=embedding,
            metadata=metadata,
        )

        logger.debug(f"Created image segment: {segment_id} for {attachment.filename}")
        return segment

    def _detect_image_type(self, filename: str) -> str:
        """
        Detect image type based on filename for better VLM prompts.

        Args:
            filename: Image filename

        Returns:
            Image type hint: "table", "chart", "diagram", or "general"
        """
        filename_lower = filename.lower()

        # Table indicators
        table_keywords = ["table", "表格", "matrix", "grid", "fee", "price", "rate"]
        if any(kw in filename_lower for kw in table_keywords):
            return "table"

        # Chart indicators
        chart_keywords = ["chart", "graph", "图表", "diagram", "flow", "架构", "架构图"]
        if any(kw in filename_lower for kw in chart_keywords):
            return "chart"

        # Diagram indicators
        diagram_keywords = ["diagram", "流程", "sequence", "uml", "erd"]
        if any(kw in filename_lower for kw in diagram_keywords):
            return "diagram"

        return "general"

    async def delete_document_images(
        self,
        document_id: str,
        tenant_id: str,
    ) -> int:
        """
        删除文档的所有图片

        Args:
            document_id: 文档 ID
            tenant_id: 租户 ID

        Returns:
            删除的图片数量
        """
        try:
            count = await self.storage.delete_document_images(tenant_id, document_id)
            logger.info(f"Deleted {count} images for document {document_id}")
            return count
        except Exception as e:
            logger.error(f"Failed to delete images for document {document_id}: {e}")
            return 0

    async def reprocess_page_images(
        self,
        page_id: str,
        document_id: str,
        tenant_id: str,
        page_content: str | None = None,
    ) -> ImageProcessingResult:
        """
        重新处理页面图片（先删除旧图片）

        Args:
            page_id: 页面 ID
            document_id: 文档 ID
            tenant_id: 租户 ID
            page_content: 页面内容

        Returns:
            处理结果
        """
        # Delete existing images
        await self.delete_document_images(document_id, tenant_id)

        # Process new images
        return await self.process_page_images(
            page_id=page_id,
            document_id=document_id,
            tenant_id=tenant_id,
            page_content=page_content,
        )


async def create_image_processor(
    confluence_client: ConfluenceClient,
    storage_service: ImageStorageService,
    dashscope_api_key: str | None = None,
    vlm_model: str = "qwen-vl-max",
    max_image_size: int = MAX_IMAGE_SIZE_BYTES,
    enable_vlm_descriptions: bool = True,
) -> ConfluenceImageProcessor:
    """
    创建图片处理器实例

    Text-First RAG 架构:
    - 只初始化 VLM 服务用于生成图片描述
    - 不再初始化多模态嵌入（embedding 由 sync_service 统一使用 Gemini 生成）

    Args:
        confluence_client: Confluence 客户端
        storage_service: 存储服务
        dashscope_api_key: DashScope API key (用于 VLM 服务)
        vlm_model: VLM 图片描述模型名称 (default: qwen-vl-max)
        max_image_size: 最大图片大小
        enable_vlm_descriptions: 是否启用 VLM 描述生成

    Returns:
        ConfluenceImageProcessor 实例
    """
    vlm_service = None

    # Initialize VLM service for image descriptions (if API key provided)
    if dashscope_api_key and enable_vlm_descriptions:
        from ..vlm_service import DashScopeVLMService

        vlm_service = DashScopeVLMService(
            api_key=dashscope_api_key,
            model=vlm_model,
        )
        logger.info(f"Initialized VLM service with model: {vlm_model}")

    return ConfluenceImageProcessor(
        confluence_client=confluence_client,
        storage_service=storage_service,
        vlm_service=vlm_service,
        max_image_size=max_image_size,
        generate_vlm_descriptions=enable_vlm_descriptions,
    )
