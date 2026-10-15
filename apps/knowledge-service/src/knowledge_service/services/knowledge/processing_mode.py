"""
Processing Mode Definitions for Knowledge Base Documents

Defines the three processing modes for document ingestion:
- text_only: Traditional OCR + text embedding
- scanned: Page-as-Image vision embedding (for scanned PDFs)
- multimodal: Combined text + image embedding
"""

from enum import Enum
from typing import NamedTuple


class ProcessingMode(str, Enum):
    """Document processing mode enumeration."""

    AUTO = "auto"
    """Automatic detection: choose the best processing mode based on document analysis."""

    TEXT_ONLY = "text_only"
    """Traditional text processing: OCR extraction -> chunking -> text embedding.
    Best for: Digital PDFs, Word documents, plain text files.
    Embedding: Gemini/DashScope text models.
    """

    SCANNED = "scanned"
    """Vision-first processing: Each page as image -> vision embedding.
    Best for: Scanned PDFs, documents with complex layouts.
    Embedding: tongyi-embedding-vision-plus (1024 dim).
    Based on ColPali approach - 18x faster, 23% more accurate than OCR.
    """

    MULTIMODAL = "multimodal"
    """Combined processing: Extract text + images separately -> dual embedding.
    Best for: Mixed content documents with both text and images.
    Embedding: Text model + Vision model in same vector space.
    """


class ProcessingModeConfig(NamedTuple):
    """Configuration for each processing mode."""

    mode: ProcessingMode
    display_name: str
    description: str
    embedding_model: str
    batch_size: int
    max_concurrent: int
    timeout_seconds: float
    supports_progress: bool  # Whether to show page-by-page progress


# Default configurations for each mode
MODE_CONFIGS: dict[ProcessingMode, ProcessingModeConfig] = {
    ProcessingMode.AUTO: ProcessingModeConfig(
        mode=ProcessingMode.AUTO,
        display_name="Auto",
        description="Automatic detection and processing",
        embedding_model="text-embedding-v3",
        batch_size=50,
        max_concurrent=5,
        timeout_seconds=60.0,
        supports_progress=False,
    ),
    ProcessingMode.TEXT_ONLY: ProcessingModeConfig(
        mode=ProcessingMode.TEXT_ONLY,
        display_name="Pure Text",
        description="OCR + text chunking + text embedding",
        embedding_model="text-embedding-v3",  # Or Gemini
        batch_size=50,
        max_concurrent=5,
        timeout_seconds=60.0,
        supports_progress=False,  # Chunk-based, not page-based
    ),
    ProcessingMode.SCANNED: ProcessingModeConfig(
        mode=ProcessingMode.SCANNED,
        display_name="Scanned Document",
        description="Page-as-Image vision embedding",
        embedding_model="tongyi-embedding-vision-plus",
        batch_size=8,  # Pages per batch
        max_concurrent=3,
        timeout_seconds=90.0,
        supports_progress=True,  # Page-by-page progress
    ),
    ProcessingMode.MULTIMODAL: ProcessingModeConfig(
        mode=ProcessingMode.MULTIMODAL,
        display_name="Multimodal",
        description="Text + image dual embedding",
        embedding_model="tongyi-embedding-vision-plus",
        batch_size=10,
        max_concurrent=5,
        timeout_seconds=90.0,
        supports_progress=True,
    ),
}


def get_mode_config(mode: ProcessingMode) -> ProcessingModeConfig:
    """Get configuration for a processing mode."""
    return MODE_CONFIGS.get(mode, MODE_CONFIGS[ProcessingMode.TEXT_ONLY])


def parse_processing_mode(mode_str: str | None) -> ProcessingMode:
    """Parse string to ProcessingMode enum, defaulting to TEXT_ONLY."""
    if not mode_str:
        return ProcessingMode.TEXT_ONLY

    try:
        return ProcessingMode(mode_str.lower())
    except ValueError:
        return ProcessingMode.TEXT_ONLY
