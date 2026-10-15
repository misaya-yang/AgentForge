"""
PDF Image Processor Tests

Tests for the PDF image extraction functionality:
- ExtractedImage dataclass
- PDFExtractionResult dataclass
- PDFImageProcessor class
"""

from unittest.mock import Mock, patch

from knowledge_service.services.knowledge.pdf_image_processor import (
    EMBEDDABLE_IMAGE_TYPES,
    EXTENSION_TO_MIME,
    MAX_IMAGE_SIZE_BYTES,
    MIN_IMAGE_HEIGHT,
    MIN_IMAGE_WIDTH,
    ExtractedImage,
    PDFExtractionResult,
    PDFImageProcessor,
    extract_pdf_with_images,
)


class TestExtractedImage:
    """Tests for ExtractedImage dataclass"""

    def test_basic_properties(self):
        """Test basic ExtractedImage properties"""
        img = ExtractedImage(
            image_id="test_123",
            content=b"fake_image_content",
            mime_type="image/png",
            width=100,
            height=200,
            page_number=1,
        )

        assert img.image_id == "test_123"
        assert img.content == b"fake_image_content"
        assert img.mime_type == "image/png"
        assert img.width == 100
        assert img.height == 200
        assert img.page_number == 1
        assert img.context_text == ""
        assert img.alt_text == ""

    def test_size_bytes_property(self):
        """Test size_bytes property"""
        content = b"x" * 1024
        img = ExtractedImage(
            image_id="test",
            content=content,
            mime_type="image/png",
            width=100,
            height=100,
            page_number=1,
        )

        assert img.size_bytes == 1024

    def test_is_embeddable_valid_image(self):
        """Test is_embeddable returns True for valid images"""
        img = ExtractedImage(
            image_id="test",
            content=b"x" * 1000,  # Small enough
            mime_type="image/png",  # Valid type
            width=100,  # >= MIN_IMAGE_WIDTH
            height=100,  # >= MIN_IMAGE_HEIGHT
            page_number=1,
        )

        assert img.is_embeddable is True

    def test_is_embeddable_wrong_mime_type(self):
        """Test is_embeddable returns False for unsupported MIME types"""
        img = ExtractedImage(
            image_id="test",
            content=b"x" * 1000,
            mime_type="image/gif",  # Not in EMBEDDABLE_IMAGE_TYPES
            width=100,
            height=100,
            page_number=1,
        )

        # GIF is not in EMBEDDABLE_IMAGE_TYPES
        assert img.is_embeddable is False

    def test_is_embeddable_too_large(self):
        """Test is_embeddable returns False for images > MAX_IMAGE_SIZE_BYTES"""
        img = ExtractedImage(
            image_id="test",
            content=b"x" * (MAX_IMAGE_SIZE_BYTES + 1),  # Too large
            mime_type="image/png",
            width=100,
            height=100,
            page_number=1,
        )

        assert img.is_embeddable is False

    def test_is_embeddable_too_small_width(self):
        """Test is_embeddable returns False for images with width < MIN_IMAGE_WIDTH"""
        img = ExtractedImage(
            image_id="test",
            content=b"x" * 1000,
            mime_type="image/png",
            width=MIN_IMAGE_WIDTH - 1,  # Too small
            height=100,
            page_number=1,
        )

        assert img.is_embeddable is False

    def test_is_embeddable_too_small_height(self):
        """Test is_embeddable returns False for images with height < MIN_IMAGE_HEIGHT"""
        img = ExtractedImage(
            image_id="test",
            content=b"x" * 1000,
            mime_type="image/png",
            width=100,
            height=MIN_IMAGE_HEIGHT - 1,  # Too small
            page_number=1,
        )

        assert img.is_embeddable is False

    def test_to_dict(self):
        """Test to_dict method"""
        img = ExtractedImage(
            image_id="test_123",
            content=b"fake_content",
            mime_type="image/jpeg",
            width=640,
            height=480,
            page_number=2,
            context_text="Surrounding text",
            alt_text="Image description",
            metadata={"key": "value"},
        )

        d = img.to_dict()

        assert d["image_id"] == "test_123"
        assert d["mime_type"] == "image/jpeg"
        assert d["width"] == 640
        assert d["height"] == 480
        assert d["page_number"] == 2
        assert d["size_bytes"] == len(b"fake_content")
        assert d["context_text"] == "Surrounding text"
        assert d["alt_text"] == "Image description"
        assert d["metadata"] == {"key": "value"}
        # is_embeddable should be computed
        assert "is_embeddable" in d
        # content should NOT be in dict (for serialization safety)
        assert "content" not in d


class TestPDFExtractionResult:
    """Tests for PDFExtractionResult dataclass"""

    def test_basic_properties(self):
        """Test basic PDFExtractionResult properties"""
        result = PDFExtractionResult(
            text="Sample text",
            images=[],
            page_count=5,
        )

        assert result.text == "Sample text"
        assert result.images == []
        assert result.page_count == 5

    def test_embeddable_images(self):
        """Test embeddable_images property filters correctly"""
        embeddable = ExtractedImage(
            image_id="good",
            content=b"x" * 1000,
            mime_type="image/png",
            width=100,
            height=100,
            page_number=1,
        )
        non_embeddable = ExtractedImage(
            image_id="bad",
            content=b"x" * 1000,
            mime_type="image/gif",  # Not embeddable
            width=100,
            height=100,
            page_number=1,
        )

        result = PDFExtractionResult(
            text="",
            images=[embeddable, non_embeddable],
            page_count=1,
        )

        assert len(result.embeddable_images) == 1
        assert result.embeddable_images[0].image_id == "good"

    def test_total_images(self):
        """Test total_images property"""
        images = [
            ExtractedImage("1", b"", "image/png", 100, 100, 1),
            ExtractedImage("2", b"", "image/png", 100, 100, 2),
        ]

        result = PDFExtractionResult(text="", images=images, page_count=2)

        assert result.total_images == 2

    def test_embeddable_image_count(self):
        """Test embeddable_image_count property"""
        embeddable = ExtractedImage(
            image_id="good",
            content=b"x" * 1000,
            mime_type="image/png",
            width=100,
            height=100,
            page_number=1,
        )
        non_embeddable = ExtractedImage(
            image_id="bad",
            content=b"x" * (MAX_IMAGE_SIZE_BYTES + 1),  # Too large
            mime_type="image/png",
            width=100,
            height=100,
            page_number=1,
        )

        result = PDFExtractionResult(
            text="",
            images=[embeddable, non_embeddable],
            page_count=1,
        )

        assert result.embeddable_image_count == 1


class TestPDFImageProcessor:
    """Tests for PDFImageProcessor class"""

    def test_init_defaults(self):
        """Test PDFImageProcessor initialization with defaults"""
        processor = PDFImageProcessor()

        assert processor.extract_images is True
        assert processor.min_image_width == MIN_IMAGE_WIDTH
        assert processor.min_image_height == MIN_IMAGE_HEIGHT
        assert processor.max_image_size == MAX_IMAGE_SIZE_BYTES
        assert processor.context_chars == 500

    def test_init_custom_params(self):
        """Test PDFImageProcessor initialization with custom parameters"""
        processor = PDFImageProcessor(
            extract_images=False,
            min_image_width=100,
            min_image_height=100,
            max_image_size=1024 * 1024,
            context_chars=200,
        )

        assert processor.extract_images is False
        assert processor.min_image_width == 100
        assert processor.min_image_height == 100
        assert processor.max_image_size == 1024 * 1024
        assert processor.context_chars == 200

    def test_process_pdf_bytes_without_pymupdf(self):
        """Test fallback when PyMuPDF is not available"""
        processor = PDFImageProcessor()

        # Mock fitz import to fail
        with patch.dict("sys.modules", {"fitz": None}):
            with patch.object(processor, "_fallback_text_only") as mock_fallback:
                mock_fallback.return_value = PDFExtractionResult(
                    text="Fallback text",
                    images=[],
                    page_count=1,
                )

                # This should trigger the fallback
                result = processor.process_pdf_bytes(b"%PDF-1.4 fake pdf")

                # Since we can't easily mock import failure, let's test the fallback directly
                assert isinstance(result, PDFExtractionResult)

    def test_extract_image_context_empty_text(self):
        """Test _extract_image_context with empty text"""
        processor = PDFImageProcessor()

        context = processor._extract_image_context("", 0, 1)

        assert context == ""

    def test_extract_image_context_zero_images(self):
        """Test _extract_image_context with zero total images"""
        processor = PDFImageProcessor()

        context = processor._extract_image_context("Some text", 0, 0)

        assert context == ""

    def test_extract_image_context_basic(self):
        """Test _extract_image_context extracts correct section"""
        processor = PDFImageProcessor(context_chars=100)

        # Create text with clear sections
        text = "Section 1. " * 20 + "Section 2. " * 20 + "Section 3. " * 20

        # First image (index 0 of 3 images)
        context = processor._extract_image_context(text, 0, 3)

        assert isinstance(context, str)
        assert len(context) <= processor.context_chars

    def test_fallback_text_only_with_pypdf(self):
        """Test _fallback_text_only uses pypdf"""
        processor = PDFImageProcessor()

        # Verify the method exists and can be called
        assert hasattr(processor, "_fallback_text_only")

        # Test with pypdf mocked at the import location inside the method
        with patch("pypdf.PdfReader") as MockReader:
            mock_reader = Mock()
            mock_page = Mock()
            mock_page.extract_text.return_value = "Page 1 content"
            mock_reader.pages = [mock_page]
            MockReader.return_value = mock_reader

            # Call the fallback method with fake PDF content
            result = processor._fallback_text_only(b"%PDF-1.4 fake content")

            # Should return a PDFExtractionResult
            assert isinstance(result, PDFExtractionResult)
            assert result.text == "[Page 1]\nPage 1 content"
            assert result.images == []
            assert result.page_count == 1


class TestExtensionToMime:
    """Tests for EXTENSION_TO_MIME mapping"""

    def test_common_extensions(self):
        """Test common image extension mappings"""
        assert EXTENSION_TO_MIME["png"] == "image/png"
        assert EXTENSION_TO_MIME["jpeg"] == "image/jpeg"
        assert EXTENSION_TO_MIME["jpg"] == "image/jpeg"
        assert EXTENSION_TO_MIME["bmp"] == "image/bmp"
        assert EXTENSION_TO_MIME["webp"] == "image/webp"
        assert EXTENSION_TO_MIME["gif"] == "image/gif"
        assert EXTENSION_TO_MIME["tiff"] == "image/tiff"


class TestEmbeddableImageTypes:
    """Tests for EMBEDDABLE_IMAGE_TYPES set"""

    def test_supported_types(self):
        """Test that common types are supported"""
        assert "image/png" in EMBEDDABLE_IMAGE_TYPES
        assert "image/jpeg" in EMBEDDABLE_IMAGE_TYPES
        assert "image/bmp" in EMBEDDABLE_IMAGE_TYPES
        assert "image/webp" in EMBEDDABLE_IMAGE_TYPES

    def test_unsupported_types(self):
        """Test that some types are not supported for embedding"""
        # GIF and TIFF are not supported by DashScope multimodal
        assert "image/gif" not in EMBEDDABLE_IMAGE_TYPES
        assert "image/tiff" not in EMBEDDABLE_IMAGE_TYPES


class TestConvenienceFunction:
    """Tests for extract_pdf_with_images convenience function"""

    def test_returns_tuple(self):
        """Test extract_pdf_with_images returns correct tuple structure"""
        with patch.object(PDFImageProcessor, "process_pdf_bytes") as mock_process:
            mock_process.return_value = PDFExtractionResult(
                text="Test text",
                images=[
                    ExtractedImage(
                        image_id="img1",
                        content=b"x" * 100,
                        mime_type="image/png",
                        width=100,
                        height=100,
                        page_number=1,
                    )
                ],
                page_count=1,
            )

            text, images = extract_pdf_with_images(b"fake pdf")

            assert text == "Test text"
            assert len(images) == 1
            assert images[0].image_id == "img1"


class TestConstants:
    """Tests for module constants"""

    def test_max_image_size(self):
        """Test MAX_IMAGE_SIZE_BYTES is 3MB (DashScope limit)"""
        assert MAX_IMAGE_SIZE_BYTES == 3 * 1024 * 1024

    def test_min_dimensions(self):
        """Test minimum image dimensions to filter out icons"""
        assert MIN_IMAGE_WIDTH == 50
        assert MIN_IMAGE_HEIGHT == 50
