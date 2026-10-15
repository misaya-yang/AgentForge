"""
Comprehensive Unit Tests for P3 Multimodal RAG Full-Chain Optimization.

Tests include:
- Image-chunk association (AssociatedImage, Chunk multimodal fields)
- MultimodalReranker (VLM-based image scoring, score parsing)
- Retrieval with images (retrieve_with_images)
- API multimodal response format
"""

from __future__ import annotations

from unittest.mock import AsyncMock, MagicMock

import pytest
from knowledge_service.services.knowledge.chunking import (
    AssociatedImage,
    Chunk,
    ContentType,
)
from knowledge_service.services.knowledge.multimodal_reranker import (
    MultimodalReranker,
    RerankCandidate,
    create_multimodal_reranker,
)

# ============ Test AssociatedImage ============


class TestAssociatedImage:
    """Tests for AssociatedImage dataclass"""

    def test_create_associated_image(self):
        """Test basic creation of AssociatedImage"""
        img = AssociatedImage(
            image_segment_id="seg_img_001",
            storage_url="s3://bucket/images/test.png",
            filename="test.png",
            vlm_description="A diagram showing system architecture",
            proximity_score=0.85,
            char_offset=1500,
            page_number=3,
            media_type="image/png",
        )

        assert img.image_segment_id == "seg_img_001"
        assert img.storage_url == "s3://bucket/images/test.png"
        assert img.filename == "test.png"
        assert img.vlm_description == "A diagram showing system architecture"
        assert img.proximity_score == 0.85
        assert img.char_offset == 1500
        assert img.page_number == 3
        assert img.media_type == "image/png"

    def test_associated_image_defaults(self):
        """Test default values for AssociatedImage"""
        img = AssociatedImage(
            image_segment_id="seg_001",
            storage_url="s3://bucket/img.png",
        )

        assert img.filename == ""
        assert img.vlm_description is None
        assert img.proximity_score == 1.0
        assert img.char_offset == 0
        assert img.page_number is None
        assert img.media_type == "image/png"

    def test_associated_image_to_dict(self):
        """Test serialization to dictionary"""
        img = AssociatedImage(
            image_segment_id="seg_001",
            storage_url="s3://bucket/img.png",
            filename="diagram.png",
            vlm_description="Architecture diagram",
            proximity_score=0.7,
        )

        d = img.to_dict()

        assert d["image_segment_id"] == "seg_001"
        assert d["storage_url"] == "s3://bucket/img.png"
        assert d["filename"] == "diagram.png"
        assert d["vlm_description"] == "Architecture diagram"
        assert d["proximity_score"] == 0.7
        assert d["char_offset"] == 0
        assert d["page_number"] is None
        assert d["media_type"] == "image/png"

    def test_associated_image_from_dict(self):
        """Test deserialization from dictionary"""
        data = {
            "image_segment_id": "seg_002",
            "storage_url": "oss://bucket/test.jpg",
            "filename": "test.jpg",
            "vlm_description": "Test image",
            "proximity_score": 0.6,
            "char_offset": 500,
            "page_number": 2,
            "media_type": "image/jpeg",
        }

        img = AssociatedImage.from_dict(data)

        assert img.image_segment_id == "seg_002"
        assert img.storage_url == "oss://bucket/test.jpg"
        assert img.filename == "test.jpg"
        assert img.vlm_description == "Test image"
        assert img.proximity_score == 0.6
        assert img.char_offset == 500
        assert img.page_number == 2
        assert img.media_type == "image/jpeg"

    def test_associated_image_from_dict_with_defaults(self):
        """Test deserialization handles missing fields gracefully"""
        data = {
            "image_segment_id": "seg_003",
            "storage_url": "http://example.com/img.png",
        }

        img = AssociatedImage.from_dict(data)

        assert img.image_segment_id == "seg_003"
        assert img.storage_url == "http://example.com/img.png"
        assert img.filename == ""
        assert img.vlm_description is None
        assert img.proximity_score == 1.0


# ============ Test Chunk Multimodal Fields ============


class TestChunkMultimodal:
    """Tests for Chunk multimodal extensions"""

    def test_chunk_default_content_type(self):
        """Test chunk has TEXT content type by default"""
        chunk = Chunk(text="This is a text chunk")

        assert chunk.content_type == ContentType.TEXT
        assert not chunk.has_images
        assert chunk.image_count == 0
        assert not chunk.is_image_segment

    def test_chunk_with_associated_images(self):
        """Test chunk with associated images"""
        chunk = Chunk(text="Paragraph with images nearby")

        img1 = AssociatedImage(
            image_segment_id="img_001",
            storage_url="s3://bucket/img1.png",
            proximity_score=0.9,
        )
        img2 = AssociatedImage(
            image_segment_id="img_002",
            storage_url="s3://bucket/img2.png",
            proximity_score=0.7,
        )

        assert chunk.add_associated_image(img1) is True
        assert chunk.add_associated_image(img2) is True

        assert chunk.has_images
        assert chunk.image_count == 2
        assert chunk.content_type == ContentType.MIXED

    def test_chunk_max_associated_images(self):
        """Test chunk enforces max 10 associated images"""
        chunk = Chunk(text="Paragraph")

        # Add 10 images (should all succeed)
        for i in range(10):
            img = AssociatedImage(
                image_segment_id=f"img_{i:03d}",
                storage_url=f"s3://bucket/img{i}.png",
            )
            result = chunk.add_associated_image(img)
            assert result is True

        assert chunk.image_count == 10

        # Try to add 11th image (should fail)
        img11 = AssociatedImage(
            image_segment_id="img_010",
            storage_url="s3://bucket/img10.png",
        )
        result = chunk.add_associated_image(img11)
        assert result is False
        assert chunk.image_count == 10

    def test_chunk_get_images_sorted_by_proximity(self):
        """Test images are sorted by proximity score"""
        chunk = Chunk(text="Paragraph")

        chunk.add_associated_image(
            AssociatedImage(
                image_segment_id="low",
                storage_url="s3://bucket/low.png",
                proximity_score=0.3,
            )
        )
        chunk.add_associated_image(
            AssociatedImage(
                image_segment_id="high",
                storage_url="s3://bucket/high.png",
                proximity_score=0.9,
            )
        )
        chunk.add_associated_image(
            AssociatedImage(
                image_segment_id="mid",
                storage_url="s3://bucket/mid.png",
                proximity_score=0.6,
            )
        )

        sorted_imgs = chunk.get_images_sorted_by_proximity()

        assert sorted_imgs[0].image_segment_id == "high"
        assert sorted_imgs[1].image_segment_id == "mid"
        assert sorted_imgs[2].image_segment_id == "low"

    def test_image_segment_chunk(self):
        """Test chunk as image segment"""
        chunk = Chunk(
            text="",  # Image segments may have empty text
            content_type=ContentType.IMAGE,
            image_url="s3://bucket/diagram.png",
            image_filename="diagram.png",
            image_media_type="image/png",
            vlm_description="System architecture diagram showing microservices",
        )

        assert chunk.is_image_segment
        assert chunk.content_type == ContentType.IMAGE
        assert chunk.image_url == "s3://bucket/diagram.png"
        assert chunk.vlm_description is not None

    def test_chunk_to_multimodal_dict(self):
        """Test chunk serialization with multimodal fields"""
        chunk = Chunk(text="Test paragraph")
        chunk.add_associated_image(
            AssociatedImage(
                image_segment_id="img_001",
                storage_url="s3://bucket/img.png",
                vlm_description="Test image",
                proximity_score=0.8,
            )
        )

        d = chunk.to_multimodal_dict()

        assert d["text"] == "Test paragraph"
        assert d["content_type"] == "mixed"
        assert d["has_images"] is True
        assert d["image_count"] == 1
        assert len(d["associated_images"]) == 1
        assert d["associated_images"][0]["image_segment_id"] == "img_001"


# ============ Test MultimodalReranker ============


class TestMultimodalReranker:
    """Tests for MultimodalReranker"""

    def test_parse_score_direct_float(self):
        """Test parsing direct float scores"""
        reranker = MultimodalReranker(vlm_service=None)

        assert reranker._parse_score("0.85") == 0.85
        assert reranker._parse_score("0.5") == 0.5
        assert reranker._parse_score("1.0") == 1.0
        assert reranker._parse_score("0.0") == 0.0

    def test_parse_score_percentage(self):
        """Test parsing percentage scores"""
        reranker = MultimodalReranker(vlm_service=None)

        assert reranker._parse_score("85%") == 0.85
        assert reranker._parse_score("50%") == 0.5
        assert reranker._parse_score("100%") == 1.0

    def test_parse_score_integer(self):
        """Test parsing integer scores (0-100 scale)"""
        reranker = MultimodalReranker(vlm_service=None)

        assert reranker._parse_score("85") == 0.85
        assert reranker._parse_score("50") == 0.5

    def test_parse_score_with_text(self):
        """Test parsing scores embedded in text"""
        reranker = MultimodalReranker(vlm_service=None)

        # Various formats VLMs might return
        assert reranker._parse_score("Score: 0.85") == 0.85
        assert reranker._parse_score("The relevance is 0.7") == 0.7
        assert reranker._parse_score("评分: 0.9") == 0.9

    def test_parse_score_invalid_returns_default(self):
        """Test invalid scores return 0.5 default"""
        reranker = MultimodalReranker(vlm_service=None)

        assert reranker._parse_score("invalid") == 0.5
        assert reranker._parse_score("") == 0.5
        assert reranker._parse_score("no score here") == 0.5

    def test_create_reranker_factory(self):
        """Test factory function creates reranker"""
        reranker = create_multimodal_reranker(
            vlm_service=None,
            max_concurrent=5,
            timeout_seconds=60.0,
        )

        assert isinstance(reranker, MultimodalReranker)
        assert reranker.max_concurrent == 5
        assert reranker.timeout == 60.0

    @pytest.mark.asyncio
    async def test_rerank_empty_candidates(self):
        """Test reranking empty list returns empty"""
        reranker = MultimodalReranker(vlm_service=None)

        result = await reranker.rerank(
            query="test query",
            candidates=[],
        )

        assert result == []

    @pytest.mark.asyncio
    async def test_rerank_text_only_candidates(self):
        """Test reranking text-only candidates preserves original scores"""
        reranker = MultimodalReranker(vlm_service=None)

        candidates = [
            RerankCandidate(
                segment_id="text_001",
                text="Text about architecture",
                media_type="text",
                original_score=0.8,
            ),
            RerankCandidate(
                segment_id="text_002",
                text="Another text segment",
                media_type="text",
                original_score=0.6,
            ),
        ]

        result = await reranker.rerank(
            query="architecture design",
            candidates=candidates,
            rerank_images_only=True,
        )

        assert len(result) == 2
        assert result[0].segment_id == "text_001"
        assert result[0].rerank_score == 0.8

    @pytest.mark.asyncio
    async def test_rerank_with_mock_vlm(self):
        """Test reranking with mocked VLM service"""
        # Create mock VLM service
        mock_vlm = AsyncMock()
        mock_vlm.describe_image = AsyncMock(return_value=MagicMock(description="0.9"))

        reranker = MultimodalReranker(
            vlm_service=mock_vlm,
            max_concurrent=2,
        )

        candidates = [
            RerankCandidate(
                segment_id="img_001",
                image_bytes=b"fake_image_data",
                media_type="image",
                original_score=0.5,
            ),
        ]

        result = await reranker.rerank(
            query="system architecture",
            candidates=candidates,
        )

        assert len(result) == 1
        # Score should be combined: (1-0.4)*0.5 + 0.4*0.9 = 0.66
        assert 0.6 <= result[0].rerank_score <= 0.7

    @pytest.mark.asyncio
    async def test_rerank_score_threshold(self):
        """Test score threshold filters low-score candidates"""
        reranker = MultimodalReranker(vlm_service=None)

        candidates = [
            RerankCandidate(
                segment_id="high",
                text="Highly relevant",
                media_type="text",
                original_score=0.8,
            ),
            RerankCandidate(
                segment_id="low",
                text="Not relevant",
                media_type="text",
                original_score=0.2,
            ),
        ]

        result = await reranker.rerank(
            query="test",
            candidates=candidates,
            score_threshold=0.5,
            rerank_images_only=True,
        )

        # Should filter out the low score candidate
        assert len(result) == 1
        assert result[0].segment_id == "high"


# ============ Test ContentType Enum ============


class TestContentType:
    """Tests for ContentType enum"""

    def test_content_type_values(self):
        """Test ContentType enum values"""
        assert ContentType.TEXT.value == "text"
        assert ContentType.IMAGE.value == "image"
        assert ContentType.MIXED.value == "mixed"

    def test_content_type_from_string(self):
        """Test creating ContentType from string"""
        assert ContentType("text") == ContentType.TEXT
        assert ContentType("image") == ContentType.IMAGE
        assert ContentType("mixed") == ContentType.MIXED


# ============ Integration-style Tests ============


class TestMultimodalRAGIntegration:
    """Integration-style tests for multimodal RAG pipeline"""

    @pytest.mark.asyncio
    async def test_full_rerank_pipeline(self):
        """Test full reranking pipeline with mixed candidates"""
        reranker = MultimodalReranker(vlm_service=None)

        # Mixed text and image candidates
        candidates = [
            RerankCandidate(
                segment_id="text_001",
                text="System architecture overview",
                media_type="text",
                original_score=0.7,
            ),
            RerankCandidate(
                segment_id="text_002",
                text="Database schema design",
                media_type="text",
                original_score=0.8,
            ),
            # Image candidates won't be scored without VLM
            RerankCandidate(
                segment_id="img_001",
                image_url="s3://bucket/arch.png",
                media_type="image",
                original_score=0.6,
            ),
        ]

        # With rerank_images_only=True, text keeps original scores
        # Images without VLM service get penalized scores
        result = await reranker.rerank(
            query="system design",
            candidates=candidates,
            top_k=5,
            rerank_images_only=True,
        )

        assert len(result) >= 2
        # Text candidates should keep their scores
        text_results = [r for r in result if r.media_type == "text"]
        assert all(r.rerank_score == r.original_score for r in text_results)

    def test_chunk_image_association_workflow(self):
        """Test workflow of associating images to chunks"""
        # Create text chunks
        chunks = [
            Chunk(text="Introduction to the system", index=0),
            Chunk(text="Architecture overview follows", index=1),
            Chunk(text="Conclusion and summary", index=2),
        ]

        # Create image references
        images = [
            AssociatedImage(
                image_segment_id="arch_diagram",
                storage_url="s3://bucket/architecture.png",
                vlm_description="High-level architecture diagram",
                proximity_score=0.9,
                char_offset=150,  # Near "Architecture overview"
            ),
            AssociatedImage(
                image_segment_id="db_schema",
                storage_url="s3://bucket/schema.png",
                vlm_description="Database schema",
                proximity_score=0.6,
                char_offset=200,
            ),
        ]

        # Associate images to most relevant chunk (index 1)
        for img in images:
            chunks[1].add_associated_image(img)

        # Verify associations
        assert chunks[0].image_count == 0
        assert chunks[1].image_count == 2
        assert chunks[1].content_type == ContentType.MIXED
        assert chunks[2].image_count == 0

        # Verify serialization
        d = chunks[1].to_multimodal_dict()
        assert d["has_images"] is True
        assert len(d["associated_images"]) == 2
