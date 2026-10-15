from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Any

# ============================================================
# Pre-processing Rules (Dify-style)
# ============================================================

PRE_PROCESSING_RULES = {
    "remove_extra_spaces": lambda text: re.sub(r"[ \t]+", " ", text),
    "remove_urls_emails": lambda text: _remove_urls_emails(text),
    "remove_stopwords": lambda text: text,  # Placeholder - would need language-specific stopwords
}


def _remove_urls_emails(text: str) -> str:
    """Remove URLs and email addresses from text."""
    # Remove URLs
    text = re.sub(r'https?://[^\s<>"{}|\\^`\[\]]+', "", text)
    text = re.sub(r'www\.[^\s<>"{}|\\^`\[\]]+', "", text)
    # Remove emails
    text = re.sub(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,}\b", "", text)
    return text


def normalize_text(text: str) -> str:
    """Normalize text by fixing line endings and collapsing blank lines."""
    text = (text or "").replace("\r\n", "\n").replace("\r", "\n")
    # collapse excessive blank lines
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text.strip()


def apply_pre_processing_rules(text: str, rules: list[dict[str, Any]]) -> str:
    """Apply pre-processing rules to text (Dify-style)."""
    for rule in rules:
        rule_id = rule.get("id", "")
        enabled = rule.get("enabled", True)
        if enabled and rule_id in PRE_PROCESSING_RULES:
            text = PRE_PROCESSING_RULES[rule_id](text)
    return text


def estimate_tokens(text: str) -> int:
    """Cheap token estimation for UI/progress only (not model-accurate)."""
    t = (text or "").strip()
    if not t:
        return 0
    # Count "words" for latin; fall back to char-based for CJK/others.
    words = re.findall(r"[A-Za-z0-9]+", t)
    if words:
        non_word_chars = max(len(t) - sum(len(w) for w in words), 0)
        return len(words) + max(non_word_chars // 4, 0)
    return max(len(t) // 2, 1)


def count_words(text: str) -> int:
    """Count words in text (handles CJK and Latin)."""
    if not text:
        return 0

    # Count Latin words
    latin_words = len(re.findall(r"[A-Za-z0-9]+", text))

    # Count CJK characters as individual words
    cjk_chars = len(re.findall(r"[\u4e00-\u9fff\u3400-\u4dbf\uF900-\uFAFF]", text))

    return latin_words + cjk_chars


# ============================================================
# Segmentation Configuration
# ============================================================


@dataclass
class SegmentationConfig:
    """Segmentation configuration - matches Dify's format"""

    separator: str = "\n"
    max_tokens: int = 500
    chunk_overlap: int = 50

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> SegmentationConfig:
        return cls(
            separator=data.get("separator", "\n"),
            max_tokens=data.get("max_tokens", 500),
            chunk_overlap=data.get("chunk_overlap", 50),
        )


@dataclass
class ProcessingConfig:
    """Complete processing configuration"""

    mode: str = "automatic"  # automatic|custom|hierarchical
    pre_processing_rules: list[dict[str, Any]] = None
    segmentation: SegmentationConfig = None
    parent_mode: str | None = None  # paragraph|full_doc (hierarchical only)
    child_chunk_size: int | None = None

    def __post_init__(self):
        if self.pre_processing_rules is None:
            self.pre_processing_rules = [
                {"id": "remove_extra_spaces", "enabled": True},
                {"id": "remove_urls_emails", "enabled": False},
            ]
        if self.segmentation is None:
            self.segmentation = SegmentationConfig()

    @classmethod
    def automatic(cls) -> ProcessingConfig:
        """Default automatic processing config"""
        return cls(mode="automatic")

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> ProcessingConfig:
        seg_data = data.get("segmentation", {})
        return cls(
            mode=data.get("mode", "automatic"),
            pre_processing_rules=data.get(
                "pre_processing_rules",
                [
                    {"id": "remove_extra_spaces", "enabled": True},
                    {"id": "remove_urls_emails", "enabled": False},
                ],
            ),
            segmentation=SegmentationConfig.from_dict(seg_data)
            if seg_data
            else SegmentationConfig(),
            parent_mode=data.get("parent_mode"),
            child_chunk_size=data.get("child_chunk_size"),
        )


# ============================================================
# Segmentation Functions
# ============================================================


def split_into_segments(
    text: str,
    max_chars: int = 1200,
    overlap_chars: int = 120,
    min_chars: int = 50,
) -> list[tuple[str, int]]:
    """Split text into overlapping chunks.

    Returns list of (chunk_text, token_count).
    """
    text = normalize_text(text)
    if not text:
        return []

    segments: list[tuple[str, int]] = []
    n = len(text)
    start = 0

    def _best_break(s: int, e: int) -> int:
        window = text[s:e]
        candidates = [
            window.rfind("\n\n"),
            window.rfind("\n"),
            window.rfind("。"),
            window.rfind("."),
            window.rfind("!"),
            window.rfind("?"),
            window.rfind("；"),
            window.rfind(";"),
            window.rfind(","),
            window.rfind("，"),
            window.rfind(" "),
        ]
        cut = max(candidates)
        if cut <= 0:
            return e
        # Avoid overly small chunks.
        abs_cut = s + cut + 1
        if abs_cut - s < min_chars:
            return e
        return abs_cut

    while start < n:
        end = min(start + max_chars, n)
        end = _best_break(start, end)
        chunk = text[start:end].strip()
        if chunk:
            segments.append((chunk, estimate_tokens(chunk)))
        if end >= n:
            break
        next_start = max(end - overlap_chars, 0)
        # Prevent non-progress loops when the chosen break is within the overlap window.
        if next_start <= start:
            next_start = end
        start = next_start

    return segments


def split_by_separator(
    text: str,
    separator: str = "\n",
    max_tokens: int = 500,
    chunk_overlap: int = 50,
) -> list[tuple[str, int]]:
    """Split text by separator with overlap (Dify custom mode style).

    Returns list of (chunk_text, token_count).
    """
    text = normalize_text(text)
    if not text:
        return []

    # Estimate chars per token (rough approximation)
    chars_per_token = 4  # Typical for English, adjust for CJK
    max_chars = max_tokens * chars_per_token
    overlap_chars = chunk_overlap * chars_per_token

    # First split by separator
    if separator == "\\n":
        separator = "\n"
    elif separator == "\\n\\n":
        separator = "\n\n"

    parts = text.split(separator)

    segments: list[tuple[str, int]] = []
    current_chunk = ""

    for part in parts:
        part = part.strip()
        if not part:
            continue

        # Check if adding this part would exceed max
        test_chunk = current_chunk + (separator if current_chunk else "") + part
        if len(test_chunk) > max_chars and current_chunk:
            # Save current chunk and start new one with overlap
            segments.append((current_chunk, estimate_tokens(current_chunk)))

            # Calculate overlap
            if overlap_chars > 0 and len(current_chunk) > overlap_chars:
                overlap_text = current_chunk[-overlap_chars:]
                current_chunk = overlap_text + separator + part
            else:
                current_chunk = part
        else:
            current_chunk = test_chunk

    # Don't forget the last chunk
    if current_chunk.strip():
        segments.append((current_chunk, estimate_tokens(current_chunk)))

    return segments


def split_hierarchical(
    text: str,
    parent_mode: str = "paragraph",
    max_tokens: int = 500,
    child_chunk_size: int = 100,
) -> list[dict[str, Any]]:
    """Split text hierarchically (parent-child chunks).

    Returns list of dicts with 'text', 'token_count', 'children'.
    """
    text = normalize_text(text)
    if not text:
        return []

    parents: list[dict[str, Any]] = []

    if parent_mode == "full_doc":
        # Whole document as single parent
        children = split_by_separator(text, "\n", child_chunk_size, 0)
        parents.append(
            {
                "text": text[:2000],  # Truncate for storage
                "token_count": estimate_tokens(text),
                "children": children,
            }
        )
    else:
        # Split by paragraphs (double newline)
        paragraphs = re.split(r"\n\n+", text)

        for para in paragraphs:
            para = para.strip()
            if not para:
                continue

            # Split paragraph into child chunks
            children = split_by_separator(para, "\n", child_chunk_size, 0)

            parents.append(
                {
                    "text": para,
                    "token_count": estimate_tokens(para),
                    "children": children,
                }
            )

    return parents


def process_document_text(
    text: str,
    config: ProcessingConfig | None = None,
) -> list[tuple[str, int]]:
    """Process document text with given configuration.

    Returns list of (chunk_text, token_count).
    """
    if config is None:
        config = ProcessingConfig.automatic()

    # Apply pre-processing rules
    text = normalize_text(text)
    text = apply_pre_processing_rules(text, config.pre_processing_rules)

    if not text:
        return []

    # Split based on mode
    if config.mode == "hierarchical":
        # For hierarchical, we return parent chunks
        # Child chunks are handled separately
        hierarchy = split_hierarchical(
            text,
            parent_mode=config.parent_mode or "paragraph",
            max_tokens=config.segmentation.max_tokens,
            child_chunk_size=config.child_chunk_size or 100,
        )
        return [(p["text"], p["token_count"]) for p in hierarchy]
    elif config.mode == "custom":
        return split_by_separator(
            text,
            separator=config.segmentation.separator,
            max_tokens=config.segmentation.max_tokens,
            chunk_overlap=config.segmentation.chunk_overlap,
        )
    else:
        # automatic mode
        chars_per_token = 4
        return split_into_segments(
            text,
            max_chars=config.segmentation.max_tokens * chars_per_token,
            overlap_chars=config.segmentation.chunk_overlap * chars_per_token,
        )
