"""
Document Ingestion Module

Provides document processing and image extraction for multimodal RAG:
- PDF image extraction (via PyMuPDF)
- DOCX image extraction (via python-docx)
- HTML image extraction (via BeautifulSoup)
- Image-text association for context-aware retrieval
"""

from .document_image_extractor import (
    EMBEDDABLE_IMAGE_TYPES,
    MAX_IMAGE_SIZE_BYTES,
    MIN_IMAGE_HEIGHT,
    MIN_IMAGE_WIDTH,
    DocumentExtractionResult,
    DocumentImageExtractor,
    DOCXExtractor,
    ExtractedImage,
    HTMLExtractor,
    PDFExtractor,
    detect_mime_type,
    extract_images_from_document,
    generate_image_id,
    get_image_dimensions,
)

__all__ = [
    # Main extractor
    "DocumentImageExtractor",
    # Results
    "DocumentExtractionResult",
    "ExtractedImage",
    # Specialized extractors
    "PDFExtractor",
    "DOCXExtractor",
    "HTMLExtractor",
    # Utilities
    "extract_images_from_document",
    "detect_mime_type",
    "get_image_dimensions",
    "generate_image_id",
    # Constants
    "EMBEDDABLE_IMAGE_TYPES",
    "MAX_IMAGE_SIZE_BYTES",
    "MIN_IMAGE_WIDTH",
    "MIN_IMAGE_HEIGHT",
]
