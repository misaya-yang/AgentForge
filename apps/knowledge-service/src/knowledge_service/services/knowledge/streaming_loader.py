"""
Streaming Document Loader - Memory-efficient large file processing

Supports processing 300MB+ PDF files without running out of memory:
- Processes pages in batches
- Releases memory after each batch
- Supports both file path and S3 key loading
- Progress callbacks for UI updates
"""

import asyncio
import gc
import inspect
from collections.abc import AsyncIterator, Callable
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

from ...core.observability.logging import get_logger

logger = get_logger(__name__)


@dataclass
class PageContent:
    """Content extracted from a single page."""

    page_number: int  # 1-indexed
    text: str
    images: list[bytes] = field(default_factory=list)
    image_count: int = 0
    char_count: int = 0

    def __post_init__(self):
        self.char_count = len(self.text)
        self.image_count = len(self.images)


@dataclass
class BatchResult:
    """Result of processing a batch of pages."""

    pages: list[PageContent]
    batch_index: int
    start_page: int  # 1-indexed
    end_page: int  # 1-indexed (inclusive)
    total_pages: int
    progress: float  # 0.0 - 1.0

    @property
    def page_count(self) -> int:
        return len(self.pages)

    @property
    def total_chars(self) -> int:
        return sum(p.char_count for p in self.pages)

    @property
    def total_images(self) -> int:
        return sum(p.image_count for p in self.pages)


class StreamingDocumentLoader:
    """
    Memory-efficient document loader for large files.

    Processes documents in batches, releasing memory after each batch.
    Suitable for 300MB+ PDF files that would otherwise cause OOM.
    """

    # Default configuration (mirrors KnowledgeSettings defaults)
    LARGE_FILE_THRESHOLD = 50 * 1024 * 1024  # 50MB
    DEFAULT_BATCH_SIZE = 20
    MIN_BATCH_SIZE = 5
    MAX_BATCH_SIZE = 50

    # Timeout for page processing operations (seconds)
    PAGE_PROCESSING_TIMEOUT = 30.0
    IMAGE_EXTRACTION_TIMEOUT = 10.0

    def __init__(
        self,
        batch_size: int = DEFAULT_BATCH_SIZE,
        extract_images: bool = True,
        extract_images_if_no_text: bool = False,
        max_image_size: int = 1024,  # Max dimension for extracted images
        storage_service: Any | None = None,  # For S3/OSS loading
    ):
        """
        Initialize the streaming loader.

        Args:
            batch_size: Number of pages to process per batch
            extract_images: Whether to extract images from pages
            max_image_size: Maximum image dimension (resize if larger)
            storage_service: Optional storage service for remote files
        """
        self.batch_size = max(self.MIN_BATCH_SIZE, min(batch_size, self.MAX_BATCH_SIZE))
        self.extract_images = extract_images
        self.extract_images_if_no_text = extract_images_if_no_text
        self.max_image_size = max_image_size
        self.storage_service = storage_service
        self._fitz = None

    def _get_fitz(self):
        """Lazy load PyMuPDF."""
        if self._fitz is None:
            try:
                import pymupdf as fitz

                self._fitz = fitz
            except ImportError:
                try:
                    import fitz  # type: ignore

                    self._fitz = fitz
                except ImportError as exc:
                    raise ImportError(
                        "PyMuPDF is required for streaming document loading. "
                        "Install with: pip install 'ai-gateway[documents]' or pip install pymupdf"
                    ) from exc
        return self._fitz

    async def get_page_count(self, source: bytes | str) -> int:
        """
        Get total page count without loading entire document.

        Args:
            source: PDF bytes or file path

        Returns:
            Total number of pages
        """
        # FIX: Run blocking I/O in thread pool to avoid blocking event loop
        return await asyncio.to_thread(self._sync_get_page_count, source)

    def _sync_get_page_count(self, source: bytes | str) -> int:
        """Synchronous implementation of get_page_count."""
        fitz = self._get_fitz()

        if isinstance(source, bytes):
            doc = fitz.open(stream=source, filetype="pdf")
        else:
            path = Path(source)
            if path.exists():
                doc = fitz.open(path)
            else:
                # This shouldn't happen in sync context, but handle gracefully
                raise FileNotFoundError(f"File not found: {source}")

        try:
            return len(doc)
        finally:
            doc.close()

    async def iter_batches(
        self,
        source: bytes | str,
        on_progress: Callable[[float], None] | None = None,
    ) -> AsyncIterator[BatchResult]:
        """
        Iterate over document pages in batches.

        Yields BatchResult for each batch, releasing memory between batches.

        Args:
            source: PDF bytes or file path/S3 key
            on_progress: Optional callback for progress updates (0.0 - 1.0)

        Yields:
            BatchResult for each batch of pages
        """
        # FIX: Load content first if needed, then process in thread pool
        if isinstance(source, str):
            path = Path(source)
            if not path.exists():
                # Load remote content asynchronously first
                source = await self._load_content(source)

        # Run blocking PDF processing in thread pool
        async for batch in self._iter_batches_sync(source, on_progress):
            yield batch

    async def _iter_batches_sync(
        self,
        source: bytes | str,
        on_progress: Callable[[float], None] | None = None,
    ) -> AsyncIterator[BatchResult]:
        """Internal batch iterator that runs blocking operations in thread pool."""
        # Open document in thread pool to avoid blocking
        doc = await asyncio.to_thread(self._open_document, source)

        try:
            total_pages = len(doc)
            batch_index = 0

            for start_idx in range(0, total_pages, self.batch_size):
                end_idx = min(start_idx + self.batch_size, total_pages)

                # Extract pages in this batch (running in thread pool)
                pages = await asyncio.to_thread(self._extract_pages_batch, doc, start_idx, end_idx)

                # Calculate progress
                progress = end_idx / total_pages

                # Create batch result
                batch = BatchResult(
                    pages=pages,
                    batch_index=batch_index,
                    start_page=start_idx + 1,
                    end_page=end_idx,
                    total_pages=total_pages,
                    progress=progress,
                )

                yield batch

                # Update progress callback
                if on_progress:
                    result = on_progress(progress)
                    if inspect.isawaitable(result):
                        await result

                # Memory management: shrink store and run GC
                await asyncio.to_thread(self._release_memory)

                batch_index += 1

        finally:
            await asyncio.to_thread(self._close_document, doc)
            # Final memory cleanup
            await asyncio.to_thread(self._release_memory)

    async def load_page_range(
        self,
        source: bytes | str,
        start_page: int,
        end_page: int,
    ) -> list[PageContent]:
        """
        Load a specific range of pages.

        Args:
            source: PDF bytes or file path
            start_page: Start page (1-indexed)
            end_page: End page (1-indexed, inclusive)

        Returns:
            List of PageContent for the specified range
        """
        # FIX: Load remote content first if needed
        if isinstance(source, str):
            path = Path(source)
            if not path.exists():
                source = await self._load_content(source)

        # Run blocking operations in thread pool
        return await asyncio.to_thread(self._sync_load_page_range, source, start_page, end_page)

    def _sync_load_page_range(
        self,
        source: bytes | str,
        start_page: int,
        end_page: int,
    ) -> list[PageContent]:
        """Synchronous implementation of load_page_range."""
        doc = self._open_document(source)

        try:
            pages = []
            for page_idx in range(start_page - 1, min(end_page, len(doc))):
                page_content = self._sync_extract_page(doc, page_idx)
                pages.append(page_content)
            return pages
        finally:
            self._close_document(doc)

    def _sync_extract_page(self, doc: Any, page_idx: int) -> PageContent:
        """Synchronous implementation of page extraction."""
        page = doc[page_idx]

        # Extract text
        text = page.get_text("text")

        # Extract images if enabled (or if needed for OCR when text is empty)
        images = []
        should_extract_images = self.extract_images or (
            self.extract_images_if_no_text and not text.strip()
        )
        if should_extract_images:
            try:
                images = self._sync_extract_page_images(doc, page, page_idx)
            except Exception as e:
                logger.debug(f"Failed to extract images from page {page_idx + 1}: {e}")

        return PageContent(
            page_number=page_idx + 1,
            text=text,
            images=images,
        )

    async def _extract_page(self, doc: Any, page_idx: int) -> PageContent:
        """Extract content from a single page (async wrapper)."""
        # FIX: Run in thread pool with timeout to avoid blocking indefinitely
        return await asyncio.wait_for(
            asyncio.to_thread(self._sync_extract_page, doc, page_idx),
            timeout=self.PAGE_PROCESSING_TIMEOUT,
        )

    async def _extract_page_images(
        self,
        doc: Any,
        page: Any,
        page_idx: int,
    ) -> list[bytes]:
        """Extract images from a page."""
        self._get_fitz()
        images = []

        try:
            image_list = page.get_images()

            for img_idx, img_info in enumerate(image_list):
                try:
                    xref = img_info[0]
                    base_image = doc.extract_image(xref)

                    if base_image and "image" in base_image:
                        image_bytes = base_image["image"]

                        # FIX: Check pixel dimensions, not just file size
                        # Large file size doesn't always mean large dimensions
                        # (e.g., uncompressed images)
                        should_resize = await self._should_resize_image(
                            image_bytes, self.max_image_size
                        )

                        if should_resize:
                            image_bytes = await self._resize_image(
                                image_bytes,
                                self.max_image_size,
                            )

                        if image_bytes:
                            images.append(image_bytes)

                except Exception as e:
                    logger.debug(f"Failed to extract image {img_idx} from page {page_idx + 1}: {e}")
                    continue

        except Exception as e:
            logger.debug(f"Error getting image list from page {page_idx + 1}: {e}")

        return images

    async def _resize_image(self, image_bytes: bytes, max_size: int) -> bytes:
        """Resize image if it exceeds max dimensions."""
        try:
            import io

            from PIL import Image
        except ImportError:
            logger.warning(
                "PIL (Pillow) not installed, skipping image resize. "
                "Install with: pip install Pillow"
            )
            return image_bytes
        try:
            img = Image.open(io.BytesIO(image_bytes))

            # Calculate new size maintaining aspect ratio
            width, height = img.size
            if width > max_size or height > max_size:
                ratio = min(max_size / width, max_size / height)
                new_size = (int(width * ratio), int(height * ratio))
                img = img.resize(new_size, Image.Resampling.LANCZOS)

            # Convert to PNG bytes
            output = io.BytesIO()
            img.save(output, format="PNG", optimize=True)
            return output.getvalue()

        except Exception as e:
            logger.debug(f"Failed to resize image: {e}")
            return image_bytes

    async def _should_resize_image(self, image_bytes: bytes, max_size: int) -> bool:
        """Check if image exceeds maximum dimensions."""
        try:
            import io

            from PIL import Image
        except ImportError:
            return False

        try:
            img = Image.open(io.BytesIO(image_bytes))
            width, height = img.size
            return width > max_size or height > max_size
        except Exception as e:
            logger.debug(f"Failed to check image size: {e}")
            return False

    async def _load_content(self, source: str) -> bytes:
        """
        Load content from file path or S3 key.

        Args:
            source: File path or S3 key

        Returns:
            File content as bytes
        """
        # Check if it's a local file
        path = Path(source)
        if path.exists():
            return await asyncio.to_thread(path.read_bytes)

        # Try loading from storage service (S3/OSS)
        if self.storage_service:
            try:
                return await self.storage_service.download_original_file(source)
            except Exception as e:
                logger.error(f"Failed to load from storage: {e}")
                raise

        raise FileNotFoundError(f"Cannot load content from: {source}")

    def _release_memory(self):
        """Release memory after processing a batch."""
        fitz = self._get_fitz()

        try:
            # Shrink PyMuPDF's internal store
            fitz.Tools().store_shrink(100)  # Keep minimal cache
        except Exception as e:
            logger.debug(f"store_shrink failed: {e}")

        # Force garbage collection
        gc.collect()

    def _open_document(self, source: bytes | str) -> Any:
        """Open a PDF document from bytes or file path."""
        fitz = self._get_fitz()

        if isinstance(source, bytes):
            return fitz.open(stream=source, filetype="pdf")
        else:
            path = Path(source)
            if path.exists():
                return fitz.open(path)
            else:
                raise FileNotFoundError(f"File not found: {source}")

    def _close_document(self, doc: Any) -> None:
        """Close a PDF document safely."""
        try:
            doc.close()
        except Exception as e:
            logger.debug(f"Error closing document: {e}")

    def _extract_pages_batch(
        self,
        doc: Any,
        start_idx: int,
        end_idx: int,
    ) -> list[PageContent]:
        """Extract pages in a batch range."""
        pages = []
        for page_idx in range(start_idx, end_idx):
            page_content = self._sync_extract_page(doc, page_idx)
            pages.append(page_content)
        return pages

    def _sync_extract_page_images(
        self,
        doc: Any,
        page: Any,
        page_idx: int,
    ) -> list[bytes]:
        """Synchronous implementation of image extraction."""
        self._get_fitz()
        images = []

        try:
            image_list = page.get_images()

            for img_idx, img_info in enumerate(image_list):
                try:
                    xref = img_info[0]
                    base_image = doc.extract_image(xref)

                    if base_image and "image" in base_image:
                        image_bytes = base_image["image"]

                        # FIX: Check pixel dimensions, not just file size
                        images.append(image_bytes)

                except Exception as e:
                    logger.debug(f"Failed to extract image {img_idx} from page {page_idx + 1}: {e}")
                    continue

        except Exception as e:
            logger.debug(f"Error getting image list from page {page_idx + 1}: {e}")

        return images

    @staticmethod
    def is_large_file(size_bytes: int) -> bool:
        """Check if file is considered large."""
        return size_bytes > StreamingDocumentLoader.LARGE_FILE_THRESHOLD


class InMemoryLoader:
    """
    Simple in-memory loader for smaller files.

    Loads entire document into memory at once.
    Use for files < 50MB.
    """

    def __init__(self, content: bytes):
        """Initialize with file content."""
        self.content = content
        self._fitz = None

    def _get_fitz(self):
        if self._fitz is None:
            try:
                import pymupdf as fitz
            except ImportError:
                import fitz
            self._fitz = fitz
        return self._fitz

    async def iter_batches(
        self,
        source: bytes | str = None,
        on_progress: Callable[[float], None] | None = None,
    ) -> AsyncIterator[BatchResult]:
        """
        Yield all pages as a single batch.

        For API compatibility with StreamingDocumentLoader.
        """
        fitz = self._get_fitz()

        content = source if isinstance(source, bytes) else self.content
        doc = fitz.open(stream=content, filetype="pdf")

        try:
            total_pages = len(doc)
            pages = []

            for page_idx in range(total_pages):
                page = doc[page_idx]
                text = page.get_text("text")
                pages.append(
                    PageContent(
                        page_number=page_idx + 1,
                        text=text,
                    )
                )

            if on_progress:
                result = on_progress(1.0)
                if inspect.isawaitable(result):
                    await result

            yield BatchResult(
                pages=pages,
                batch_index=0,
                start_page=1,
                end_page=total_pages,
                total_pages=total_pages,
                progress=1.0,
            )

        finally:
            doc.close()
