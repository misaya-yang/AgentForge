from __future__ import annotations

import math
import re
from collections import Counter, defaultdict
from collections.abc import Sequence
from dataclasses import dataclass
from typing import Any

# =============================================================================
# Multilingual Tokenization Patterns (Latin, CJK, Arabic)
# =============================================================================

_RE_LATIN_WORD = re.compile(r"[A-Za-z0-9\-_]+")
_RE_CJK_RUN = re.compile(r"[\u4e00-\u9fff]+")
_RE_QUOTED = re.compile(r'["\']([^"\']+)["\']')

# Arabic Unicode ranges (comprehensive)
# - Basic Arabic: \u0600-\u06FF (letters, numbers, diacritics)
# - Arabic Supplement: \u0750-\u077F
# - Arabic Extended-A: \u08A0-\u08FF
# - Arabic Presentation Forms-A: \uFB50-\uFDFF
# - Arabic Presentation Forms-B: \uFE70-\uFEFF
_RE_ARABIC_RUN = re.compile(r"[\u0600-\u06ff\u0750-\u077f\u08a0-\u08ff\ufb50-\ufdff\ufe70-\ufeff]+")

# Arabic diacritics (tashkeel) - remove for tokenization
_RE_ARABIC_DIACRITICS = re.compile(r"[\u064b-\u0652\u0670]")

LEGACY_RRF_SEMANTICS = "legacy_unweighted_rrf_v1"
QDRANT_WEIGHTED_RRF_SEMANTICS = "qdrant_weighted_rrf_v1"

# Arabic stopwords (common words to filter out for better BM25)
ARABIC_STOPWORDS = frozenset(
    {
        # Definite article
        "ال",
        "الى",
        "إلى",
        "على",
        "عن",
        "من",
        "في",
        "إلي",
        "إن",
        "أن",
        # Prepositions and conjunctions
        "و",
        "ف",
        "ب",
        "ل",
        "ك",
        "لا",
        "ما",
        "مع",
        "أو",
        "ثم",
        "هذا",
        "هذه",
        "ذلك",
        "تلك",
        # Pronouns
        "هو",
        "هي",
        "هم",
        "نحن",
        "أنا",
        "أنت",
        "أنتم",
        "هن",
        # Question words
        "كيف",
        "متى",
        "أين",
        "لماذا",
        "ماذا",
        # Common verbs
        "كان",
        "يكون",
        "كانت",
        "كانوا",
        "يكونون",
        "قال",
        "قالوا",
        # Common Arabic words
        "عليه",
        "وسلم",
        "صلى",
        "الله",
        "رسول",
        "النبي",
        "عنه",
        "رضي",
    }
)

# English stopwords (minimal set for BM25)
ENGLISH_STOPWORDS = frozenset(
    {
        "the",
        "a",
        "an",
        "is",
        "are",
        "was",
        "were",
        "be",
        "been",
        "being",
        "have",
        "has",
        "had",
        "do",
        "does",
        "did",
        "will",
        "would",
        "could",
        "should",
        "may",
        "might",
        "must",
        "shall",
        "can",
        "need",
        "of",
        "in",
        "to",
        "for",
        "with",
        "on",
        "at",
        "by",
        "from",
        "as",
        "into",
        "through",
        "during",
        "before",
        "after",
        "above",
        "below",
        "between",
        "under",
        "and",
        "but",
        "or",
        "nor",
        "so",
        "yet",
        "both",
        "either",
        "neither",
        "not",
        "only",
        "own",
        "same",
        "than",
        "too",
        "very",
        "just",
        "also",
        "now",
        "here",
        "there",
        "when",
        "where",
        "why",
        "how",
        "all",
        "each",
        "every",
        "few",
        "more",
        "most",
        "other",
        "some",
        "such",
        "no",
        "any",
        "this",
        "that",
        "these",
        "those",
        "it",
        "its",
        "he",
        "she",
        "him",
        "her",
        "his",
        "hers",
        "they",
        "them",
        "their",
        "theirs",
        "we",
        "us",
        "our",
        "ours",
        "you",
        "your",
    }
)


def detect_language(text: str) -> str:
    """
    Detect primary language from text.

    Returns: "ar" | "en" | "zh" | "mixed"
    """
    if not text:
        return "en"

    sample = text[:2000]
    total = max(len(sample), 1)

    arabic_count = sum(len(m) for m in _RE_ARABIC_RUN.findall(sample))
    cjk_count = sum(len(m) for m in _RE_CJK_RUN.findall(sample))
    latin_count = sum(len(m) for m in _RE_LATIN_WORD.findall(sample))

    arabic_ratio = arabic_count / total
    cjk_ratio = cjk_count / total

    if arabic_ratio > 0.2:
        if latin_count > arabic_count * 0.3:
            return "mixed"
        return "ar"
    elif cjk_ratio > 0.1:
        return "zh"
    else:
        return "en"


def normalize_arabic(text: str) -> str:
    """
    Normalize Arabic text for better matching.

    - Remove diacritics (tashkeel)
    - Normalize alef variations (أ إ آ ا → ا)
    - Normalize taa marbuta (ة → ه)
    - Normalize yaa (ى → ي)
    """
    if not text:
        return ""

    # Remove diacritics
    result = _RE_ARABIC_DIACRITICS.sub("", text)

    # Normalize alef variations
    result = re.sub(r"[أإآٱ]", "ا", result)

    # Normalize taa marbuta
    result = result.replace("ة", "ه")

    # Normalize alef maksura
    result = result.replace("ى", "ي")

    return result


def tokenize_arabic(text: str, remove_stopwords: bool = True) -> list[str]:
    """
    Tokenize Arabic text with proper handling.

    Arabic morphology is complex:
    - Agglutinative: prefixes (و, ب, ف, ل, ك) + root + suffixes
    - Rich morphology based on trilateral roots

    Strategy:
    - Normalize text first
    - Split by whitespace and punctuation
    - Remove stopwords
    - Keep whole words (no stemming for accuracy)
    """
    if not text:
        return []

    # Normalize
    normalized = normalize_arabic(text.lower())

    # Extract Arabic words
    arabic_tokens = []
    for run in _RE_ARABIC_RUN.findall(normalized):
        # Split by spaces within the run (shouldn't happen, but defensive)
        words = run.split()
        for word in words:
            word = word.strip()
            if not word or len(word) < 2:
                continue

            # Remove stopwords if enabled
            if remove_stopwords and word in ARABIC_STOPWORDS:
                continue

            arabic_tokens.append(word)

            # For common prefixes, also add the word without prefix
            # This helps match "والصلاة" with "صلاة"
            if len(word) > 3:
                for prefix in ["و", "ف", "ب", "ل", "ك", "ال", "وال", "فال", "بال", "لل"]:
                    if word.startswith(prefix):
                        stem = word[len(prefix) :]
                        if len(stem) >= 2 and stem not in ARABIC_STOPWORDS:
                            arabic_tokens.append(stem)
                        break

    return arabic_tokens


def tokenize(
    text: str,
    keep_original: bool = False,
    remove_stopwords: bool = False,
    *,
    deduplicate: bool = True,
) -> list[str]:
    """
    Tokenize text for multilingual lexical search (BM25).

    Supports:
    - Latin: lowercased alnum words (including hyphenated like Q-Flow)
    - CJK: whole run, bigrams, and single characters
    - Arabic: normalized words with prefix handling
    - Quoted phrases: kept intact

    Args:
        text: Input text to tokenize
        keep_original: If True, include the original query as a token
        remove_stopwords: If True, filter out common stopwords
        deduplicate: Preserve legacy unique-token behavior when True. Set False
            only for an explicitly versioned term-frequency pipeline.

    Returns:
        List of tokens for BM25 scoring
    """
    t = (text or "").strip()
    if not t:
        return []

    tokens: list[str] = []

    # Keep the original query for exact matching
    if keep_original and len(t) > 1:
        tokens.append(t.lower())

    # Extract quoted phrases first
    for match in _RE_QUOTED.finditer(t):
        phrase = match.group(1).strip()
        if phrase:
            tokens.append(phrase.lower())

    # Remove quotes for further processing
    t_clean = _RE_QUOTED.sub(" ", t).lower()

    # Extract Latin words (including hyphenated like Q-Flow)
    latin_words = _RE_LATIN_WORD.findall(t_clean)
    for word in latin_words:
        if remove_stopwords and word.lower() in ENGLISH_STOPWORDS:
            continue
        tokens.append(word)

    # For CJK, include whole run + bigrams + single characters
    for run in _RE_CJK_RUN.findall(t_clean):
        # Add whole CJK phrase for exact matching
        if len(run) >= 2:
            tokens.append(run)
            # Add bigrams (e.g., 智能, 知识) for better matching
            for i in range(len(run) - 1):
                tokens.append(run[i : i + 2])
        # Also add individual characters
        tokens.extend(list(run))

    # Process Arabic text
    arabic_tokens = tokenize_arabic(t_clean, remove_stopwords=remove_stopwords)
    tokens.extend(arabic_tokens)

    if not deduplicate:
        return [token for token in tokens if token]

    # Deduplicate while preserving order for query/exact-match compatibility.
    seen = set()
    result = []
    for tok in tokens:
        if tok and tok not in seen:
            seen.add(tok)
            result.append(tok)

    return result


def bm25_scores(
    query_tokens: Sequence[str],
    documents_tokens: Sequence[Sequence[str]],
    *,
    k1: float = 1.2,
    b: float = 0.75,
) -> list[float]:
    """Calculate BM25 scores for documents against query tokens."""
    if not documents_tokens:
        return []
    if not query_tokens:
        return [0.0 for _ in documents_tokens]

    N = len(documents_tokens)
    df: Counter[str] = Counter()
    doc_lens: list[int] = []

    for doc in documents_tokens:
        terms = [t for t in doc if t]
        doc_lens.append(len(terms))
        for term in set(terms):
            df[term] += 1

    avgdl = sum(doc_lens) / max(N, 1)
    q_terms = list(Counter([t for t in query_tokens if t]).keys())

    scores: list[float] = []
    for doc in documents_tokens:
        tf = Counter([t for t in doc if t])
        dl = len(doc)
        score = 0.0
        for term in q_terms:
            f = float(tf.get(term, 0))
            if f <= 0:
                continue
            n = float(df.get(term, 0))
            idf = math.log(1.0 + (N - n + 0.5) / (n + 0.5))
            denom = f + k1 * (1.0 - b + b * (dl / max(avgdl, 1e-9)))
            score += idf * (f * (k1 + 1.0)) / max(denom, 1e-9)
        scores.append(float(score))
    return scores


def _token_hash(token: str) -> int:
    """Deterministic hash for a token string → sparse vector index.

    Uses FNV-1a 32-bit for speed and good distribution. The index space
    is large enough (~4B) that collisions are negligible for typical
    document sizes.
    """
    h = 0x811C9DC5  # FNV offset basis
    for b in token.encode("utf-8"):
        h ^= b
        h = (h * 0x01000193) & 0xFFFFFFFF  # FNV prime, keep 32-bit
    return h


def text_to_sparse_vector(
    text: str,
    remove_stopwords: bool = True,
) -> tuple[list[int], list[float]]:
    """Convert text to a sparse vector for Qdrant BM25 search.

    Tokenizes the text, computes term frequencies, and maps each token
    to a deterministic integer index via hashing.

    Returns:
        (indices, values) — suitable for ``qdrant_client.models.SparseVector``.
        Indices are hashed token IDs; values are term frequencies.
    """
    # Keep the existing term-presence sparse schema stable. True BM25 needs a
    # versioned shadow field because changing stored weights in-place would mix
    # incompatible vectors inside existing collections.
    tokens = tokenize(
        text,
        remove_stopwords=remove_stopwords,
        deduplicate=True,
    )
    if not tokens:
        return [], []

    tf: dict[int, float] = {}
    for token in tokens:
        idx = _token_hash(token)
        tf[idx] = tf.get(idx, 0.0) + 1.0

    indices = sorted(tf.keys())  # Qdrant requires sorted indices
    values = [tf[i] for i in indices]
    return indices, values


def query_to_sparse_vector(
    query: str,
    remove_stopwords: bool = False,
) -> tuple[list[int], list[float]]:
    """Convert a search query to a sparse vector.

    Similar to ``text_to_sparse_vector`` but keeps stopwords by default
    (queries are short) and uses uniform weights so Qdrant's IDF modifier
    handles the scoring.
    """
    tokens = tokenize(query, remove_stopwords=remove_stopwords)
    if not tokens:
        return [], []

    # Deduplicate: each query term gets weight 1.0
    seen: dict[int, float] = {}
    for token in tokens:
        idx = _token_hash(token)
        seen[idx] = 1.0  # Uniform weight — IDF modifier handles ranking

    indices = sorted(seen.keys())
    values = [seen[i] for i in indices]
    return indices, values


def compute_text_match_score(
    query: str,
    text: str,
    *,
    exact_match_boost: float = 5.0,  # Increased from 2.0 for bigger score gaps
    term_match_weight: float = 0.3,
) -> tuple[float, dict[str, Any]]:
    """Compute a text matching score based on exact and term matches.

    Returns:
        (score, debug_info) where score is 0.0-1.0 normalized

    Score interpretation:
    - 1.0: Exact query match found in text
    - 0.5-0.9: High term match ratio
    - 0.1-0.5: Partial term matches
    - 0.0: No matches
    """
    if not query or not text:
        return 0.0, {"exact_match": False, "term_matches": 0, "term_ratio": 0.0}

    query_lower = query.lower().strip()
    text_lower = text.lower()

    # Check exact query match (the whole query appears in the text)
    exact_match = query_lower in text_lower

    # Also check for partial exact match (each word matches exactly)
    query_words = query_lower.split()
    partial_exact = all(w in text_lower for w in query_words) if query_words else False

    # Extract meaningful terms from query (>1 char)
    query_terms = [
        t.strip()
        for t in re.split(r"[\s\"\'\-\(\)\[\]，。：；！？]+", query_lower)
        if t.strip() and len(t.strip()) > 1
    ]

    if not query_terms:
        query_terms = [query_lower] if len(query_lower) > 1 else []

    # Count term matches
    term_matches = sum(1 for t in query_terms if t in text_lower)
    term_ratio = term_matches / max(len(query_terms), 1)

    # Calculate score with larger gaps between match types
    score = 0.0
    if exact_match:
        score = 1.0  # Perfect score for exact match
    elif partial_exact:
        score = 0.85  # High score for all words matching
    elif term_ratio >= 0.8:
        score = 0.7 + term_ratio * 0.15  # High term ratio
    elif term_ratio >= 0.5:
        score = 0.4 + term_ratio * 0.3  # Medium term ratio
    elif term_ratio > 0:
        score = term_ratio * 0.4  # Low term ratio
    else:
        score = 0.0  # No matches

    return score, {
        "exact_match": exact_match,
        "partial_exact": partial_exact,
        "term_matches": term_matches,
        "total_terms": len(query_terms),
        "term_ratio": term_ratio,
    }


def reciprocal_rank_fusion(
    ranked_lists: dict[str, list[str]],
    *,
    k: int = 60,
    weights: dict[str, float] | None = None,
    qdrant_weighted: bool = False,
) -> dict[str, float]:
    """
    Reciprocal Rank Fusion (RRF) for combining ranked lists.

    RRF is preferred over score normalization because:
    - More stable across different score distributions
    - Immune to outliers that distort min-max normalization
    - Works well when score scales differ (BM25 vs cosine similarity)

    The default preserves the original lexical_v1 contract:
    ``RRF(d) = Σ weight_i / (k + rank_i(d))``.

    When ``qdrant_weighted`` is explicitly enabled, the fallback matches
    Qdrant's weighted RRF contract:
    ``RRF(d) = Σ 1 / (rank_i(d) / weight_i + k - 1)`` for positive
    weights and one-based ranks. A zero weight contributes zero.

    Args:
        ranked_lists: Dict mapping source name to list of document IDs (ranked)
        k: RRF constant (typically 60)
        weights: Optional weights per source
        qdrant_weighted: Opt in to Qdrant's weighted denominator semantics.

    Returns:
        Dict mapping document ID to fused RRF score
    """
    ranking_constant = int(k)
    if ranking_constant < 1:
        raise ValueError("RRF k must be positive")
    weights = weights or {}
    fused: dict[str, float] = defaultdict(float)
    for source, ids in (ranked_lists or {}).items():
        w = float(weights.get(source, 1.0))
        if not math.isfinite(w) or w < 0.0:
            raise ValueError("RRF weights must be finite and non-negative")
        for rank, item_id in enumerate(ids, start=1):
            if not item_id:
                continue
            if qdrant_weighted:
                contribution = (
                    0.0 if w == 0.0 else 1.0 / (rank / w + ranking_constant - 1.0)
                )
            else:
                contribution = w / float(ranking_constant + rank)
            fused[item_id] += contribution
    return dict(fused)


# =============================================================================
# Score Normalization Strategies (Best Practice 2025)
# =============================================================================


class ScoreNormalization:
    """
    Score normalization strategies for hybrid search.

    Best practices from OpenSearch and Azure Cognitive Search:
    - Min-max: Simple but sensitive to outliers
    - L2: More robust but can compress score ranges
    - Sigmoid: Good for converting arbitrary scores to [0,1]
    - Percentile: Robust to outliers, good for diverse distributions
    """

    @staticmethod
    def min_max(
        scores: dict[str, float], min_val: float | None = None, max_val: float | None = None
    ) -> dict[str, float]:
        """
        Min-max normalization to [0, 1].

        Formula: (score - min) / (max - min)

        Note: Sensitive to outliers. A single very high score
        can compress all other scores to near 0.
        """
        if not scores:
            return {}

        values = list(scores.values())
        actual_min = min_val if min_val is not None else min(values)
        actual_max = max_val if max_val is not None else max(values)

        range_val = actual_max - actual_min
        if range_val < 1e-9:
            # All scores are the same
            return dict.fromkeys(scores, 0.5)

        return {k: (v - actual_min) / range_val for k, v in scores.items()}

    @staticmethod
    def l2_normalize(scores: dict[str, float]) -> dict[str, float]:
        """
        L2 normalization (unit vector).

        Formula: score / sqrt(sum(score^2))

        Good for cosine similarity scores.
        """
        if not scores:
            return {}

        l2_norm = math.sqrt(sum(v * v for v in scores.values()))
        if l2_norm < 1e-9:
            return dict.fromkeys(scores, 0.0)

        return {k: v / l2_norm for k, v in scores.items()}

    @staticmethod
    def sigmoid_normalize(
        scores: dict[str, float], center: float = 0.5, scale: float = 1.0
    ) -> dict[str, float]:
        """
        Sigmoid normalization for smooth mapping to [0, 1].

        Formula: 1 / (1 + exp(-(score - center) * scale))

        Useful for BM25 scores which can have wide ranges.
        """
        if not scores:
            return {}

        return {k: 1.0 / (1.0 + math.exp(-(v - center) * scale)) for k, v in scores.items()}

    @staticmethod
    def percentile_normalize(scores: dict[str, float]) -> dict[str, float]:
        """
        Percentile-based normalization.

        Maps each score to its percentile rank in the distribution.
        Very robust to outliers.
        """
        if not scores:
            return {}

        if len(scores) == 1:
            return dict.fromkeys(scores, 0.5)

        # Sort scores to compute percentiles
        sorted_items = sorted(scores.items(), key=lambda x: x[1])
        n = len(sorted_items)

        return {k: (i + 1) / n for i, (k, _) in enumerate(sorted_items)}

    @staticmethod
    def robust_normalize(
        scores: dict[str, float], clip_percentile: float = 0.05
    ) -> dict[str, float]:
        """
        Robust min-max normalization with outlier clipping.

        Clips extreme values at specified percentiles before normalizing.
        Default clips at 5th and 95th percentiles.
        """
        if not scores:
            return {}

        values = sorted(scores.values())
        n = len(values)

        if n < 2:
            return dict.fromkeys(scores, 0.5)

        # Compute percentile bounds
        low_idx = max(0, int(n * clip_percentile))
        high_idx = min(n - 1, int(n * (1 - clip_percentile)))

        low_val = values[low_idx]
        high_val = values[high_idx]

        range_val = high_val - low_val
        if range_val < 1e-9:
            return dict.fromkeys(scores, 0.5)

        return {k: max(0.0, min(1.0, (v - low_val) / range_val)) for k, v in scores.items()}


def normalize_hybrid_scores(
    dense_scores: dict[str, float],
    bm25_scores: dict[str, float],
    method: str = "robust",
    dense_weight: float = 0.7,
    bm25_weight: float = 0.3,
) -> dict[str, float]:
    """
    Normalize and combine dense and BM25 scores for hybrid search.

    Best Practice (2025):
    - Use RRF for rank-based fusion (preferred)
    - Use robust normalization for score-based fusion
    - Dense typically weighted higher (0.6-0.8) for semantic queries
    - BM25 weighted higher (0.4-0.6) for exact keyword matches

    Args:
        dense_scores: Dict of document_id -> dense similarity score
        bm25_scores: Dict of document_id -> BM25 score
        method: Normalization method ("minmax", "robust", "percentile", "sigmoid")
        dense_weight: Weight for dense scores [0, 1]
        bm25_weight: Weight for BM25 scores [0, 1]

    Returns:
        Dict of document_id -> combined score
    """
    # Choose normalization method
    if method == "minmax":
        normalize = ScoreNormalization.min_max
    elif method == "percentile":
        normalize = ScoreNormalization.percentile_normalize
    elif method == "sigmoid":

        def normalize(s):
            return ScoreNormalization.sigmoid_normalize(s, center=0.5, scale=2.0)
    else:  # "robust" is default
        normalize = ScoreNormalization.robust_normalize

    # Normalize each score set
    dense_norm = normalize(dense_scores) if dense_scores else {}
    bm25_norm = normalize(bm25_scores) if bm25_scores else {}

    # Get all document IDs
    all_ids = set(dense_norm.keys()) | set(bm25_norm.keys())

    # Combine scores
    combined = {}
    for doc_id in all_ids:
        d_score = dense_norm.get(doc_id)
        b_score = bm25_norm.get(doc_id)

        if d_score is not None and b_score is not None:
            # Both scores available: weighted sum
            combined[doc_id] = d_score * dense_weight + b_score * bm25_weight
        elif d_score is not None:
            # Only dense score
            combined[doc_id] = d_score * dense_weight
        else:
            # Only BM25 score
            combined[doc_id] = b_score * bm25_weight

    return combined


def compute_language_weights(
    query: str,
    default_dense_weight: float = 0.7,
    default_bm25_weight: float = 0.3,
) -> tuple[float, float]:
    """
    Adjust fusion weights based on detected query language.

    Rationale:
    - Arabic: Higher BM25 weight due to complex morphology benefiting from exact matches
    - English: Balanced weights, semantic search works well
    - Chinese: Higher dense weight, semantic embeddings handle well

    Returns:
        (dense_weight, bm25_weight)
    """
    lang = detect_language(query)

    return {
        "ar": (0.55, 0.45),
        "zh": (0.75, 0.25),
        "mixed": (0.65, 0.35),
    }.get(lang, (default_dense_weight, default_bm25_weight))


def cosine_similarity(a: Sequence[float], b: Sequence[float]) -> float:
    if not a or not b or len(a) != len(b):
        return 0.0
    try:
        import numpy as np
        va, vb = np.asarray(a, dtype=float), np.asarray(b, dtype=float)
        denom = np.linalg.norm(va) * np.linalg.norm(vb)
        return float(np.dot(va, vb) / denom) if denom > 0 else 0.0
    except ImportError:
        dot = sum(float(x) * float(y) for x, y in zip(a, b, strict=True))
        na = sum(float(x) ** 2 for x in a)
        nb = sum(float(y) ** 2 for y in b)
        if na <= 0.0 or nb <= 0.0:
            return 0.0
        return float(dot / (math.sqrt(na) * math.sqrt(nb)))


@dataclass(frozen=True)
class MMRPick:
    item_id: str
    mmr_score: float
    relevance: float
    max_sim_to_selected: float


MMR_FILL_POLICIES = ("strict", "fill")


def _mmr_unit_vector(vec: Sequence[float] | None) -> list[float] | None:
    """Return a unit-length copy of ``vec``, or None when it cannot be used.

    Missing/empty/zero-norm/malformed vectors map to similarity 0.0, the same
    convention the pre-B5 implementation produced via cosine_similarity().
    """
    if not vec:
        return None
    try:
        floats = [float(x) for x in vec]
        norm = math.sqrt(sum(x * x for x in floats))
    except (TypeError, ValueError):
        return None
    if norm <= 0.0:
        return None
    return [x / norm for x in floats]


def mmr_select(
    candidates: list[str],
    relevance: dict[str, float],
    vectors: dict[str, Sequence[float]],
    *,
    top_k: int,
    lambda_mult: float = 0.5,
    similarity_threshold: float | None = None,
    fill_policy: str = "strict",
) -> tuple[list[str], dict[str, MMRPick]]:
    """MMR selection (diversify while keeping relevance).

    Returns (selected_ids, pick_info_by_id).

    ``fill_policy`` (B5 contract):
      - ``"strict"`` (default): a candidate rejected by
        ``similarity_threshold`` stays rejected. Maximal similarity against
        the selected set is non-decreasing as picks accumulate, so rejection
        is permanent; when rejection empties the pool the result is simply
        shorter than ``top_k``. Returning fewer results is preferred over
        resurrecting a diversity-violating candidate.
      - ``"fill"`` (legacy escape hatch): if the threshold pass leaves fewer
        than ``top_k`` selected, the unselected candidates are appended in
        the given candidate order (callers pass relevance-descending order),
        reproducing the pre-B5 caller-side fill-remaining behaviour.

    Complexity (perf-review 2026-08-16): the running per-candidate maximum
    similarity is updated only against the vector picked in the current
    round — O(k * n * d) dot products total. The pre-B5 implementation
    recomputed similarity of every remaining candidate against the whole
    selected set on every round: O(k^2 * n * d). Numpy is used when
    importable; results are identical without it.
    """
    if not candidates or top_k <= 0:
        return [], {}
    policy = "strict" if fill_policy is None else str(fill_policy).strip().lower()
    if policy not in MMR_FILL_POLICIES:
        raise ValueError(
            f"unknown MMR fill_policy {fill_policy!r}; expected one of {MMR_FILL_POLICIES}"
        )

    lam = float(lambda_mult)
    lam = max(0.0, min(1.0, lam))
    threshold = float(similarity_threshold) if similarity_threshold is not None else None

    remaining = list(dict.fromkeys(c for c in candidates if c))
    n = len(remaining)
    if n == 0:
        return [], {}
    rel = [float(relevance.get(cid, 0.0)) for cid in remaining]
    units = [_mmr_unit_vector(vectors.get(cid)) for cid in remaining]

    selected: list[str] = []
    picks: dict[str, MMRPick] = {}
    alive = [True] * n
    max_sim = [0.0] * n

    try:
        import numpy as np
    except ImportError:  # pragma: no cover - numpy absence is env-dependent
        np = None

    mat = None
    dims = {len(u) for u in units if u is not None}
    if np is not None and len(dims) <= 1:
        dim = next(iter(dims), 0)
        if dim:
            mat = np.zeros((n, dim), dtype=float)
            for i, u in enumerate(units):
                if u is not None and len(u) == dim:
                    mat[i] = u

    def refresh_max_sim(pick_idx: int) -> None:
        """Fold similarity-to-newly-picked into each candidate's running max."""
        if not any(alive):
            return
        if mat is not None:
            sims = mat @ mat[pick_idx]
            for i in range(n):
                if alive[i]:
                    s = float(sims[i])
                    if s > max_sim[i]:
                        max_sim[i] = s
            return
        picked = units[pick_idx]
        if picked is None:
            return
        for i in range(n):
            if not alive[i]:
                continue
            u = units[i]
            if u is None or len(u) != len(picked):
                continue
            s = sum(a * b for a, b in zip(u, picked, strict=True))
            if s > max_sim[i]:
                max_sim[i] = s

    for _ in range(min(int(top_k), n)):
        if threshold is not None and selected:
            # B5: rejection is permanent — max_sim never decreases.
            for i in range(n):
                if alive[i] and max_sim[i] >= threshold:
                    alive[i] = False
        best_idx = -1
        best_mmr = -1e30
        for i in range(n):
            if not alive[i]:
                continue
            mmr = lam * rel[i] - (1.0 - lam) * max_sim[i]
            if mmr > best_mmr:
                best_mmr = mmr
                best_idx = i
        if best_idx < 0:
            # Every remaining candidate is diversity-rejected (or pool empty).
            break
        alive[best_idx] = False
        cid = remaining[best_idx]
        selected.append(cid)
        picks[cid] = MMRPick(
            item_id=cid,
            mmr_score=float(best_mmr),
            relevance=float(rel[best_idx]),
            max_sim_to_selected=float(max_sim[best_idx]),
        )
        refresh_max_sim(best_idx)

    if policy == "fill" and len(selected) < int(top_k):
        chosen = set(selected)
        for cid in remaining:
            if len(selected) >= int(top_k):
                break
            if cid in chosen:
                continue
            selected.append(cid)
            chosen.add(cid)

    return selected, picks
