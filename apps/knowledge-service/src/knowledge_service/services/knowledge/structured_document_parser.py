"""
Structured Document Parser for Multimodal RAG

Provides document structure-aware parsing with:
- Semantic chunking with structure preservation
- Image-text binding
- Table structure extraction
- VLM-enhanced image understanding

Usage:
    parser = StructuredDocumentParser(vlm_service=vlm_service)
    result = await parser.parse_pdf(pdf_bytes, filename="doc.pdf")

    for chunk in result.chunks:
        print(f"Type: {chunk.type}, Content: {chunk.content[:100]}")
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

logger = logging.getLogger(__name__)


class ChunkType(str, Enum):
    """Type of document chunk."""

    TEXT = "text"  # Pure text paragraph
    HEADING = "heading"  # Section/chapter heading
    TABLE = "table"  # Table (structured data)
    IMAGE = "image"  # Image with description
    MIXED = "mixed"  # Text + images combined
    CAPTION = "caption"  # Figure/table caption


@dataclass
class DocumentElement:
    """A single element in a document (paragraph, image, table, etc.)."""

    type: ChunkType
    content: str  # Text content or description
    page_number: int
    bbox: tuple[float, float, float, float] | None = None  # x0, y0, x1, y1
    metadata: dict[str, Any] = field(default_factory=dict)

    # For images
    image_bytes: bytes | None = None
    image_mime: str | None = None
    vlm_description: str | None = None

    # For tables
    table_html: str | None = None
    table_markdown: str | None = None

    # Relationships
    parent_id: str | None = None  # Parent heading/section
    children_ids: list[str] = field(default_factory=list)
    related_image_ids: list[str] = field(default_factory=list)


@dataclass
class StructuredChunk:
    """A semantic chunk ready for embedding."""

    chunk_id: str
    type: ChunkType
    content: str  # Primary text content
    content_for_embedding: str  # Enhanced content for embedding
    page_number: int

    # Multimodal data
    text: str = ""  # Clean text for text embedding
    images: list[dict[str, Any]] = field(default_factory=list)  # Image data
    has_images: bool = False

    # Metadata
    metadata: dict[str, Any] = field(default_factory=dict)
    section_title: str | None = None
    section_level: int = 0

    # Source tracking
    source_elements: list[str] = field(default_factory=list)  # Element IDs
    char_range: tuple[int, int] | None = None


@dataclass
class ParseResult:
    """Result of structured document parsing."""

    chunks: list[StructuredChunk]
    elements: list[DocumentElement]  # Original elements
    document_metadata: dict[str, Any] = field(default_factory=dict)

    @property
    def text_chunks(self) -> list[StructuredChunk]:
        """Get text-only chunks."""
        return [c for c in self.chunks if c.type in (ChunkType.TEXT, ChunkType.HEADING)]

    @property
    def image_chunks(self) -> list[StructuredChunk]:
        """Get image chunks."""
        return [c for c in self.chunks if c.type == ChunkType.IMAGE]

    @property
    def table_chunks(self) -> list[StructuredChunk]:
        """Get table chunks."""
        return [c for c in self.chunks if c.type == ChunkType.TABLE]


class StructuredDocumentParser:
    """
    Document parser that preserves structure for multimodal RAG.

    Features:
    - Extracts document hierarchy (chapters, sections)
    - Binds images with their captions
    - Preserves table structure
    - VLM-based image description (optional)
    """

    def __init__(
        self,
        vlm_service: Any | None = None,
        enable_vlm_description: bool = False,
        min_image_width: int = 100,
        min_image_height: int = 100,
        max_vlm_images: int = 10,  # Limit VLM calls per document
    ):
        self.vlm_service = vlm_service
        self.enable_vlm_description = enable_vlm_description and vlm_service is not None
        self.min_image_width = min_image_width
        self.min_image_height = min_image_height
        self.max_vlm_images = max_vlm_images

        # Statistics
        self._vlm_calls = 0

    async def parse_pdf(
        self,
        pdf_bytes: bytes,
        filename: str | None = None,
    ) -> ParseResult:
        """
        Parse PDF with structure awareness.

        Flow:
        1. Extract elements with positions
        2. Build document hierarchy
        3. Group related elements (image + caption)
        4. Generate semantic chunks
        5. Optional: VLM description for key images
        """
        try:
            try:
                import pymupdf as fitz  # type: ignore
            except ImportError:
                import fitz  # type: ignore
        except ImportError:
            raise RuntimeError("PyMuPDF required for PDF parsing")

        self._vlm_calls = 0

        doc = fitz.open(stream=pdf_bytes, filetype="pdf")

        try:
            # Stage 1: Extract all elements with positions
            elements = await self._extract_elements(doc, pdf_bytes)

            # Stage 2: Build document hierarchy
            elements = self._build_hierarchy(elements)

            # Stage 3: Bind related elements
            elements = self._bind_related_elements(elements)

            # Stage 4: Generate VLM descriptions for key images
            if self.enable_vlm_description:
                elements = await self._enrich_images_with_vlm(elements)

            # Stage 5: Create semantic chunks
            chunks = self._create_semantic_chunks(elements)

            return ParseResult(
                chunks=chunks,
                elements=elements,
                document_metadata={
                    "filename": filename,
                    "page_count": len(doc),
                    "total_elements": len(elements),
                    "vlm_calls": self._vlm_calls,
                },
            )
        finally:
            doc.close()

    async def _extract_elements(
        self,
        doc: Any,
        pdf_bytes: bytes,
    ) -> list[DocumentElement]:
        """Extract all elements from PDF with positions."""
        elements: list[DocumentElement] = []

        for page_num, page in enumerate(doc):
            page_elements = await self._extract_page_elements(page, page_num)
            elements.extend(page_elements)

        # Sort by page, then by vertical position (reading order)
        elements.sort(key=lambda e: (e.page_number, e.bbox[1] if e.bbox else 0))

        return elements

    async def _extract_page_elements(
        self,
        page: Any,
        page_num: int,
    ) -> list[DocumentElement]:
        """Extract elements from a single page."""
        elements: list[DocumentElement] = []

        # Get page dimensions
        page_rect = page.rect

        # Extract text blocks with their bounding boxes
        text_dict = page.get_text("dict")

        for block in text_dict.get("blocks", []):
            if block.get("type") == 0:  # Text block
                element = self._parse_text_block(block, page_num, page_rect)
                if element:
                    elements.append(element)

            elif block.get("type") == 1:  # Image block
                element = await self._parse_image_block(block, page_num, page)
                if element:
                    elements.append(element)

        # Extract tables (if table detection is available)
        table_elements = self._detect_tables(page, page_num)
        elements.extend(table_elements)

        return elements

    def _parse_text_block(
        self,
        block: dict[str, Any],
        page_num: int,
        page_rect: Any,
    ) -> DocumentElement | None:
        """Parse a text block into DocumentElement."""
        bbox = block.get("bbox")
        if not bbox:
            return None

        # Extract text from lines
        text_parts = []
        for line in block.get("lines", []):
            line_text = "".join(span.get("text", "") for span in line.get("spans", []))
            text_parts.append(line_text)

        text = " ".join(text_parts).strip()
        if not text:
            return None

        # Determine if this is a heading
        chunk_type = self._classify_text_block(block, text)

        return DocumentElement(
            type=chunk_type,
            content=text,
            page_number=page_num,
            bbox=tuple(bbox),
            metadata={
                "font_size": self._get_dominant_font_size(block),
                "is_bold": self._is_bold_block(block),
            },
        )

    def _classify_text_block(
        self,
        block: dict[str, Any],
        text: str,
    ) -> ChunkType:
        """Classify if a text block is heading, caption, or regular text."""
        # Get font info
        font_sizes = []
        for line in block.get("lines", []):
            for span in line.get("spans", []):
                font_sizes.append(span.get("size", 12))

        if not font_sizes:
            return ChunkType.TEXT

        avg_size = sum(font_sizes) / len(font_sizes)

        # Heuristics for headings
        if avg_size > 16 or text.isupper():
            # Check if it looks like a caption
            lower = text.lower()
            if any(lower.startswith(x) for x in ["figure", "table", "fig.", "tab."]):
                return ChunkType.CAPTION
            return ChunkType.HEADING

        # Check for caption patterns
        lower = text.lower()
        if any(lower.startswith(x) for x in ["figure", "table", "fig.", "tab.", "chart"]):
            return ChunkType.CAPTION

        return ChunkType.TEXT

    async def _parse_image_block(
        self,
        block: dict[str, Any],
        page_num: int,
        page: Any,
    ) -> DocumentElement | None:
        """Parse an image block."""
        bbox = block.get("bbox")
        if not bbox:
            return None

        try:
            # Import fitz here since it's only needed when PDF parsing is active
            try:
                import pymupdf as fitz  # type: ignore
            except ImportError:
                import fitz  # type: ignore

            # Extract image from page
            pix = page.get_pixmap(clip=bbox, matrix=fitz.Matrix(2, 2))  # 2x for quality
            img_bytes = pix.tobytes("png")

            width, height = pix.width, pix.height

            # Skip small images
            if width < self.min_image_width or height < self.min_image_height:
                return None

            return DocumentElement(
                type=ChunkType.IMAGE,
                content=f"[Image: {width}x{height}]",
                page_number=page_num,
                bbox=tuple(bbox),
                image_bytes=img_bytes,
                image_mime="image/png",
                metadata={
                    "width": width,
                    "height": height,
                    "size_bytes": len(img_bytes),
                },
            )
        except Exception as e:
            logger.debug(f"Failed to extract image: {e}")
            return None

    def _detect_tables(
        self,
        page: Any,
        page_num: int,
    ) -> list[DocumentElement]:
        """Detect tables on the page."""
        elements = []

        try:
            # Try to find tables using PyMuPDF's table detection
            tables = page.find_tables()

            for table_idx, table in enumerate(tables):
                try:
                    # Extract table as markdown
                    df = table.to_pandas()
                    markdown = df.to_markdown(index=False)

                    # Also get HTML representation
                    html = df.to_html(index=False)

                    bbox = table.bbox if hasattr(table, "bbox") else None

                    elements.append(
                        DocumentElement(
                            type=ChunkType.TABLE,
                            content=markdown,
                            page_number=page_num,
                            bbox=bbox,
                            table_markdown=markdown,
                            table_html=html,
                            metadata={
                                "table_index": table_idx,
                                "row_count": len(df),
                                "col_count": len(df.columns),
                            },
                        )
                    )
                except Exception as e:
                    logger.debug(f"Failed to extract table: {e}")
        except Exception as e:
            logger.debug(f"Table detection not available: {e}")

        return elements

    def _build_hierarchy(
        self,
        elements: list[DocumentElement],
    ) -> list[DocumentElement]:
        """Build document hierarchy (sections, subsections)."""
        # Simple hierarchy: track current section
        current_section_id = None
        current_section_level = 0

        for i, element in enumerate(elements):
            element_id = f"elem_{i}"
            element.metadata["id"] = element_id

            if element.type == ChunkType.HEADING:
                # Determine heading level by font size
                level = self._estimate_heading_level(element)
                current_section_id = element_id
                current_section_level = level
                element.metadata["heading_level"] = level

            elif current_section_id:
                element.parent_id = current_section_id
                element.section_level = current_section_level

        return elements

    def _bind_related_elements(
        self,
        elements: list[DocumentElement],
    ) -> list[DocumentElement]:
        """Bind related elements (image + caption, table + caption)."""
        # Find captions and bind to nearest preceding image/table
        for i, element in enumerate(elements):
            if element.type == ChunkType.CAPTION:
                # Look for preceding image/table within 3 elements
                for j in range(i - 1, max(0, i - 4), -1):
                    prev = elements[j]
                    if prev.type in (ChunkType.IMAGE, ChunkType.TABLE):
                        prev.metadata["caption"] = element.content
                        prev.related_image_ids.append(element.metadata.get("id", ""))
                        element.metadata["binds_to"] = prev.metadata.get("id", "")
                        break

        return elements

    async def _enrich_images_with_vlm(
        self,
        elements: list[DocumentElement],
    ) -> list[DocumentElement]:
        """Generate VLM descriptions for key images."""
        if not self.vlm_service or not self.enable_vlm_description:
            return elements

        # Select most important images (large ones, or those with captions)
        image_elements = [e for e in elements if e.type == ChunkType.IMAGE]

        # Prioritize: has caption > larger size
        image_elements.sort(
            key=lambda e: (
                e.metadata.get("caption", "") != "",  # Has caption first
                e.metadata.get("width", 0) * e.metadata.get("height", 0),  # Then by size
            ),
            reverse=True,
        )

        # Process top N images
        for element in image_elements[: self.max_vlm_images]:
            if self._vlm_calls >= self.max_vlm_images:
                break

            try:
                description = await self._get_vlm_description(element)
                element.vlm_description = description
                element.content = f"[Image: {description}]"
                self._vlm_calls += 1
            except Exception as e:
                logger.warning(f"VLM description failed: {e}")

        return elements

    async def _get_vlm_description(self, element: DocumentElement) -> str:
        """Get VLM description for an image."""
        if not self.vlm_service or not element.image_bytes:
            return ""

        try:
            result = await self.vlm_service.describe_image(
                image_bytes=element.image_bytes,
                prompt="Describe this image in detail. What is shown? Include any text, diagrams, charts, or visual elements.",
                max_tokens=200,
            )
            return result.description if hasattr(result, "description") else str(result)
        except Exception as e:
            logger.debug(f"VLM call failed: {e}")
            return ""

    def _create_semantic_chunks(
        self,
        elements: list[DocumentElement],
    ) -> list[StructuredChunk]:
        """Create semantic chunks from elements."""
        chunks: list[StructuredChunk] = []

        for i, element in enumerate(elements):
            chunk_id = f"chunk_{i}"

            if element.type == ChunkType.TEXT:
                chunk = self._create_text_chunk(element, chunk_id)
            elif element.type == ChunkType.HEADING:
                chunk = self._create_heading_chunk(element, chunk_id)
            elif element.type == ChunkType.IMAGE:
                chunk = self._create_image_chunk(element, chunk_id)
            elif element.type == ChunkType.TABLE:
                chunk = self._create_table_chunk(element, chunk_id)
            elif element.type == ChunkType.CAPTION:
                # Captions are typically merged with their parent
                continue
            else:
                chunk = self._create_text_chunk(element, chunk_id)

            chunks.append(chunk)

        return chunks

    def _create_text_chunk(
        self,
        element: DocumentElement,
        chunk_id: str,
    ) -> StructuredChunk:
        """Create a text chunk."""
        content = element.content

        # Enhance content with section context
        content_for_embedding = content
        if element.metadata.get("section_title"):
            content_for_embedding = f"Section: {element.metadata['section_title']}\n\n{content}"

        return StructuredChunk(
            chunk_id=chunk_id,
            type=ChunkType.TEXT,
            content=content,
            content_for_embedding=content_for_embedding,
            page_number=element.page_number,
            text=content,
            metadata=element.metadata,
            section_title=element.metadata.get("section_title"),
            section_level=element.metadata.get("section_level", 0),
        )

    def _create_heading_chunk(
        self,
        element: DocumentElement,
        chunk_id: str,
    ) -> StructuredChunk:
        """Create a heading chunk."""
        content = element.content

        # Headings are important for navigation
        content_for_embedding = f"HEADING: {content}"

        return StructuredChunk(
            chunk_id=chunk_id,
            type=ChunkType.HEADING,
            content=content,
            content_for_embedding=content_for_embedding,
            page_number=element.page_number,
            text=content,
            metadata=element.metadata,
            section_level=element.metadata.get("heading_level", 1),
        )

    def _create_image_chunk(
        self,
        element: DocumentElement,
        chunk_id: str,
    ) -> StructuredChunk:
        """Create an image chunk."""
        # Combine VLM description, caption, and context
        descriptions = []

        if element.vlm_description:
            descriptions.append(f"Description: {element.vlm_description}")

        if element.metadata.get("caption"):
            descriptions.append(f"Caption: {element.metadata['caption']}")

        descriptions.append(
            f"Image size: {element.metadata.get('width')}x{element.metadata.get('height')}"
        )

        content = "\n".join(descriptions)

        # For embedding: include both description and context
        content_for_embedding = content

        return StructuredChunk(
            chunk_id=chunk_id,
            type=ChunkType.IMAGE,
            content=content,
            content_for_embedding=content_for_embedding,
            page_number=element.page_number,
            text=element.vlm_description or element.metadata.get("caption", ""),
            images=[
                {
                    "image_id": element.metadata.get("id", chunk_id),
                    "bytes": element.image_bytes,
                    "mime_type": element.image_mime,
                    "width": element.metadata.get("width"),
                    "height": element.metadata.get("height"),
                }
            ]
            if element.image_bytes
            else [],
            has_images=True,
            metadata=element.metadata,
        )

    def _create_table_chunk(
        self,
        element: DocumentElement,
        chunk_id: str,
    ) -> StructuredChunk:
        """Create a table chunk."""
        # Use markdown for content, but include HTML for structure
        content = element.table_markdown or element.content

        # Enhance with caption if available
        caption = element.metadata.get("caption", "")
        content_for_embedding = f"{caption}\n\n{content}" if caption else content

        return StructuredChunk(
            chunk_id=chunk_id,
            type=ChunkType.TABLE,
            content=content,
            content_for_embedding=content_for_embedding,
            page_number=element.page_number,
            text=content,
            metadata={
                **element.metadata,
                "table_html": element.table_html,
            },
        )

    # Helper methods
    def _get_dominant_font_size(self, block: dict[str, Any]) -> float:
        """Get dominant font size in a block."""
        sizes = []
        for line in block.get("lines", []):
            for span in line.get("spans", []):
                sizes.append(span.get("size", 12))
        return sum(sizes) / len(sizes) if sizes else 12

    def _is_bold_block(self, block: dict[str, Any]) -> bool:
        """Check if block is predominantly bold."""
        bold_count = 0
        total = 0
        for line in block.get("lines", []):
            for span in line.get("spans", []):
                total += 1
                flags = span.get("flags", 0)
                if flags & 2**4:  # Bold flag
                    bold_count += 1
        return bold_count / total > 0.5 if total else False

    def _estimate_heading_level(self, element: DocumentElement) -> int:
        """Estimate heading level (1-6)."""
        font_size = element.metadata.get("font_size", 12)
        is_bold = element.metadata.get("is_bold", False)

        # Larger font = lower level (higher importance)
        if font_size >= 24 or (is_bold and font_size >= 20):
            return 1
        elif font_size >= 20 or (is_bold and font_size >= 16):
            return 2
        elif font_size >= 16:
            return 3
        elif font_size >= 14:
            return 4
        else:
            return 5
