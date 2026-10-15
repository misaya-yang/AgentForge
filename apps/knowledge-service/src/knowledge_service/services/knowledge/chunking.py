"""
Production-grade Document Chunking Module

Supports multiple chunking strategies similar to Alibaba Cloud / Dify:
1. Automatic (智能切分) - Auto-detect best strategy
2. Fixed Size (按长度切分) - Fixed character/token count with overlap
3. Paragraph (按段落切分) - Split by paragraphs
4. Page (按页切分) - For PDF page-based splitting
5. Heading/Section (按标题切分) - Split by markdown/document headings
6. Regex (按正则切分) - Custom regex pattern
7. Separator (按符号切分) - Split by custom separators
8. Recursive (递归切分) - Hierarchical recursive splitting
9. Hierarchical/Parent-Child (父子切分) - Parent chunks with child sub-chunks

Additional features:
- Metadata extraction
- Text preprocessing
- Token counting
- Overlap handling
"""

from __future__ import annotations

import hashlib
import logging
import re
import threading
from abc import ABC, abstractmethod
from bisect import bisect_left, bisect_right
from collections import deque
from dataclasses import dataclass, field
from enum import Enum
from typing import Any

from ...core.exceptions import ValidationFailedError

# Token counting with tiktoken (GPT-4 compatible)
try:
    import tiktoken

    _TIKTOKEN_AVAILABLE = True
    _DEFAULT_ENCODING = tiktoken.get_encoding("cl100k_base")  # GPT-4 encoding
except ImportError:
    _TIKTOKEN_AVAILABLE = False
    _DEFAULT_ENCODING = None

logger = logging.getLogger(__name__)

MAX_CHUNK_OUTPUTS = 10_000
MAX_WHOLE_ENCODER_INPUT_BYTES = 4 * 1024 * 1024
SAFE_HEADING_PATTERNS = (
    r"^#{1,6}\s+.+$",
    r"^第[一二三四五六七八九十\d]+[章节条款]",
    r"^[A-Z][A-Z \t]{4,}:?$",
)
_SAFE_PAGE_MARKERS = frozenset({r"\f", "\f"})
_SAFE_CHUNKING_MODES = frozenset(
    {
        "automatic",
        "auto",
        "fixed_size",
        "fixed",
        "custom",
        "paragraph",
        "page",
        "heading",
        "section",
        "separator",
        "recursive",
        "hierarchical",
        "parent_child",
        "qa",
    }
)
_SAFE_CHUNKING_KEYS = frozenset(
    {
        "mode",
        "chunk_size",
        "chunk_overlap",
        "overlap",
        "max_chunk_size",
        "min_chunk_size",
        "use_token_count",
        "token_limit",
        "max_tokens",
        "min_chunk_tokens",
        "max_chunk_tokens",
        "separators",
        "separator",
        "primary_separator",
        "keep_separator",
        "regex_pattern",
        "regex",
        "heading_level",
        "heading_patterns",
        "min_paragraph_length",
        "merge_short_paragraphs",
        "parent_chunk_size",
        "parent_overlap",
        "parent_chunk_overlap",
        "child_chunk_size",
        "child_overlap",
        "child_chunk_overlap",
        "parent_mode",
        "parent_token_limit",
        "child_token_limit",
        "question_prefix",
        "answer_prefix",
        "preserve_images",
        "image_context_chars",
        "remove_extra_spaces",
        "remove_urls_emails",
        "normalize_whitespace",
        "strip_html",
        "extract_metadata",
        "metadata_fields",
        "page_marker",
        "strict_section_traceability",
        "segmentation",
    }
)
_CHUNKING_INTEGER_BOUNDS = {
    "chunk_size": (50, 100_000),
    "chunk_overlap": (0, 50_000),
    "overlap": (0, 50_000),
    "max_chunk_size": (50, 100_000),
    "min_chunk_size": (50, 100_000),
    "token_limit": (1, 100_000),
    "max_tokens": (1, 100_000),
    "min_chunk_tokens": (1, 100_000),
    "max_chunk_tokens": (1, 100_000),
    "min_paragraph_length": (1, 100_000),
    "parent_chunk_size": (50, 100_000),
    "parent_overlap": (0, 50_000),
    "parent_chunk_overlap": (0, 50_000),
    "child_chunk_size": (10, 100_000),
    "child_overlap": (0, 50_000),
    "child_chunk_overlap": (0, 50_000),
    "parent_token_limit": (1, 100_000),
    "child_token_limit": (1, 100_000),
    "image_context_chars": (0, 1_000_000),
}
_CHUNKING_BOOLEAN_FIELDS = frozenset(
    {
        "use_token_count",
        "keep_separator",
        "merge_short_paragraphs",
        "preserve_images",
        "remove_extra_spaces",
        "remove_urls_emails",
        "normalize_whitespace",
        "strip_html",
        "extract_metadata",
        "strict_section_traceability",
    }
)


def require_chunk_output_budget(chunks: Any) -> None:
    if not isinstance(chunks, (list, tuple)) or len(chunks) > MAX_CHUNK_OUTPUTS:
        raise ValidationFailedError(
            f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
        )


def _append_bounded_output(output: list[Any], item: Any) -> None:
    if len(output) >= MAX_CHUNK_OUTPUTS:
        raise ValidationFailedError(
            f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
        )
    output.append(item)


def _extend_bounded_output(output: list[Any], items: list[Any]) -> None:
    if len(output) + len(items) > MAX_CHUNK_OUTPUTS:
        raise ValidationFailedError(
            f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
        )
    output.extend(items)


def _require_literal_split_budget(text: str, separator: str) -> None:
    """Reject an eager split before Python materializes more than the output cap."""
    if not separator:
        return
    matches = 0
    start = 0
    while True:
        index = text.find(separator, start)
        if index < 0:
            return
        matches += 1
        if matches >= MAX_CHUNK_OUTPUTS:
            raise ValidationFailedError(
                f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
            )
        start = index + len(separator)


def _require_regex_split_budget(text: str, pattern: str) -> None:
    for matches, _match in enumerate(re.finditer(pattern, text), start=1):
        if matches >= MAX_CHUNK_OUTPUTS:
            raise ValidationFailedError(
                f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
            )


def _require_token_window_input_budget(
    text: str,
    token_limit: int,
    overlap_tokens: int = 0,
) -> None:
    """Bound token-list materialization before calling a whole-text encoder."""
    overlap = min(max(int(overlap_tokens), 0), max(token_limit - 1, 0))
    step = max(int(token_limit) - overlap, 1)
    max_input_units = step * MAX_CHUNK_OUTPUTS
    if len(text) > max_input_units:
        raise ValidationFailedError(
            f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
        )
    if len(text.encode("utf-8")) > max_input_units:
        raise ValidationFailedError(
            f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
        )


def _require_whole_encoder_input_budget(text: str) -> None:
    if len(text) > MAX_WHOLE_ENCODER_INPUT_BYTES:
        raise ValidationFailedError(
            "text exceeds the safe whole-tokenizer materialization limit"
        )
    if len(text.encode("utf-8")) > MAX_WHOLE_ENCODER_INPUT_BYTES:
        raise ValidationFailedError(
            "text exceeds the safe whole-tokenizer materialization limit"
        )


def _reject_custom_chunking_regex_surfaces(value: Any) -> None:
    if isinstance(value, dict):
        for raw_key, nested in value.items():
            key = str(raw_key).strip().lower().replace("-", "_")
            if key == "mode" and str(nested or "").strip().lower() == "regex":
                raise ValidationFailedError("custom regex chunking is disabled")
            if key in {"regex", "regex_pattern"} and nested:
                raise ValidationFailedError("custom regex chunking is disabled")
            if (
                key == "heading_patterns"
                and nested
                and (
                    not isinstance(nested, (list, tuple))
                    or tuple(nested) != SAFE_HEADING_PATTERNS
                )
            ):
                raise ValidationFailedError("custom heading regex patterns are disabled")
            if (
                key == "page_marker"
                and nested not in (None, "")
                and (not isinstance(nested, str) or nested not in _SAFE_PAGE_MARKERS)
            ):
                raise ValidationFailedError("custom page marker regex is disabled")
            _reject_custom_chunking_regex_surfaces(nested)
    elif isinstance(value, (list, tuple)):
        for item in value:
            _reject_custom_chunking_regex_surfaces(item)


def validate_persisted_chunking_config(value: Any) -> dict[str, Any]:
    """Validate raw persisted chunking config before it can amplify work."""

    if value in (None, {}):
        return {}
    if not isinstance(value, dict):
        raise ValidationFailedError("chunking config must be an object")

    unknown = {str(key) for key in value}.difference(_SAFE_CHUNKING_KEYS)
    if unknown:
        raise ValidationFailedError(
            f"unsupported chunking config field: {sorted(unknown)[0]}"
        )
    _reject_custom_chunking_regex_surfaces(
        {
            key: value.get(key)
            for key in (
                "mode",
                "regex",
                "regex_pattern",
                "heading_patterns",
                "page_marker",
            )
            if key in value
        }
    )

    mode = str(value.get("mode") or "automatic").strip().lower()
    if mode not in _SAFE_CHUNKING_MODES:
        raise ValidationFailedError("unsupported chunking mode")

    for key, (minimum, maximum) in _CHUNKING_INTEGER_BOUNDS.items():
        candidate = value.get(key)
        if candidate is None:
            continue
        if (
            isinstance(candidate, bool)
            or not isinstance(candidate, int)
            or not minimum <= candidate <= maximum
        ):
            raise ValidationFailedError(
                f"chunking {key} must be an integer between {minimum} and {maximum}"
            )
    for key in _CHUNKING_BOOLEAN_FIELDS:
        candidate = value.get(key)
        if candidate is not None and not isinstance(candidate, bool):
            raise ValidationFailedError(f"chunking {key} must be a boolean")

    for key, maximum in {
        "separator": 1_024,
        "primary_separator": 1_024,
        "heading_level": 32,
        "parent_mode": 32,
        "question_prefix": 128,
        "answer_prefix": 128,
    }.items():
        candidate = value.get(key)
        if candidate is not None and (
            not isinstance(candidate, str) or len(candidate) > maximum
        ):
            raise ValidationFailedError(f"chunking {key} is invalid")

    separators = value.get("separators")
    if separators is not None and (
        not isinstance(separators, list)
        or not 1 <= len(separators) <= 32
        or any(
            not isinstance(separator, str)
            or not separator
            or len(separator) > 1_024
            for separator in separators
        )
    ):
        raise ValidationFailedError("chunking separators are invalid")
    metadata_fields = value.get("metadata_fields")
    if metadata_fields is not None and (
        not isinstance(metadata_fields, list)
        or len(metadata_fields) > 32
        or any(
            not isinstance(field_name, str)
            or not field_name
            or len(field_name) > 64
            for field_name in metadata_fields
        )
    ):
        raise ValidationFailedError("chunking metadata_fields are invalid")
    segmentation = value.get("segmentation")
    if segmentation is not None:
        if not isinstance(segmentation, dict) or set(segmentation) != {"max_tokens"}:
            raise ValidationFailedError("chunking segmentation is invalid")
        max_tokens = segmentation.get("max_tokens")
        if (
            isinstance(max_tokens, bool)
            or not isinstance(max_tokens, int)
            or not 1 <= max_tokens <= 100_000
        ):
            raise ValidationFailedError(
                "chunking segmentation.max_tokens must be an integer between 1 and 100000"
            )

    chunk_size = int(value.get("chunk_size") or 2_000)
    explicit_overlap = (
        value.get("chunk_overlap")
        if value.get("chunk_overlap") is not None
        else value.get("overlap")
    )
    if explicit_overlap is None:
        if bool(value.get("use_token_count", True)):
            token_limit = int(
                value.get("token_limit")
                or value.get("max_tokens")
                or value.get("chunk_size")
                or 500
            )
            overlap = 50 if mode in {"automatic", "auto", "hierarchical"} else max(
                int(token_limit * 0.1),
                20,
            )
        else:
            overlap = 300
    else:
        overlap = int(explicit_overlap)
    if overlap >= chunk_size:
        raise ValidationFailedError("chunking overlap must be smaller than chunk_size")
    min_chunk_size = int(value.get("min_chunk_size") or 400)
    max_chunk_size = int(value.get("max_chunk_size") or 3_000)
    if min_chunk_size > max_chunk_size:
        raise ValidationFailedError("min_chunk_size must not exceed max_chunk_size")
    child_size = int(value.get("child_chunk_size") or 2_000)
    child_overlap_raw = (
        value.get("child_overlap")
        if value.get("child_overlap") is not None
        else value.get("child_chunk_overlap")
    )
    child_overlap = overlap if child_overlap_raw is None else int(child_overlap_raw)
    if child_overlap >= child_size:
        raise ValidationFailedError("child overlap must be smaller than child_chunk_size")
    parent_size = int(value.get("parent_chunk_size") or 8_000)
    parent_overlap_raw = (
        value.get("parent_overlap")
        if value.get("parent_overlap") is not None
        else value.get("parent_chunk_overlap")
    )
    parent_overlap = (
        child_overlap if parent_overlap_raw is None else int(parent_overlap_raw)
    )
    if parent_overlap >= parent_size:
        raise ValidationFailedError("parent overlap must be smaller than parent_chunk_size")
    min_tokens = value.get("min_chunk_tokens")
    max_tokens = value.get("max_chunk_tokens")
    if min_tokens is not None and max_tokens is not None and min_tokens > max_tokens:
        raise ValidationFailedError("min_chunk_tokens must not exceed max_chunk_tokens")
    return dict(value)

# =============================================================================
# Multilingual Token Counter (Best Practice: 256-512 tokens for RAG)
# =============================================================================

# Arabic Unicode ranges (comprehensive)
_ARABIC_RANGE = re.compile(r"[\u0600-\u06ff\u0750-\u077f\ufb50-\ufdff\ufe70-\ufeff]")
# Arabic diacritics (tashkeel) - these don't add tokens
_ARABIC_DIACRITICS = re.compile(r"[\u064b-\u0652\u0670]")
# CJK characters
_CJK_RANGE = re.compile(r"[\u4e00-\u9fff\u3400-\u4dbf]")


class TokenCounter:
    """
    Precise token counter with multilingual support.

    Best Practice (2025):
    - Uses tiktoken cl100k_base for accurate counting
    - Fallback heuristics for Arabic (not well-supported by tiktoken)
    - Thread-safe caching via instance-level LRU cache

    Token estimation guidelines:
    - English: ~1.3 tokens per word
    - Chinese: ~0.7-1.5 tokens per character
    - Arabic: ~1.5-2.5 tokens per word (complex morphology)

    Note: This class is thread-safe. Uses instance-level cache with threading.Lock
    to ensure thread safety for cache operations.
    """

    def __init__(self, use_tiktoken: bool = True, cache_size: int = 10000):
        self.use_tiktoken = use_tiktoken and _TIKTOKEN_AVAILABLE
        self.encoder = _DEFAULT_ENCODING if self.use_tiktoken else None
        # Instance-level cache to avoid @lru_cache issues with instance methods
        self._cache: dict[str, int] = {}
        self._cache_size = cache_size
        self._cache_keys: list[str] = []  # For LRU eviction
        # Thread lock for cache operations to ensure thread safety
        self._cache_lock = threading.Lock()

    def count_tokens(self, text: str) -> int:
        """
        Count tokens accurately with tiktoken + Arabic heuristics.

        Returns exact token count for production use.
        This method is thread-safe due to instance-level caching with Lock.
        """
        if not text:
            return 0

        # Check cache first (use stable hash to avoid collisions on common prefixes)
        cache_key = f"{len(text)}:{hashlib.md5(text.encode('utf-8', 'ignore')).hexdigest()}"
        with self._cache_lock:
            if cache_key in self._cache:
                return self._cache[cache_key]

        # Detect language composition
        arabic_chars = sum(1 for _match in _ARABIC_RANGE.finditer(text))
        cjk_chars = sum(1 for _match in _CJK_RANGE.finditer(text))
        total_chars = len(text)

        arabic_ratio = arabic_chars / max(total_chars, 1)
        cjk_ratio = cjk_chars / max(total_chars, 1)

        # Calculate result
        # For Arabic-heavy text, use specialized Arabic token counter for better accuracy
        # tiktoken doesn't handle Arabic morphology well (prefixes/suffixes add tokens)
        if arabic_ratio > 0.5 and self.encoder:
            # Use Arabic-specific heuristics for text with >50% Arabic content
            result = self._count_arabic_tokens(text, arabic_ratio)
        elif self.encoder:
            try:
                result = self._count_encoded_tokens(text)
            except (UnicodeEncodeError, ValueError, TypeError) as e:
                # 特定编码错误使用 fallback，其他错误继续抛出
                logger.debug(f"Token encoding failed for text length {len(text)}: {e}")
                result = self._estimate_fallback(text, cjk_ratio)
        else:
            result = self._estimate_fallback(text, cjk_ratio)

        # Cache result with LRU eviction (thread-safe)
        self._add_to_cache(cache_key, result)
        return result

    def _count_encoded_tokens(self, text: str) -> int:
        if not self.encoder:
            return 0
        block_chars = 64 * 1024
        if len(text) <= block_chars:
            return len(self.encoder.encode(text))
        total = 0
        for start in range(0, len(text), block_chars):
            total += len(self.encoder.encode(text[start : start + block_chars]))
        return total

    def _add_to_cache(self, key: str, value: int) -> None:
        """Add result to cache with LRU eviction. Thread-safe."""
        with self._cache_lock:
            if key in self._cache:
                # Move to end (most recently used)
                self._cache_keys.remove(key)
            elif len(self._cache) >= self._cache_size:
                # Evict least recently used
                oldest_key = self._cache_keys.pop(0)
                del self._cache[oldest_key]

            self._cache[key] = value
            self._cache_keys.append(key)

    def _count_arabic_tokens(self, text: str, _arabic_ratio: float) -> int:
        """
        Count tokens for Arabic text with calibrated heuristics.

        Arabic tokenization is complex:
        - Words are agglutinative (prefixes + root + suffixes)
        - Diacritics (tashkeel) don't add tokens
        - Average Arabic word = 3.0-3.5 tokens in GPT models (empirically validated)

        Calibration Notes (2025):
        - Tested against tiktoken cl100k_base with Arabic texts
        - Short Arabic words (2-3 chars): ~2.5 tokens
        - Medium Arabic words (4-6 chars): ~3.2 tokens
        - Long Arabic words with prefixes/suffixes: ~4.0 tokens
        - Weighted average: ~3.2 tokens per word

        Previous value of 2.0 was underestimating by ~60%, causing chunks
        to exceed token limits significantly.
        """
        # Remove diacritics for counting
        clean_text = _ARABIC_DIACRITICS.sub("", text)

        # Split into words (Arabic uses spaces)
        arabic_word_count = 0
        non_arabic_word_count = 0
        for word_match in re.finditer(r"\S+", clean_text):
            if _ARABIC_RANGE.search(word_match.group(0)):
                arabic_word_count += 1
            else:
                non_arabic_word_count += 1

        # Arabic: ~3.2 tokens per word (calibrated against actual tokenizer)
        # This accounts for the complex morphology of Arabic:
        # - Prefixes: و، ف، ب، ل، ك، ال
        # - Suffixes: ة، ات، ون، ين، etc.
        arabic_tokens = arabic_word_count * 3.2
        non_arabic_tokens = non_arabic_word_count * 1.3

        return int(arabic_tokens + non_arabic_tokens)

    def _estimate_fallback(self, text: str, _cjk_ratio: float) -> int:
        """Fallback estimation when tiktoken unavailable."""
        if not text:
            return 0

        # CJK characters
        cjk_count = sum(1 for _match in _CJK_RANGE.finditer(text))

        # Arabic characters - count words after removing diacritics
        arabic_words = 0
        word_count = 0
        for word_match in re.finditer(r"\S+", text):
            word = word_match.group(0)
            if _ARABIC_RANGE.search(word):
                arabic_words += 1
            elif not _CJK_RANGE.fullmatch(word):
                word_count += 1

        # Estimates: CJK ~0.7, Arabic ~3.2/word (calibrated), English ~1.3/word
        cjk_tokens = cjk_count * 0.7
        arabic_tokens = arabic_words * 3.2  # Updated from 2.0 to 3.2 for accuracy
        english_tokens = word_count * 1.3

        return int(cjk_tokens + arabic_tokens + english_tokens)

    def count_tokens_for_chunks(self, texts: list[str]) -> list[int]:
        """Batch token counting."""
        return [self.count_tokens(t) for t in texts]


# Global token counter instance
_token_counter: TokenCounter | None = None


def get_token_counter() -> TokenCounter:
    """Get or create global token counter."""
    global _token_counter
    if _token_counter is None:
        _token_counter = TokenCounter(use_tiktoken=True)
    return _token_counter


def count_tokens(text: str) -> int:
    """Convenience function for token counting."""
    return get_token_counter().count_tokens(text)


# =============================================================================
# Language Detection and Chunk Size Calibration
# =============================================================================

# Token multipliers for different languages (relative to English)
# These account for tokenization differences in subword tokenizers
LANGUAGE_TOKEN_MULTIPLIERS = {
    "ar": 1.15,  # Arabic: more tokens per word due to morphology
    "zh": 1.0,  # Chinese: roughly similar
    "ja": 1.0,  # Japanese: roughly similar
    "ko": 1.0,  # Korean: roughly similar
    "en": 1.0,  # English: baseline
    "mixed": 1.1,  # Mixed content: slightly conservative
}


def detect_text_language(text: str) -> tuple[str, float]:
    """
    Detect the primary language of text.

    Args:
        text: Input text

    Returns:
        Tuple of (language_code, confidence)
        - language_code: "ar", "en", "zh", "mixed", etc.
        - confidence: 0.0 to 1.0
    """
    if not text:
        return ("en", 0.0)

    # Count characters by script
    arabic_chars = sum(1 for _match in _ARABIC_RANGE.finditer(text))
    cjk_chars = sum(1 for _match in _CJK_RANGE.finditer(text))
    total_chars = len(text.replace(" ", "").replace("\n", ""))

    if total_chars == 0:
        return ("en", 0.0)

    arabic_ratio = arabic_chars / total_chars
    cjk_ratio = cjk_chars / total_chars

    # Determine primary language
    if arabic_ratio > 0.6:
        return ("ar", arabic_ratio)
    elif arabic_ratio > 0.3:
        return ("mixed", arabic_ratio)
    elif cjk_ratio > 0.5:
        # Could further distinguish zh/ja/ko if needed
        return ("zh", cjk_ratio)
    elif cjk_ratio > 0.2:
        return ("mixed", cjk_ratio)
    else:
        return ("en", 1.0 - arabic_ratio - cjk_ratio)


def get_chunk_size_for_language(
    language: str,
    base_chunk_size: int = 1000,
) -> int:
    """
    Adjust chunk size based on language token density.

    Different languages have different token-to-character ratios.
    This function adjusts the chunk size to maintain consistent
    token counts across languages.

    Args:
        language: Language code ("ar", "en", "zh", etc.)
        base_chunk_size: Base chunk size in characters for English

    Returns:
        Adjusted chunk size for the target language

    Example:
        >>> get_chunk_size_for_language("ar", 1000)
        870  # Arabic needs smaller chunks (more tokens per char)
        >>> get_chunk_size_for_language("en", 1000)
        1000  # English baseline
    """
    multiplier = LANGUAGE_TOKEN_MULTIPLIERS.get(language, 1.0)
    adjusted_size = int(base_chunk_size / multiplier)
    return adjusted_size


def chunk_text(
    text: str,
    chunk_size: int = 1000,
    chunk_overlap: int = 200,
    language: str | None = None,
) -> list[str]:
    """
    Chunk text with language-aware sizing.

    This is a convenience function that:
    1. Detects language if not specified
    2. Adjusts chunk size for the language
    3. Splits text at natural boundaries

    Args:
        text: Input text to chunk
        chunk_size: Target chunk size (will be adjusted for language)
        chunk_overlap: Overlap between chunks
        language: Optional language code (auto-detected if None)

    Returns:
        List of text chunks
    """
    if not text:
        return []

    # Detect language if not specified
    if language is None:
        language, _ = detect_text_language(text)

    # Adjust chunk size for language
    adjusted_size = get_chunk_size_for_language(language, chunk_size)

    # Use appropriate separators based on language
    if language == "ar":
        # Arabic sentence separators (period, question mark, etc.)
        separators = ["\n\n", "\n", "۔", "؟", "،", ".", " "]
    elif language in ("zh", "ja"):
        separators = ["\n\n", "\n", "。", "？", "！", "，", " "]
    else:
        separators = ["\n\n", "\n", ". ", "? ", "! ", ", ", " "]

    chunks = []
    current_chunk = ""

    # Split by sentences/paragraphs
    parts = _split_by_separators(text, separators)

    for part in parts:
        if not part.strip():
            continue

        # Check if adding this part would exceed the limit
        if len(current_chunk) + len(part) <= adjusted_size:
            current_chunk += part
        else:
            # Save current chunk if non-empty
            if current_chunk.strip():
                _append_bounded_output(chunks, current_chunk.strip())

            # Start new chunk with overlap
            if chunk_overlap > 0 and current_chunk:
                # Get overlap from end of current chunk
                overlap_text = current_chunk[-chunk_overlap:]
                current_chunk = overlap_text + part
            else:
                current_chunk = part

    # Add final chunk
    if current_chunk.strip():
        _append_bounded_output(chunks, current_chunk.strip())

    return chunks


def _split_by_separators(text: str, separators: list[str]) -> list[str]:
    """Split text by multiple separators, preserving separators."""
    if not separators:
        return [text]

    sep = separators[0]
    remaining_seps = separators[1:]

    _require_literal_split_budget(text, sep)
    parts = text.split(sep)

    result = []
    for i, part in enumerate(parts):
        if remaining_seps:
            sub_parts = _split_by_separators(part, remaining_seps)
            _extend_bounded_output(result, sub_parts)
        else:
            _append_bounded_output(result, part)

        # Add separator back (except for last part)
        if i < len(parts) - 1:
            result[-1] = result[-1] + sep if result else sep

    return result


class ChunkingMode(str, Enum):
    """Supported chunking modes"""

    AUTOMATIC = "automatic"  # 智能切分
    FIXED_SIZE = "fixed_size"  # 按长度切分
    PARAGRAPH = "paragraph"  # 按段落切分
    PAGE = "page"  # 按页切分
    HEADING = "heading"  # 按标题切分
    REGEX = "regex"  # 按正则切分
    SEPARATOR = "separator"  # 按符号切分
    RECURSIVE = "recursive"  # 递归切分
    HIERARCHICAL = "hierarchical"  # 父子切分
    QA = "qa"  # QA对切分


class ContentType(str, Enum):
    """Content type for segments (multimodal support)"""

    TEXT = "text"
    IMAGE = "image"
    MIXED = "mixed"  # Text chunk with associated images


@dataclass
class AssociatedImage:
    """
    Image associated with a text chunk (Dify-style Smart Attachment Handling).

    In multimodal RAG, text chunks can have up to 10 associated images.
    Images are associated based on proximity in the source document.

    Attributes:
        image_segment_id: ID of the image segment in the database
        storage_url: URL to the image in storage (S3/OSS)
        filename: Original filename of the image
        vlm_description: VLM-generated description of the image
        proximity_score: How closely related the image is to the text [0,1]
            - 1.0 = directly embedded inline
            - 0.7 = adjacent paragraph
            - 0.5 = same page/section
            - 0.3 = related but distant
        char_offset: Character offset in source document where image was found
        page_number: Page number in PDF/multi-page documents
    """

    image_segment_id: str
    storage_url: str
    filename: str = ""
    vlm_description: str | None = None
    proximity_score: float = 1.0
    char_offset: int = 0
    page_number: int | None = None
    media_type: str = "image/png"

    def to_dict(self) -> dict[str, Any]:
        """Convert to dictionary for serialization"""
        return {
            "image_segment_id": self.image_segment_id,
            "storage_url": self.storage_url,
            "filename": self.filename,
            "vlm_description": self.vlm_description,
            "proximity_score": self.proximity_score,
            "char_offset": self.char_offset,
            "page_number": self.page_number,
            "media_type": self.media_type,
        }

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> AssociatedImage:
        """Create from dictionary"""
        return cls(
            image_segment_id=data.get("image_segment_id", ""),
            storage_url=data.get("storage_url", ""),
            filename=data.get("filename", ""),
            vlm_description=data.get("vlm_description"),
            proximity_score=float(data.get("proximity_score", 1.0)),
            char_offset=int(data.get("char_offset", 0)),
            page_number=data.get("page_number"),
            media_type=data.get("media_type", "image/png"),
        )


@dataclass
class ChunkingConfig:
    """Comprehensive chunking configuration

    Best Practices (2025):
    - Optimal chunk size: 400-512 tokens with 10-20% overlap
    - RecursiveCharacterTextSplitter achieves 85-90% recall
    - Parent-child chunking: parent 1500-2000 tokens, child 400-500 tokens
    - Semantic chunking offers 70% accuracy improvement for knowledge bases
    """

    mode: ChunkingMode = ChunkingMode.AUTOMATIC

    # Size parameters in CHARACTERS (optimized defaults based on industry research)
    # Note: 1 token ≈ 4-5 chars (English) or 1.5-2 chars (Chinese)
    # Target: 400-500 tokens = ~2000 chars for English
    chunk_size: int = 2000  # ~400-500 tokens for most RAG
    chunk_overlap: int = 300  # 15% overlap (~60-75 tokens)
    max_chunk_size: int = 3000  # Absolute max (~600-750 tokens)
    min_chunk_size: int = 400  # Min chunk size (~80-100 tokens)

    # Token-based (recommended for production)
    use_token_count: bool = True  # Token-based more accurate than char-based
    token_limit: int = 500  # ~500 tokens optimal for most embedding models
    # Optional token-based min/max (used for merge_small_chunks and strict control)
    # When not set, no min/max token enforcement is applied.
    min_chunk_tokens: int | None = None
    max_chunk_tokens: int | None = None

    # Separators (priority order: preserve semantic boundaries)
    separators: list[str] = field(
        default_factory=lambda: [
            "\n\n\n",  # Section breaks
            "\n\n",  # Paragraphs
            "\n",  # Lines
            "。",  # Chinese sentence end
            ".",  # English sentence end
            "！",
            "!",  # Exclamations
            "？",
            "?",  # Questions
            "；",
            ";",  # Semicolons
            "،",
            ",",  # Arabic comma, English comma
            " ",  # Words (last resort)
        ]
    )
    primary_separator: str = "\n\n"

    # Regex pattern (for regex mode)
    regex_pattern: str = ""

    # Heading detection (for heading mode)
    heading_patterns: list[str] = field(
        default_factory=lambda: [
            r"^#{1,6}\s+.+$",  # Markdown headings
            r"^第[一二三四五六七八九十\d]+[章节条款]",  # Chinese chapter markers
            r"^[A-Z][A-Z \t]{4,}:?$",  # ALL CAPS headings (5+ chars)
        ]
    )

    # Hierarchical/Parent-child (optimized for retrieval)
    # Parent provides context, child provides precision
    # Note: sizes in CHARACTERS (1 token ≈ 4-5 chars English)
    parent_chunk_size: int = 8000  # ~1500-2000 tokens for context
    parent_overlap: int = 400  # 5% overlap between parents (~80-100 tokens)
    child_chunk_size: int = 2000  # ~400-500 tokens for precision
    child_overlap: int = 300  # 15% overlap between children (~60-75 tokens)
    parent_mode: str = "recursive"  # recursive | paragraph | section | full_doc
    # Optional token limits for hierarchical mode (strict control)
    parent_token_limit: int | None = None
    child_token_limit: int | None = None

    # Image-aware chunking
    preserve_images: bool = True  # Keep images with surrounding context
    image_context_chars: int = 1000  # Characters around image to preserve (~200 tokens)

    # Preprocessing
    remove_extra_spaces: bool = True
    remove_urls_emails: bool = False
    normalize_whitespace: bool = True
    strip_html: bool = False

    # Metadata extraction
    extract_metadata: bool = False
    metadata_fields: list[str] = field(
        default_factory=lambda: ["title", "author", "date", "keywords"]
    )

    # Page markers (for page mode)
    page_marker: str = r"\f"  # Form feed or custom marker

    # Strict section traceability.
    # When enabled, ensures every chunk has a section_title for downstream citations.
    strict_section_traceability: bool = False
    _runtime_chunk_count: int = field(default=0, init=False, repr=False)

    def __post_init__(self) -> None:
        # Normalize optional min/max token constraints (<=0 => disabled)
        if self.min_chunk_tokens is not None and self.min_chunk_tokens <= 0:
            self.min_chunk_tokens = None
        if self.max_chunk_tokens is not None and self.max_chunk_tokens <= 0:
            self.max_chunk_tokens = None
        if self.child_token_limit is None and self.token_limit:
            self.child_token_limit = int(self.token_limit)
        if self.parent_token_limit is None and self.token_limit:
            self.parent_token_limit = max(int(self.token_limit * 3), 900)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ChunkingConfig:
        if not data:
            return cls()

        validate_persisted_chunking_config(data)

        mode_str = str(data.get("mode", "automatic")).lower()
        mode_map = {
            "automatic": ChunkingMode.AUTOMATIC,
            "auto": ChunkingMode.AUTOMATIC,
            "fixed_size": ChunkingMode.FIXED_SIZE,
            "fixed": ChunkingMode.FIXED_SIZE,
            "custom": ChunkingMode.FIXED_SIZE,
            "paragraph": ChunkingMode.PARAGRAPH,
            "page": ChunkingMode.PAGE,
            "heading": ChunkingMode.HEADING,
            "section": ChunkingMode.HEADING,
            "regex": ChunkingMode.REGEX,
            "separator": ChunkingMode.SEPARATOR,
            "recursive": ChunkingMode.RECURSIVE,
            "hierarchical": ChunkingMode.HIERARCHICAL,
            "parent_child": ChunkingMode.HIERARCHICAL,
            "qa": ChunkingMode.QA,
        }
        mode = mode_map.get(mode_str, ChunkingMode.AUTOMATIC)

        use_token_count = bool(data.get("use_token_count", True))

        # Token limit resolution (backward compatible):
        # 1) Explicit token_limit/max_tokens
        # 2) Dify-style segmentation.max_tokens
        # 3) For fixed-size mode, interpret chunk_size as tokens when use_token_count=True
        token_limit_source = "default"
        token_limit_raw = (
            data.get("token_limit")
            if data.get("token_limit") is not None
            else data.get("max_tokens")
        )
        if token_limit_raw is not None:
            token_limit_source = "explicit"
        if token_limit_raw is None:
            segmentation = (
                data.get("segmentation") if isinstance(data.get("segmentation"), dict) else {}
            )
            token_limit_raw = segmentation.get("max_tokens")
            if token_limit_raw is not None:
                token_limit_source = "segmentation"
        chunk_size_explicit = data.get("chunk_size") is not None
        chunk_size_val: int | None = None
        if chunk_size_explicit:
            try:
                chunk_size_val = int(data.get("chunk_size"))
            except Exception:
                chunk_size_val = None
        if token_limit_raw is None and use_token_count and chunk_size_explicit and chunk_size_val:
            if mode == ChunkingMode.FIXED_SIZE:
                # Fixed-size mode: chunk_size is the exact token limit.
                token_limit_raw = chunk_size_val
                token_limit_source = "chunk_size_tokens"
        if token_limit_raw is None:
            token_limit_raw = 500
        token_limit = int(token_limit_raw)
        min_chunk_tokens = data.get("min_chunk_tokens")
        max_chunk_tokens = data.get("max_chunk_tokens")
        if min_chunk_tokens is not None:
            try:
                min_chunk_tokens = int(min_chunk_tokens)
            except Exception:
                min_chunk_tokens = None
            if min_chunk_tokens is not None and min_chunk_tokens <= 0:
                min_chunk_tokens = None
        if max_chunk_tokens is not None:
            try:
                max_chunk_tokens = int(max_chunk_tokens)
            except Exception:
                max_chunk_tokens = None
            if max_chunk_tokens is not None and max_chunk_tokens <= 0:
                max_chunk_tokens = None

        parent_token_limit = data.get("parent_token_limit")
        child_token_limit = data.get("child_token_limit")

        # Resolve chunk overlap: keep defaults for char-based; token-based uses runtime ratio unless explicitly set
        chunk_overlap_val = data.get("chunk_overlap")
        if chunk_overlap_val is None:
            chunk_overlap_val = data.get("overlap")
        if chunk_overlap_val is None:
            if use_token_count:
                if mode in (ChunkingMode.HIERARCHICAL, ChunkingMode.AUTOMATIC):
                    chunk_overlap_val = 50
                else:
                    chunk_overlap_val = max(int(token_limit * 0.1), 20)
            else:
                chunk_overlap_val = 300

        # Parent/child size & overlap defaults (token-aware)
        parent_chunk_size_explicit = data.get("parent_chunk_size") is not None
        child_chunk_size_explicit = data.get("child_chunk_size") is not None
        parent_chunk_size_val = int(data.get("parent_chunk_size", 8000))
        child_chunk_size_val = int(data.get("child_chunk_size", 2000))
        parent_overlap_val = data.get("parent_overlap")
        if parent_overlap_val is None:
            parent_overlap_val = data.get("parent_chunk_overlap")
        child_overlap_val = data.get("child_overlap")
        if child_overlap_val is None:
            child_overlap_val = data.get("child_chunk_overlap")
        if child_overlap_val is None:
            child_overlap_val = chunk_overlap_val
        if parent_overlap_val is None:
            parent_overlap_val = (
                child_overlap_val if child_overlap_val is not None else chunk_overlap_val
            )

        auto_defaults = (
            mode == ChunkingMode.AUTOMATIC
            and token_limit_source == "default"
            and not any(
                k in data
                for k in (
                    "chunk_size",
                    "chunk_overlap",
                    "parent_chunk_size",
                    "child_chunk_size",
                    "parent_overlap",
                    "child_overlap",
                    "token_limit",
                    "parent_token_limit",
                    "child_token_limit",
                )
            )
        )
        if auto_defaults:
            token_limit = 400
            child_token_limit = 400
            parent_token_limit = 1500
            chunk_overlap_val = 50
            child_overlap_val = 50
            parent_overlap_val = 50

        # Derive char/token settings for token-based mode while preserving explicit char sizes.
        if use_token_count:
            if mode == ChunkingMode.FIXED_SIZE:
                if chunk_size_val is None:
                    chunk_size_val = int(token_limit)
            else:
                if chunk_size_val is None:
                    chunk_size_val = max(int(token_limit * 4), 1000)

            if not child_chunk_size_explicit and child_chunk_size_val <= 1200:
                if child_token_limit is None:
                    child_token_limit = int(child_chunk_size_val)
                child_chunk_size_val = max(
                    int((child_token_limit or child_chunk_size_val) * 4), 1000
                )
            if not parent_chunk_size_explicit and parent_chunk_size_val <= 5000:
                if parent_token_limit is None:
                    parent_token_limit = int(parent_chunk_size_val)
                parent_chunk_size_val = max(
                    int((parent_token_limit or parent_chunk_size_val) * 4), 2000
                )

        if child_token_limit is None:
            child_token_limit = token_limit
        if parent_token_limit is None:
            parent_token_limit = max(int(child_token_limit) * 4, 900)

        parent_mode = data.get("parent_mode")
        if not parent_mode and mode in (ChunkingMode.HIERARCHICAL, ChunkingMode.AUTOMATIC):
            parent_mode = "fixed"

        return cls(
            mode=mode,
            chunk_size=int(chunk_size_val) if chunk_size_val is not None else 2000,
            chunk_overlap=int(chunk_overlap_val),
            max_chunk_size=int(data.get("max_chunk_size", 3000)),
            min_chunk_size=int(data.get("min_chunk_size", 400)),
            use_token_count=use_token_count,
            token_limit=token_limit,
            min_chunk_tokens=min_chunk_tokens,
            max_chunk_tokens=max_chunk_tokens,
            separators=data.get("separators")
            or ["\n\n\n", "\n\n", "\n", "。", ".", "！", "!", "？", "?", " "],
            primary_separator=str(data.get("primary_separator") or data.get("separator") or "\n\n"),
            regex_pattern=str(data.get("regex_pattern") or data.get("regex") or ""),
            heading_patterns=data.get("heading_patterns") or [],
            parent_chunk_size=int(parent_chunk_size_val),
            parent_overlap=int(parent_overlap_val),
            child_chunk_size=int(child_chunk_size_val),
            child_overlap=int(child_overlap_val),
            parent_mode=str(parent_mode or "recursive"),
            parent_token_limit=int(parent_token_limit) if parent_token_limit is not None else None,
            child_token_limit=int(child_token_limit) if child_token_limit is not None else None,
            preserve_images=bool(data.get("preserve_images", True)),
            image_context_chars=int(data.get("image_context_chars", 1000)),
            remove_extra_spaces=bool(data.get("remove_extra_spaces", True)),
            remove_urls_emails=bool(data.get("remove_urls_emails", False)),
            normalize_whitespace=bool(data.get("normalize_whitespace", True)),
            strip_html=bool(data.get("strip_html", False)),
            extract_metadata=bool(data.get("extract_metadata", False)),
            metadata_fields=data.get("metadata_fields") or ["title", "author", "date"],
            page_marker=str(data.get("page_marker") or r"\f"),
            strict_section_traceability=bool(data.get("strict_section_traceability", False)),
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "mode": self.mode.value,
            "chunk_size": self.chunk_size,
            "chunk_overlap": self.chunk_overlap,
            "max_chunk_size": self.max_chunk_size,
            "min_chunk_size": self.min_chunk_size,
            "use_token_count": self.use_token_count,
            "token_limit": self.token_limit,
            "min_chunk_tokens": self.min_chunk_tokens,
            "max_chunk_tokens": self.max_chunk_tokens,
            "separators": self.separators,
            "primary_separator": self.primary_separator,
            "regex_pattern": self.regex_pattern,
            "heading_patterns": self.heading_patterns,
            "parent_chunk_size": self.parent_chunk_size,
            "parent_overlap": self.parent_overlap,
            "child_chunk_size": self.child_chunk_size,
            "child_overlap": self.child_overlap,
            "parent_mode": self.parent_mode,
            "parent_token_limit": self.parent_token_limit,
            "child_token_limit": self.child_token_limit,
            "preserve_images": self.preserve_images,
            "image_context_chars": self.image_context_chars,
            "remove_extra_spaces": self.remove_extra_spaces,
            "remove_urls_emails": self.remove_urls_emails,
            "normalize_whitespace": self.normalize_whitespace,
            "strip_html": self.strip_html,
            "extract_metadata": self.extract_metadata,
            "metadata_fields": self.metadata_fields,
            "page_marker": self.page_marker,
            "strict_section_traceability": self.strict_section_traceability,
        }


@dataclass
class Chunk:
    """
    Represents a single text chunk with multimodal support.

    Multimodal RAG Extension (P3):
    - Chunks can have associated images (up to 10 per Dify pattern)
    - content_type indicates if this is a text, image, or mixed chunk
    - associated_images holds references to related image segments

    For image segments:
    - content_type = "image"
    - image_url, image_filename etc. are populated
    - vlm_description contains the VLM-generated description
    """

    text: str
    index: int = 0
    token_count: int = 0
    word_count: int = 0
    char_count: int = 0
    metadata: dict[str, Any] = field(default_factory=dict)
    parent_id: str | None = None
    children: list[Chunk] = field(default_factory=list)
    hash_id: str = ""

    # Multimodal fields (P3 extension)
    content_type: ContentType = ContentType.TEXT
    associated_images: list[AssociatedImage] = field(default_factory=list)

    # Image-specific fields (for image segments)
    image_url: str | None = None
    image_filename: str | None = None
    image_media_type: str | None = None
    vlm_description: str | None = None

    # Maximum images per chunk (Dify pattern)
    MAX_ASSOCIATED_IMAGES: int = field(default=10, repr=False)

    def __post_init__(self):
        if not self.hash_id and self.text:
            self.hash_id = hashlib.md5(self.text.encode()).hexdigest()[:12]
        if not self.char_count:
            self.char_count = len(self.text)
        if not self.word_count:
            self.word_count = len(self.text.split())
        if not self.token_count:
            # Prefer accurate token counting when available
            try:
                self.token_count = count_tokens(self.text)
            except Exception:
                # Fallback estimate: ~4 chars per token for English, ~2 for Chinese
                self.token_count = self._estimate_tokens(self.text)

    @property
    def has_images(self) -> bool:
        """Check if this chunk has associated images"""
        return len(self.associated_images) > 0

    @property
    def image_count(self) -> int:
        """Number of associated images"""
        return len(self.associated_images)

    @property
    def is_image_segment(self) -> bool:
        """Check if this is an image segment (not text)"""
        return self.content_type == ContentType.IMAGE

    def add_associated_image(self, image: AssociatedImage) -> bool:
        """
        Add an associated image to this chunk.

        Args:
            image: The AssociatedImage to add

        Returns:
            True if added, False if max limit reached
        """
        if len(self.associated_images) >= self.MAX_ASSOCIATED_IMAGES:
            return False
        self.associated_images.append(image)
        # Update content type to mixed if this was pure text
        if self.content_type == ContentType.TEXT:
            self.content_type = ContentType.MIXED
        return True

    def get_images_sorted_by_proximity(self) -> list[AssociatedImage]:
        """Get associated images sorted by proximity score (highest first)"""
        return sorted(self.associated_images, key=lambda x: x.proximity_score, reverse=True)

    @staticmethod
    def _estimate_tokens(text: str) -> int:
        """
        Count tokens accurately using tiktoken with multilingual support.

        Uses the global TokenCounter which handles:
        - English: tiktoken cl100k_base encoding
        - Arabic: heuristic-based counting (2.0 tokens/word)
        - Chinese: tiktoken with CJK support
        - Mixed: language-aware hybrid counting
        """
        if not text:
            return 0
        return count_tokens(text)

    def to_multimodal_dict(self) -> dict[str, Any]:
        """Convert chunk to dictionary including multimodal fields"""
        return {
            "text": self.text,
            "index": self.index,
            "token_count": self.token_count,
            "char_count": self.char_count,
            "hash_id": self.hash_id,
            "content_type": self.content_type.value,
            "has_images": self.has_images,
            "image_count": self.image_count,
            "associated_images": [img.to_dict() for img in self.associated_images],
            "image_url": self.image_url,
            "image_filename": self.image_filename,
            "vlm_description": self.vlm_description,
            "metadata": self.metadata,
        }


class TextPreprocessor:
    """Text preprocessing utilities"""

    # Common patterns
    URL_PATTERN = re.compile(r"https?://\S+|www\.\S+")
    EMAIL_PATTERN = re.compile(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b")
    HTML_TAG_PATTERN = re.compile(r"<[^>]+>")
    EXTRA_SPACES_PATTERN = re.compile(r"[ \t]+")
    EXTRA_NEWLINES_PATTERN = re.compile(r"\n{3,}")

    @classmethod
    def preprocess(cls, text: str, config: ChunkingConfig) -> str:
        """Apply preprocessing based on config"""
        if not text:
            return ""

        result = text

        if config.strip_html:
            result = cls.HTML_TAG_PATTERN.sub(" ", result)

        if config.remove_urls_emails:
            result = cls.URL_PATTERN.sub(" ", result)
            result = cls.EMAIL_PATTERN.sub(" ", result)

        if config.normalize_whitespace:
            result = result.replace("\r\n", "\n").replace("\r", "\n")
            result = cls.EXTRA_NEWLINES_PATTERN.sub("\n\n", result)

        if config.remove_extra_spaces:
            result = cls.EXTRA_SPACES_PATTERN.sub(" ", result)

        return result.strip()

    # Simple language detection heuristics (no external dependency)
    _CJK_RANGE = re.compile(r"[\u4e00-\u9fff\u3400-\u4dbf]")
    _ARABIC_RANGE = re.compile(r"[\u0600-\u06ff\u0750-\u077f\ufb50-\ufdff\ufe70-\ufeff]")
    _CYRILLIC_RANGE = re.compile(r"[\u0400-\u04ff]")
    _HANGUL_RANGE = re.compile(r"[\uac00-\ud7af\u1100-\u11ff]")
    _HIRAGANA_KATAKANA = re.compile(r"[\u3040-\u309f\u30a0-\u30ff]")

    @classmethod
    def _detect_language(cls, text: str) -> str:
        """Detect primary language from character distribution (lightweight, no deps)."""
        sample = text[:5000]
        total = max(len(sample), 1)
        cjk = len(cls._CJK_RANGE.findall(sample))
        arabic = len(cls._ARABIC_RANGE.findall(sample))
        cyrillic = len(cls._CYRILLIC_RANGE.findall(sample))
        hangul = len(cls._HANGUL_RANGE.findall(sample))
        kana = len(cls._HIRAGANA_KATAKANA.findall(sample))

        scores = {"zh": cjk, "ar": arabic, "ru": cyrillic, "ko": hangul, "ja": kana}
        best = max(scores, key=scores.get)  # type: ignore[arg-type]
        if scores[best] / total > 0.05:
            return best
        return "en"

    @classmethod
    def _extract_keywords(cls, text: str, top_k: int = 10) -> list[str]:
        """Extract keywords via simple TF heuristic (no external dependency)."""
        stopwords = {
            "the",
            "and",
            "for",
            "are",
            "but",
            "not",
            "you",
            "all",
            "can",
            "had",
            "her",
            "was",
            "one",
            "our",
            "out",
            "has",
            "have",
            "this",
            "that",
            "with",
            "from",
            "they",
            "been",
            "said",
            "each",
            "which",
            "their",
            "will",
            "other",
            "about",
            "many",
            "then",
            "them",
            "these",
            "some",
            "would",
            "make",
            "like",
            "into",
            "more",
            "than",
            "its",
            "over",
            "such",
            "also",
            "most",
        }
        freq: dict[str, int] = {}
        for index, word_match in enumerate(
            re.finditer(r"\b[a-zA-Z\u4e00-\u9fff]{3,}\b", text, re.IGNORECASE),
            start=1,
        ):
            if index > 100_000:
                break
            w = word_match.group(0).lower()
            if w not in stopwords and len(w) >= 3:
                freq[w] = freq.get(w, 0) + 1
        sorted_words = sorted(freq, key=freq.get, reverse=True)  # type: ignore[arg-type]
        return sorted_words[:top_k]

    @classmethod
    def extract_metadata(cls, text: str, fields: list[str]) -> dict[str, Any]:
        """Extract metadata from document text"""
        metadata = {}

        # Title: first heading or short first line
        if "title" in fields:
            first_non_whitespace = re.search(r"\S", text)
            if first_non_whitespace:
                line_end = text.find("\n", first_non_whitespace.start())
                if line_end < 0:
                    line_end = len(text)
                first_line = text[first_non_whitespace.start() : line_end].strip()
                if first_line and len(first_line) < 200 and (
                    first_line.startswith("#") or len(first_line) < 100
                ):
                    metadata["title"] = first_line.lstrip("#").strip()

        # Date: scan first 1000 chars
        if "date" in fields:
            date_patterns = [
                r"\d{4}[-/]\d{1,2}[-/]\d{1,2}",
                r"\d{1,2}[-/]\d{1,2}[-/]\d{4}",
            ]
            for pattern in date_patterns:
                match = re.search(pattern, text[:1000])
                if match:
                    metadata["date"] = match.group()
                    break

        # Language detection
        if "language" in fields:
            metadata["language"] = cls._detect_language(text)

        # Keyword extraction
        if "keywords" in fields:
            metadata["keywords"] = cls._extract_keywords(text)

        # Word / char counts (always useful for traceability)
        if "word_count" in fields or "char_count" in fields:
            metadata["word_count"] = sum(1 for _match in re.finditer(r"\S+", text))
            metadata["char_count"] = len(text)

        return metadata


class BaseChunker(ABC):
    """Abstract base class for all chunking strategies"""

    def __init__(self, config: ChunkingConfig):
        self.config = config

    @abstractmethod
    def chunk(self, text: str) -> list[Chunk]:
        """Split text into chunks"""
        pass

    def _create_chunk(
        self,
        text: str,
        index: int,
        metadata: dict[str, Any] | None = None,
        parent_id: str | None = None,
    ) -> Chunk:
        """Create a Chunk object with computed fields"""
        if self.config._runtime_chunk_count >= MAX_CHUNK_OUTPUTS:
            raise ValidationFailedError(
                f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
            )
        self.config._runtime_chunk_count += 1
        return Chunk(
            text=text.strip(),
            index=index,
            metadata=metadata or {},
            parent_id=parent_id,
        )

    def _split_with_overlap(self, text: str, chunk_size: int, overlap: int) -> list[str]:
        """Split text with overlap, trying to break at natural boundaries (character-based)"""
        if not text or chunk_size <= 0:
            return []

        # Ensure overlap is reasonable
        overlap = min(overlap, chunk_size // 2)

        chunks = []
        start = 0
        text_len = len(text)
        step = max(chunk_size - min(overlap, chunk_size // 2), 1)
        if (text_len + step - 1) // step > MAX_CHUNK_OUTPUTS:
            raise ValidationFailedError(
                f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
            )

        while start < text_len:
            end = min(start + chunk_size, text_len)

            # Try to find a good break point near the end
            if end < text_len:
                # Look for sentence/paragraph breaks within the last 20% of chunk
                search_start = max(start, end - int(chunk_size * 0.2))
                search_text = text[search_start:end]

                # Priority: paragraph > sentence > word (supports multiple languages)
                best_pos = -1
                for pattern in ["\n\n", "\n", "。", ".", "！", "!", "？", "?", "۔", "؟"]:
                    pos = search_text.rfind(pattern)
                    if pos > 0:
                        best_pos = search_start + pos + len(pattern)
                        break

                if best_pos > start:
                    end = best_pos

            chunk_text = text[start:end].strip()
            if chunk_text and len(chunk_text) >= self.config.min_chunk_size:
                _append_bounded_output(chunks, chunk_text)

            # Move start forward - ensure we make progress
            # The step should be at least (chunk_size - overlap) to avoid excessive overlap
            step = max(chunk_size - overlap, 1)
            new_start = start + step

            # If we didn't reach the end, use the actual end minus overlap
            if end < text_len:
                new_start = max(new_start, end - overlap)
            else:
                new_start = text_len  # We're done

            start = new_start

        return chunks

    def _split_by_tokens(self, text: str, token_limit: int, overlap_tokens: int = 50) -> list[str]:
        """
        Split text by token count with overlap (Best Practice 2025).

        This ensures chunks are exactly within token_limit tokens,
        which is critical for:
        - Embedding models with fixed context windows
        - LLM context management
        - Accurate retrieval scoring

        Args:
            text: Input text to split
            token_limit: Maximum tokens per chunk (256-512 recommended)
            overlap_tokens: Tokens to overlap between chunks (10-20% of limit)

        Returns:
            List of text chunks, each ≤ token_limit tokens
        """
        if not text or token_limit <= 0:
            return []

        token_counter = get_token_counter()

        # Ensure overlap is reasonable
        overlap_tokens = min(overlap_tokens, token_limit // 2)
        _require_token_window_input_budget(text, token_limit, overlap_tokens)

        # Split into sentences first for better boundaries
        # Multilingual sentence endings: . ! ? 。！？۔؟
        sentence_pattern = re.compile(r"([.!?。！？۔؟]\s*|\n\n+)")
        merged_sentences: list[str] = []
        sentence_start = 0
        for sentence_match in sentence_pattern.finditer(text):
            sentence = text[sentence_start : sentence_match.end()]
            if sentence.strip():
                _append_bounded_output(merged_sentences, sentence)
            sentence_start = sentence_match.end()
        trailing_sentence = text[sentence_start:]
        if trailing_sentence.strip():
            _append_bounded_output(merged_sentences, trailing_sentence)

        chunks = []
        current_chunk = ""
        current_tokens = 0
        overlap_buffer = []  # Sentences to carry over for overlap
        overlap_buffer_tokens = 0

        for sentence in merged_sentences:
            sentence_tokens = token_counter.count_tokens(sentence)

            # If single sentence exceeds limit, split by words
            if sentence_tokens > token_limit:
                # First, flush current chunk
                if current_chunk.strip():
                    _append_bounded_output(chunks, current_chunk.strip())
                    current_chunk = ""
                    current_tokens = 0

                # Split long sentence by words
                word_chunk = ""
                word_tokens = 0

                for word_match in re.finditer(r"\S+", sentence):
                    word = word_match.group(0)
                    word_token_count = token_counter.count_tokens(word + " ")
                    if word_tokens + word_token_count > token_limit:
                        if word_chunk.strip():
                            _append_bounded_output(chunks, word_chunk.strip())
                        word_chunk = word + " "
                        word_tokens = word_token_count
                    else:
                        word_chunk += word + " "
                        word_tokens += word_token_count

                if word_chunk.strip():
                    current_chunk = word_chunk
                    current_tokens = token_counter.count_tokens(current_chunk)

                overlap_buffer = []
                overlap_buffer_tokens = 0
                continue

            # Check if adding sentence exceeds limit
            if current_tokens + sentence_tokens > token_limit:
                # Flush current chunk
                if current_chunk.strip():
                    _append_bounded_output(chunks, current_chunk.strip())

                # Start new chunk - only include overlap if it fits with new sentence
                if overlap_buffer and overlap_buffer_tokens + sentence_tokens <= token_limit:
                    current_chunk = "".join(overlap_buffer) + sentence
                    current_tokens = overlap_buffer_tokens + sentence_tokens
                else:
                    # Overlap would exceed limit, start fresh with just the sentence
                    current_chunk = sentence
                    current_tokens = sentence_tokens

                # Reset overlap buffer
                overlap_buffer = [sentence]
                overlap_buffer_tokens = sentence_tokens
            else:
                # Add sentence to current chunk
                current_chunk += sentence
                current_tokens += sentence_tokens

                # Update overlap buffer
                overlap_buffer.append(sentence)
                overlap_buffer_tokens += sentence_tokens

                # Trim overlap buffer to target size
                while overlap_buffer_tokens > overlap_tokens and len(overlap_buffer) > 1:
                    removed = overlap_buffer.pop(0)
                    overlap_buffer_tokens -= token_counter.count_tokens(removed)

        # Don't forget the last chunk
        if current_chunk.strip():
            _append_bounded_output(chunks, current_chunk.strip())

        # Safety pass: enforce strict token limits
        def _hard_split_by_tokens(t: str) -> list[str]:
            if not t.strip():
                return []
            out: list[str] = []
            buf = ""
            buf_tokens = 0
            found_word = False
            for word_match in re.finditer(r"\S+", t):
                found_word = True
                w = word_match.group(0)
                w_tokens = token_counter.count_tokens(w + " ")
                if buf_tokens + w_tokens > token_limit:
                    if buf.strip():
                        _append_bounded_output(out, buf.strip())
                    buf = w + " "
                    buf_tokens = w_tokens
                else:
                    buf += w + " "
                    buf_tokens += w_tokens
            if buf.strip():
                _append_bounded_output(out, buf.strip())
            return out if found_word else [t.strip()]

        final: list[str] = []
        for ch in chunks:
            if token_counter.count_tokens(ch) <= token_limit:
                _append_bounded_output(final, ch)
            else:
                _extend_bounded_output(final, _hard_split_by_tokens(ch))

        return final

    def _split_by_tokens_fixed(
        self,
        text: str,
        token_limit: int,
        overlap_tokens: int = 0,
    ) -> list[str]:
        """
        Split text into fixed-size token windows (no sentence boundaries).

        This is intended for strict fixed-size chunking where each chunk
        targets the same token window size. Overlap is optional and
        expressed in tokens.
        """
        if not text or token_limit <= 0:
            return []

        token_counter = get_token_counter()
        _require_token_window_input_budget(text, token_limit, overlap_tokens)
        if token_counter.encoder:
            return _split_encoded_token_windows(
                text, token_limit, overlap_tokens, token_counter.encoder
            )

        # Cap overlap to ensure forward progress.
        overlap_tokens = 0 if token_limit <= 1 else max(0, min(overlap_tokens, token_limit - 1))

        chunks: list[str] = []
        word_buffer: deque[tuple[str, int]] = deque()
        buffer_tokens = 0
        for word_match in re.finditer(r"\S+", text):
            word = word_match.group(0)
            word_tokens = token_counter.count_tokens(word + " ")

            if word_tokens > token_limit:
                if word_buffer:
                    _append_bounded_output(
                        chunks,
                        " ".join(value for value, _count in word_buffer),
                    )
                    word_buffer.clear()
                    buffer_tokens = 0
                _append_bounded_output(chunks, word)
                continue

            if word_buffer and buffer_tokens + word_tokens > token_limit:
                _append_bounded_output(
                    chunks,
                    " ".join(value for value, _count in word_buffer),
                )
                retained: deque[tuple[str, int]] = deque()
                retained_tokens = 0
                if overlap_tokens > 0:
                    for value, count in reversed(word_buffer):
                        retained.appendleft((value, count))
                        retained_tokens += count
                        if retained_tokens >= overlap_tokens:
                            break
                word_buffer = retained
                buffer_tokens = retained_tokens
                while word_buffer and buffer_tokens + word_tokens > token_limit:
                    _value, removed_tokens = word_buffer.popleft()
                    buffer_tokens -= removed_tokens

            word_buffer.append((word, word_tokens))
            buffer_tokens += word_tokens

        if word_buffer:
            _append_bounded_output(
                chunks,
                " ".join(value for value, _count in word_buffer),
            )

        return chunks


class FixedSizeChunker(BaseChunker):
    """
    Fixed size chunking with overlap.

    Best Practice (2025):
    - When use_token_count=True, uses strict token windows
    - Target chunk size: configurable by token_limit
    - Overlap: optional (0 by default when token-based)
    """

    def chunk(self, text: str) -> list[Chunk]:
        if not text:
            return []

        # Use token-based splitting when enabled (recommended for production)
        if self.config.use_token_count:
            overlap_tokens = max(int(self.config.chunk_overlap), 0)
            chunk_texts = self._split_by_tokens_fixed(
                text, token_limit=self.config.token_limit, overlap_tokens=overlap_tokens
            )
        else:
            # Fallback to character-based splitting
            chunk_texts = self._split_with_overlap(
                text, self.config.chunk_size, self.config.chunk_overlap
            )

        return [self._create_chunk(t, i) for i, t in enumerate(chunk_texts) if t.strip()]


class ParagraphChunker(BaseChunker):
    """Split by paragraphs, merging small ones"""

    def chunk(self, text: str) -> list[Chunk]:
        if not text:
            return []

        # Split by double newlines (paragraphs)
        paragraph_pattern = r"\n\s*\n"
        _require_regex_split_budget(text, paragraph_pattern)
        paragraphs = re.split(paragraph_pattern, text)
        paragraphs = [p.strip() for p in paragraphs if p.strip()]

        chunks = []
        current = ""
        index = 0

        # Use token-based limits when enabled
        if self.config.use_token_count:
            token_counter = get_token_counter()
            token_limit = self.config.token_limit
            overlap_tokens = max(int(token_limit * 0.15), 10)

            current_tokens = 0
            for para in paragraphs:
                para_tokens = token_counter.count_tokens(para)

                if current_tokens + para_tokens <= token_limit:
                    current = f"{current}\n\n{para}" if current else para
                    current_tokens += para_tokens
                else:
                    if current:
                        chunks.append(self._create_chunk(current, index))
                        index += 1

                    if para_tokens > token_limit:
                        # Split large paragraph by tokens
                        sub_chunks = self._split_by_tokens(
                            para, token_limit=token_limit, overlap_tokens=overlap_tokens
                        )
                        for sub in sub_chunks:
                            chunks.append(self._create_chunk(sub, index))
                            index += 1
                        current = ""
                        current_tokens = 0
                    else:
                        current = para
                        current_tokens = para_tokens

            if current:
                chunks.append(self._create_chunk(current, index))
        else:
            # Character-based chunking (fallback)
            for para in paragraphs:
                if len(current) + len(para) + 2 <= self.config.chunk_size:
                    current = f"{current}\n\n{para}" if current else para
                else:
                    if current:
                        chunks.append(self._create_chunk(current, index))
                        index += 1

                    if len(para) > self.config.chunk_size:
                        # Split large paragraph
                        sub_chunks = self._split_with_overlap(
                            para, self.config.chunk_size, self.config.chunk_overlap
                        )
                        for sub in sub_chunks:
                            chunks.append(self._create_chunk(sub, index))
                            index += 1
                        current = ""
                    else:
                        current = para

            if current:
                chunks.append(self._create_chunk(current, index))

        return chunks


class PageChunker(BaseChunker):
    """Split by page markers"""

    def chunk(self, text: str) -> list[Chunk]:
        if not text:
            return []

        # Use page marker (form feed or custom)
        marker = self.config.page_marker
        if marker not in {r"\f", "\x0c"}:
            raise ValidationFailedError("custom page markers are disabled")
        _require_literal_split_budget(text, "\x0c")
        pages = text.split("\x0c")

        chunks = []
        for i, page in enumerate(pages):
            page = page.strip()
            if not page:
                continue

            # Use token-based limits when enabled
            if self.config.use_token_count:
                token_counter = get_token_counter()
                page_tokens = token_counter.count_tokens(page)
                token_limit = self.config.token_limit

                if page_tokens <= token_limit:
                    chunks.append(self._create_chunk(page, i, {"page": i + 1}))
                else:
                    # Split large pages by tokens
                    overlap_tokens = max(int(token_limit * 0.15), 10)
                    sub_chunks = self._split_by_tokens(
                        page, token_limit=token_limit, overlap_tokens=overlap_tokens
                    )
                    for j, sub in enumerate(sub_chunks):
                        chunks.append(
                            self._create_chunk(
                                sub, len(chunks), {"page": i + 1, "sub_chunk": j + 1}
                            )
                        )
            else:
                # Character-based chunking (fallback)
                if len(page) <= self.config.chunk_size:
                    chunks.append(self._create_chunk(page, i, {"page": i + 1}))
                else:
                    # Split large pages
                    sub_chunks = self._split_with_overlap(
                        page, self.config.chunk_size, self.config.chunk_overlap
                    )
                    for j, sub in enumerate(sub_chunks):
                        chunks.append(
                            self._create_chunk(
                                sub, len(chunks), {"page": i + 1, "sub_chunk": j + 1}
                            )
                        )

        return chunks


class HeadingChunker(BaseChunker):
    """Split by document headings/sections with recursive fallback"""

    def chunk(self, text: str) -> list[Chunk]:
        if not text:
            return []

        # Compile heading patterns — avoid matching numbered list items
        patterns = self.config.heading_patterns or [
            r"^#{1,6}\s+.+$",  # Markdown headings
            r"^第[一二三四五六七八九十\d]+[章节条款]",  # Chinese chapter markers
            r"^[A-Z][A-Z \t]{4,}:?$",  # ALL CAPS headings (5+ chars)
        ]

        combined_pattern = "|".join(f"({p})" for p in patterns)

        # Find all headings and their positions
        sections = []
        current_heading = None
        current_content = []

        _require_literal_split_budget(text, "\n")
        for line in text.split("\n"):
            is_heading = bool(re.match(combined_pattern, line, re.MULTILINE))

            if is_heading:
                if current_heading is not None or current_content:
                    sections.append(
                        {"heading": current_heading, "content": "\n".join(current_content)}
                    )
                current_heading = line
                current_content = []
            else:
                current_content.append(line)

        # Add last section
        if current_heading is not None or current_content:
            sections.append({"heading": current_heading, "content": "\n".join(current_content)})

        # Create chunks from sections
        chunks = []

        # Use token-based limits when enabled
        if self.config.use_token_count:
            token_counter = get_token_counter()
            token_limit = self.config.token_limit
            overlap_tokens = max(int(token_limit * 0.15), 10)

            for _i, section in enumerate(sections):
                section_text = section["content"].strip()
                if section["heading"]:
                    section_text = f"{section['heading']}\n\n{section_text}"

                if not section_text.strip():
                    continue

                section_tokens = token_counter.count_tokens(section_text)

                if section_tokens <= token_limit:
                    chunks.append(
                        self._create_chunk(
                            section_text, len(chunks), {"heading": section["heading"]}
                        )
                    )
                else:
                    # Split large sections by tokens
                    sub_chunks = self._split_by_tokens(
                        section_text, token_limit=token_limit, overlap_tokens=overlap_tokens
                    )
                    for j, sub in enumerate(sub_chunks):
                        chunks.append(
                            self._create_chunk(
                                sub,
                                len(chunks),
                                {"heading": section["heading"], "sub_chunk": j + 1},
                            )
                        )
        else:
            # Character-based chunking (fallback)
            recursive_chunker = RecursiveChunker(self.config)

            for _i, section in enumerate(sections):
                section_text = section["content"].strip()
                if section["heading"]:
                    section_text = f"{section['heading']}\n\n{section_text}"

                if not section_text.strip():
                    continue

                if len(section_text) <= self.config.chunk_size:
                    chunks.append(
                        self._create_chunk(
                            section_text, len(chunks), {"heading": section["heading"]}
                        )
                    )
                else:
                    # Split large sections using RecursiveChunker for better semantic preservation
                    sub_chunks = recursive_chunker.chunk(section_text)
                    for sub in sub_chunks:
                        # Update metadata with heading info
                        sub_meta = sub.metadata.copy()
                        sub_meta["heading"] = section["heading"]

                        chunks.append(self._create_chunk(sub.text, len(chunks), sub_meta))

        return chunks


class RegexChunker(BaseChunker):
    """Split by custom regex pattern"""

    def chunk(self, text: str) -> list[Chunk]:
        if not text:
            return []

        pattern = self.config.regex_pattern
        if pattern:
            raise ValidationFailedError("custom regex chunking is disabled")
        raise ValidationFailedError("regex chunking is disabled")


class SeparatorChunker(BaseChunker):
    """Split by custom separators with recursive fallback"""

    def chunk(self, text: str) -> list[Chunk]:
        if not text:
            return []

        separators = self.config.separators or ["\n\n", "\n", " "]
        # Use primary separator first
        sep = self.config.primary_separator or separators[0]
        try:
            _require_literal_split_budget(text, sep)
            parts = text.split(sep)
        except ValidationFailedError:
            raise
        except Exception:
            return RecursiveChunker(self.config).chunk(text)

        chunks = []

        # Use token-based limits when enabled
        if self.config.use_token_count:
            token_counter = get_token_counter()
            token_limit = self.config.token_limit
            overlap_tokens = max(int(token_limit * 0.15), 10)

            for part in parts:
                part = part.strip()
                if not part:
                    continue

                part_tokens = token_counter.count_tokens(part)

                if part_tokens <= token_limit:
                    chunks.append(self._create_chunk(part, len(chunks)))
                else:
                    # Split large parts by tokens
                    sub_chunks = self._split_by_tokens(
                        part, token_limit=token_limit, overlap_tokens=overlap_tokens
                    )
                    for _j, sub in enumerate(sub_chunks):
                        chunks.append(self._create_chunk(sub, len(chunks)))
        else:
            # Character-based chunking (fallback)
            recursive_chunker = RecursiveChunker(self.config)

            for part in parts:
                part = part.strip()
                if not part:
                    continue

                if len(part) <= self.config.chunk_size:
                    chunks.append(self._create_chunk(part, len(chunks)))
                else:
                    sub_chunks = recursive_chunker.chunk(part)
                    for sub in sub_chunks:
                        sub.index = len(chunks)
                        _append_bounded_output(chunks, sub)

        return chunks


class RecursiveChunker(BaseChunker):
    """Recursive character text splitter - splits hierarchically"""

    def chunk(self, text: str) -> list[Chunk]:
        if not text:
            return []

        separators = self.config.separators or ["\n\n", "\n", "。", ".", " ", ""]

        # Token-aware splitting for strict chunk control
        if self.config.use_token_count:
            return self._recursive_split_tokens(text, separators, 0)
        return self._recursive_split(text, separators, 0)

    def _recursive_split(self, text: str, separators: list[str], depth: int) -> list[Chunk]:
        """Recursively split text using separator hierarchy"""
        if not text.strip():
            return []

        # Safety limit: prevent infinite recursion
        MAX_RECURSION_DEPTH = 20
        if depth > MAX_RECURSION_DEPTH:
            logger.warning(
                f"Recursive chunking depth limit ({MAX_RECURSION_DEPTH}) reached, falling back to fixed-size split"
            )
            return [
                self._create_chunk(t, i)
                for i, t in enumerate(
                    self._split_with_overlap(
                        text, self.config.chunk_size, self.config.chunk_overlap
                    )
                )
            ]

        # Base case: text fits in chunk size
        if len(text) <= self.config.chunk_size:
            return [self._create_chunk(text, 0)]

        # No more separators: fall back to fixed size split
        if not separators:
            return [
                self._create_chunk(t, i)
                for i, t in enumerate(
                    self._split_with_overlap(
                        text, self.config.chunk_size, self.config.chunk_overlap
                    )
                )
            ]

        sep = separators[0]
        remaining_seps = separators[1:]

        # Special case for empty separator (char split)
        if sep == "":
            return [
                self._create_chunk(t, i)
                for i, t in enumerate(
                    self._split_with_overlap(
                        text, self.config.chunk_size, self.config.chunk_overlap
                    )
                )
            ]

        # Try splitting with current separator
        _require_literal_split_budget(text, sep)
        parts = text.split(sep)

        # If this separator doesn't actually split (only 1 part), try next
        if len(parts) <= 1:
            return self._recursive_split(text, remaining_seps, depth + 1)

        # Re-assemble parts into chunks within size limits
        chunks = []
        current_chunk_parts = []
        current_len = 0

        for part in parts:
            part_len = len(part)
            sep_len = len(sep)

            # If single part is too big, it needs to be processed recursively
            if part_len > self.config.chunk_size:
                # First, flush current buffer if any
                if current_chunk_parts:
                    completed_text = sep.join(current_chunk_parts)
                    _append_bounded_output(
                        chunks, self._create_chunk(completed_text, len(chunks))
                    )
                    current_chunk_parts = []
                    current_len = 0

                # Recursively split this large part
                sub_chunks = self._recursive_split(part, remaining_seps, depth + 1)
                for sub in sub_chunks:
                    sub.index = len(chunks)
                    _append_bounded_output(chunks, sub)

            else:
                # If adding this part exceeds size, flush buffer
                if current_len + sep_len + part_len > self.config.chunk_size:
                    if current_chunk_parts:
                        completed_text = sep.join(current_chunk_parts)
                        _append_bounded_output(
                            chunks, self._create_chunk(completed_text, len(chunks))
                        )
                    current_chunk_parts = [part]
                    current_len = part_len
                else:
                    if current_chunk_parts:
                        current_len += sep_len
                    current_chunk_parts.append(part)
                    current_len += part_len

        # Flush final buffer
        if current_chunk_parts:
            completed_text = sep.join(current_chunk_parts)
            _append_bounded_output(chunks, self._create_chunk(completed_text, len(chunks)))

        return chunks

    def _recursive_split_tokens(
        self,
        text: str,
        separators: list[str],
        depth: int,
    ) -> list[Chunk]:
        """Recursively split text using token limits while preserving separators."""
        if not text.strip():
            return []

        token_counter = get_token_counter()
        token_limit = int(self.config.token_limit or 0)
        if token_limit <= 0:
            return [self._create_chunk(text, 0)]

        # Apply min/max token constraints when configured (all modes, not just FIXED_SIZE)
        min_tokens = self.config.min_chunk_tokens
        max_tokens = self.config.max_chunk_tokens

        # Safety limit: prevent infinite recursion
        MAX_RECURSION_DEPTH = 20
        if depth > MAX_RECURSION_DEPTH:
            logger.warning(
                f"Token-based recursive chunking depth limit ({MAX_RECURSION_DEPTH}) reached, "
                "falling back to token split"
            )
            return [
                self._create_chunk(t, i)
                for i, t in enumerate(self._split_by_limits(text, token_limit))
            ]

        # Check if text should be split
        _require_token_window_input_budget(text, token_limit)
        total_tokens = token_counter.count_tokens(text)
        text_len = len(text)

        # Return as single chunk ONLY if BOTH conditions are met:
        # 1. Token count <= token_limit
        # 2. Character length <= chunk_size
        within_token_limit = total_tokens <= token_limit
        within_char_limit = text_len <= self.config.chunk_size

        if within_token_limit and within_char_limit:
            # Both limits satisfied - return as single chunk
            return [self._create_chunk(text, 0)]

        # If text exceeds char limit but is within token limit, we need to split
        # Continue with the separator-based splitting below

        # Special case: very short text that doesn't need splitting
        if (
            min_tokens is not None
            and total_tokens < min_tokens
            and text_len < self.config.chunk_size * 2
        ):
            return [self._create_chunk(text, 0)]

        # No more separators: fall back to token split
        if not separators:
            return [
                self._create_chunk(t, i)
                for i, t in enumerate(self._split_by_limits(text, token_limit))
            ]

        sep = separators[0]
        remaining_seps = separators[1:]

        if sep == "":
            return [
                self._create_chunk(t, i)
                for i, t in enumerate(self._split_by_limits(text, token_limit))
            ]

        _require_literal_split_budget(text, sep)
        parts = text.split(sep)
        if len(parts) <= 1:
            return self._recursive_split_tokens(text, remaining_seps, depth + 1)

        chunks: list[Chunk] = []
        current_parts: list[str] = []
        current_tokens = 0
        current_chars = 0
        sep_tokens = token_counter.count_tokens(sep) if sep else 0

        for part in parts:
            part = part.strip()
            if not part:
                continue

            part_tokens = token_counter.count_tokens(part)

            # If single part is too large by token or char constraints, split recursively
            part_too_large = part_tokens > token_limit or len(part) > self.config.chunk_size
            if part_too_large:
                if current_parts:
                    completed_text = sep.join(current_parts)
                    _append_bounded_output(
                        chunks, self._create_chunk(completed_text, len(chunks))
                    )
                    current_parts = []
                    current_tokens = 0
                    current_chars = 0
                sub_chunks = self._recursive_split_tokens(part, remaining_seps, depth + 1)
                for sub in sub_chunks:
                    sub.index = len(chunks)
                    _append_bounded_output(chunks, sub)
                continue

            # Check if adding this part exceeds limit
            projected = current_tokens + part_tokens
            projected_chars = current_chars + len(part)
            if current_parts:
                projected += sep_tokens
                projected_chars += len(sep)

            if projected > token_limit or projected_chars > self.config.chunk_size:
                if current_parts:
                    completed_text = sep.join(current_parts)
                    _append_bounded_output(
                        chunks, self._create_chunk(completed_text, len(chunks))
                    )
                current_parts = [part]
                current_tokens = part_tokens
                current_chars = len(part)
            else:
                if current_parts:
                    current_tokens += sep_tokens
                    current_chars += len(sep)
                current_parts.append(part)
                current_tokens += part_tokens
                current_chars += len(part)

        # Handle remaining parts - apply min/max constraints
        if current_parts:
            completed_text = sep.join(current_parts)
            completed_tokens = token_counter.count_tokens(completed_text)

            # If last chunk is too small and we have previous chunks, try to merge
            if (
                min_tokens is not None
                and max_tokens is not None
                and completed_tokens < min_tokens
                and len(chunks) >= 1
            ):
                prev_chunk = chunks[-1]
                token_counter.count_tokens(prev_chunk.text)
                merged_text = prev_chunk.text + sep + completed_text
                merged_tokens = token_counter.count_tokens(merged_text)

                # Only merge if combined doesn't exceed max_tokens
                if merged_tokens <= max_tokens:
                    # Merge with previous chunk
                    chunks[-1] = self._create_chunk(merged_text, len(chunks) - 1)
                    chunks[-1].metadata = prev_chunk.metadata
                else:
                    # Can't merge, add as separate chunk (even if small)
                    _append_bounded_output(
                        chunks, self._create_chunk(completed_text, len(chunks))
                    )
            else:
                # Normal size or only chunk, add as-is
                _append_bounded_output(
                    chunks, self._create_chunk(completed_text, len(chunks))
                )

        return chunks

    def _split_by_limits(self, text: str, token_limit: int) -> list[str]:
        """Split text by token limits and enforce char hard-cap as a safety net."""
        token_chunks = self._split_by_tokens(text, token_limit)
        if not token_chunks:
            return []

        char_limit = max(int(self.config.chunk_size), 1)

        final_chunks: list[str] = []
        for chunk_text in token_chunks:
            if len(chunk_text) <= char_limit:
                if chunk_text.strip():
                    _append_bounded_output(final_chunks, chunk_text)
                continue

            for sub in self._split_with_overlap(
                chunk_text, char_limit, max(int(self.config.chunk_overlap), 0)
            ):
                if sub.strip():
                    _append_bounded_output(final_chunks, sub)

        return final_chunks


class HierarchicalChunker(BaseChunker):
    """
    Parent-child hierarchical chunking (Small-to-Large retrieval).

    Best for:
    - Complex Q&A needing both precision and broad context
    - Long documents where answers are specific but context matters
    - Enterprise use cases with strict token budgets

    Strategy:
    - Index small "child" chunks for precision retrieval
    - Keep larger "parent" chunks for context when needed
    - Children are retrieved first, parent provides context

    Token limits are used as targets; min/max enforcement is optional.
    """

    def chunk(self, text: str) -> list[Chunk]:
        if not text:
            return []

        token_counter = get_token_counter()

        parent_mode = str(self.config.parent_mode or "recursive").lower()
        use_fixed_window = parent_mode in {"fixed", "fixed_size", "window", "token"}

        # Token-based config for parents
        parent_token_limit = self.config.parent_token_limit
        if parent_token_limit is None:
            # Default: 4x child token limit (1600 if child is 400)
            parent_token_limit = max(self.config.token_limit * 4, 800)

        parent_min_tokens = None
        parent_max_tokens = None

        parent_config = ChunkingConfig(
            mode=ChunkingMode.FIXED_SIZE if use_fixed_window else ChunkingMode.RECURSIVE,
            chunk_size=self.config.parent_chunk_size,
            chunk_overlap=self.config.parent_overlap,
            min_chunk_size=self.config.min_chunk_size,
            use_token_count=True,  # FORCE token-based
            token_limit=parent_token_limit,
            min_chunk_tokens=parent_min_tokens,
            max_chunk_tokens=parent_max_tokens,
            separators=self.config.separators,
        )

        # Create parents using strict token windows if requested, else recursive splitter
        parent_chunker = (
            FixedSizeChunker(parent_config) if use_fixed_window else RecursiveChunker(parent_config)
        )
        parents = parent_chunker.chunk(text)

        # Post-process parents only if min/max constraints are configured
        parents = self._enforce_token_constraints(
            parents, parent_min_tokens, parent_max_tokens, token_counter
        )

        logger.debug(
            f"[HierarchicalChunker] Created {len(parents)} parents with "
            f"token_limit={parent_token_limit}, min={parent_min_tokens}, max={parent_max_tokens}"
        )

        # Token-based config for children
        child_token_limit = self.config.child_token_limit or self.config.token_limit
        child_min_tokens = None
        child_max_tokens = None

        child_config = ChunkingConfig(
            mode=ChunkingMode.FIXED_SIZE if use_fixed_window else ChunkingMode.RECURSIVE,
            chunk_size=self.config.child_chunk_size,
            chunk_overlap=self.config.child_overlap,
            min_chunk_size=max(50, self.config.min_chunk_size // 2),
            use_token_count=True,  # FORCE token-based
            token_limit=child_token_limit,
            min_chunk_tokens=child_min_tokens,
            max_chunk_tokens=child_max_tokens,
            separators=self.config.separators,
        )
        child_chunker = (
            FixedSizeChunker(child_config) if use_fixed_window else RecursiveChunker(child_config)
        )
        child_config._runtime_chunk_count = len(parents)

        # Create hierarchical structure
        all_chunks = []

        for parent_idx, parent in enumerate(parents):
            parent.index = parent_idx
            parent.metadata["is_parent"] = True
            parent.metadata["chunk_type"] = "parent"
            parent.metadata["parent_index"] = parent_idx
            _append_bounded_output(all_chunks, parent)

            # Create children from parent text - RE-SPLIT from parent content
            children = child_chunker.chunk(parent.text)

            # Post-process children only if min/max constraints are configured
            children = self._enforce_token_constraints(
                children, child_min_tokens, child_max_tokens, token_counter
            )

            logger.debug(
                f"[HierarchicalChunker] Parent {parent_idx}: "
                f"{parent.token_count} tokens -> {len(children)} children "
                f"(limit={child_token_limit}, min={child_min_tokens}, max={child_max_tokens})"
            )

            for child_idx, child in enumerate(children):
                child.index = len(all_chunks) + child_idx  # Unique index across all chunks
                child.metadata["is_child"] = True
                child.metadata["chunk_type"] = "child"
                child.metadata["parent_index"] = parent_idx
                child.metadata["parent_hash"] = parent.hash_id
                child.metadata["child_position"] = child_idx
                child.parent_id = parent.hash_id

                _append_bounded_output(parent.children, child)

        return all_chunks

    def _enforce_token_constraints(
        self,
        chunks: list[Chunk],
        min_tokens: int | None,
        max_tokens: int | None,
        token_counter: TokenCounter,
    ) -> list[Chunk]:
        """
        Post-process chunks to enforce min/max token constraints.

        - Merges chunks that are below min_tokens with neighbors
        - Only merges if combined chunk doesn't exceed max_tokens
        """
        if not chunks:
            return chunks
        if min_tokens is None or max_tokens is None:
            return chunks

        # Single chunk that's too small - keep it (document is short)
        if len(chunks) == 1:
            return chunks

        result: list[Chunk] = []

        for chunk in chunks:
            chunk_tokens = token_counter.count_tokens(chunk.text)

            # If chunk is too small and we have a previous chunk, try to merge
            if chunk_tokens < min_tokens and result:
                prev = result[-1]
                token_counter.count_tokens(prev.text)
                combined_text = prev.text + "\n\n" + chunk.text
                combined_tokens = token_counter.count_tokens(combined_text)

                # Only merge if combined doesn't exceed max
                if combined_tokens <= max_tokens:
                    # Merge with previous
                    result[-1] = Chunk(
                        text=combined_text,
                        index=prev.index,
                        metadata={**prev.metadata, **chunk.metadata},
                        parent_id=prev.parent_id,
                        content_type=prev.content_type,
                        associated_images=prev.associated_images + chunk.associated_images,
                    )
                    continue

            result.append(chunk)

        # Second pass: if first chunk is too small, try merging with second
        if len(result) >= 2:
            first_tokens = token_counter.count_tokens(result[0].text)
            if first_tokens < min_tokens:
                first, second = result[0], result[1]
                combined_text = first.text + "\n\n" + second.text
                combined_tokens = token_counter.count_tokens(combined_text)

                if combined_tokens <= max_tokens:
                    merged = Chunk(
                        text=combined_text,
                        index=0,
                        metadata={**first.metadata, **second.metadata},
                        parent_id=second.parent_id,
                        content_type=second.content_type,
                        associated_images=first.associated_images + second.associated_images,
                    )
                    result[1] = merged
                    result.pop(0)

        # Re-index
        for i, c in enumerate(result):
            c.index = i

        return result


class AutomaticChunker(BaseChunker):
    """
    Intelligent automatic chunking with content-aware strategy selection.

    Strategy Selection (Priority Order):
    1. Structured documents (headings) → HeadingChunker
    2. Long documents (>5000 chars) → HierarchicalChunker (parent-child)
    3. Documents with images → preserve image context
    4. Default → RecursiveChunker (85-90% recall, best general purpose)

    This follows industry best practices:
    - RecursiveCharacterTextSplitter for most documents
    - Parent-child for complex Q&A scenarios
    - Structure-aware for markdown/technical docs
    """

    # Image placeholder patterns
    IMAGE_PATTERNS = [
        r"\[Image\]",  # Our parser placeholder
        r"\[图片\]",  # Chinese placeholder
        r"!\[.*?\]\(.*?\)",  # Markdown images
        r"<img[^>]+>",  # HTML images
        r"\[IMAGE:.*?\]",  # Custom placeholder
    ]

    def _apply_auto_defaults(self) -> ChunkingConfig:
        """Apply parent-child defaults for automatic mode when no explicit config is provided."""
        cfg = self.config
        if cfg.mode != ChunkingMode.AUTOMATIC:
            return cfg

        defaults_match = (
            cfg.parent_chunk_size == 8000
            and cfg.child_chunk_size == 2000
            and cfg.parent_overlap == 400
            and cfg.child_overlap == 300
            and cfg.token_limit == 500
            and cfg.child_token_limit == 500
            and cfg.parent_token_limit == 1500
        )
        if not defaults_match:
            return cfg

        # Default to parent-child indexing with token-based sizes.
        cfg.use_token_count = True
        cfg.token_limit = 400
        cfg.child_token_limit = 400
        cfg.parent_token_limit = 1500
        cfg.child_overlap = 50
        cfg.parent_overlap = 50
        cfg.chunk_overlap = 50
        cfg.child_chunk_size = max(cfg.child_chunk_size, 1600)
        cfg.parent_chunk_size = max(cfg.parent_chunk_size, 6000)
        cfg.parent_mode = "fixed"
        return cfg

    def chunk(self, text: str) -> list[Chunk]:
        if not text:
            return []

        self._apply_auto_defaults()

        # Analyze document characteristics
        has_images = any(re.search(p, text) for p in self.IMAGE_PATTERNS)
        has_heading_structure = self._has_heading_structure(text)

        # Strategy 1: Handle documents with images
        if has_images and self.config.preserve_images:
            return self._chunk_with_image_awareness(text)

        # Strategy 2: Structured documents should preserve heading boundaries
        if has_heading_structure:
            return HeadingChunker(self.config).chunk(text)

        # Strategy 3: Long plain documents benefit from hierarchical chunking
        if len(text) > 5000:
            return HierarchicalChunker(self.config).chunk(text)

        # Strategy 4: General default
        return RecursiveChunker(self.config).chunk(text)

    def _has_heading_structure(self, text: str) -> bool:
        """Detect whether a document likely has meaningful heading structure."""
        patterns = self.config.heading_patterns or [
            r"^#{1,6}\s+.+$",
            r"^第[一二三四五六七八九十\d]+[章节条款]",
            r"^[A-Z][A-Z \t]{4,}:?$",
        ]
        _require_literal_split_budget(text, "\n")
        lines = [line.strip() for line in text.split("\n") if line.strip()]
        if not lines:
            return False

        heading_count = 0
        for line in lines:
            if any(re.match(pattern, line) for pattern in patterns):
                heading_count += 1
                if heading_count >= 2:
                    return True
        return False

    def _chunk_with_image_awareness(self, text: str) -> list[Chunk]:
        """
        Chunk text while preserving image context.

        Images should not be split from their surrounding context.
        We identify image placeholders and ensure they stay with
        adjacent text for proper retrieval.
        """
        # Find all image positions
        image_positions = []
        for pattern in self.IMAGE_PATTERNS:
            for match in re.finditer(pattern, text):
                _append_bounded_output(image_positions, (match.start(), match.end()))

        if not image_positions:
            return RecursiveChunker(self.config).chunk(text)

        # Sort by position
        image_positions.sort(key=lambda x: x[0])

        # Create chunks ensuring images stay with context
        chunks = []
        current_pos = 0
        context_size = self.config.image_context_chars

        for img_start, img_end in image_positions:
            # Get text before this image (but after previous chunk)
            pre_text = text[current_pos:img_start].strip()

            if pre_text and len(pre_text) > self.config.min_chunk_size:
                # Chunk the pre-image text
                pre_chunks = RecursiveChunker(self.config).chunk(pre_text)
                for c in pre_chunks:
                    c.index = len(chunks)
                    _append_bounded_output(chunks, c)

            # Create image chunk with surrounding context
            ctx_start = max(0, img_start - context_size)
            ctx_end = min(len(text), img_end + context_size)

            # Extend to sentence boundaries
            while ctx_start > 0 and text[ctx_start] not in ".。!！?？\n":
                ctx_start -= 1
            while ctx_end < len(text) and text[ctx_end - 1] not in ".。!！?？\n":
                ctx_end += 1

            image_chunk_text = text[ctx_start:ctx_end].strip()
            if image_chunk_text:
                chunk = self._create_chunk(
                    image_chunk_text,
                    len(chunks),
                    {"has_image": True, "chunk_type": "image_context"},
                )
                _append_bounded_output(chunks, chunk)

            current_pos = ctx_end

        # Handle remaining text after last image
        if current_pos < len(text):
            remaining = text[current_pos:].strip()
            if remaining and len(remaining) > self.config.min_chunk_size:
                remaining_chunks = RecursiveChunker(self.config).chunk(remaining)
                for c in remaining_chunks:
                    c.index = len(chunks)
                    _append_bounded_output(chunks, c)

        return chunks if chunks else RecursiveChunker(self.config).chunk(text)


def create_chunker(config: ChunkingConfig) -> BaseChunker:
    """Factory function to create appropriate chunker"""
    chunker_map = {
        ChunkingMode.AUTOMATIC: AutomaticChunker,
        ChunkingMode.FIXED_SIZE: FixedSizeChunker,
        ChunkingMode.PARAGRAPH: ParagraphChunker,
        ChunkingMode.PAGE: PageChunker,
        ChunkingMode.HEADING: HeadingChunker,
        ChunkingMode.REGEX: RegexChunker,
        ChunkingMode.SEPARATOR: SeparatorChunker,
        ChunkingMode.RECURSIVE: RecursiveChunker,
        ChunkingMode.HIERARCHICAL: HierarchicalChunker,
        ChunkingMode.QA: ParagraphChunker,
    }

    chunker_cls = chunker_map.get(config.mode, RecursiveChunker)
    return chunker_cls(config)


def process_document(
    text: str,
    config: ChunkingConfig,
    document_id: str | None = None,
) -> list[Chunk]:
    """
    Main entry point for document processing.

    1. Preprocess text
    2. Extract metadata (optional)
    3. Chunk text
    4. Add document-level metadata to chunks
    """
    validate_persisted_chunking_config(config.to_dict())
    config._runtime_chunk_count = 0
    if not text:
        return []

    # Preprocess
    processed_text = TextPreprocessor.preprocess(text, config)

    if not processed_text:
        return []

    # Extract metadata
    doc_metadata = {}
    if config.extract_metadata:
        doc_metadata = TextPreprocessor.extract_metadata(processed_text, config.metadata_fields)

    # VALIDATION LOG: Log config before chunking
    min_tokens_log = config.min_chunk_tokens if config.mode == ChunkingMode.FIXED_SIZE else None
    max_tokens_log = config.max_chunk_tokens if config.mode == ChunkingMode.FIXED_SIZE else None
    logger.info(
        f"[Chunking] Processing document {document_id or 'unknown'}: "
        f"mode={config.mode}, token_limit={config.token_limit}, "
        f"use_token_count={config.use_token_count}, "
        f"min_tokens={min_tokens_log}, max_tokens={max_tokens_log}"
    )

    # Chunk
    chunker = create_chunker(config)
    chunks = chunker.chunk(processed_text)
    if len(chunks) > MAX_CHUNK_OUTPUTS:
        raise ValidationFailedError(
            f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
        )

    # VALIDATION LOG: Log chunk statistics
    if chunks:
        token_counts = [c.token_count for c in chunks if c.token_count > 0]
        if token_counts:
            min_tok = min(token_counts)
            max_tok = max(token_counts)
            avg_tok = sum(token_counts) / len(token_counts)
            logger.info(
                f"[Chunking] Document {document_id or 'unknown'}: "
                f"generated {len(chunks)} chunks, "
                f"tokens min={min_tok}, max={max_tok}, avg={avg_tok:.1f}, "
                f"target={config.token_limit}"
            )
            # Warn if chunks are significantly different from target
            if (
                config.mode == ChunkingMode.FIXED_SIZE
                and config.min_chunk_tokens is not None
                and min_tok < config.min_chunk_tokens
            ):
                logger.warning(
                    f"[Chunking] Document {document_id or 'unknown'}: found "
                    f"{sum(1 for t in token_counts if t < config.min_chunk_tokens)} "
                    f"chunks below min_tokens ({config.min_chunk_tokens})"
                )
            if (
                config.mode == ChunkingMode.FIXED_SIZE
                and config.max_chunk_tokens is not None
                and max_tok > config.max_chunk_tokens
            ):
                logger.warning(
                    f"[Chunking] Document {document_id or 'unknown'}: found "
                    f"{sum(1 for t in token_counts if t > config.max_chunk_tokens)} "
                    f"chunks above max_tokens ({config.max_chunk_tokens})"
                )

    # NOTE: merge_small_chunks is NOT called here because hierarchical chunking
    # stores children inside parent.children. Merging must happen AFTER
    # flatten_chunks() to operate on the final leaf chunks. Callers should call:
    #   flat = flatten_chunks(process_document(...))
    #   flat = merge_small_chunks(flat, config.min_chunk_size, config.max_chunk_size)

    # Add document-level metadata
    for chunk in chunks:
        if document_id:
            chunk.metadata["document_id"] = document_id
        chunk.metadata.update(doc_metadata)

    return chunks


def validate_chunk_distribution(
    chunks: list[Chunk],
    target_tokens: int,
    tolerance: float = 0.1,
    min_tokens: int | None = None,
    max_tokens: int | None = None,
) -> dict[str, Any]:
    """
    Validate that chunks meet the token distribution requirements.

    Args:
        chunks: List of chunks to validate
        target_tokens: Target token count per chunk
        tolerance: Allowed tolerance (default 10%)
        min_tokens: Minimum allowed tokens (default: target * 0.8)
        max_tokens: Maximum allowed tokens (default: target * 1.1)

    Returns:
        Dict with validation results and statistics
    """
    if not chunks:
        return {
            "valid": True,
            "total_chunks": 0,
            "min_tokens": 0,
            "max_tokens": 0,
            "avg_tokens": 0,
            "violations": [],
        }

    token_counter = get_token_counter()

    # Calculate bounds
    _min_tokens = min_tokens or int(target_tokens * (1 - tolerance))
    _max_tokens = max_tokens or int(target_tokens * (1 + tolerance))

    token_counts = [token_counter.count_tokens(c.text) for c in chunks]

    stats = {
        "total_chunks": len(chunks),
        "min_tokens": min(token_counts),
        "max_tokens": max(token_counts),
        "avg_tokens": sum(token_counts) / len(token_counts),
        "target_tokens": target_tokens,
        "allowed_min": _min_tokens,
        "allowed_max": _max_tokens,
    }

    violations = []
    within_range_count = 0

    for i, (_chunk, tokens) in enumerate(zip(chunks, token_counts, strict=False)):
        # Check if within target ± tolerance
        lower_bound = int(target_tokens * (1 - tolerance))
        upper_bound = int(target_tokens * (1 + tolerance))

        if lower_bound <= tokens <= upper_bound:
            within_range_count += 1

        # Check hard limits
        if tokens < _min_tokens:
            violations.append(
                {
                    "index": i,
                    "type": "too_small",
                    "tokens": tokens,
                    "limit": _min_tokens,
                }
            )
        elif tokens > _max_tokens:
            violations.append(
                {
                    "index": i,
                    "type": "too_large",
                    "tokens": tokens,
                    "limit": _max_tokens,
                }
            )

    # Calculate percentage within target range
    within_range_pct = (within_range_count / len(chunks)) * 100

    stats["within_range_pct"] = within_range_pct
    stats["violations"] = violations
    stats["valid"] = len(violations) == 0

    return stats


def log_chunking_stats(
    chunks: list[Chunk],
    target_tokens: int,
    stage: str = "after_chunking",
    tolerance: float = 0.1,
) -> None:
    """
    Log chunking statistics and warnings for violations.

    Args:
        chunks: List of chunks to analyze
        target_tokens: Target token count per chunk
        stage: Identifier for the chunking stage (for logging context)
        tolerance: Allowed tolerance around target
    """
    if not chunks:
        logger.info(f"[{stage}] No chunks to analyze")
        return

    stats = validate_chunk_distribution(chunks, target_tokens, tolerance)

    # Log statistics
    logger.info(
        f"[{stage}] Chunk stats: "
        f"total={stats['total_chunks']}, "
        f"target={target_tokens}, "
        f"min={stats['min_tokens']:.0f}, "
        f"max={stats['max_tokens']:.0f}, "
        f"avg={stats['avg_tokens']:.1f}, "
        f"within_range={stats.get('within_range_pct', 0):.1f}%"
    )

    # Log violations as warnings
    for v in stats.get("violations", []):
        if v["type"] == "too_small":
            logger.warning(
                f"[{stage}] Chunk {v['index']} too small: {v['tokens']} tokens (min: {v['limit']})"
            )
        else:
            logger.warning(
                f"[{stage}] Chunk {v['index']} too large: {v['tokens']} tokens (max: {v['limit']})"
            )


def flatten_chunks(chunks: list[Chunk]) -> list[Chunk]:
    """
    Flatten hierarchical chunks to a single list.
    For hierarchical chunking, this returns only the leaf (child) chunks.
    """
    result = []
    for chunk in chunks:
        if chunk.children:
            # Has children, use children instead
            children = flatten_chunks(chunk.children)
            if len(result) + len(children) > MAX_CHUNK_OUTPUTS:
                raise ValidationFailedError(
                    f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
                )
            result.extend(children)
        else:
            if len(result) >= MAX_CHUNK_OUTPUTS:
                raise ValidationFailedError(
                    f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
                )
            result.append(chunk)
    return result


def _split_encoded_token_windows(
    text: str, token_limit: int, overlap_tokens: int, encoder: Any
) -> list[str]:
    """Use scalar-safe byte spans; overlap is a maximum, never a byte split."""
    _require_token_window_input_budget(text, token_limit, overlap_tokens)
    _require_whole_encoder_input_budget(text)
    raw = text.encode("utf-8")
    tokens = encoder.encode(text)
    overlap_tokens = max(0, min(overlap_tokens, token_limit - 1))
    step = token_limit - overlap_tokens
    if (len(tokens) + step - 1) // step > MAX_CHUNK_OUTPUTS:
        raise ValidationFailedError(
            f"chunking output exceeds the {MAX_CHUNK_OUTPUTS} chunk limit"
        )

    offsets = [0]  # Original token byte offsets estimate each window's budget.
    for token in tokens:
        token_bytes = encoder.decode_single_token_bytes(token)
        end = offsets[-1] + len(token_bytes)
        if raw[offsets[-1] : end] != token_bytes:
            raise ValidationFailedError("tokenizer bytes do not match source text")
        offsets.append(end)
    if offsets[-1] != len(raw):
        raise ValidationFailedError("tokenizer bytes do not match source text")

    chunks: list[str] = []
    start = previous_end = 0
    while start < len(raw):
        start_token = bisect_right(offsets, start) - 1
        end = offsets[min(start_token + token_limit, len(tokens))]
        while end > start and end < len(raw) and raw[end] & 0xC0 == 0x80:
            end -= 1
        while end > start:
            value = raw[start:end].decode("utf-8").strip()
            if len(encoder.encode(value)) <= token_limit:
                break
            end -= 1
            while end > start and raw[end] & 0xC0 == 0x80:
                end -= 1
        # A retained overlap must not prevent coverage of new source text.
        if end <= previous_end and start < previous_end:
            start = previous_end
            continue
        if end <= start:
            # A scalar can cross original token boundaries; try it independently.
            end = start + 1
            while end < len(raw) and raw[end] & 0xC0 == 0x80:
                end += 1
        value = raw[start:end].decode("utf-8").strip()
        if len(encoder.encode(value)) > token_limit:
            raise ValidationFailedError(
                "token limit cannot contain the next Unicode character; increase token_limit"
            )
        if value:
            _append_bounded_output(chunks, value)
        if end == len(raw):
            break
        previous_end = end
        next_start = min(end, max(start + 1, offsets[max(0, bisect_left(offsets, end) - overlap_tokens)]))
        while next_start < end and raw[next_start] & 0xC0 == 0x80:
            next_start += 1
        while next_start < end:
            if len(encoder.encode(raw[next_start:end].decode("utf-8"))) <= overlap_tokens:
                break
            next_start += 1
            while next_start < end and raw[next_start] & 0xC0 == 0x80:
                next_start += 1
        start = next_start
    return chunks


def _split_text_strict_by_tokens(text: str, max_tokens: int) -> list[str]:
    """Split text into chunks that strictly respect max_tokens (no overlap)."""
    if not text or max_tokens <= 0:
        return []
    token_counter = get_token_counter()
    if token_counter.encoder:
        return _split_encoded_token_windows(text, max_tokens, 0, token_counter.encoder)
    # Fallback: token-aware sentence splitter (best-effort without tiktoken)
    splitter = RecursiveChunker(ChunkingConfig(use_token_count=True, token_limit=max_tokens))
    return splitter._split_by_tokens(text, token_limit=max_tokens, overlap_tokens=0)


def enforce_token_limits(
    chunks: list[Chunk],
    max_tokens: int,
    *,
    min_tokens: int | None = None,
) -> list[Chunk]:
    """
    Enforce strict max token limits by splitting oversized chunks.

    This is a safety pass to ensure no chunk exceeds max_tokens.
    """
    if not chunks or not max_tokens:
        return chunks

    normalized: list[Chunk] = []
    for chunk in chunks:
        # Skip non-text chunks
        if chunk.content_type != ContentType.TEXT:
            _append_bounded_output(normalized, chunk)
            continue

        if chunk.token_count <= max_tokens:
            if min_tokens is not None and chunk.token_count < min_tokens:
                chunk.metadata["_too_small"] = True
            _append_bounded_output(normalized, chunk)
            continue

        # Split oversized chunk
        sub_texts = _split_text_strict_by_tokens(chunk.text, max_tokens)
        if not sub_texts:
            _append_bounded_output(normalized, chunk)
            continue

        for idx, sub in enumerate(sub_texts):
            meta = {**chunk.metadata, "_split_from": chunk.hash_id, "_split_index": idx}
            _append_bounded_output(
                normalized,
                Chunk(
                    text=sub,
                    metadata=meta,
                    parent_id=chunk.parent_id,
                    content_type=chunk.content_type,
                    associated_images=chunk.associated_images,
                    image_url=chunk.image_url,
                    image_filename=chunk.image_filename,
                    image_media_type=chunk.image_media_type,
                    vlm_description=chunk.vlm_description,
                ),
            )

    # Re-index after normalization
    for i, c in enumerate(normalized):
        c.index = i

    return normalized


def merge_small_chunks(
    chunks: list[Chunk],
    min_size: int,
    max_size: int,
    separator: str = "\n\n",
    *,
    min_tokens: int | None = None,
    max_tokens: int | None = None,
) -> list[Chunk]:
    """
    Merge undersized chunks with their neighbors to prevent fragment pollution.

    Multi-pass algorithm:
    1. First pass: merge small chunks into previous chunk
    2. Second pass: merge leading tiny chunks forward
    3. Final pass: iteratively merge remaining small chunks until stable

    This ensures no orphan tiny chunks remain.
    """
    if not chunks or min_size <= 0:
        return chunks

    merged: list[Chunk] = []

    def _within_limits(combined_len: int, combined_tokens: int) -> bool:
        """Allow bounded overflow when merging tiny chunks to avoid orphan fragments."""
        if max_tokens is None:
            return combined_len <= max_size
        if combined_tokens <= max_tokens:
            return combined_len <= max_size
        # Allow bounded overflow (up to 512 tokens) for tiny-chunk absorption
        hard_cap = max_tokens if max_tokens >= 512 else 512
        return combined_len <= (max_size + min_size) and combined_tokens <= hard_cap

    for chunk in chunks:
        text = chunk.text.strip()
        if not text:
            continue
        text_tokens = count_tokens(text)

        # Try to merge small chunk into previous
        is_too_small = (len(text) < min_size) or (
            min_tokens is not None and text_tokens < min_tokens
        )
        if is_too_small and merged:
            prev = merged[-1]
            combined = f"{prev.text}{separator}{text}"
            combined_tokens = count_tokens(combined)
            if _within_limits(len(combined), combined_tokens):
                combined_meta = {**chunk.metadata, **prev.metadata}
                merged[-1] = Chunk(
                    text=combined,
                    index=prev.index,
                    metadata=combined_meta,
                    parent_id=prev.parent_id,
                    content_type=prev.content_type,
                    associated_images=prev.associated_images + chunk.associated_images,
                )
                continue

        merged.append(chunk)

    # Second pass: merge any leading tiny chunk forward
    if len(merged) >= 2:
        first_tokens = count_tokens(merged[0].text)
        first_too_small = (len(merged[0].text) < min_size) or (
            min_tokens is not None and first_tokens < min_tokens
        )
    else:
        first_too_small = False

    if len(merged) >= 2 and first_too_small:
        first = merged[0]
        second = merged[1]
        combined = f"{first.text}{separator}{second.text}"
        combined_tokens = count_tokens(combined)
        if _within_limits(len(combined), combined_tokens):
            combined_meta = {**first.metadata, **second.metadata}
            merged[1] = Chunk(
                text=combined,
                index=second.index,
                metadata=combined_meta,
                parent_id=second.parent_id,
                content_type=second.content_type,
                associated_images=first.associated_images + second.associated_images,
            )
            merged.pop(0)

    # Third pass: iteratively merge remaining small chunks until stable
    # This handles cases where many consecutive small chunks exist
    changed = True
    max_iterations = 10  # Prevent infinite loops
    iteration = 0
    while changed and iteration < max_iterations:
        changed = False
        iteration += 1
        new_merged: list[Chunk] = []

        for chunk in merged:
            text = chunk.text.strip()

            text_tokens = count_tokens(text) if text else 0
            is_too_small = (len(text) < min_size) or (
                min_tokens is not None and text_tokens < min_tokens
            )

            # Try to merge small chunk into previous
            if is_too_small and new_merged:
                prev = new_merged[-1]
                combined = f"{prev.text}{separator}{text}"
                combined_tokens = count_tokens(combined)
                if _within_limits(len(combined), combined_tokens):
                    combined_meta = {**chunk.metadata, **prev.metadata}
                    new_merged[-1] = Chunk(
                        text=combined,
                        index=prev.index,
                        metadata=combined_meta,
                        parent_id=prev.parent_id,
                        content_type=prev.content_type,
                        associated_images=prev.associated_images + chunk.associated_images,
                    )
                    changed = True
                    continue

            new_merged.append(chunk)

        merged = new_merged

    # Re-index
    for i, c in enumerate(merged):
        c.index = i

    return merged


# Convenience functions
def chunk_text(
    text: str,
    chunk_size: int = 500,
    overlap: int | None = None,
    mode: str = "automatic",
    chunk_overlap: int | None = None,
    language: str | None = None,
) -> list[str]:
    """Simple interface to chunk text and return list of strings.

    Backward-compatible args:
    - ``chunk_overlap`` is accepted as an alias of ``overlap``
    - ``language`` triggers language-aware chunk size adjustment
    """
    effective_overlap = 50 if overlap is None else overlap
    if chunk_overlap is not None:
        effective_overlap = chunk_overlap

    effective_chunk_size = chunk_size
    if language:
        effective_chunk_size = get_chunk_size_for_language(language, chunk_size)

    config = ChunkingConfig(
        mode=ChunkingMode(mode)
        if mode in [m.value for m in ChunkingMode]
        else ChunkingMode.AUTOMATIC,
        chunk_size=effective_chunk_size,
        chunk_overlap=effective_overlap,
    )
    chunks = process_document(text, config)
    flat = flatten_chunks(chunks)
    flat = merge_small_chunks(
        flat,
        min_size=config.min_chunk_size,
        max_size=config.max_chunk_size,
        min_tokens=config.min_chunk_tokens if config.use_token_count else None,
        max_tokens=config.max_chunk_tokens if config.use_token_count else None,
    )
    return [c.text for c in flat]
