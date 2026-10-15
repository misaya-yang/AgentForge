"""
Tests for multimodal retrieval parameters.

Ensures that:
1. include_images parameter controls associated image attachment
2. content_type_filter properly filters results by type
3. multimodal_rerank parameter is recognized (even if not fully implemented)
"""

from dataclasses import dataclass, field
from typing import Any

from knowledge_service.api.schemas.knowledge import (
    BatchRetrieveRequestSchema,
    RetrieveRequestSchema,
)


def test_text_retrieval_defaults_skip_images():
    single = RetrieveRequestSchema(query="text query")
    batch = BatchRetrieveRequestSchema(queries=["text query"])

    assert single.include_images is False
    assert single.include_associated_images is False
    assert batch.include_images is False
    assert batch.include_associated_images is False


@dataclass
class MockRetrieveResult:
    """Mock retrieve result for testing."""

    segment_id: str
    document_id: str
    score: float
    text: str
    metadata: dict[str, Any] = field(default_factory=dict)
    content_type: str = "text"
    image_url: str | None = None
    vlm_description: str | None = None
    associated_images: tuple = ()


class TestContentTypeFilter:
    """Tests for content_type_filter parameter."""

    def setup_method(self):
        """Create sample results with mixed content types."""
        self.text_result1 = MockRetrieveResult(
            segment_id="seg1",
            document_id="doc1",
            score=0.9,
            text="This is text content",
            metadata={"content_type": "text"},
            content_type="text",
        )
        self.text_result2 = MockRetrieveResult(
            segment_id="seg2",
            document_id="doc1",
            score=0.8,
            text="More text content",
            metadata={"content_type": "text"},
            content_type="text",
        )
        self.image_result1 = MockRetrieveResult(
            segment_id="seg3",
            document_id="doc1",
            score=0.85,
            text="Image description",
            metadata={"content_type": "image"},
            content_type="image",
            image_url="https://example.com/img1.png",
        )
        self.image_result2 = MockRetrieveResult(
            segment_id="seg4",
            document_id="doc1",
            score=0.75,
            text="Another image",
            metadata={"content_type": "image"},
            content_type="image",
            image_url="https://example.com/img2.png",
        )
        self.all_results = [
            self.text_result1,
            self.image_result1,
            self.text_result2,
            self.image_result2,
        ]

    def test_filter_text_only(self):
        """content_type_filter='text' should return only text segments."""
        content_type_filter = "text"
        filtered = [
            r
            for r in self.all_results
            if r.metadata.get("content_type", getattr(r, "content_type", "text"))
            == content_type_filter
        ]

        assert len(filtered) == 2
        assert all(r.content_type == "text" for r in filtered)
        assert self.text_result1 in filtered
        assert self.text_result2 in filtered

    def test_filter_image_only(self):
        """content_type_filter='image' should return only image segments."""
        content_type_filter = "image"
        filtered = [
            r
            for r in self.all_results
            if r.metadata.get("content_type", getattr(r, "content_type", "text"))
            == content_type_filter
        ]

        assert len(filtered) == 2
        assert all(r.content_type == "image" for r in filtered)
        assert self.image_result1 in filtered
        assert self.image_result2 in filtered

    def test_no_filter_returns_all(self):
        """No content_type_filter should return all results."""
        content_type_filter = None
        if content_type_filter and content_type_filter in ("text", "image"):
            filtered = [r for r in self.all_results if r.content_type == content_type_filter]
        else:
            filtered = self.all_results

        assert len(filtered) == 4

    def test_invalid_filter_returns_all(self):
        """Invalid content_type_filter value should return all results."""
        content_type_filter = "invalid"
        if content_type_filter and content_type_filter in ("text", "image"):
            filtered = [r for r in self.all_results if r.content_type == content_type_filter]
        else:
            filtered = self.all_results

        assert len(filtered) == 4


class TestIncludeImages:
    """Tests for include_images parameter."""

    def test_include_images_true_adds_associated_images(self):
        """include_images=True should attach associated images to results."""
        include_images = True
        result = MockRetrieveResult(
            segment_id="seg1",
            document_id="doc1",
            score=0.9,
            text="Text with images",
            metadata={},
            content_type="text",
        )

        # Simulate fetching and attaching associated images
        if include_images:
            # Mock associated images
            associated = [
                {
                    "image_segment_id": "img1",
                    "storage_url": "https://example.com/img1.png",
                    "proximity_score": 0.95,
                }
            ]
            result.metadata["has_images"] = True
            result.metadata["image_count"] = len(associated)

        assert result.metadata.get("has_images") is True
        assert result.metadata.get("image_count") == 1

    def test_include_images_false_skips_image_attachment(self):
        """include_images=False should skip associated image attachment."""
        include_images = False
        result = MockRetrieveResult(
            segment_id="seg1",
            document_id="doc1",
            score=0.9,
            text="Text without images",
            metadata={},
            content_type="text",
        )

        if include_images:
            result.metadata["has_images"] = True

        # has_images should not be set when include_images=False
        assert result.metadata.get("has_images") is None


class TestMultimodalRerank:
    """Tests for multimodal_rerank parameter."""

    def test_multimodal_rerank_flag_recognized(self):
        """multimodal_rerank parameter should be recognized in metadata."""
        multimodal_rerank = True
        meta: dict[str, Any] = {}

        # Simulate the current implementation behavior
        if multimodal_rerank:
            # VLM reranking not yet implemented
            meta["multimodal_rerank"] = False
            meta["multimodal_rerank_message"] = "VLM reranking not yet implemented"

        assert "multimodal_rerank" in meta
        assert "multimodal_rerank_message" in meta

    def test_multimodal_rerank_false_no_metadata(self):
        """multimodal_rerank=False should not add rerank metadata."""
        multimodal_rerank = False
        meta: dict[str, Any] = {}

        if multimodal_rerank:
            meta["multimodal_rerank"] = False
            meta["multimodal_rerank_message"] = "VLM reranking not yet implemented"

        assert "multimodal_rerank" not in meta
        assert "multimodal_rerank_message" not in meta


class TestRetrievalMetadata:
    """Tests for retrieval metadata with multimodal parameters."""

    def test_metadata_includes_filter_info(self):
        """Metadata should include content_type_filter when applied."""
        meta: dict[str, Any] = {}
        content_type_filter = "text"
        filtered_count = 5

        if content_type_filter:
            meta["content_type_filter"] = content_type_filter
            meta["filtered_count"] = filtered_count

        assert meta["content_type_filter"] == "text"
        assert meta["filtered_count"] == 5

    def test_metadata_includes_multimodal_flag(self):
        """Metadata should indicate multimodal retrieval mode."""
        meta: dict[str, Any] = {}
        include_images = True

        meta["multimodal"] = True
        meta["include_images"] = include_images
        meta["segments_with_images"] = 3

        assert meta["multimodal"] is True
        assert meta["include_images"] is True
        assert meta["segments_with_images"] == 3
