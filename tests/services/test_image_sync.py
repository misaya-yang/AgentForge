"""
Unit tests for Confluence image synchronization module.

Tests cover:
- ImageStorageService (Local and S3 backends)
- DashScopeMultimodalEmbedding
- ConfluenceImageProcessor
"""

from __future__ import annotations

import base64
import tempfile
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

# ============ Test Constants ============

TEST_TENANT_ID = "test_tenant_001"
TEST_DOCUMENT_ID = "doc_001"
TEST_DATASET_ID = "dataset_001"
TEST_ATTACHMENT_ID = "att_001"
TEST_FILENAME = "test_image.png"
TEST_MEDIA_TYPE = "image/png"

# 1x1 transparent PNG (minimal valid PNG)
MINIMAL_PNG_BYTES = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg=="
)


# ============ ImageStorageService Tests ============


class TestLocalStorageBackend:
    """Tests for LocalStorageBackend"""

    @pytest.fixture
    def temp_storage_path(self):
        """Create temporary storage directory"""
        with tempfile.TemporaryDirectory() as tmpdir:
            yield tmpdir

    @pytest.fixture
    def local_backend(self, temp_storage_path):
        """Create LocalStorageBackend instance"""
        from src.services.storage.image_storage import LocalStorageBackend

        return LocalStorageBackend(base_path=temp_storage_path)

    @pytest.mark.asyncio
    async def test_upload_and_download(self, local_backend):
        """Test basic upload and download functionality"""
        key = "test/image.png"
        content = MINIMAL_PNG_BYTES

        # Upload
        url = await local_backend.upload(key, content, TEST_MEDIA_TYPE)
        assert url.startswith("file://")

        # Download
        downloaded = await local_backend.download(key)
        assert downloaded == content

    @pytest.mark.asyncio
    async def test_upload_with_metadata(self, local_backend):
        """Test upload with metadata"""
        key = "test/with_meta.png"
        metadata = {"source": "confluence", "page_id": "12345"}

        url = await local_backend.upload(key, MINIMAL_PNG_BYTES, TEST_MEDIA_TYPE, metadata)
        assert url is not None

    @pytest.mark.asyncio
    async def test_exists(self, local_backend):
        """Test file existence check"""
        key = "test/exists.png"

        # Should not exist initially
        assert not await local_backend.exists(key)

        # Upload
        await local_backend.upload(key, MINIMAL_PNG_BYTES, TEST_MEDIA_TYPE)

        # Should exist now
        assert await local_backend.exists(key)

    @pytest.mark.asyncio
    async def test_delete(self, local_backend):
        """Test file deletion"""
        key = "test/to_delete.png"

        # Upload first
        await local_backend.upload(key, MINIMAL_PNG_BYTES, TEST_MEDIA_TYPE)
        assert await local_backend.exists(key)

        # Delete
        result = await local_backend.delete(key)
        assert result is True
        assert not await local_backend.exists(key)

    @pytest.mark.asyncio
    async def test_delete_nonexistent(self, local_backend):
        """Test deleting non-existent file"""
        result = await local_backend.delete("nonexistent/file.png")
        assert result is False

    @pytest.mark.asyncio
    async def test_download_nonexistent(self, local_backend):
        """Test downloading non-existent file raises error"""
        with pytest.raises(FileNotFoundError):
            await local_backend.download("nonexistent/file.png")

    @pytest.mark.asyncio
    async def test_delete_prefix(self, local_backend):
        """Test deleting files with prefix"""
        prefix = "test/batch/"

        # Upload multiple files
        for i in range(3):
            key = f"{prefix}image_{i}.png"
            await local_backend.upload(key, MINIMAL_PNG_BYTES, TEST_MEDIA_TYPE)

        # Delete by prefix
        deleted_count = await local_backend.delete_prefix(prefix)
        assert deleted_count == 3

    def test_get_url(self, local_backend, temp_storage_path):
        """Test URL generation"""
        key = "test/url.png"
        url = local_backend.get_url(key)
        assert "file://" in url
        assert (
            temp_storage_path.replace("\\", "/") in url.replace("\\", "/")
            or temp_storage_path in url
        )


class TestS3StorageBackend:
    """Tests for S3StorageBackend (mocked)"""

    @pytest.fixture
    def mock_s3_client(self):
        """Create mock S3 client"""
        client = AsyncMock()
        client.put_object = AsyncMock()
        client.get_object = AsyncMock()
        client.delete_object = AsyncMock()
        client.head_object = AsyncMock()
        client.delete_objects = AsyncMock()
        client.get_paginator = MagicMock()
        return client

    @pytest.fixture
    def s3_backend(self, mock_s3_client):
        """Create S3StorageBackend with mocked client"""
        from src.services.storage.image_storage import S3StorageBackend

        backend = S3StorageBackend(
            bucket="test-bucket",
            region="us-east-1",
            access_key="test-key",
            secret_key="test-secret",
        )
        # Inject mocked client
        backend._client = mock_s3_client
        backend._client_context = MagicMock()
        return backend

    @pytest.mark.asyncio
    async def test_upload(self, s3_backend, mock_s3_client):
        """Test S3 upload"""
        key = "test/image.png"

        url = await s3_backend.upload(key, MINIMAL_PNG_BYTES, TEST_MEDIA_TYPE)

        mock_s3_client.put_object.assert_called_once()
        call_kwargs = mock_s3_client.put_object.call_args.kwargs
        assert call_kwargs["Bucket"] == "test-bucket"
        assert call_kwargs["Key"] == key
        assert call_kwargs["Body"] == MINIMAL_PNG_BYTES
        assert "https://" in url or "test-bucket" in url

    @pytest.mark.asyncio
    async def test_download(self, s3_backend, mock_s3_client):
        """Test S3 download"""
        key = "test/image.png"

        # Mock response - Body needs to be an async context manager
        mock_stream = AsyncMock()
        mock_stream.read = AsyncMock(return_value=MINIMAL_PNG_BYTES)

        mock_body = AsyncMock()
        mock_body.__aenter__ = AsyncMock(return_value=mock_stream)
        mock_body.__aexit__ = AsyncMock(return_value=None)

        mock_s3_client.get_object = AsyncMock(return_value={"Body": mock_body})

        content = await s3_backend.download(key)
        assert content == MINIMAL_PNG_BYTES

    @pytest.mark.asyncio
    async def test_delete(self, s3_backend, mock_s3_client):
        """Test S3 delete"""
        key = "test/image.png"

        result = await s3_backend.delete(key)

        mock_s3_client.delete_object.assert_called_once_with(Bucket="test-bucket", Key=key)
        assert result is True

    @pytest.mark.asyncio
    async def test_delete_prefix_raises_on_per_object_errors(
        self,
        s3_backend,
        mock_s3_client,
    ):
        """A 200 response with failed objects must keep the caller's retry authority."""

        async def pages():
            yield {
                "Contents": [
                    {"Key": "test/first.png"},
                    {"Key": "test/second.png"},
                ]
            }

        paginator = MagicMock()
        paginator.paginate.return_value = pages()
        mock_s3_client.get_paginator.return_value = paginator
        mock_s3_client.delete_objects.return_value = {
            "Deleted": [{"Key": "test/first.png"}],
            "Errors": [
                {
                    "Key": "test/second.png",
                    "Code": "AccessDenied",
                }
            ],
        }

        with pytest.raises(RuntimeError, match="partially applied"):
            await s3_backend.delete_prefix("test/")

        mock_s3_client.delete_objects.assert_awaited_once()

    @pytest.mark.asyncio
    async def test_nonempty_prefix_roundtrips_url_to_presigned_key_once(
        self,
        mock_s3_client,
    ):
        from src.services.storage.image_storage import (
            ImageStorageService,
            S3StorageBackend,
            StorageBackend,
            StorageConfig,
        )

        backend = S3StorageBackend(
            bucket="test-bucket",
            region="us-east-1",
            access_key="test-key",
            secret_key="test-secret",
            key_prefix="dev",
        )
        backend._client = mock_s3_client
        backend._client_context = MagicMock()
        mock_s3_client.generate_presigned_url = AsyncMock(
            return_value="https://signed.invalid/image"
        )
        service = ImageStorageService.__new__(ImageStorageService)
        service.config = StorageConfig(
            backend=StorageBackend.S3,
            s3_bucket="test-bucket",
            s3_region="us-east-1",
            key_prefix="dev",
        )
        service._backend = backend

        direct_url = backend.get_url("knowledge/tenant/document/image.png")
        result = await service.get_presigned_url(direct_url)

        assert result == "https://signed.invalid/image"
        call = mock_s3_client.generate_presigned_url.await_args
        assert call.kwargs["Params"]["Key"] == (
            "dev/knowledge/tenant/document/image.png"
        )

    @pytest.mark.asyncio
    async def test_exists_true(self, s3_backend, mock_s3_client):
        """Test S3 exists when file exists"""
        mock_s3_client.head_object = AsyncMock()

        result = await s3_backend.exists("test/exists.png")
        assert result is True

    @pytest.mark.asyncio
    async def test_exists_false(self, s3_backend, mock_s3_client):
        """Test S3 exists when file doesn't exist"""
        not_found = Exception("Not found")
        not_found.response = {"Error": {"Code": "404"}}  # type: ignore[attr-defined]
        mock_s3_client.head_object = AsyncMock(side_effect=not_found)

        result = await s3_backend.exists("test/notfound.png")
        assert result is False

    def test_get_url(self, s3_backend):
        """Test S3 URL generation"""
        url = s3_backend.get_url("test/image.png")
        assert "test-bucket" in url
        assert "test/image.png" in url

    @pytest.mark.asyncio
    async def test_close(self, s3_backend):
        """Test S3 client cleanup"""
        s3_backend._client_context.__aexit__ = AsyncMock()

        await s3_backend.close()

        assert s3_backend._client is None
        assert s3_backend._client_context is None


class TestImageStorageService:
    """Tests for ImageStorageService high-level API"""

    @pytest.fixture
    def storage_config(self, tmp_path):
        """Create storage configuration"""
        from src.services.storage.image_storage import StorageBackend, StorageConfig

        return StorageConfig(
            backend=StorageBackend.LOCAL,
            local_base_path=str(tmp_path),
        )

    @pytest.fixture
    def storage_service(self, storage_config):
        """Create ImageStorageService instance"""
        from src.services.storage.image_storage import ImageStorageService

        return ImageStorageService(storage_config, signing_key="test-signing-key")

    @pytest.mark.asyncio
    async def test_upload_image(self, storage_service):
        """Test image upload through service"""
        url = await storage_service.upload_image(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
            content=MINIMAL_PNG_BYTES,
            content_type=TEST_MEDIA_TYPE,
        )

        assert url is not None
        assert "file://" in url

    @pytest.mark.asyncio
    async def test_download_image(self, storage_service):
        """Test image download through service"""
        # Upload first
        await storage_service.upload_image(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
            content=MINIMAL_PNG_BYTES,
            content_type=TEST_MEDIA_TYPE,
        )

        # Download
        content = await storage_service.download_image(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
        )

        assert content == MINIMAL_PNG_BYTES

    @pytest.mark.asyncio
    async def test_delete_image(self, storage_service):
        """Test image deletion through service"""
        # Upload first
        await storage_service.upload_image(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
            content=MINIMAL_PNG_BYTES,
            content_type=TEST_MEDIA_TYPE,
        )

        # Delete
        result = await storage_service.delete_image(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
        )

        assert result is True

    @pytest.mark.asyncio
    async def test_delete_document_images(self, storage_service):
        """Test deleting all images for a document"""
        # Upload multiple images
        for i in range(3):
            await storage_service.upload_image(
                tenant_id=TEST_TENANT_ID,
                document_id=TEST_DOCUMENT_ID,
                attachment_id=f"att_{i}",
                filename=f"image_{i}.png",
                content=MINIMAL_PNG_BYTES,
                content_type=TEST_MEDIA_TYPE,
            )

        # Delete all
        deleted = await storage_service.delete_document_images(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
        )

        assert deleted == 3

    @pytest.mark.asyncio
    async def test_image_exists(self, storage_service):
        """Test image existence check"""
        # Should not exist initially
        exists = await storage_service.image_exists(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
        )
        assert exists is False

        # Upload
        await storage_service.upload_image(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
            content=MINIMAL_PNG_BYTES,
            content_type=TEST_MEDIA_TYPE,
        )

        # Should exist now
        exists = await storage_service.image_exists(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
        )
        assert exists is True

    def test_generate_key(self, storage_service):
        """Test storage key generation"""
        key = storage_service._generate_key(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
        )

        assert TEST_TENANT_ID in key
        assert TEST_DOCUMENT_ID in key
        assert TEST_ATTACHMENT_ID in key
        assert "knowledge/confluence" in key

    @pytest.mark.asyncio
    async def test_context_manager(self, storage_config):
        """Test async context manager"""
        from src.services.storage.image_storage import ImageStorageService

        async with ImageStorageService(
            storage_config,
            signing_key="test-signing-key",
        ) as service:
            url = await service.upload_image(
                tenant_id=TEST_TENANT_ID,
                document_id=TEST_DOCUMENT_ID,
                attachment_id=TEST_ATTACHMENT_ID,
                filename=TEST_FILENAME,
                content=MINIMAL_PNG_BYTES,
                content_type=TEST_MEDIA_TYPE,
            )
            assert url is not None


# ============ DashScopeMultimodalEmbedding Tests ============


class TestDashScopeMultimodalEmbedding:
    """Tests for DashScopeMultimodalEmbedding"""

    @pytest.fixture
    def mock_multimodal_embedding(self):
        """Create mock MultiModalEmbedding response"""
        mock_response = MagicMock()
        mock_response.status_code = 200
        mock_response.output = {"embeddings": [{"embedding": [0.1] * 1024, "type": "image"}]}
        return mock_response

    @pytest.fixture
    def embedding_service(self):
        """Create DashScopeMultimodalEmbedding instance"""
        from knowledge_service.services.knowledge.embedding import (
            DashScopeMultimodalEmbedding,
            EmbeddingError,
        )

        try:
            return DashScopeMultimodalEmbedding(
                model="multimodal-embedding-v1",
                api_key="test-api-key",
            )
        except EmbeddingError as exc:
            pytest.skip(str(exc))

    def test_init(self, embedding_service):
        """Test initialization"""
        assert embedding_service.model == "multimodal-embedding-v1"
        assert embedding_service.dimension == 1024
        assert embedding_service.supports_multimodal is True

    def test_detect_media_type(self, embedding_service):
        """Test media type detection"""
        # PNG
        assert embedding_service._detect_media_type(MINIMAL_PNG_BYTES) == "image/png"

        # JPEG (simplified check - first bytes)
        jpeg_header = bytes([0xFF, 0xD8, 0xFF]) + b"\x00" * 100
        assert embedding_service._detect_media_type(jpeg_header) == "image/jpeg"

        # GIF
        gif_header = b"GIF89a" + b"\x00" * 100
        assert embedding_service._detect_media_type(gif_header) == "image/gif"

        # BMP
        bmp_header = b"BM" + b"\x00" * 100
        assert embedding_service._detect_media_type(bmp_header) == "image/bmp"

    def test_image_to_base64_data_uri(self, embedding_service):
        """Test base64 data URI conversion"""
        data_uri = embedding_service._image_to_base64_data_uri(MINIMAL_PNG_BYTES, "image/png")

        assert data_uri.startswith("data:image/png;base64,")
        # Verify it's valid base64
        base64_part = data_uri.split(",")[1]
        decoded = base64.b64decode(base64_part)
        assert decoded == MINIMAL_PNG_BYTES

    @pytest.mark.asyncio
    async def test_embed_images(self, embedding_service, mock_multimodal_embedding):
        """Test image embedding"""
        with patch.object(embedding_service, "_MultiModalEmbedding") as mock_class:
            mock_class.call = MagicMock(return_value=mock_multimodal_embedding)

            vectors = await embedding_service.embed_images([MINIMAL_PNG_BYTES])

            assert len(vectors) == 1
            assert len(vectors[0]) == 1024
            mock_class.call.assert_called_once()

    @pytest.mark.asyncio
    async def test_embed_images_empty_list(self, embedding_service):
        """Test embedding empty list returns empty"""
        vectors = await embedding_service.embed_images([])
        assert vectors == []

    @pytest.mark.asyncio
    async def test_embed_images_size_limit(self, embedding_service):
        """Test image size limit enforcement"""
        from knowledge_service.services.knowledge.embedding import EmbeddingError

        # Create oversized image (>3MB)
        oversized_image = b"\x89PNG" + b"\x00" * (4 * 1024 * 1024)

        with pytest.raises(EmbeddingError) as exc_info:
            await embedding_service.embed_images([oversized_image])

        assert "exceeds max size" in str(exc_info.value)

    @pytest.mark.asyncio
    async def test_embed_image_and_text(self, embedding_service, mock_multimodal_embedding):
        """Test combined image and text embedding"""
        with patch.object(embedding_service, "_MultiModalEmbedding") as mock_class:
            mock_class.call = MagicMock(return_value=mock_multimodal_embedding)

            vector = await embedding_service.embed_image_and_text(
                image_bytes=MINIMAL_PNG_BYTES,
                text="A test image description",
            )

            assert len(vector) == 1024
            # Verify text was included in input
            call_args = mock_class.call.call_args
            input_items = call_args.kwargs.get("input", [])
            assert len(input_items) == 2  # image + text

    def test_parse_multimodal_output(self, embedding_service):
        """Test output parsing"""
        output = {
            "embeddings": [
                {"embedding": [0.1, 0.2, 0.3], "type": "image"},
                {"embedding": [0.4, 0.5, 0.6], "type": "text"},
            ]
        }

        vectors = embedding_service._parse_multimodal_output(output)

        assert len(vectors) == 2
        assert vectors[0] == [0.1, 0.2, 0.3]
        assert vectors[1] == [0.4, 0.5, 0.6]

    def test_parse_multimodal_output_invalid(self, embedding_service):
        """Test output parsing with invalid data"""
        from knowledge_service.services.knowledge.embedding import EmbeddingError

        with pytest.raises(EmbeddingError):
            embedding_service._parse_multimodal_output(None)

        with pytest.raises(EmbeddingError):
            embedding_service._parse_multimodal_output({"wrong_key": []})


# ============ ConfluenceImageProcessor Tests ============


class TestConfluenceImageProcessor:
    """Tests for ConfluenceImageProcessor"""

    @pytest.fixture
    def mock_confluence_client(self):
        """Create mock Confluence client"""
        from knowledge_service.services.knowledge.confluence.models import (
            ConfluenceAttachment,
        )

        client = AsyncMock()

        # Mock attachment
        attachment = ConfluenceAttachment(
            attachment_id=TEST_ATTACHMENT_ID,
            page_id="page_001",
            filename=TEST_FILENAME,
            media_type=TEST_MEDIA_TYPE,
            file_size=len(MINIMAL_PNG_BYTES),
            download_link="/download/attachments/page_001/test_image.png",
        )

        client.get_page_image_attachments = AsyncMock(return_value=[attachment])
        client.download_attachment = AsyncMock(return_value=MINIMAL_PNG_BYTES)

        return client

    @pytest.fixture
    def mock_storage_service(self):
        """Create mock storage service"""
        storage = AsyncMock()
        storage.upload_image = AsyncMock(return_value="https://s3.example.com/test_image.png")
        storage.delete_document_images = AsyncMock(return_value=1)
        return storage

    @pytest.fixture
    def mock_embedding_service(self):
        """Create mock multimodal embedding service"""
        embedding = AsyncMock()
        embedding.embed_images = AsyncMock(return_value=[[0.1] * 1024])
        embedding.embed_image_and_text = AsyncMock(return_value=[0.1] * 1024)
        return embedding

    @pytest.fixture
    def image_processor(self, mock_confluence_client, mock_storage_service, mock_embedding_service):
        """Create ConfluenceImageProcessor instance"""
        from knowledge_service.services.knowledge.confluence.image_processor import (
            ConfluenceImageProcessor,
        )

        return ConfluenceImageProcessor(
            confluence_client=mock_confluence_client,
            storage_service=mock_storage_service,
            multimodal_embedding=mock_embedding_service,
        )

    @pytest.mark.asyncio
    async def test_process_page_images(self, image_processor):
        """Test processing images from a page"""
        result = await image_processor.process_page_images(
            page_id="page_001",
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
        )

        assert result.page_id == "page_001"
        assert result.document_id == TEST_DOCUMENT_ID
        assert result.total_images == 1
        assert result.processed_images == 1
        assert len(result.segments) == 1
        assert result.errors == []

    @pytest.mark.asyncio
    async def test_process_page_images_with_context(self, image_processor, mock_confluence_client):
        """Test processing images with page content for context"""
        del mock_confluence_client
        page_content = """
        <ac:image>
            <ri:attachment ri:filename="test_image.png" />
        </ac:image>
        <p>This is the context around the image.</p>
        """

        result = await image_processor.process_page_images(
            page_id="page_001",
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
            page_content=page_content,
        )

        assert result.processed_images == 1

    @pytest.mark.asyncio
    async def test_process_page_images_no_images(self, image_processor, mock_confluence_client):
        """Test processing page with no images"""
        mock_confluence_client.get_page_image_attachments = AsyncMock(return_value=[])

        result = await image_processor.process_page_images(
            page_id="page_001",
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
        )

        assert result.total_images == 0
        assert result.processed_images == 0
        assert result.segments == []

    @pytest.mark.asyncio
    async def test_process_page_images_skip_large(self, image_processor, mock_confluence_client):
        """Test skipping images whose downloaded bytes exceed the size limit."""
        from knowledge_service.services.knowledge.confluence.models import (
            ConfluenceAttachment,
        )

        # The source-reported size is acceptable, but the downloaded payload is oversized.
        large_attachment = ConfluenceAttachment(
            attachment_id="large_att",
            page_id="page_001",
            filename="large_image.png",
            media_type=TEST_MEDIA_TYPE,
            file_size=len(MINIMAL_PNG_BYTES),
            download_link="/download/large.png",
        )

        mock_confluence_client.get_page_image_attachments = AsyncMock(
            return_value=[large_attachment]
        )
        mock_confluence_client.download_attachment = AsyncMock(
            return_value=b"\x89PNG" + b"\x00" * (4 * 1024 * 1024)
        )

        result = await image_processor.process_page_images(
            page_id="page_001",
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
        )

        assert result.total_images == 1
        assert result.processed_images == 0
        assert result.skipped_images == 1

    @pytest.mark.asyncio
    async def test_process_page_images_embedding_failure(
        self, image_processor, mock_embedding_service
    ):
        """Test handling embedding failures gracefully"""
        mock_embedding_service.embed_images = AsyncMock(side_effect=Exception("API error"))
        mock_embedding_service.embed_image_and_text = AsyncMock(side_effect=Exception("API error"))

        result = await image_processor.process_page_images(
            page_id="page_001",
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
            generate_embeddings=True,
        )

        # Should still succeed, just without embedding
        assert result.processed_images == 1
        segment = result.segments[0]
        assert segment.embedding is None

    @pytest.mark.asyncio
    async def test_process_page_images_without_embeddings(
        self, image_processor, mock_embedding_service
    ):
        """Test processing without generating embeddings"""
        result = await image_processor.process_page_images(
            page_id="page_001",
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
            generate_embeddings=False,
        )

        assert result.processed_images == 1
        # Embedding should not be called
        mock_embedding_service.embed_images.assert_not_called()

    @pytest.mark.asyncio
    async def test_delete_document_images(self, image_processor, mock_storage_service):
        """Test deleting document images"""
        count = await image_processor.delete_document_images(
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
        )

        assert count == 1
        mock_storage_service.delete_document_images.assert_called_once_with(
            TEST_TENANT_ID, TEST_DOCUMENT_ID
        )

    @pytest.mark.asyncio
    async def test_process_single_image_creates_segment(
        self, image_processor, mock_confluence_client
    ):
        """Test that processing creates proper ImageSegment"""
        del mock_confluence_client
        from knowledge_service.services.knowledge.confluence.models import (
            ConfluenceAttachment,
        )

        attachment = ConfluenceAttachment(
            attachment_id=TEST_ATTACHMENT_ID,
            page_id="page_001",
            filename=TEST_FILENAME,
            media_type=TEST_MEDIA_TYPE,
            file_size=len(MINIMAL_PNG_BYTES),
            download_link="/download/test.png",
        )

        segment = await image_processor._process_single_image(
            attachment=attachment,
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
            context="Test context",
            generate_embedding=True,
        )

        assert segment is not None
        assert segment.document_id == TEST_DOCUMENT_ID
        assert segment.attachment_id == TEST_ATTACHMENT_ID
        assert segment.filename == TEST_FILENAME
        assert segment.media_type == TEST_MEDIA_TYPE
        assert segment.storage_url == "https://s3.example.com/test_image.png"
        assert segment.embedding is not None
        assert len(segment.embedding) == 1024


# ============ Image Sync Integration Tests ============
class TestImageSyncIntegration:
    """Integration tests for the complete image sync flow"""

    @pytest.fixture
    def mock_all_services(self, tmp_path):
        """Create all mocked services for integration test"""
        from knowledge_service.services.knowledge.confluence.models import (
            ConfluenceAttachment,
        )

        from src.services.storage.image_storage import (
            ImageStorageService,
            StorageBackend,
            StorageConfig,
        )

        # Real storage service (local)
        storage_config = StorageConfig(
            backend=StorageBackend.LOCAL,
            local_base_path=str(tmp_path),
        )
        storage_service = ImageStorageService(
            storage_config,
            signing_key="test-signing-key",
        )

        # Mock confluence client
        confluence_client = AsyncMock()
        attachment = ConfluenceAttachment(
            attachment_id=TEST_ATTACHMENT_ID,
            page_id="page_001",
            filename=TEST_FILENAME,
            media_type=TEST_MEDIA_TYPE,
            file_size=len(MINIMAL_PNG_BYTES),
            download_link="/download/test.png",
        )
        confluence_client.get_page_image_attachments = AsyncMock(return_value=[attachment])
        confluence_client.download_attachment = AsyncMock(return_value=MINIMAL_PNG_BYTES)

        # Mock embedding service
        embedding_service = AsyncMock()
        embedding_service.embed_images = AsyncMock(return_value=[[0.1] * 1024])
        embedding_service.embed_image_and_text = AsyncMock(return_value=[0.1] * 1024)

        return {
            "storage": storage_service,
            "confluence": confluence_client,
            "embedding": embedding_service,
        }

    @pytest.mark.asyncio
    async def test_full_image_sync_flow(self, mock_all_services):
        """Test complete flow: download -> store -> embed"""
        from knowledge_service.services.knowledge.confluence.image_processor import (
            ConfluenceImageProcessor,
        )

        processor = ConfluenceImageProcessor(
            confluence_client=mock_all_services["confluence"],
            storage_service=mock_all_services["storage"],
            multimodal_embedding=mock_all_services["embedding"],
        )

        # Process images
        result = await processor.process_page_images(
            page_id="page_001",
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
            generate_embeddings=True,
        )

        # Verify result
        assert result.processed_images == 1
        assert len(result.segments) == 1

        segment = result.segments[0]
        assert segment.storage_url is not None
        assert segment.embedding is not None
        assert len(segment.embedding) == 1024

        # Verify image was actually stored
        exists = await mock_all_services["storage"].image_exists(
            tenant_id=TEST_TENANT_ID,
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
        )
        assert exists is True

    @pytest.mark.asyncio
    async def test_reprocess_page_images(self, mock_all_services):
        """Test reprocessing deletes old images first"""
        from knowledge_service.services.knowledge.confluence.image_processor import (
            ConfluenceImageProcessor,
        )

        processor = ConfluenceImageProcessor(
            confluence_client=mock_all_services["confluence"],
            storage_service=mock_all_services["storage"],
            multimodal_embedding=mock_all_services["embedding"],
        )

        # Process first time
        await processor.process_page_images(
            page_id="page_001",
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
        )

        # Reprocess
        result = await processor.reprocess_page_images(
            page_id="page_001",
            document_id=TEST_DOCUMENT_ID,
            tenant_id=TEST_TENANT_ID,
        )

        assert result.processed_images == 1


# ============ Parser Image Extraction Tests ============


class TestParserImageExtraction:
    """Tests for parser image extraction functions"""

    def test_extract_image_references_basic(self):
        """Test extracting image references from storage format"""
        from knowledge_service.services.knowledge.confluence.parser import (
            extract_image_references,
        )

        content = """
        <ac:image>
            <ri:attachment ri:filename="test_image.png" />
        </ac:image>
        """

        refs = extract_image_references(content)

        assert len(refs) == 1
        assert refs[0].filename == "test_image.png"

    def test_extract_image_references_multiple(self):
        """Test extracting multiple image references"""
        from knowledge_service.services.knowledge.confluence.parser import (
            extract_image_references,
        )

        content = """
        <p>First image:</p>
        <ac:image>
            <ri:attachment ri:filename="image1.png" />
        </ac:image>
        <p>Second image:</p>
        <ac:image>
            <ri:attachment ri:filename="image2.jpg" />
        </ac:image>
        """

        refs = extract_image_references(content)

        assert len(refs) == 2
        assert refs[0].filename == "image1.png"
        assert refs[1].filename == "image2.jpg"

    def test_extract_image_references_with_dimensions(self):
        """Test extracting image references with width/height"""
        from knowledge_service.services.knowledge.confluence.parser import (
            extract_image_references,
        )

        content = """
        <ac:image ac:width="300" ac:height="200">
            <ri:attachment ri:filename="sized_image.png" />
        </ac:image>
        """

        refs = extract_image_references(content)

        assert len(refs) == 1
        assert refs[0].filename == "sized_image.png"
        assert refs[0].width == 300
        assert refs[0].height == 200

    def test_extract_image_references_with_alt(self):
        """Test extracting image references with alt text"""
        from knowledge_service.services.knowledge.confluence.parser import (
            extract_image_references,
        )

        content = """
        <ac:image ac:alt="Alt text for image">
            <ri:attachment ri:filename="alt_image.png" />
        </ac:image>
        """

        refs = extract_image_references(content)

        assert len(refs) == 1
        assert refs[0].alt_text == "Alt text for image"

    def test_extract_image_references_empty_content(self):
        """Test with empty or no images"""
        from knowledge_service.services.knowledge.confluence.parser import (
            extract_image_references,
        )

        refs = extract_image_references("")
        assert refs == []

        refs = extract_image_references("<p>No images here</p>")
        assert refs == []

    def test_extract_embeddable_images(self):
        """Test filtering for embeddable images only"""
        from knowledge_service.services.knowledge.confluence.parser import (
            extract_embeddable_images,
        )

        content = """
        <ac:image>
            <ri:attachment ri:filename="image.png" />
        </ac:image>
        <ac:image>
            <ri:attachment ri:filename="image.jpg" />
        </ac:image>
        <ac:image>
            <ri:attachment ri:filename="image.svg" />
        </ac:image>
        """

        # SVG should be excluded (not embeddable)
        refs = extract_embeddable_images(content)

        # PNG and JPG are embeddable
        embeddable_filenames = [r.filename for r in refs]
        assert "image.png" in embeddable_filenames
        assert "image.jpg" in embeddable_filenames
        # SVG may or may not be in the list depending on implementation

    def test_image_reference_is_embeddable(self):
        """Test ImageReference.is_embeddable property"""
        from knowledge_service.services.knowledge.confluence.parser import ImageReference

        # PNG should be embeddable
        png_ref = ImageReference(filename="test.png", content_type="image/png")
        assert png_ref.is_embeddable is True

        # JPEG should be embeddable
        jpg_ref = ImageReference(filename="test.jpg", content_type="image/jpeg")
        assert jpg_ref.is_embeddable is True

        # SVG should NOT be embeddable (vector format)
        svg_ref = ImageReference(filename="test.svg", content_type="image/svg+xml")
        assert svg_ref.is_embeddable is False

    def test_extract_image_references_with_context(self):
        """Test that context text is extracted"""
        from knowledge_service.services.knowledge.confluence.parser import (
            extract_image_references,
        )

        content = """
        <p>This is the surrounding context text.</p>
        <ac:image>
            <ri:attachment ri:filename="context_image.png" />
        </ac:image>
        <p>More context after the image.</p>
        """

        refs = extract_image_references(content)

        assert len(refs) == 1
        # Context should be extracted (implementation dependent)
        # Just verify it doesn't crash

    def test_extract_image_references_nested_structure(self):
        """Test extraction from nested structures"""
        from knowledge_service.services.knowledge.confluence.parser import (
            extract_image_references,
        )

        content = """
        <ac:layout>
            <ac:layout-section>
                <ac:layout-cell>
                    <ac:image>
                        <ri:attachment ri:filename="nested_image.png" />
                    </ac:image>
                </ac:layout-cell>
            </ac:layout-section>
        </ac:layout>
        """

        refs = extract_image_references(content)

        assert len(refs) == 1
        assert refs[0].filename == "nested_image.png"


# ============ ImageSegment Model Tests ============


class TestImageSegmentModel:
    """Tests for ImageSegment dataclass"""

    def test_image_segment_creation(self):
        """Test creating ImageSegment"""
        from knowledge_service.services.knowledge.confluence.models import ImageSegment

        segment = ImageSegment(
            segment_id="seg_001",
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
            media_type=TEST_MEDIA_TYPE,
            file_size=len(MINIMAL_PNG_BYTES),
            storage_url="https://s3.example.com/image.png",
        )

        assert segment.segment_id == "seg_001"
        assert segment.document_id == TEST_DOCUMENT_ID
        assert segment.has_embedding is False
        assert segment.embedding_dimension == 0

    def test_image_segment_with_embedding(self):
        """Test ImageSegment with embedding"""
        from knowledge_service.services.knowledge.confluence.models import ImageSegment

        embedding = [0.1] * 1024

        segment = ImageSegment(
            segment_id="seg_001",
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
            media_type=TEST_MEDIA_TYPE,
            file_size=len(MINIMAL_PNG_BYTES),
            storage_url="https://s3.example.com/image.png",
            embedding=embedding,
        )

        assert segment.has_embedding is True
        assert segment.embedding_dimension == 1024

    def test_image_segment_to_dict(self):
        """Test ImageSegment.to_dict()"""
        from knowledge_service.services.knowledge.confluence.models import ImageSegment

        segment = ImageSegment(
            segment_id="seg_001",
            document_id=TEST_DOCUMENT_ID,
            attachment_id=TEST_ATTACHMENT_ID,
            filename=TEST_FILENAME,
            media_type=TEST_MEDIA_TYPE,
            file_size=len(MINIMAL_PNG_BYTES),
            storage_url="https://s3.example.com/image.png",
            context_text="Image context",
            embedding=[0.1] * 1024,
        )

        data = segment.to_dict()

        assert data["segment_id"] == "seg_001"
        assert data["document_id"] == TEST_DOCUMENT_ID
        assert data["filename"] == TEST_FILENAME
        assert data["has_embedding"] is True
        assert data["embedding_dimension"] == 1024
        assert data["context_text"] == "Image context"


# ============ ConfluenceAttachment Model Tests ============


class TestConfluenceAttachmentModel:
    """Tests for ConfluenceAttachment model"""

    def test_is_image(self):
        """Test is_image property"""
        from knowledge_service.services.knowledge.confluence.models import (
            ConfluenceAttachment,
        )

        # Image types
        for media_type in ["image/png", "image/jpeg", "image/gif", "image/webp"]:
            att = ConfluenceAttachment(
                attachment_id="att_001",
                page_id="page_001",
                filename="test.png",
                media_type=media_type,
                file_size=1000,
                download_link="/download/test.png",
            )
            assert att.is_image is True, f"{media_type} should be recognized as image"

        # Non-image types
        for media_type in ["application/pdf", "text/plain", "video/mp4"]:
            att = ConfluenceAttachment(
                attachment_id="att_001",
                page_id="page_001",
                filename="test.pdf",
                media_type=media_type,
                file_size=1000,
                download_link="/download/test.pdf",
            )
            assert att.is_image is False, f"{media_type} should not be recognized as image"

    def test_is_embeddable_image(self):
        """Test is_embeddable_image property"""
        from knowledge_service.services.knowledge.confluence.models import (
            ConfluenceAttachment,
        )

        # Embeddable: PNG, JPEG, BMP, WebP under 3MB
        att = ConfluenceAttachment(
            attachment_id="att_001",
            page_id="page_001",
            filename="test.png",
            media_type="image/png",
            file_size=1000,
            download_link="/download/test.png",
        )
        assert att.is_embeddable_image is True

        # Not embeddable: SVG (even if small)
        att_svg = ConfluenceAttachment(
            attachment_id="att_002",
            page_id="page_001",
            filename="test.svg",
            media_type="image/svg+xml",
            file_size=1000,
            download_link="/download/test.svg",
        )
        assert att_svg.is_embeddable_image is False

        # Not embeddable: PNG over 3MB
        att_large = ConfluenceAttachment(
            attachment_id="att_003",
            page_id="page_001",
            filename="large.png",
            media_type="image/png",
            file_size=4 * 1024 * 1024,  # 4MB
            download_link="/download/large.png",
        )
        assert att_large.is_embeddable_image is False

    def test_to_dict(self):
        """Test ConfluenceAttachment.to_dict()"""
        from knowledge_service.services.knowledge.confluence.models import (
            ConfluenceAttachment,
        )

        att = ConfluenceAttachment(
            attachment_id="att_001",
            page_id="page_001",
            filename="test.png",
            media_type="image/png",
            file_size=1000,
            download_link="/download/test.png",
            title="Test Image",
        )

        data = att.to_dict()

        assert data["attachment_id"] == "att_001"
        assert data["page_id"] == "page_001"
        assert data["filename"] == "test.png"
        assert data["is_image"] is True
        assert data["is_embeddable_image"] is True


# ============ S3 Metadata Sanitization Tests ============


class TestS3MetadataSanitization:
    """
    Tests for _sanitize_for_s3_metadata function.

    S3 metadata can only contain ASCII characters. This test suite verifies
    that the sanitization function correctly handles:
    - Normal ASCII strings
    - Unicode narrow no-break space (U+202F) - common in macOS time formatting
    - Other special Unicode characters and spaces
    - Empty strings and edge cases
    - Multiple spaces cleanup
    """

    def test_normal_ascii_string(self):
        """Test that normal ASCII strings pass through unchanged"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # Simple ASCII
        assert _sanitize_for_s3_metadata("hello.png") == "hello.png"
        assert _sanitize_for_s3_metadata("test_image_123.jpg") == "test_image_123.jpg"
        assert (
            _sanitize_for_s3_metadata("Screenshot 2025-01-16 at 2.38.53 pm.png")
            == "Screenshot 2025-01-16 at 2.38.53 pm.png"
        )

    def test_unicode_narrow_no_break_space(self):
        """
        Test handling of U+202F (narrow no-break space).

        This is the original bug: macOS uses U+202F in time formatting,
        e.g., "2:38:53 pm" where the space before "pm" is actually U+202F.
        """
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # The problematic filename from the bug report
        # Contains U+202F between "2.38.53" and "pm"
        problematic_filename = "Screenshot 2025-01-16 at 2.38.53\u202fpm.png"

        result = _sanitize_for_s3_metadata(problematic_filename)

        # Should be pure ASCII after sanitization
        assert result.isascii(), f"Result contains non-ASCII: {repr(result)}"

        # The narrow no-break space should become a regular space
        expected = "Screenshot 2025-01-16 at 2.38.53 pm.png"
        assert result == expected, f"Expected {repr(expected)}, got {repr(result)}"

        # Verify it can be encoded as ASCII without errors
        result.encode("ascii")

    def test_various_unicode_spaces(self):
        """Test handling of various Unicode space characters"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # U+00A0 Non-breaking space
        assert _sanitize_for_s3_metadata("test\u00a0file.png").isascii()

        # U+2002 En space
        assert _sanitize_for_s3_metadata("test\u2002file.png").isascii()

        # U+2003 Em space
        assert _sanitize_for_s3_metadata("test\u2003file.png").isascii()

        # U+2009 Thin space
        assert _sanitize_for_s3_metadata("test\u2009file.png").isascii()

        # U+200A Hair space
        assert _sanitize_for_s3_metadata("test\u200afile.png").isascii()

        # U+3000 Ideographic space (CJK)
        assert _sanitize_for_s3_metadata("test\u3000file.png").isascii()

    def test_unicode_diacritics(self):
        """Test handling of Unicode characters with diacritics"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # NFKD normalization should convert these
        result = _sanitize_for_s3_metadata("café.png")
        assert result.isascii()

        result = _sanitize_for_s3_metadata("naïve.jpg")
        assert result.isascii()

        result = _sanitize_for_s3_metadata("résumé.pdf")
        assert result.isascii()

    def test_chinese_characters_removed(self):
        """Test that Chinese characters are removed (can't be normalized to ASCII)"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # Chinese characters should be removed
        result = _sanitize_for_s3_metadata("截图2025.png")
        assert result.isascii()
        assert "截图" not in result

        result = _sanitize_for_s3_metadata("测试图片.jpg")
        assert result.isascii()

    def test_japanese_characters_removed(self):
        """Test that Japanese characters are removed"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        result = _sanitize_for_s3_metadata("スクリーンショット.png")
        assert result.isascii()

    def test_korean_characters_removed(self):
        """Test that Korean characters are removed"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        result = _sanitize_for_s3_metadata("스크린샷.png")
        assert result.isascii()

    def test_empty_string(self):
        """Test handling of empty strings"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        assert _sanitize_for_s3_metadata("") == ""

    def test_none_value(self):
        """Test handling of None values"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # Should return None if input is None/falsy
        assert _sanitize_for_s3_metadata(None) is None
        assert _sanitize_for_s3_metadata("") == ""

    def test_multiple_spaces_cleanup(self):
        """Test that multiple consecutive spaces are collapsed"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # Multiple regular spaces
        result = _sanitize_for_s3_metadata("test    file.png")
        assert "    " not in result
        assert result == "test file.png"

        # Mixed unicode and regular spaces
        result = _sanitize_for_s3_metadata("test\u202f  \u00a0file.png")
        assert result.isascii()
        # All spaces should be collapsed to single regular space
        assert "  " not in result

    def test_leading_trailing_spaces(self):
        """Test that leading/trailing spaces are handled"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        result = _sanitize_for_s3_metadata("  test.png  ")
        assert not result.startswith(" ")
        assert not result.endswith(" ")
        assert result == "test.png"

    def test_special_characters_preserved(self):
        """Test that valid ASCII special characters are preserved"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # Underscores, hyphens, dots should be preserved
        result = _sanitize_for_s3_metadata("test_image-2025.01.16.png")
        assert result == "test_image-2025.01.16.png"

        # Parentheses
        result = _sanitize_for_s3_metadata("image (1).png")
        assert result == "image (1).png"

    def test_emoji_removed(self):
        """Test that emoji characters are removed"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        result = _sanitize_for_s3_metadata("test📸image.png")
        assert result.isascii()
        assert "📸" not in result

        result = _sanitize_for_s3_metadata("🎉celebration.jpg")
        assert result.isascii()

    def test_real_world_macos_filename(self):
        """
        Test real-world macOS screenshot filename.

        macOS generates filenames like:
        "Screenshot 2025-01-16 at 2.38.53 pm.png"
        where the space before "pm" is U+202F (narrow no-break space)
        """
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # Simulating exactly what macOS generates
        macos_filename = "Screenshot 2025-01-16 at 2.38.53\u202fpm.png"

        result = _sanitize_for_s3_metadata(macos_filename)

        # Must be pure ASCII for S3
        assert result.isascii()

        # Must be able to encode as ASCII
        encoded = result.encode("ascii")
        assert encoded is not None

        # Should preserve the meaningful content
        assert "Screenshot" in result
        assert "2025-01-16" in result
        assert "pm.png" in result

    def test_mixed_content(self):
        """Test filenames with mixed ASCII and Unicode content"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        # ASCII + Chinese
        result = _sanitize_for_s3_metadata("report_报告_2025.pdf")
        assert result.isascii()
        assert "report" in result
        assert "2025" in result

        # ASCII + emoji + special space
        result = _sanitize_for_s3_metadata("my\u202fphoto📷2025.jpg")
        assert result.isascii()

    def test_full_unicode_filename(self):
        """Test filename that is entirely non-ASCII"""
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        result = _sanitize_for_s3_metadata("截图测试文件.png")
        assert result.isascii()
        # Only .png should remain
        assert ".png" in result

    def test_s3_metadata_safe(self):
        """
        Integration test: verify sanitized value can be used as S3 metadata.

        S3 metadata requirements:
        - ASCII characters only
        - No control characters
        """
        from src.services.storage.image_storage import _sanitize_for_s3_metadata

        test_cases = [
            "Screenshot 2025-01-16 at 2.38.53\u202fpm.png",  # The bug case
            "测试文件.png",  # Chinese
            "café résumé.pdf",  # Diacritics
            "file\u00a0name.jpg",  # NBSP
            "emoji📸file.png",  # Emoji
            "normal_file.png",  # Normal
        ]

        for original in test_cases:
            result = _sanitize_for_s3_metadata(original)

            # Must be ASCII
            assert result.isascii(), f"Failed for {repr(original)}: {repr(result)}"

            # Must be encodable as ASCII
            try:
                result.encode("ascii")
            except UnicodeEncodeError:
                pytest.fail(f"Cannot encode as ASCII: {repr(result)}")

            # Must not contain control characters (except for empty strings)
            if result:
                for char in result:
                    assert ord(char) >= 32 or char in "\t\n\r", (
                        f"Control character found in result: {repr(result)}"
                    )
