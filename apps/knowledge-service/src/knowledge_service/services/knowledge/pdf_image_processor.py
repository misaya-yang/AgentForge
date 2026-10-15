"""
PDF Image Processor

Extracts images from PDF documents for multimodal embedding.
Uses PyMuPDF (fitz) for reliable image extraction.
"""

from __future__ import annotations

import hashlib
import logging
from dataclasses import dataclass, field
from typing import Any

logger = logging.getLogger(__name__)


# Supported image formats for multimodal embedding
EMBEDDABLE_IMAGE_TYPES = {
    "image/png",
    "image/jpeg",
    "image/jpg",
    "image/bmp",
    "image/webp",
}

# PyMuPDF image extension to MIME type mapping
EXTENSION_TO_MIME = {
    "png": "image/png",
    "jpeg": "image/jpeg",
    "jpg": "image/jpeg",
    "jpe": "image/jpeg",
    "bmp": "image/bmp",
    "webp": "image/webp",
    "gif": "image/gif",
    "tiff": "image/tiff",
    "tif": "image/tiff",
}

# Maximum image size for embedding (3MB for DashScope)
MAX_IMAGE_SIZE_BYTES = 3 * 1024 * 1024

# Minimum image dimensions (skip tiny images like icons)
MIN_IMAGE_WIDTH = 50
MIN_IMAGE_HEIGHT = 50


@dataclass
class ExtractedImage:
    """Represents an image extracted from a PDF"""

    image_id: str  # Unique ID (hash of content)
    content: bytes  # Raw image bytes
    mime_type: str  # MIME type (image/png, etc.)
    width: int  # Image width in pixels
    height: int  # Image height in pixels
    page_number: int  # Page where image was found (1-indexed)
    context_text: str = ""  # Surrounding text context
    alt_text: str = ""  # Alt text if available
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def size_bytes(self) -> int:
        """Size of image in bytes"""
        return len(self.content)

    @property
    def is_embeddable(self) -> bool:
        """Check if image can be embedded (right format and size)"""
        return (
            self.mime_type in EMBEDDABLE_IMAGE_TYPES
            and self.size_bytes <= MAX_IMAGE_SIZE_BYTES
            and self.width >= MIN_IMAGE_WIDTH
            and self.height >= MIN_IMAGE_HEIGHT
        )

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary (without content for serialization)"""
        return {
            "image_id": self.image_id,
            "mime_type": self.mime_type,
            "width": self.width,
            "height": self.height,
            "page_number": self.page_number,
            "size_bytes": self.size_bytes,
            "context_text": self.context_text,
            "alt_text": self.alt_text,
            "is_embeddable": self.is_embeddable,
            "metadata": self.metadata,
        }


@dataclass
class PDFExtractionResult:
    """Result of PDF extraction containing text and images"""

    text: str  # Extracted text content
    images: list[ExtractedImage]  # Extracted images
    page_count: int  # Total pages in PDF
    metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def embeddable_images(self) -> list[ExtractedImage]:
        """Get only images that can be embedded"""
        return [img for img in self.images if img.is_embeddable]

    @property
    def total_images(self) -> int:
        return len(self.images)

    @property
    def embeddable_image_count(self) -> int:
        return len(self.embeddable_images)


class PDFImageProcessor:
    """
    Extracts text and images from PDF documents.

    Uses PyMuPDF for:
    - Text extraction with layout preservation
    - Image extraction with metadata
    - Page-by-page processing
    """

    def __init__(
        self,
        extract_images: bool = True,
        min_image_width: int = MIN_IMAGE_WIDTH,
        min_image_height: int = MIN_IMAGE_HEIGHT,
        max_image_size: int = MAX_IMAGE_SIZE_BYTES,
        context_chars: int = 500,
    ):
        """
        Initialize PDF processor.

        Args:
            extract_images: Whether to extract images
            min_image_width: Minimum image width to extract
            min_image_height: Minimum image height to extract
            max_image_size: Maximum image size in bytes
            context_chars: Characters of context to extract around images
        """
        self.extract_images = extract_images
        self.min_image_width = min_image_width
        self.min_image_height = min_image_height
        self.max_image_size = max_image_size
        self.context_chars = context_chars

    def process_pdf_bytes(self, content: bytes) -> PDFExtractionResult:
        """
        Process PDF from bytes and extract text + images.

        Args:
            content: PDF file content as bytes

        Returns:
            PDFExtractionResult with text and images
        """
        try:
            try:
                import pymupdf as fitz  # type: ignore
            except ImportError:
                import fitz  # type: ignore
        except ImportError:
            logger.warning("PyMuPDF not installed, falling back to text-only extraction")
            return self._fallback_text_only(content)

        try:
            doc = fitz.open(stream=content, filetype="pdf")
        except Exception as e:
            logger.error(f"Failed to open PDF: {e}")
            return self._fallback_text_only(content)

        try:
            text_parts: list[str] = []
            images: list[ExtractedImage] = []
            seen_image_hashes: set = set()  # Deduplicate images

            metadata = {
                "title": doc.metadata.get("title", ""),
                "author": doc.metadata.get("author", ""),
                "subject": doc.metadata.get("subject", ""),
                "creator": doc.metadata.get("creator", ""),
                "page_count": doc.page_count,
            }

            for page_num, page in enumerate(doc, start=1):
                # Extract text from page
                page_text = page.get_text("text")
                if page_text.strip():
                    text_parts.append(f"[Page {page_num}]\n{page_text}")

                # Extract images from page
                if self.extract_images:
                    page_images = self._extract_page_images(
                        doc, page, page_num, page_text, seen_image_hashes
                    )
                    images.extend(page_images)

            doc.close()

            full_text = "\n\n".join(text_parts)

            logger.info(
                f"PDF extraction complete: {len(text_parts)} pages, "
                f"{len(images)} images ({sum(1 for i in images if i.is_embeddable)} embeddable)"
            )

            return PDFExtractionResult(
                text=full_text,
                images=images,
                page_count=doc.page_count if hasattr(doc, "page_count") else len(text_parts),
                metadata=metadata,
            )

        except Exception as e:
            logger.error(f"PDF extraction failed: {e}")
            doc.close()
            return self._fallback_text_only(content)

    def _extract_page_images(
        self,
        doc,
        page,
        page_num: int,
        page_text: str,
        seen_hashes: set,
    ) -> list[ExtractedImage]:
        """Extract images from a single PDF page."""
        try:
            import pymupdf as fitz  # type: ignore
        except ImportError:
            pass  # type: ignore

        images: list[ExtractedImage] = []

        try:
            # Get list of images on page
            image_list = page.get_images(full=True)

            for img_index, img_info in enumerate(image_list):
                try:
                    xref = img_info[0]  # Image reference number

                    # Extract image
                    base_image = doc.extract_image(xref)
                    if not base_image:
                        continue

                    image_bytes = base_image.get("image")
                    if not image_bytes:
                        continue

                    # Check size
                    if len(image_bytes) > self.max_image_size:
                        logger.debug(f"Skipping large image: {len(image_bytes)} bytes")
                        continue

                    # Get image properties
                    ext = base_image.get("ext", "png").lower()
                    width = base_image.get("width", 0)
                    height = base_image.get("height", 0)

                    # Check dimensions
                    if width < self.min_image_width or height < self.min_image_height:
                        logger.debug(f"Skipping small image: {width}x{height}")
                        continue

                    # Generate hash to deduplicate
                    image_hash = hashlib.md5(image_bytes).hexdigest()
                    if image_hash in seen_hashes:
                        logger.debug(f"Skipping duplicate image: {image_hash[:8]}")
                        continue
                    seen_hashes.add(image_hash)

                    # Determine MIME type
                    mime_type = EXTENSION_TO_MIME.get(ext, f"image/{ext}")

                    # Extract context (text around image position)
                    context = self._extract_image_context(page_text, img_index, len(image_list))

                    extracted = ExtractedImage(
                        image_id=f"pdf_img_{image_hash[:12]}",
                        content=image_bytes,
                        mime_type=mime_type,
                        width=width,
                        height=height,
                        page_number=page_num,
                        context_text=context,
                        metadata={
                            "xref": xref,
                            "extension": ext,
                            "colorspace": base_image.get("colorspace", ""),
                            "bpc": base_image.get("bpc", 0),
                        },
                    )

                    images.append(extracted)
                    logger.debug(
                        f"Extracted image: {extracted.image_id}, "
                        f"{width}x{height}, {mime_type}, {len(image_bytes)} bytes"
                    )

                except Exception as e:
                    logger.warning(f"Failed to extract image {img_index} from page {page_num}: {e}")
                    continue

        except Exception as e:
            logger.warning(f"Failed to get images from page {page_num}: {e}")

        return images

    def _extract_image_context(
        self,
        page_text: str,
        img_index: int,
        total_images: int,
    ) -> str:
        """
        Extract contextual text around an image.

        Since we don't have exact image positions in text flow,
        we divide the page text by image count and take relevant section.
        """
        if not page_text or total_images == 0:
            return ""

        # Rough estimation: divide text by number of images
        text_len = len(page_text)
        section_size = text_len // max(total_images, 1)

        # Get section around this image
        start = max(0, img_index * section_size - self.context_chars // 2)
        end = min(text_len, (img_index + 1) * section_size + self.context_chars // 2)

        context = page_text[start:end].strip()

        # Extend to sentence boundaries if possible
        # Support both English and Chinese punctuation marks
        sentence_end_markers = [". ", "。", "！", "？", "；", "\n"]

        if start > 0:
            # Find sentence start - look for the first sentence boundary
            best_pos = -1
            best_marker_len = 0
            for marker in sentence_end_markers:
                pos = context.find(marker)
                if pos >= 0 and pos < 100:
                    # Take the earliest marker found
                    if best_pos < 0 or pos < best_pos:
                        best_pos = pos
                        best_marker_len = len(marker)
            if best_pos >= 0:
                context = context[best_pos + best_marker_len :]

        # Limit length
        if len(context) > self.context_chars:
            context = context[: self.context_chars]
            # Try to end at sentence - look for the last sentence boundary
            best_pos = -1
            for marker in sentence_end_markers:
                pos = context.rfind(marker)
                if pos > len(context) // 2 and pos > best_pos:
                    best_pos = pos
            if best_pos > 0:
                context = context[: best_pos + 1]

        return context.strip()

    def _fallback_text_only(self, content: bytes) -> PDFExtractionResult:
        """Fallback to text-only extraction when PyMuPDF fails."""
        try:
            from io import BytesIO

            from pypdf import PdfReader

            reader = PdfReader(BytesIO(content))
            text_parts = []

            for i, page in enumerate(reader.pages, start=1):
                try:
                    text = page.extract_text() or ""
                    if text.strip():
                        text_parts.append(f"[Page {i}]\n{text}")
                except Exception as e:
                    logger.warning(f"Failed to extract text from page {i}: {e}")

            return PDFExtractionResult(
                text="\n\n".join(text_parts),
                images=[],
                page_count=len(reader.pages),
                metadata={},
            )
        except Exception as e:
            logger.error(f"Fallback text extraction failed: {e}")
            return PDFExtractionResult(text="", images=[], page_count=0)


def extract_pdf_with_images(content: bytes) -> tuple[str, list[ExtractedImage]]:
    """
    Convenience function to extract text and images from PDF.

    Args:
        content: PDF file bytes

    Returns:
        Tuple of (text, list of images)
    """
    processor = PDFImageProcessor()
    result = processor.process_pdf_bytes(content)
    return result.text, result.embeddable_images
