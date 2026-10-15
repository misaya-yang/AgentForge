"""
OCR utilities for knowledge base document processing.

This module provides shared OCR functionality to avoid code duplication
between worker.py and knowledge_service.py.

Supports three OCR strategies:
- TESSERACT: Traditional Tesseract CLI OCR
- VLM: High-accuracy Qwen-VL based OCR (recommended for Arabic)
- HYBRID: VLM primary with Tesseract fallback
"""

from __future__ import annotations

import asyncio
import contextlib
import enum
import logging
import os
import shutil
import subprocess
import tempfile
from typing import Any

logger = logging.getLogger(__name__)


# =============================================================================
# OCR Constants
# =============================================================================

# Security: whitelist validation to prevent command injection
ALLOWED_OCR_LANGS_SINGLE = frozenset(
    {
        "eng",
        "ara",
        "chi_sim",
        "chi_tra",
        "fra",
        "deu",
        "spa",
        "rus",
        "jpn",
        "kor",
        "por",
        "ita",
        "nld",
        "tur",
        "vie",
        "tha",
    }
)

ALLOWED_OCR_COMBINATIONS = frozenset(
    {
        "eng+ara",
        "ara+eng",
        "chi_sim+eng",
        "jpn+eng",
        "kor+eng",
    }
)

# Default OCR settings
DEFAULT_OCR_LANGUAGE = "eng+ara"
DEFAULT_OCR_DPI = 200
DEFAULT_OCR_TIMEOUT = 60
MIN_OCR_DPI = 72
MAX_OCR_DPI = 1200
MIN_OCR_TIMEOUT = 1
MAX_OCR_TIMEOUT = 300


# =============================================================================
# OCR Configuration
# =============================================================================


class OCRCConfig:
    """Configuration for OCR operations."""

    def __init__(
        self,
        languages: str = DEFAULT_OCR_LANGUAGE,
        dpi: int = DEFAULT_OCR_DPI,
        timeout_seconds: int = DEFAULT_OCR_TIMEOUT,
    ):
        self.languages = self._validate_languages(languages)
        self.dpi = self._validate_dpi(dpi)
        self.timeout_seconds = self._validate_timeout(timeout_seconds)

    @classmethod
    def from_settings(cls, settings: Any | None = None) -> OCRCConfig:
        """Create OCR config from knowledge settings."""
        if settings is None:
            return cls()

        ks = settings
        languages = getattr(ks, "ocr_languages", DEFAULT_OCR_LANGUAGE) or DEFAULT_OCR_LANGUAGE
        dpi = getattr(ks, "ocr_render_dpi", DEFAULT_OCR_DPI)
        timeout = getattr(ks, "ocr_tesseract_timeout_seconds", DEFAULT_OCR_TIMEOUT)

        return cls(languages=languages, dpi=dpi, timeout_seconds=timeout)

    def _validate_languages(self, langs: str) -> str:
        """Validate and normalize OCR language setting."""
        if not langs:
            return DEFAULT_OCR_LANGUAGE

        langs = langs.strip()

        # Check pre-defined combinations
        if langs in ALLOWED_OCR_COMBINATIONS:
            return langs

        # Check single language
        if langs in ALLOWED_OCR_LANGS_SINGLE:
            return langs

        # Validate custom combinations (e.g., "eng+fra")
        if "+" in langs:
            parts = langs.split("+")
            if all(p.strip() in ALLOWED_OCR_LANGS_SINGLE for p in parts):
                return langs

        logger.warning(f"Invalid OCR languages: {langs}, falling back to '{DEFAULT_OCR_LANGUAGE}'")
        return DEFAULT_OCR_LANGUAGE

    def _validate_dpi(self, dpi: int) -> int:
        """Validate and clamp DPI to safe range."""
        try:
            dpi = int(dpi)
        except (TypeError, ValueError):
            dpi = DEFAULT_OCR_DPI
        return max(MIN_OCR_DPI, min(dpi, MAX_OCR_DPI))

    def _validate_timeout(self, timeout: int) -> int:
        """Validate and clamp timeout to safe range."""
        try:
            timeout = int(timeout)
        except (TypeError, ValueError):
            timeout = DEFAULT_OCR_TIMEOUT
        return max(MIN_OCR_TIMEOUT, min(timeout, MAX_OCR_TIMEOUT))


# =============================================================================
# OCR Operations
# =============================================================================


def check_tesseract_available() -> str | None:
    """Check if tesseract is available and return its path."""
    return shutil.which("tesseract")


def ocr_image_bytes(
    image_bytes: bytes,
    config: OCRCConfig | None = None,
    fallback_to_eng: bool = True,
) -> str:
    """
    Run OCR on a single image using Tesseract CLI.

    Args:
        image_bytes: The image data as bytes
        config: OCR configuration. Uses defaults if not provided.
        fallback_to_eng: Whether to fallback to English if primary language fails

    Returns:
        Extracted text from the image
    """
    tesseract = check_tesseract_available()
    if not tesseract:
        logger.warning("Tesseract not found in PATH")
        return ""

    cfg = config or OCRCConfig()

    # Write image to temp file
    with tempfile.NamedTemporaryFile(delete=False, suffix=".png") as img_file:
        img_file.write(image_bytes)
        img_path = img_file.name

    with tempfile.NamedTemporaryFile(delete=False) as out_file:
        out_base = out_file.name

    try:
        # Try OCR with configured languages
        text = _run_tesseract(
            tesseract=tesseract,
            img_path=img_path,
            out_base=out_base,
            langs=cfg.languages,
            dpi=cfg.dpi,
            timeout=cfg.timeout_seconds,
        )

        # Fallback to English if needed and configured
        if not text and fallback_to_eng and cfg.languages != "eng":
            logger.debug("OCR fallback to English for image")
            text = _run_tesseract(
                tesseract=tesseract,
                img_path=img_path,
                out_base=out_base,
                langs="eng",
                dpi=cfg.dpi,
                timeout=cfg.timeout_seconds,
            )

        return text

    finally:
        _cleanup_temp_files(img_path, out_base)


def _run_tesseract(
    tesseract: str,
    img_path: str,
    out_base: str,
    langs: str,
    dpi: int,
    timeout: int,
) -> str:
    """Execute tesseract command and return extracted text."""
    try:
        proc = subprocess.run(
            [tesseract, img_path, out_base, "-l", langs, "--dpi", str(dpi)],
            capture_output=True,
            text=True,
            check=False,
            timeout=timeout,
        )
    except subprocess.TimeoutExpired:
        logger.warning(f"OCR timed out (timeout={timeout}s)")
        return ""
    except Exception as e:
        logger.warning(f"OCR subprocess failed: {e}")
        return ""

    # Handle language data file error
    if proc.returncode != 0 and "Error opening data file" in (proc.stderr or ""):
        logger.warning(f"OCR language data file error for '{langs}': {proc.stderr}")
        return ""

    # Read output
    text_path = f"{out_base}.txt"
    try:
        with open(text_path, encoding="utf-8", errors="ignore") as f:
            return f.read().strip()
    except FileNotFoundError:
        return ""


def _cleanup_temp_files(img_path: str, out_base: str) -> None:
    """Clean up temporary files created during OCR."""
    for path in (img_path, out_base, f"{out_base}.txt"):
        try:
            if os.path.exists(path):
                os.unlink(path)
        except Exception:
            pass


# =============================================================================
# PDF OCR Operations
# =============================================================================


def ocr_pdf_bytes(
    content: bytes,
    config: OCRCConfig | None = None,
    max_workers: int = 2,
    max_pending_factor: int = 2,
) -> str:
    """
    OCR a PDF using PyMuPDF rendering + Tesseract CLI.

    Args:
        content: PDF file content as bytes
        config: OCR configuration. Uses defaults if not provided.
        max_workers: Maximum number of concurrent OCR workers
        max_pending_factor: Factor to calculate max pending futures (workers * factor)

    Returns:
        Extracted text from all pages
    """
    from concurrent.futures import ThreadPoolExecutor, as_completed

    tesseract = check_tesseract_available()
    if not tesseract:
        logger.warning("Tesseract binary not found; OCR skipped")
        return ""

    try:
        import pymupdf as fitz  # type: ignore
    except ImportError:
        try:
            import fitz  # type: ignore
        except ImportError:
            logger.warning("PyMuPDF not installed; OCR skipped")
            return ""

    cfg = config or OCRCConfig()
    max_workers = max(1, min(int(max_workers), 4))
    max_pending = max_workers * max_pending_factor

    doc = None
    try:
        doc = fitz.open(stream=content, filetype="pdf")
        parts_by_page: dict[int, str] = {}

        def _ocr_page(page_idx: int, img_bytes: bytes) -> tuple[int, str]:
            """OCR a single page."""
            text = _ocr_single_image(tesseract, img_bytes, cfg)
            return page_idx, text

        futures = []
        with ThreadPoolExecutor(max_workers=max_workers) as executor:
            for page_index, page in enumerate(doc):
                try:
                    pix = page.get_pixmap(dpi=cfg.dpi, alpha=False)
                    img_bytes = pix.tobytes("png")
                except Exception as e:
                    logger.warning(f"OCR render failed on page {page_index + 1}: {e}")
                    continue

                futures.append(executor.submit(_ocr_page, page_index, img_bytes))

                # Flow control: limit pending futures
                if len(futures) >= max_pending:
                    _process_completed_futures(futures, parts_by_page)

            # Process remaining futures
            for fut in as_completed(futures):
                try:
                    page_idx, page_text = fut.result()
                    if page_text and page_idx >= 0:
                        parts_by_page[page_idx] = page_text
                except Exception as e:
                    logger.warning(f"OCR failed on page task: {e}")

        ordered_parts = [parts_by_page[idx] for idx in sorted(parts_by_page.keys())]
        return "\n\n".join(ordered_parts)

    except Exception as e:
        logger.warning(f"OCR failed for PDF: {e}")
        return ""
    finally:
        if doc is not None:
            with contextlib.suppress(Exception):
                doc.close()


def _ocr_single_image(tesseract: str, img_bytes: bytes, config: OCRCConfig) -> str:
    """OCR a single image using tesseract."""
    return _run_tesseract_with_temp(
        tesseract=tesseract,
        img_bytes=img_bytes,
        langs=config.languages,
        dpi=config.dpi,
        timeout=config.timeout_seconds,
    )


def _run_tesseract_with_temp(
    tesseract: str,
    img_bytes: bytes,
    langs: str,
    dpi: int,
    timeout: int,
) -> str:
    """Run tesseract on image bytes with temporary file handling."""
    with tempfile.NamedTemporaryFile(delete=False, suffix=".png") as img_file:
        img_file.write(img_bytes)
        img_path = img_file.name

    with tempfile.NamedTemporaryFile(delete=False) as out_file:
        out_base = out_file.name

    try:
        text = _run_tesseract(tesseract, img_path, out_base, langs, dpi, timeout)

        # Fallback to English if no text extracted
        if not text and langs != "eng":
            text = _run_tesseract(tesseract, img_path, out_base, "eng", dpi, timeout)

        return text
    finally:
        _cleanup_temp_files(img_path, out_base)


def _process_completed_futures(futures: list, parts_by_page: dict[int, str]) -> None:
    """Process completed futures from the executor.

    Safely removes completed futures from the list without corrupting iteration.
    """

    # Collect completed futures first, then remove — avoids modifying list during iteration
    done = [f for f in futures if f.done()]
    for fut in done:
        futures.remove(fut)
        try:
            page_idx, page_text = fut.result()
            if page_text and page_idx >= 0:
                parts_by_page[page_idx] = page_text
        except Exception as e:
            logger.warning(f"OCR failed on page task: {e}")


# =============================================================================
# OCR Strategy (VLM / Tesseract / Hybrid)
# =============================================================================


class OCRStrategy(str, enum.Enum):
    """OCR strategy selection."""

    TESSERACT = "tesseract"
    VLM = "vlm"
    HYBRID = "hybrid"


async def ocr_image_bytes_auto(
    image_bytes: bytes,
    vlm_ocr_service: Any | None = None,
    config: OCRCConfig | None = None,
    strategy: str = "hybrid",
) -> str:
    """OCR a single image using the configured strategy.

    - tesseract: Use Tesseract CLI only (legacy).
    - vlm: Use VLM OCR only (high accuracy, requires API).
    - hybrid: Try VLM first; fall back to Tesseract on failure.

    Args:
        image_bytes: Image data as bytes.
        vlm_ocr_service: Optional VLMOCRService instance.
        config: OCR config for Tesseract fallback.
        strategy: One of "tesseract", "vlm", "hybrid".

    Returns:
        Extracted text.
    """
    strat = strategy.lower().strip()

    # SPO-04 / K2: Tesseract runs a subprocess; keep it off the event loop.
    if strat == OCRStrategy.TESSERACT or (strat != OCRStrategy.TESSERACT and vlm_ocr_service is None):
        return await asyncio.to_thread(
            ocr_image_bytes, image_bytes, config=config, fallback_to_eng=True
        )

    if strat == OCRStrategy.VLM:
        try:
            text = await vlm_ocr_service.ocr_image(image_bytes)
            if text:
                return text
        except Exception as e:
            logger.warning(f"VLM OCR failed: {e}")
        return ""

    # hybrid: VLM first, Tesseract fallback
    try:
        text = await vlm_ocr_service.ocr_image(image_bytes)
        if text:
            return text
    except Exception as e:
        logger.warning(f"VLM OCR failed in hybrid mode, falling back to Tesseract: {e}")

    return await asyncio.to_thread(
        ocr_image_bytes, image_bytes, config=config, fallback_to_eng=True
    )
