"""
Confluence Integration Module for Agent Gateway Knowledge Base.

This module provides integration with Atlassian Confluence for importing
and synchronizing documentation into the knowledge base.

Features:
- URL-based single page import
- Space-wide batch import
- Manual and scheduled synchronization
- Content parsing (Storage Format to plain text)
- Image extraction and multimodal embedding

Phase 1:
- URL import
- Space import
- Manual sync
- Polling sync

Phase 2:
- Image synchronization with S3/OSS storage
- Multimodal embedding via DashScope

Phase 3 (Future):
- Webhook real-time sync
- Encrypted API token storage
"""

from .client import ConfluenceAPIError, ConfluenceClient
from .image_processor import ConfluenceImageProcessor, create_image_processor
from .models import (
    ConfluenceAttachment,
    ConfluenceConnection,
    ConfluenceCredentials,
    ConfluencePage,
    ConfluencePageRecord,
    ConfluenceSpace,
    ConfluenceSpaceBinding,
    ConfluenceSyncTask,
    ImageSegment,
    SyncResult,
)
from .parser import (
    ImageReference,
    extract_embeddable_images,
    extract_image_references,
    extract_markdown,
    extract_plain_text,
    parse_storage_format,
)
from .scheduler import ConfluenceScheduler, SchedulerManager

__all__ = [
    # Client
    "ConfluenceClient",
    "ConfluenceAPIError",
    # Models
    "ConfluenceCredentials",
    "ConfluencePage",
    "ConfluenceSpace",
    "SyncResult",
    "ConfluenceConnection",
    "ConfluenceSpaceBinding",
    "ConfluencePageRecord",
    "ConfluenceSyncTask",
    "ConfluenceAttachment",
    "ImageSegment",
    # Parser
    "parse_storage_format",
    "extract_plain_text",
    "extract_markdown",
    "extract_image_references",
    "extract_embeddable_images",
    "ImageReference",
    # Scheduler
    "ConfluenceScheduler",
    "SchedulerManager",
    # Image Processing
    "ConfluenceImageProcessor",
    "create_image_processor",
]
