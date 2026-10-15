"""Retrieval service for knowledge base.

Handles document retrieval, search, and ranking.
Migrated from KnowledgeService as part of Phase 2 refactoring.
"""

from __future__ import annotations

import asyncio
import inspect
import math
import re
import time
from dataclasses import dataclass
from functools import wraps
from typing import TYPE_CHECKING, Any

import httpx

from ...config import get_settings as _live_settings
from ...config.settings import Settings
from ...core.exceptions import PermissionDeniedError, ValidationFailedError
from ...core.observability import metrics as _metrics
from ...core.observability.logging import get_logger
from ...persistence.database import (
    DatabaseStorage,
    IndexLeaseUnavailableError,
    dataset_index_deletion_fence,
)
from .common import ensure_dict as _ensure_dict
from .common import maybe_await
from .embedding import (
    MULTIMODAL_EMBEDDING_MODELS,
    BaseEmbedding,
    get_cached_embedder,
)
from .embedding_migration import (
    MixedModelEmbeddingError,
    assert_query_embedding_identity,
)
from .lexical_config import LexicalConfig, LexicalConfigError
from .parent_child_retrieval import (
    apply_recall_fanout,
    fold_candidates_to_parents,
    merge_summary_siblings,
    parse_parent_child_settings,
    parse_summary_index_settings,
)
from .query_observability import new_query_observation
from .query_preset_config import parse_query_preset_settings
from .retrieval import (
    LEGACY_RRF_SEMANTICS,
    MMR_FILL_POLICIES,
    QDRANT_WEIGHTED_RRF_SEMANTICS,
    ScoreNormalization,
    bm25_scores,
    compute_language_weights,
    compute_text_match_score,
    cosine_similarity,
    mmr_select,
    query_to_sparse_vector,
    reciprocal_rank_fusion,
    tokenize,
)
from .retrieval_failures import raise_batch_failure, require_recall_result
from .structural_routing import (
    apply_structural_routing,
    parse_structural_settings,
)
from .vector_store_contracts import (
    CollectionReadAuthorityError,
    remaining_interactive_budget_seconds,
)

if TYPE_CHECKING:
    from ...core.auth.user_resolver import UserContext
    from .knowledge_service import KnowledgeService

logger = get_logger(__name__)

# SPO-04/K3: interactive recall uses 12 dense + 12 lexical; rerank and MMR stay opt-in.
_INTERACTIVE_DEFAULT_VECTOR_K = 12
_INTERACTIVE_DEFAULT_KEYWORD_K = 12
_INTERACTIVE_DEFAULT_CANDIDATE_K = 24
_PUBLICATION_RETRY_LIMIT = 24
_PUBLICATION_RETRY_BASE_SECONDS = 0.025
_PUBLICATION_RETRY_MAX_SECONDS = 0.2


class _DatasetIndexPublicationInProgress(IndexLeaseUnavailableError):
    """The durable revision seqlock says a cross-store publish is active."""


class _RetrievalGenerationChanged(RuntimeError):
    """A retrieval overlapped a publish and must rerun from its entrypoint."""


def _with_interactive_qdrant_budget(fn):
    """Apply one deadline and bounded publication retries to an entrypoint."""

    @wraps(fn)
    async def wrapped(self, *args, **kwargs):
        vector_store = getattr(self, "vector_store", None)
        # Static lookup avoids invoking a defensive/mock __getattr__ before
        # the entrypoint's own request and release-fence validation runs.
        budget_supported = (
            inspect.getattr_static(vector_store, "begin_interactive_budget", None) is not None
            and inspect.getattr_static(vector_store, "end_interactive_budget", None) is not None
        )
        begin = getattr(vector_store, "begin_interactive_budget", None) if budget_supported else None
        end = getattr(vector_store, "end_interactive_budget", None) if budget_supported else None
        token = begin() if callable(begin) and callable(end) else None
        try:
            for attempt in range(_PUBLICATION_RETRY_LIMIT + 1):
                try:
                    return await fn(self, *args, **kwargs)
                except (
                    _DatasetIndexPublicationInProgress,
                    _RetrievalGenerationChanged,
                ) as exc:
                    delay = min(
                        _PUBLICATION_RETRY_BASE_SECONDS * (attempt + 1),
                        _PUBLICATION_RETRY_MAX_SECONDS,
                    )
                    remaining = remaining_interactive_budget_seconds()
                    exhausted = attempt >= _PUBLICATION_RETRY_LIMIT or (
                        remaining is not None and remaining <= delay
                    )
                    if exhausted:
                        raise IndexLeaseUnavailableError(
                            "dataset index publication is still in progress; retry the request"
                        ) from exc
                    await asyncio.sleep(delay)
            raise AssertionError("publication retry loop exhausted unexpectedly")
        finally:
            if token is not None and callable(end):
                end(token)

    return wrapped


MULTI_QUERY_TOP_K = {1: 5, 2: 6, 3: 8, 4: 9, 5: 10}
_SERVER_OWNED_RERANK_FIELD_ALIASES = frozenset(
    {
        "apikey",
        "key",
        "accesskey",
        "secret",
        "secretkey",
        "token",
        "bearertoken",
        "authorization",
        "auth",
        "credentials",
        "headers",
        "baseurl",
        "endpoint",
        "endpointurl",
        "apibase",
        "apiurl",
        "url",
        "host",
    }
)


def _contains_server_owned_rerank_field(value: Any) -> bool:
    if isinstance(value, dict):
        for key, nested in value.items():
            normalized = "".join(character for character in str(key).lower() if character.isalnum())
            if any(normalized.endswith(alias) for alias in _SERVER_OWNED_RERANK_FIELD_ALIASES):
                return True
            if _contains_server_owned_rerank_field(nested):
                return True
    elif isinstance(value, (list, tuple)):
        return any(_contains_server_owned_rerank_field(item) for item in value)
    return False


def _require_server_owned_rerank_config(value: Any) -> None:
    if _contains_server_owned_rerank_field(value):
        raise ValidationFailedError(
            "stored rerank config contains a legacy credential or endpoint; "
            "remove it before retrieval"
        )


def _require_bounded_retrieval_config(value: Any, *, scope: str) -> None:
    """Reject resource poison before any retrieval dependency is called."""

    retrieval = _ensure_dict(value)
    vector = _ensure_dict(retrieval.get("vector"))
    keyword = _ensure_dict(retrieval.get("keyword"))
    fusion = _ensure_dict(retrieval.get("fusion"))
    rerank = _ensure_dict(retrieval.get("rerank"))
    mmr = _ensure_dict(retrieval.get("mmr"))

    def require_integer(path: str, raw: Any, upper_bound: int) -> None:
        if raw is None:
            return
        if isinstance(raw, bool) or not isinstance(raw, int):
            raise ValidationFailedError(
                f"{scope} {path} must be an integer between 1 and {upper_bound}"
            )
        if not 1 <= raw <= upper_bound:
            raise ValidationFailedError(
                f"{scope} {path} must be an integer between 1 and {upper_bound}"
            )

    integer_values = (
        ("top_k", retrieval.get("top_k"), 100),
        ("vector_top_k", retrieval.get("vector_top_k"), 1000),
        ("keyword_top_k", retrieval.get("keyword_top_k"), 1000),
        ("candidate_top_k", retrieval.get("candidate_top_k"), 2000),
        ("keyword_candidate_k", retrieval.get("keyword_candidate_k"), 500),
        ("rerank_top_n", retrieval.get("rerank_top_n"), 1000),
        ("rrf_k", retrieval.get("rrf_k"), 10_000),
        ("vector.top_k", vector.get("top_k"), 1000),
        ("keyword.top_k", keyword.get("top_k"), 1000),
        (
            "keyword.candidate_pool_size",
            keyword.get("candidate_pool_size"),
            500,
        ),
        ("fusion.rrf_k", fusion.get("rrf_k"), 10_000),
        ("rerank.top_n", rerank.get("top_n"), 1000),
    )
    for path, raw, upper_bound in integer_values:
        require_integer(path, raw, upper_bound)

    def require_unit_interval(path: str, raw: Any) -> float | None:
        if raw is None:
            return None
        if isinstance(raw, bool) or not isinstance(raw, (int, float)):
            raise ValidationFailedError(f"{scope} {path} must be finite and between 0 and 1")
        numeric = float(raw)
        if not math.isfinite(numeric) or not 0.0 <= numeric <= 1.0:
            raise ValidationFailedError(f"{scope} {path} must be finite and between 0 and 1")
        return numeric

    unit_interval_values = (
        ("dense_weight", retrieval.get("dense_weight")),
        ("bm25_weight", retrieval.get("bm25_weight")),
        ("alpha", retrieval.get("alpha")),
        ("score_threshold", retrieval.get("score_threshold")),
        ("mmr_lambda", retrieval.get("mmr_lambda")),
        ("mmr_threshold", retrieval.get("mmr_threshold")),
        ("vector.score_threshold", vector.get("score_threshold")),
        ("fusion.dense_weight", fusion.get("dense_weight")),
        ("fusion.bm25_weight", fusion.get("bm25_weight")),
        ("fusion.alpha", fusion.get("alpha")),
        ("rerank.score_threshold", rerank.get("score_threshold")),
        ("mmr.lambda", mmr.get("lambda")),
        ("mmr.lambda_mult", mmr.get("lambda_mult")),
        ("mmr.threshold", mmr.get("threshold")),
        ("mmr.similarity_threshold", mmr.get("similarity_threshold")),
    )
    validated_units = {path: require_unit_interval(path, raw) for path, raw in unit_interval_values}

    # B5: MMR fill-remaining contract keys are validated here so a bad
    # stored config fails at the gate instead of silently disabling the
    # diversity guarantee at pick time.
    mmr_fill_policy_raw = mmr.get("fill_policy")
    if mmr_fill_policy_raw is not None and (
        not isinstance(mmr_fill_policy_raw, str)
        or mmr_fill_policy_raw.strip().lower() not in MMR_FILL_POLICIES
    ):
        raise ValidationFailedError(
            f"{scope} mmr.fill_policy must be one of {list(MMR_FILL_POLICIES)}"
        )
    mmr_strict_raw = mmr.get("strict_diversity")
    if mmr_strict_raw is not None and not isinstance(mmr_strict_raw, bool):
        raise ValidationFailedError(f"{scope} mmr.strict_diversity must be a boolean")

    for prefix in ("", "fusion."):
        dense = validated_units[f"{prefix}dense_weight"]
        keyword_weight = validated_units[f"{prefix}bm25_weight"]
        if (
            dense is not None
            and keyword_weight is not None
            and not (dense > 0.0 or keyword_weight > 0.0)
        ):
            raise ValidationFailedError(f"{scope} {prefix}weights must include a positive value")

    def require_rrf_weights(path: str, raw: Any) -> None:
        if raw is None:
            return
        if not isinstance(raw, dict) or len(raw) > 16:
            raise ValidationFailedError(f"{scope} {path} must contain at most 16 weights")
        weights: list[float] = []
        for name, weight in raw.items():
            if not str(name).strip() or len(str(name)) > 64:
                raise ValidationFailedError(f"{scope} {path} keys must contain 1-64 characters")
            if isinstance(weight, bool) or not isinstance(weight, (int, float)):
                raise ValidationFailedError(
                    f"{scope} {path} values must be finite and between 0 and 100"
                )
            numeric = float(weight)
            if not math.isfinite(numeric) or not 0.0 <= numeric <= 100.0:
                raise ValidationFailedError(
                    f"{scope} {path} values must be finite and between 0 and 100"
                )
            weights.append(numeric)
        if weights and not any(weight > 0.0 for weight in weights):
            raise ValidationFailedError(f"{scope} {path} must include a positive weight")

    require_rrf_weights("rrf_weights", retrieval.get("rrf_weights"))
    require_rrf_weights("fusion.rrf_weights", fusion.get("rrf_weights"))


def _require_bounded_persisted_retrieval_config(value: Any) -> None:
    _require_bounded_retrieval_config(value, scope="stored retrieval config")


# B6 (PRD T1-8): zero-result FTS fallback is scoped to scripts where the
# 'simple' tsvector config and the Python (jieba-style) tokenizer genuinely
# disagree. PostgreSQL's default parser emits ONE lexeme per unbroken CJK
# run, so Chinese/Japanese/Korean word tokens can never match as separate
# tsquery terms; the substring ILIKE matcher is the documented compatibility
# path for exactly that mismatch. Latin scripts match 'simple' tsquery
# directly, so a zero-result English query stays zero-result instead of
# paying an O(N) sequential scan.
_CJK_TEXT_RE = re.compile(r"[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uac00-\ud7af]")


def _resolve_mmr_fill_policy(mmr_cfg: dict[str, Any]) -> str:
    """B5 fill policy from the dataset mmr config (PRD T1-8).

    ``fill_policy`` ("strict" | "fill") wins; the boolean shorthand
    ``strict_diversity`` follows it; the default is "strict" — candidates
    rejected by the diversity pass are never resurrected by fill-remaining.
    """
    raw = mmr_cfg.get("fill_policy")
    if raw is not None:
        policy = str(raw).strip().lower()
        if policy in MMR_FILL_POLICIES:
            return policy
        raise ValidationFailedError(f"mmr.fill_policy must be one of {list(MMR_FILL_POLICIES)}")
    strict = mmr_cfg.get("strict_diversity")
    if strict is not None:
        if isinstance(strict, bool):
            return "strict" if strict else "fill"
        raise ValidationFailedError("mmr.strict_diversity must be a boolean")
    return "strict"


def _require_bounded_retrieval_request(
    *,
    query: Any,
    top_k: Any,
    vector_top_k: Any = None,
    keyword_top_k: Any = None,
    candidate_top_k: Any = None,
    keyword_candidate_k: Any = None,
    rerank_top_n: Any = None,
    rrf_k: Any = None,
    dense_weight: Any = None,
    bm25_weight: Any = None,
    alpha: Any = None,
    score_threshold: Any = None,
    mmr_lambda: Any = None,
    mmr_threshold: Any = None,
    rrf_weights: Any = None,
    scope: str = "retrieval request",
) -> None:
    if not isinstance(query, str) or not 1 <= len(query.strip()) <= 4096:
        raise ValidationFailedError(f"{scope} query must contain 1-4096 characters")
    _require_bounded_retrieval_config(
        {
            "top_k": top_k,
            "vector_top_k": vector_top_k,
            "keyword_top_k": keyword_top_k,
            "candidate_top_k": candidate_top_k,
            "keyword_candidate_k": keyword_candidate_k,
            "rerank_top_n": rerank_top_n,
            "rrf_k": rrf_k,
            "dense_weight": dense_weight,
            "bm25_weight": bm25_weight,
            "alpha": alpha,
            "score_threshold": score_threshold,
            "mmr_lambda": mmr_lambda,
            "mmr_threshold": mmr_threshold,
            "rrf_weights": rrf_weights,
        },
        scope=scope,
    )


def _require_bounded_batch_request(
    queries: Any,
    *,
    max_parallel: Any,
) -> None:
    if not isinstance(queries, list) or not 1 <= len(queries) <= 20:
        raise ValidationFailedError(
            "retrieval batch queries must be a list containing 1-20 entries"
        )
    if (
        isinstance(max_parallel, bool)
        or not isinstance(max_parallel, int)
        or not 1 <= max_parallel <= 10
    ):
        raise ValidationFailedError(
            "retrieval batch max_parallel must be an integer between 1 and 10"
        )
    for index, item in enumerate(queries):
        if isinstance(item, str):
            _require_bounded_retrieval_request(
                query=item,
                top_k=1,
                scope=f"retrieval batch query[{index}]",
            )
            continue
        if not isinstance(item, dict):
            raise ValidationFailedError(
                f"retrieval batch query[{index}] must be a string or object"
            )
        _require_bounded_retrieval_request(
            query=item.get("query"),
            top_k=item.get("top_k", 1),
            vector_top_k=item.get("vector_top_k"),
            keyword_top_k=item.get("keyword_top_k"),
            candidate_top_k=item.get("candidate_top_k"),
            keyword_candidate_k=item.get("keyword_candidate_k"),
            rerank_top_n=item.get("rerank_top_n"),
            rrf_k=item.get("rrf_k"),
            dense_weight=item.get("dense_weight"),
            bm25_weight=item.get("bm25_weight"),
            alpha=item.get("alpha"),
            score_threshold=item.get("score_threshold"),
            mmr_lambda=item.get("mmr_lambda"),
            mmr_threshold=item.get("mmr_threshold"),
            rrf_weights=item.get("rrf_weights"),
            scope=f"retrieval batch query[{index}]",
        )


def _candidate_is_image(candidate: dict[str, Any]) -> bool:
    image_types = {"image", "page_image", "mixed", "multimodal"}

    def _contains_image_type(value: Any) -> bool:
        if isinstance(value, dict):
            for key, nested in value.items():
                normalized = "".join(
                    character for character in str(key).lower() if character.isalnum()
                )
                if normalized == "contenttype" and str(nested).strip().lower() in image_types:
                    return True
                if _contains_image_type(nested):
                    return True
        elif isinstance(value, (list, tuple)):
            return any(_contains_image_type(item) for item in value)
        return False

    return _contains_image_type(candidate)


def _dataset_uses_multimodal_profile(dataset: dict[str, Any]) -> bool:
    provider = str(dataset.get("embedding_provider") or "").strip().lower()
    if provider in {
        "unified_multimodal",
        "unified",
        "cross_modal",
        "dashscope_multimodal",
        "multimodal",
    }:
        return True
    if str(dataset.get("embedding_model") or "") in MULTIMODAL_EMBEDDING_MODELS:
        return True
    index_config = _ensure_dict(dataset.get("index_config"))
    return bool(index_config.get("multimodal_enabled") or index_config.get("enable_multimodal"))


def require_shadow_only_dataset(dataset: dict[str, Any]) -> LexicalConfig:
    """Validate the dataset's persisted lexical profile before retrieval.

    An active ``bm25_v2`` selection is only ever written by the T6 lifecycle
    protocol (which has already cross-verified Qdrant against PostgreSQL), so
    serving it is allowed from here on: the vector store enforces the service
    kill switch and recomputes receipt evidence per query, failing closed.
    A profile that merely *claims* bm25_v2 in PostgreSQL without a cut-over
    collection still fails loudly at ``require_collection_readable``.
    """
    try:
        deletion_fence = dataset_index_deletion_fence(dataset)
    except RuntimeError as exc:
        raise ValidationFailedError(str(exc)) from exc
    if deletion_fence is not None:
        raise ValidationFailedError("dataset index deletion is pending; retrieval is unavailable")
    try:
        lexical_config = LexicalConfig.from_index_config(_ensure_dict(dataset.get("index_config")))
    except LexicalConfigError as exc:
        raise ValidationFailedError(str(exc)) from exc
    return lexical_config


def dataset_retrieval_generation(
    dataset: dict[str, Any],
) -> tuple[str, Any, str, str, str, str, int]:
    """Capture the authoritative generation that a retrieval may return."""

    require_shadow_only_dataset(dataset)
    content_revision = dataset.get("content_revision")
    if isinstance(content_revision, int) and not isinstance(content_revision, bool) and content_revision < 0:
        raise _DatasetIndexPublicationInProgress(
            "dataset index publication is in progress"
        )
    return (
        str(dataset.get("tenant_id") or "").strip(),
        content_revision,
        str(dataset.get("collection_name") or "").strip(),
        str(dataset.get("embedding_provider") or "").strip().lower(),
        str(dataset.get("embedding_model") or "").strip().lower(),
        str(dataset.get("embedding_model_version") or "").strip(),
        int(dataset.get("embedding_dimension") or 0),
    )


def _has_explicit_rrf_weighting(
    *,
    retrieval_defaults: dict[str, Any],
    dense_weight: float | None,
    bm25_weight: float | None,
    alpha: float | None,
    rrf_weights: dict[str, float] | None,
) -> bool:
    """Return whether weighted RRF was selected rather than merely defaulted."""
    if any(value is not None for value in (dense_weight, bm25_weight, alpha)):
        return True
    if isinstance(rrf_weights, dict) and bool(rrf_weights):
        return True

    for key in ("dense_weight", "bm25_weight", "rrf_weights"):
        value = retrieval_defaults.get(key)
        if value is not None and value != {}:
            return True

    nested_fusion = _ensure_dict(retrieval_defaults.get("fusion"))
    for key in ("alpha", "dense_weight", "bm25_weight", "rrf_weights"):
        value = nested_fusion.get(key)
        if value is not None and value != {}:
            return True

    # Adaptive weights historically defaulted on but did not alter RRF. Treat
    # them as a weighted-RRF opt-in only when the dataset stores the flag.
    return retrieval_defaults.get("adaptive_weights") is True


@dataclass(frozen=True)
class RetrieveResult:
    """Result from document retrieval."""

    segment_id: str
    document_id: str
    score: float
    text: str
    metadata: dict[str, Any]
    content_type: str = "text"
    image_url: str | None = None
    vlm_description: str | None = None
    associated_images: tuple = ()


@dataclass
class RetrievalConfig:
    """Configuration for retrieval."""

    mode: str = "auto"
    top_k: int = 5
    score_threshold: float = 0.5
    use_mmr: bool = False
    mmr_diversity: float = 0.3
    expand_queries: bool = False
    max_query_expansions: int = 3
    fusion_method: str = "rrf"
    use_adaptive_weights: bool = True


def _discard_telemetry_result(task: asyncio.Task) -> None:
    """Done-callback keeping fire-and-forget telemetry from going unnoticed."""
    if task.cancelled():
        return
    exc = task.exception()
    if exc is not None:
        logger.debug("dataset query telemetry insert failed: %s", exc)


# Rerank calls slower than this almost never finish; skip early and serve the
# fusion order instead of burning the remaining interactive budget on them.
_RERANK_MIN_BUDGET_SECONDS = 0.2


class _RerankBudgetExhausted(Exception):
    """Rerank declined to start: the interactive budget is (nearly) spent."""


class RetrievalService:
    """Service for retrieving relevant documents from knowledge base.

    Accepts a ``_ks`` (parent KnowledgeService) reference for shared resources
    like ``vector_store``, ``cache_manager``, ``vlm_service``, etc.
    Set post-init by the parent because these are created after sub-service
    construction.
    """

    _ks: KnowledgeService | None

    def __init__(
        self,
        settings: Settings,
        database: DatabaseStorage,
    ):
        self.settings = settings
        self.db = database
        self.vector_store = None  # Set post-init by KnowledgeService
        self._ks = None  # Set post-init by KnowledgeService

    async def _require_unchanged_retrieval_generation(
        self,
        user: UserContext,
        dataset_id: str,
        expected: tuple[str, Any, str, str, str, str, int],
    ) -> None:
        """Discard any result that overlapped deletion set/clear or failure."""

        authoritative = await self._ks.require_dataset_access(
            user,
            dataset_id,
            required="viewer",
        )
        try:
            current = dataset_retrieval_generation(authoritative)
        except _DatasetIndexPublicationInProgress as exc:
            raise _RetrievalGenerationChanged(
                "dataset index generation changed during retrieval"
            ) from exc
        if current != expected:
            raise _RetrievalGenerationChanged(
                "dataset index generation changed during retrieval"
            )

    async def _require_collection_readable(
        self,
        dataset: dict[str, Any],
        dataset_id: str,
    ) -> None:
        """Fail before cache, embedding, or PostgreSQL fallback on unsafe Qdrant state."""

        index_config = _ensure_dict(dataset.get("index_config"))
        retrieval_defaults = _ensure_dict(index_config.get("retrieval"))
        _require_bounded_persisted_retrieval_config(retrieval_defaults)
        _require_server_owned_rerank_config(retrieval_defaults.get("rerank"))
        profile_detector = getattr(self._ks, "_is_multimodal_dataset", None)
        is_multimodal = _dataset_uses_multimodal_profile(dataset) or (
            callable(profile_detector) and bool(profile_detector(dataset))
        )
        if is_multimodal:
            raise ValidationFailedError(
                "multimodal dataset retrieval is unavailable in this release"
            )
        collection_name = str(dataset.get("collection_name") or "").strip()
        tenant_id = str(dataset.get("tenant_id") or "").strip()
        if not collection_name:
            raise ValidationFailedError("dataset retrieval requires a persisted Qdrant collection")
        guard = getattr(self.vector_store, "require_collection_readable", None)
        if not callable(guard):
            raise ValidationFailedError("vector store collection-read authority is unavailable")
        try:
            await guard(
                collection_name,
                tenant_id=tenant_id,
                dataset_id=dataset_id,
            )
        except Exception as exc:
            raise ValidationFailedError(f"dataset collection is not readable: {exc}") from exc

    async def _active_segment_ids(
        self,
        *,
        dataset_id: str,
        tenant_id: str,
        segment_ids: list[str],
    ) -> set[str]:
        """Resolve the authoritative active subset in one PostgreSQL query."""

        normalized_ids = list(
            dict.fromkeys(
                str(segment_id or "").strip()
                for segment_id in segment_ids
                if str(segment_id or "").strip()
            )
        )
        if not normalized_ids:
            return set()
        normalized_tenant = str(tenant_id or "").strip()
        if not normalized_tenant:
            raise ValidationFailedError(
                "active-segment authority requires a non-empty dataset tenant scope"
            )
        filter_active = getattr(self.db, "filter_active_segment_ids", None)
        if not callable(filter_active):
            raise ValidationFailedError("active-segment database authority is unavailable")
        try:
            active = await filter_active(
                dataset_id=dataset_id,
                tenant_id=normalized_tenant,
                segment_ids=normalized_ids,
            )
        except Exception as exc:
            raise ValidationFailedError(f"active-segment database authority failed: {exc}") from exc
        active_ids = {
            str(segment_id or "").strip()
            for segment_id in (active or set())
            if str(segment_id or "").strip()
        }
        if not active_ids.issubset(set(normalized_ids)):
            raise ValidationFailedError(
                "active-segment database authority returned an unexpected segment"
            )
        return active_ids

    # ========================================================================
    # Query telemetry (PRD C1) — fire-and-forget dataset_queries records
    # ========================================================================

    def _record_retrieval_telemetry(
        self,
        *,
        user: UserContext,
        dataset_id: str,
        query: str,
        mode: str,
        top_k: int,
        results: list[Any],
        meta: dict[str, Any],
        source: str = "api",
    ) -> None:
        """Schedule a dataset_queries INSERT without ever blocking retrieval.

        Independent transaction (own pool connection inside the helper), and
        every failure mode — missing db support, no running loop, insert
        error — degrades to silence. Telemetry must never fail a retrieval.
        """
        db = getattr(self, "db", None)
        record = getattr(db, "record_dataset_query", None)
        if not callable(record):
            return
        normalized_query = " ".join((query or "").strip().split())
        trace_id = str(meta.get("trace_id") or "").strip()
        fingerprint = str(meta.get("query_fingerprint") or "").strip()
        if not trace_id or not fingerprint:
            return
        normalized_mode = {"vector": "dense", "keyword": "bm25"}.get(
            str(mode or "hybrid").strip().lower(),
            str(mode or "hybrid").strip().lower(),
        )
        stage_timings = meta.get("timings_ms")
        metadata: dict[str, Any] = {
            "query_fingerprint": fingerprint,
            "mode": normalized_mode,
            "top_k": top_k,
            "hit_count": len(results),
            "cache_hit": bool(meta.get("retrieval_cache_hit")),
            "trace_id": trace_id,
        }
        if isinstance(stage_timings, dict):
            metadata["stage_timings"] = stage_timings
        rerank_degraded = meta.get("rerank_degraded")
        if rerank_degraded is not None:
            metadata["rerank_degraded"] = rerank_degraded
        user_id = str(getattr(user, "user_id", "") or "").strip() or None
        user_tier = str(getattr(user, "user_tier", "") or "").strip() or None
        def _segment_id(result: Any) -> str:
            if isinstance(result, dict):
                return str(result.get("segment_id") or "").strip()
            return str(getattr(result, "segment_id", "") or "").strip()

        observed_segment_ids: set[str] = set()
        for result in results:
            if segment_id := _segment_id(result):
                observed_segment_ids.add(segment_id)
        segment_ids = sorted(observed_segment_ids)
        try:
            task = asyncio.create_task(
                record(
                    dataset_id=dataset_id,
                    content=normalized_query,
                    source=source,
                    created_by=user_id,
                    created_by_role=user_tier,
                    metadata=metadata,
                    trace_id=trace_id,
                    query_fingerprint=fingerprint,
                    mode=normalized_mode,
                    top_k=top_k,
                    hit_count=len(results),
                    stage_timings=stage_timings if isinstance(stage_timings, dict) else {},
                    segment_ids=segment_ids,
                )
            )
        except RuntimeError:
            # No running loop (or loop is closing): drop the record rather
            # than raise into the request path.
            return
        task.add_done_callback(_discard_telemetry_result)

    def record_external_retrieval_observation(
        self,
        *,
        user: UserContext,
        dataset_id: str,
        query: str,
        mode: str,
        top_k: int,
        results: list[Any],
        meta: dict[str, Any],
        source: str,
    ) -> dict[str, Any]:
        """Stamp and persist a non-standard public retrieval branch."""

        observation = new_query_observation(query)
        stamped = dict(meta)
        stamped["trace_id"] = observation.trace_id
        stamped["query_fingerprint"] = observation.query_fingerprint
        self._record_retrieval_telemetry(
            user=user,
            dataset_id=dataset_id,
            query=query,
            mode=mode,
            top_k=top_k,
            results=results,
            meta=stamped,
            source=source,
        )
        return stamped

    # ========================================================================
    # Core Retrieval — the main hybrid retrieval pipeline
    # ========================================================================

    @_with_interactive_qdrant_budget
    async def retrieve(
        self,
        user: UserContext,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        mode: str = "hybrid",
        document_id: str | None = None,
        dense_weight: float | None = None,
        bm25_weight: float | None = None,
        fusion_method: str | None = None,
        rrf_k: int | None = None,
        alpha: float | None = None,
        score_threshold: float | None = None,
        vector_top_k: int | None = None,
        keyword_top_k: int | None = None,
        candidate_top_k: int | None = None,
        keyword_candidate_k: int | None = None,
        fusion: str | None = None,
        rrf_weights: dict[str, float] | None = None,
        rerank: bool | None = None,
        rerank_model: str | None = None,
        rerank_top_n: int | None = None,
        mmr: bool | None = None,
        mmr_lambda: float | None = None,
        mmr_threshold: float | None = None,
        source_type_filter: str | None = None,
        language_filter: str | None = None,
        metadata_filter: dict[str, Any] | None = None,
        telemetry_source: str = "api",
    ) -> tuple[list[RetrieveResult], dict[str, Any]]:
        """Run the existing single-query retrieval contract."""
        _require_bounded_retrieval_request(
            query=query,
            top_k=top_k,
            vector_top_k=vector_top_k,
            keyword_top_k=keyword_top_k,
            candidate_top_k=candidate_top_k,
            keyword_candidate_k=keyword_candidate_k,
            rerank_top_n=rerank_top_n,
            rrf_k=rrf_k,
            dense_weight=dense_weight,
            bm25_weight=bm25_weight,
            alpha=alpha,
            score_threshold=score_threshold,
            mmr_lambda=mmr_lambda,
            mmr_threshold=mmr_threshold,
            rrf_weights=rrf_weights,
        )
        observation = new_query_observation(query)
        retrieve_started = time.perf_counter()
        dataset = await self._ks.require_dataset_access(
            user,
            dataset_id,
            required="viewer",
        )
        retrieval_generation = dataset_retrieval_generation(dataset)
        from .dataset_service import _dataset_revision_fingerprint

        dataset_revision_fingerprint = _dataset_revision_fingerprint(dataset)
        user_id = str(getattr(user, "user_id", "") or "").strip()
        cache_get = getattr(self._ks, "_get_cached_retrieval", None)
        cache_set = getattr(self._ks, "_set_cached_retrieval", None)
        cache_fingerprint = ""
        cache_key = ""
        cache_enabled = bool(
            user_id
            and dataset_revision_fingerprint is not None
            and callable(cache_get)
            and callable(cache_set)
        )
        if cache_enabled:
            cache_fingerprint = self._ks._compute_retrieval_query_fingerprint(
                {
                    "contract": "standard-v1",
                    "user_id": user_id,
                    "dataset_id": dataset_id,
                    "dataset_revision_fingerprint": dataset_revision_fingerprint,
                    "query": " ".join((query or "").strip().split()),
                    "top_k": top_k,
                    "mode": mode,
                    "document_id": document_id,
                    "dense_weight": dense_weight,
                    "bm25_weight": bm25_weight,
                    "fusion_method": fusion_method,
                    "rrf_k": rrf_k,
                    "alpha": alpha,
                    "score_threshold": score_threshold,
                    "vector_top_k": vector_top_k,
                    "keyword_top_k": keyword_top_k,
                    "candidate_top_k": candidate_top_k,
                    "keyword_candidate_k": keyword_candidate_k,
                    "fusion": fusion,
                    "rrf_weights": rrf_weights,
                    "rerank": rerank,
                    "rerank_model": rerank_model,
                    "rerank_top_n": rerank_top_n,
                    "mmr": mmr,
                    "mmr_lambda": mmr_lambda,
                    "mmr_threshold": mmr_threshold,
                    "source_type_filter": source_type_filter,
                    "language_filter": language_filter,
                    "metadata_filter": metadata_filter,
                }
            )
            cache_key = f"standard:{user_id}:{dataset_id}:{cache_fingerprint}"
            await self._require_collection_readable(dataset, dataset_id)
            cached_response = await cache_get(cache_key)
            if cached_response is not None:
                cached_results, cached_meta = cached_response
                cached_ids = [
                    str(result.segment_id or "").strip()
                    for result in cached_results
                    if str(result.segment_id or "").strip()
                ]
                active_ids = await self._active_segment_ids(
                    dataset_id=dataset_id,
                    tenant_id=str(dataset.get("tenant_id") or ""),
                    segment_ids=cached_ids,
                )
                if active_ids == set(cached_ids):
                    await self._require_unchanged_retrieval_generation(
                        user,
                        dataset_id,
                        retrieval_generation,
                    )
                    cached_meta["retrieval_cache_hit"] = True
                    cached_meta["retrieval_query_fingerprint"] = cache_fingerprint
                    cached_meta["dataset_revision_fingerprint"] = dataset_revision_fingerprint
                    # A cache hit performed no rerank, so it must not replay a
                    # degrade state: the new entries can no longer carry one
                    # (degraded results are never cached below), but entries
                    # written before this rule would keep emitting phantom
                    # rerank_degraded telemetry for the whole cache TTL.
                    cached_meta.pop("rerank_degraded", None)
                    cached_meta.pop("rerank_error", None)
                    cached_meta["trace_id"] = observation.trace_id
                    cached_meta["query_fingerprint"] = observation.query_fingerprint
                    self._record_retrieval_telemetry(
                        user=user,
                        dataset_id=dataset_id,
                        query=query,
                        mode=mode,
                        top_k=top_k,
                        results=cached_results,
                        meta=cached_meta,
                        source=telemetry_source,
                    )
                    _metrics.record_retrieval(
                        mode,
                        cache_hit=True,
                        duration_seconds=time.perf_counter() - retrieve_started,
                    )
                    return cached_results, cached_meta

        results, meta = await self._retrieve_queries(
            user=user,
            dataset_id=dataset_id,
            query=query,
            top_k=top_k,
            mode=mode,
            document_id=document_id,
            dense_weight=dense_weight,
            bm25_weight=bm25_weight,
            fusion_method=fusion_method,
            rrf_k=rrf_k,
            alpha=alpha,
            score_threshold=score_threshold,
            vector_top_k=vector_top_k,
            keyword_top_k=keyword_top_k,
            candidate_top_k=candidate_top_k,
            keyword_candidate_k=keyword_candidate_k,
            fusion=fusion,
            rrf_weights=rrf_weights,
            rerank=rerank,
            rerank_model=rerank_model,
            rerank_top_n=rerank_top_n,
            mmr=mmr,
            mmr_lambda=mmr_lambda,
            mmr_threshold=mmr_threshold,
            source_type_filter=source_type_filter,
            language_filter=language_filter,
            metadata_filter=metadata_filter,
            _dataset=dataset,
        )
        meta["retrieval_cache_hit"] = False
        if cache_enabled:
            meta["retrieval_query_fingerprint"] = cache_fingerprint
            meta["dataset_revision_fingerprint"] = dataset_revision_fingerprint
            # Never cache a degraded rerank: rerank degrade is transient
            # provider/latency state, not a property of the query. Caching it
            # pins fusion order for the whole TTL and replays the degrade flag
            # as phantom telemetry on every hit; the next request should get a
            # fresh rerank attempt instead.
            if not meta.get("rerank_degraded"):
                await cache_set(cache_key, results, meta)
        # Per-request observation identity is stamped only after cache storage:
        # a cached result must never replay another request's trace_id.
        meta["trace_id"] = observation.trace_id
        meta["query_fingerprint"] = observation.query_fingerprint
        self._record_retrieval_telemetry(
            user=user,
            dataset_id=dataset_id,
            query=query,
            mode=mode,
            top_k=top_k,
            results=results,
            meta=meta,
            source=telemetry_source,
        )
        _metrics.record_retrieval(
            mode,
            cache_hit=False,
            duration_seconds=time.perf_counter() - retrieve_started,
        )
        return results, meta

    async def _retrieve_queries(
        self,
        user: UserContext,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        mode: str = "hybrid",  # "dense" | "bm25" | "hybrid"
        document_id: str | None = None,
        # Fusion parameters
        dense_weight: float | None = None,  # [0, 1] weight for dense scores
        bm25_weight: float | None = None,  # [0, 1] weight for BM25 scores
        fusion_method: str | None = None,  # "weighted" | "rrf"
        rrf_k: int | None = None,  # RRF constant
        # Legacy alpha parameter (converted to weights)
        alpha: float | None = None,
        score_threshold: float | None = None,  # Filter results below this score
        vector_top_k: int | None = None,
        keyword_top_k: int | None = None,
        candidate_top_k: int | None = None,
        keyword_candidate_k: int | None = None,
        fusion: str | None = None,  # Legacy: rrf | alpha
        rrf_weights: dict[str, float] | None = None,  # Legacy
        rerank: bool | None = None,
        rerank_model: str | None = None,
        rerank_top_n: int | None = None,
        mmr: bool | None = None,
        mmr_lambda: float | None = None,
        mmr_threshold: float | None = None,
        # Additional filters (not implemented in core retrieve, for API compatibility)
        source_type_filter: str | None = None,
        language_filter: str | None = None,
        metadata_filter: dict[str, Any] | None = None,
        # Internal batch-retrieval inputs. Public callers keep using ``query``.
        _query_specs: list[dict[str, Any]] | None = None,
        _recall_max_parallel: int | None = None,
        _dataset: dict[str, Any] | None = None,
    ) -> tuple[list[RetrieveResult], dict[str, Any]]:
        retrieval_started = time.perf_counter()
        stage_timings = {
            "dense_prepare_ms": 0.0,
            "dense_search_ms": 0.0,
            "bm25_search_ms": 0.0,
            "filter_ms": 0.0,
            "rerank_ms": 0.0,
            "mmr_ms": 0.0,
        }
        dataset = (
            _dataset
            if _dataset is not None
            else await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        )

        q = (query or "").strip()
        if not q:
            raise ValidationFailedError("query is required")

        query_specs: list[dict[str, Any]] = []
        seen_queries: set[str] = set()
        for item in _query_specs or [{"query": q}]:
            query_text = str(item.get("query") or "").strip()
            if not query_text or query_text in seen_queries:
                continue
            seen_queries.add(query_text)
            query_specs.append({**item, "query": query_text})
        if q not in seen_queries:
            query_specs.insert(0, {"query": q})
        is_multi_query = len(query_specs) > 1
        query_spec_by_text = {str(item["query"]): item for item in query_specs}
        recall_errors: dict[str, dict[str, str]] = {}
        # B6: queries whose PG FTS leg returned zero rows and reached the
        # ILIKE zero-result fallback (surfaced in meta for SLO accounting).
        fts_ilike_fallback_queries: set[str] = set()

        # Dataset-level defaults (Dify-like): index_config.retrieval.* can define
        # default retrieval behavior per dataset.
        index_config = _ensure_dict(dataset.get("index_config"))
        retrieval_defaults = _ensure_dict(index_config.get("retrieval"))
        # T9: parent-child / summary-index / structural-routing are dormant
        # dataset switches under index_config.retrieval.*; parsing them here
        # fails a bad stored config before any embedding or store call runs,
        # and returning None keeps the pipeline exactly on its legacy path.
        parent_child_settings = parse_parent_child_settings(retrieval_defaults)
        summary_index_settings = parse_summary_index_settings(retrieval_defaults)
        structural_settings = parse_structural_settings(retrieval_defaults)
        # T2-8: query_rewrite / multi_query_expansion / hyde are flag-only
        # until the T0 eval gate promotes them — parsing validates the stored
        # config (fail-closed before any store call) and yields a meta echo;
        # the pipeline itself is untouched either way.
        query_preset_report = parse_query_preset_settings(retrieval_defaults)
        lexical_config = require_shadow_only_dataset(dataset)
        retrieval_generation = dataset_retrieval_generation(dataset)
        dataset_tenant_id = str(dataset.get("tenant_id") or "").strip()
        dataset_content_revision = dataset.get("content_revision")
        # Preflight against the datasets-row collection name so a known-bad
        # collection fails before any embedding or query work runs. This is
        # NOT the authoritative scope fence: a cutover may redirect the query
        # to the serving binding's collection further down (see the
        # binding-redirect block), and every vector_store read primitive
        # re-runs require_collection_readable against the collection it
        # actually queries — that per-search check is what authorizes the
        # final target, so a stale row name here can never widen authority.
        await self._require_collection_readable(dataset, dataset_id)
        if lexical_config.reads_bm25_v2 and not dataset_tenant_id:
            raise ValidationFailedError(
                "bm25_v2 retrieval requires a non-empty dataset tenant scope"
            )
        if lexical_config.reads_bm25_v2 and (
            isinstance(dataset_content_revision, bool)
            or not isinstance(dataset_content_revision, int)
            or dataset_content_revision < 0
        ):
            raise ValidationFailedError(
                "bm25_v2 retrieval requires the current dataset content_revision"
            )

        # Enforce dataset-level retrieval config (ignore request overrides) if enabled.
        retrieval_enforce = bool(
            retrieval_defaults.get("enforce_config")
            or retrieval_defaults.get("locked")
            or retrieval_defaults.get("lock")
        )
        if retrieval_enforce:
            mode = dense_weight = bm25_weight = fusion_method = rrf_k = rrf_weights = alpha = None
            score_threshold = vector_top_k = keyword_top_k = candidate_top_k = (
                keyword_candidate_k
            ) = fusion = None
            rerank = rerank_model = rerank_top_n = None
            mmr = mmr_lambda = mmr_threshold = None
            for query_spec in query_specs:
                for key in (
                    "mode",
                    "vector_top_k",
                    "keyword_top_k",
                    "keyword_candidate_k",
                ):
                    query_spec.pop(key, None)

        # Mode: dense, bm25, or hybrid
        def _normalize_mode(value: Any) -> str:
            normalized = str(value or "hybrid").lower()
            if normalized in ("keyword", "bm25"):
                return "bm25"
            if normalized in ("vector", "dense"):
                return "dense"
            if normalized == "hybrid":
                return normalized
            raise ValidationFailedError("mode must be dense|bm25|hybrid")

        effective_mode = _normalize_mode(mode or retrieval_defaults.get("mode") or "hybrid")

        # Fusion method and weights (supports nested retrieval.fusion config)
        fusion_config = self._ks._resolve_fusion_config(
            retrieval_defaults=retrieval_defaults,
            fusion_method=fusion_method,
            fusion=fusion,
            alpha=alpha,
            dense_weight=dense_weight,
            bm25_weight=bm25_weight,
            rrf_k=rrf_k,
            rrf_weights=rrf_weights,
        )
        effective_fusion_method = fusion_config["method"]
        effective_dense_weight = fusion_config["dense_weight"]
        effective_bm25_weight = fusion_config["bm25_weight"]
        weighted_rrf_enabled = effective_fusion_method == "rrf" and (
            _has_explicit_rrf_weighting(
                retrieval_defaults=retrieval_defaults,
                dense_weight=dense_weight,
                bm25_weight=bm25_weight,
                alpha=alpha,
                rrf_weights=rrf_weights,
            )
        )

        top_k = max(int(top_k), 1)
        # SPO-04 / K3: the interactive default profile is a short hybrid
        # (12 dense + 12 lexical) without the evaluation-profile over-retrieval
        # (top_k*4 / *10 candidate expansion). Explicit request overrides and
        # dataset-level retrieval configs (balanced/accurate presets) keep
        # their own values.
        vector_k = int(
            vector_top_k
            if vector_top_k is not None
            else retrieval_defaults.get("vector_top_k") or max(top_k, _INTERACTIVE_DEFAULT_VECTOR_K)
        )
        keyword_k = int(
            keyword_top_k
            if keyword_top_k is not None
            else retrieval_defaults.get("keyword_top_k")
            or max(top_k, _INTERACTIVE_DEFAULT_KEYWORD_K)
        )
        candidate_k = int(
            candidate_top_k
            if candidate_top_k is not None
            else retrieval_defaults.get("candidate_top_k")
            or max(top_k * 2, _INTERACTIVE_DEFAULT_CANDIDATE_K)
        )
        candidate_k = max(candidate_k, top_k)
        candidate_k = min(candidate_k, 2000)

        # Keyword candidate pool for BM25 scoring.
        # Reduced from max(keyword_k*10, 200) to max(keyword_k*3, 50) because
        # tokenizing 200 documents in Python takes ~1.7s (Arabic+multilingual regex).
        # 50 candidates is sufficient for top_k=5 with good FTS ranking.
        keyword_pool_k = int(
            keyword_candidate_k
            if keyword_candidate_k is not None
            else retrieval_defaults.get("keyword_candidate_k") or max(keyword_k * 3, 50)
        )
        keyword_pool_k = max(keyword_pool_k, keyword_k)
        keyword_pool_k = min(keyword_pool_k, 500)

        # T9 (PRD T9-2): child->parent fan-out raises the effective recall
        # top-k so the post-fold parent count can still reach top_k; the
        # ceiling is the explicit parent_child.fanout_top_k config. Disabled
        # datasets keep the values above untouched.
        parent_child_report: dict[str, Any] | None = None
        if parent_child_settings is not None:
            (
                vector_k,
                keyword_k,
                candidate_k,
                keyword_pool_k,
                parent_child_report,
            ) = apply_recall_fanout(
                parent_child_settings,
                vector_k=vector_k,
                keyword_k=keyword_k,
                candidate_k=candidate_k,
                keyword_pool_k=keyword_pool_k,
                top_k=top_k,
            )

        # RRF params
        rrf_k_value = int(fusion_config["rrf_k"])

        # Rerank params (bool or dict in index_config)
        rerank_cfg = retrieval_defaults.get("rerank")
        rerank_provider = None
        if isinstance(rerank_cfg, dict):
            rerank_enabled = (
                bool(rerank_cfg.get("enabled", False)) if rerank is None else bool(rerank)
            )
            rerank_provider = str(rerank_cfg.get("provider") or "").strip() or None
            effective_rerank_model = str(rerank_model or rerank_cfg.get("model") or "qwen3-rerank")
            effective_rerank_top_n = (
                int(rerank_top_n)
                if rerank_top_n is not None
                else (int(rerank_cfg["top_n"]) if rerank_cfg.get("top_n") is not None else None)
            )
        else:
            # Rerank defaults to OFF unless explicitly configured
            rerank_enabled = bool(rerank_cfg) if rerank is None else bool(rerank)
            effective_rerank_model = str(rerank_model or "qwen3-rerank")
            effective_rerank_top_n = int(rerank_top_n) if rerank_top_n is not None else None

        from .text_reranker import (
            create_reranker,
            normalize_rerank_model,
            normalize_rerank_provider,
        )

        effective_rerank_provider = normalize_rerank_provider(
            rerank_provider, effective_rerank_model
        )
        effective_rerank_model = normalize_rerank_model(
            effective_rerank_provider, effective_rerank_model
        )

        # MMR params (bool or dict in index_config)
        mmr_cfg = retrieval_defaults.get("mmr")
        effective_mmr_fill_policy = "strict"
        if isinstance(mmr_cfg, dict):
            mmr_enabled = bool(mmr_cfg.get("enabled", False)) if mmr is None else bool(mmr)
            effective_mmr_lambda = float(
                mmr_lambda if mmr_lambda is not None else mmr_cfg.get("lambda", 0.5)
            )
            effective_mmr_threshold = (
                float(mmr_threshold)
                if mmr_threshold is not None
                else (float(mmr_cfg["threshold"]) if mmr_cfg.get("threshold") is not None else None)
            )
            effective_mmr_fill_policy = _resolve_mmr_fill_policy(mmr_cfg)
        else:
            mmr_enabled = bool(mmr_cfg) if mmr is None else bool(mmr)
            effective_mmr_lambda = float(mmr_lambda if mmr_lambda is not None else 0.5)
            effective_mmr_threshold = float(mmr_threshold) if mmr_threshold is not None else None

        # Score threshold - filter out low-relevance results (applied after fusion)
        effective_score_threshold = float(
            score_threshold
            if score_threshold is not None
            else retrieval_defaults.get("score_threshold") or 0.0
        )
        # Ensure threshold is within valid range (0 = no filtering)
        effective_score_threshold = max(0.0, min(1.0, effective_score_threshold))
        embedding_provider = str(dataset.get("embedding_provider") or "local")
        embedding_model = str(dataset.get("embedding_model") or "hash-384")
        embedding_model_version = str(
            dataset.get("embedding_model_version") or ""
        )
        embedding_config = _ensure_dict(dataset.get("embedding_config"))
        dim = int(dataset.get("embedding_dimension") or 0) or None
        collection = str(dataset.get("collection_name") or "")
        is_multimodal = False  # Rejected by the release profile preflight above.

        # PRD T3 item 1 — refuse mixed-model queries at the query boundary.
        # The serving binding (when registered) is the authoritative
        # indirection layer for which collection/generation serves this
        # dataset. The query embedder above is resolved from the datasets
        # row, and cutover flips the row and the binding atomically, so any
        # divergence here means the query would embed with a different
        # generation than the one indexed in the serving collection — which
        # returns noise, not answers. The lookup is read-only (queries never
        # mutate versioning state) and degrades open: a version-store outage
        # must not take retrieval down, only a *known* mismatch is refused.
        migration_service = getattr(self._ks, "embedding_migration_service", None)
        if migration_service is not None:
            serving_binding: dict[str, Any] | None = None
            try:
                serving_binding = await migration_service.store.get_serving_binding(
                    dataset_id
                )
            except Exception as binding_lookup_err:
                logger.warning(
                    f"T3 serving-binding lookup failed for dataset {dataset_id}; "
                    f"falling back to the datasets row: {binding_lookup_err}"
                )
            if serving_binding:
                try:
                    assert_query_embedding_identity(
                        serving_binding,
                        embedding_provider=embedding_provider,
                        embedding_model=embedding_model,
                        embedding_model_version=embedding_model_version,
                        embedding_dimension=dim,
                    )
                except MixedModelEmbeddingError as exc:
                    # A cutover transaction updates the datasets row and the
                    # serving binding atomically, but this request may have
                    # read the old row before that commit and the new binding
                    # after it. Re-read the full generation fence: if it
                    # moved, the entrypoint's bounded retry restarts from a
                    # coherent new snapshot. A stable mismatch remains a hard
                    # mixed-model failure and is never relaxed.
                    await self._require_unchanged_retrieval_generation(
                        user,
                        dataset_id,
                        retrieval_generation,
                    )
                    raise ValidationFailedError(str(exc)) from exc
                bound_collection = str(
                    serving_binding.get("collection_name") or ""
                ).strip()
                if bound_collection and bound_collection != collection:
                    # The binding is the indirection layer; cutover moves it
                    # and the datasets row together, so this only fires in a
                    # race — follow the binding, which is what actually serves.
                    collection = bound_collection

        queries_to_run = [str(item["query"]) for item in query_specs]

        # --- Parallel Dense + BM25 retrieval for better latency ---
        configured_query_concurrency = max(
            int(getattr(self.settings.knowledge, "retrieval_query_max_concurrency", 3) or 3),
            1,
        )
        requested_query_concurrency = (
            int(_recall_max_parallel)
            if _recall_max_parallel is not None
            else configured_query_concurrency
        )
        retrieval_query_concurrency = min(
            max(requested_query_concurrency, 1),
            configured_query_concurrency,
        )

        def _query_option(query_text: str, key: str, default: Any) -> Any:
            value = query_spec_by_text.get(query_text, {}).get(key)
            return default if value is None else value

        query_filter_configs = [
            (
                _query_option(query_text, "source_type_filter", source_type_filter),
                _query_option(query_text, "language_filter", language_filter),
                _query_option(query_text, "metadata_filter", metadata_filter),
            )
            for query_text in queries_to_run
        ]
        filters_vary_by_query = bool(query_filter_configs) and any(
            config != query_filter_configs[0] for config in query_filter_configs[1:]
        )

        def _matches_query_filters(query_text: str, payload: dict[str, Any]) -> bool:
            if not filters_vary_by_query:
                return True
            return bool(
                self._ks._filter_candidates_by_metadata(
                    [{"metadata": payload}],
                    _query_option(query_text, "source_type_filter", source_type_filter),
                    _query_option(query_text, "language_filter", language_filter),
                    _query_option(query_text, "metadata_filter", metadata_filter),
                )
            )

        query_modes = {
            query_text: _normalize_mode(_query_option(query_text, "mode", effective_mode))
            for query_text in queries_to_run
        }
        dense_queries = [
            query_text
            for query_text in queries_to_run
            if query_modes[query_text] in {"dense", "hybrid"}
        ]
        bm25_queries = [
            query_text
            for query_text in queries_to_run
            if query_modes[query_text] in {"bm25", "hybrid"}
        ]
        if dense_queries and bm25_queries:
            effective_mode = "hybrid"
        elif dense_queries:
            effective_mode = "dense"
        else:
            effective_mode = "bm25"

        # Resolve the final language-adaptive weights before either fusion
        # implementation runs. Native Qdrant RRF and the Python fallback must
        # consume and report the same weights for an identical request.
        adaptive_weights = bool(retrieval_defaults.get("adaptive_weights", True))
        if effective_mode == "hybrid" and adaptive_weights:
            effective_dense_weight, effective_bm25_weight = compute_language_weights(
                q,
                default_dense_weight=effective_dense_weight,
                default_bm25_weight=effective_bm25_weight,
            )

        if dataset.get("needs_reindex") and dense_queries:
            raise ValidationFailedError(
                "Dataset embeddings were migrated and require re-indexing before vector retrieval. "
                "Please re-index this dataset (or use mode='bm25' temporarily)."
            )
        # The tenant score threshold is calibrated against absolute scores
        # (dense cosine, or a reranker's relevance). Fusion scores (min-max /
        # RRF) are per-request relative values, so the threshold never
        # pre-filters the legs (they always search threshold=0 — Dify #35233
        # lesson) and, outside dense mode, is only re-armed post-rerank when
        # a reranker actually produced the final scores (PRD T2-4,
        # runbook §4.6-6 raw/calibrated boundary).
        tenant_score_threshold = effective_score_threshold
        if not self._ks._should_apply_score_threshold(effective_mode):
            effective_score_threshold = 0.0

        # Precompute query vectors for dense queries (BM25 runs in parallel)
        query_vectors: dict[str, list[float]] = {}
        dense_disabled_reason: str | None = None

        async def _dense_search(query_text: str) -> tuple[list, int]:
            """Dense (vector) retrieval task for a single query."""
            if effective_mode not in {"dense", "hybrid"}:
                return [], 0
            qvec_local = query_vectors.get(query_text)
            if not qvec_local:
                if effective_mode == "dense" and not is_multi_query:
                    raise ValidationFailedError("dense retrieval requires query embedding")
                return [], 0
            try:
                raw_hits = await self.vector_store.search(
                    collection_name=collection,
                    query_vector=qvec_local,
                    top_k=max(int(_query_option(query_text, "vector_top_k", vector_k)), 1),
                    dataset_id=dataset_id,
                    tenant_id=dataset_tenant_id,
                    document_id=_query_option(query_text, "document_id", document_id),
                    source_type=_query_option(query_text, "source_type_filter", source_type_filter),
                    language=_query_option(query_text, "language_filter", language_filter),
                    with_payload=True,
                    metadata_filter=_query_option(query_text, "metadata_filter", metadata_filter),
                )
                raw_count = len(raw_hits)
                filtered = []
                for h in raw_hits:
                    payload = dict(h.payload or {})
                    text = str(payload.get("text") or "").strip()
                    if not text or not _matches_query_filters(query_text, payload):
                        continue
                    score = float(getattr(h, "score", 0.0))
                    filtered.append(
                        {
                            "payload": payload,
                            "score": score,
                            "point_id": getattr(h, "point_id", None),
                        }
                    )
                return filtered, raw_count
            except CollectionReadAuthorityError as vec_err:
                raise ValidationFailedError(
                    f"dataset collection is not readable: {vec_err}"
                ) from vec_err
            except Exception as vec_err:
                logger.warning(f"Dense search failed: {vec_err}")
                recall_errors.setdefault(query_text, {})["dense"] = str(vec_err)
                if effective_mode == "dense":
                    raise ValidationFailedError(f"Dense search failed: {vec_err}")
                return [], 0

        async def _bm25_search(query_text: str) -> tuple[list, int]:
            """BM25 retrieval: PostgreSQL FTS candidates -> Python BM25 re-scoring.

            PostgreSQL already maintains a GIN-indexed tsvector for every segment,
            so lexical retrieval works for existing and newly ingested documents
            without a separate sparse-vector indexing pass.
            """
            if effective_mode not in {"bm25", "hybrid"}:
                return [], 0

            query_keyword_k = max(int(_query_option(query_text, "keyword_top_k", keyword_k)), 1)
            if lexical_config.reads_bm25_v2:
                if not collection:
                    raise ValidationFailedError(
                        "bm25_v2 retrieval requires a persisted Qdrant collection"
                    )
                try:
                    sparse_hits = await self.vector_store.sparse_search(
                        collection_name=collection,
                        sparse_indices=[],
                        sparse_values=[],
                        top_k=query_keyword_k,
                        dataset_id=dataset_id,
                        tenant_id=dataset_tenant_id,
                        document_id=_query_option(query_text, "document_id", document_id),
                        source_type=_query_option(
                            query_text, "source_type_filter", source_type_filter
                        ),
                        language=_query_option(query_text, "language_filter", language_filter),
                        metadata_filter=_query_option(
                            query_text, "metadata_filter", metadata_filter
                        ),
                        with_payload=True,
                        query_text=query_text,
                        lexical_config=lexical_config,
                        authority_content_revision=dataset_content_revision,
                    )
                except Exception as exc:
                    recall_errors.setdefault(query_text, {})["bm25_v2"] = str(exc)
                    raise ValidationFailedError(f"bm25_v2 sparse search failed: {exc}") from exc

                hits = []
                for hit in sparse_hits:
                    payload = dict(hit.payload or {})
                    text = str(payload.get("text") or "").strip()
                    segment_id = str(payload.get("segment_id") or hit.point_id or "")
                    if (
                        not text
                        or not segment_id
                        or not _matches_query_filters(query_text, payload)
                    ):
                        continue
                    hits.append(
                        {
                            "segment_id": segment_id,
                            "document_id": str(payload.get("document_id") or ""),
                            "text": text,
                            "metadata": payload,
                            "bm25_score": float(hit.score),
                        }
                    )
                return hits, len(sparse_hits)

            query_tokens = tokenize(query_text, keep_original=True, remove_stopwords=True)
            if not query_tokens:
                return [], 0

            # Step 1: PostgreSQL GIN FTS retrieval for candidates
            query_keyword_pool_k = max(
                int(_query_option(query_text, "keyword_candidate_k", keyword_pool_k)),
                query_keyword_k,
            )
            bm25_pool = min(query_keyword_pool_k, 80)
            try:
                raw_hits = await self.db.search_segments_text(
                    dataset_id=dataset_id,
                    tenant_id=dataset_tenant_id,
                    terms=query_tokens,
                    document_id=_query_option(query_text, "document_id", document_id),
                    source_type=_query_option(query_text, "source_type_filter", source_type_filter),
                    language=_query_option(query_text, "language_filter", language_filter),
                    limit=bm25_pool,
                    metadata_filter=_query_option(query_text, "metadata_filter", metadata_filter),
                )
            except Exception as fts_err:
                logger.warning(f"PostgreSQL FTS search failed: {fts_err}")
                recall_errors.setdefault(query_text, {})["bm25"] = str(fts_err)
                return [], 0

            if not raw_hits and _CJK_TEXT_RE.search(query_text):
                # B6 zero-result fallback. The GIN leg AND-joins
                # plainto_tsquery('simple', <jieba token>) terms, but the
                # text_search tsvector is built with to_tsvector('simple',
                # text) whose default parser emits ONE lexeme per unbroken
                # CJK run — Chinese word tokens therefore cannot match even
                # when every segment contains them, and the lexically-most
                # important leg would die with zero candidates. The
                # documented compatibility matcher is the substring ILIKE
                # path (same dataset/tenant/enabled/archived predicates as
                # the FTS leg; terms are OR-joined, which is the widening
                # intended for this tokenization mismatch). Only reached on
                # zero rows AND only for CJK-bearing queries; a legitimate
                # English zero-result query does not pay an O(N) scan.
                ilike_fallback = getattr(self.db, "_search_segments_ilike", None)
                if callable(ilike_fallback):
                    try:
                        fallback_hits = await ilike_fallback(
                            dataset_id=dataset_id,
                            tenant_id=dataset_tenant_id,
                            terms=query_tokens,
                            document_id=_query_option(query_text, "document_id", document_id),
                            source_type=_query_option(
                                query_text, "source_type_filter", source_type_filter
                            ),
                            language=_query_option(query_text, "language_filter", language_filter),
                            limit=bm25_pool,
                            metadata_filter=_query_option(
                                query_text, "metadata_filter", metadata_filter
                            ),
                        )
                    except Exception as fallback_err:
                        logger.warning(
                            "PostgreSQL FTS zero-result ILIKE fallback failed: %s",
                            fallback_err,
                        )
                        fallback_hits = []
                    if fallback_hits:
                        fts_ilike_fallback_queries.add(query_text)
                        raw_hits = fallback_hits

            if not raw_hits:
                return [], 0

            # Step 2: Python BM25 re-scoring (accurate doc-length normalization)
            valid = []
            for row in raw_hits:
                text = str(row.get("text") or "").strip()
                if text:
                    metadata = _ensure_dict(row.get("metadata"))
                    payload = {
                        "dataset_id": row.get("dataset_id"),
                        "document_id": row.get("document_id"),
                        "segment_id": row.get("segment_id"),
                        "position": row.get("position"),
                        "text": text,
                        "token_count": row.get("token_count"),
                        "source_type": row.get("source_type", "unknown"),
                        "language": row.get("language", "en"),
                        "metadata": metadata,
                        "citation_text": row.get("citation_text"),
                        "source_reference": row.get("source_reference"),
                    }
                    if _matches_query_filters(query_text, payload):
                        valid.append((row, payload, text))

            # lexical_v1 is a term-presence index. Repeated terms were
            # historically deduplicated before BM25 scoring; changing that
            # silently reorders existing datasets.
            doc_tokens = [tokenize(text) for _, _, text in valid]
            scores = bm25_scores(query_tokens, doc_tokens)

            hits = []
            for (row, payload, text), score in zip(valid, scores, strict=False):
                seg_id = str(payload.get("segment_id") or "")
                if not seg_id or score <= 0.0:
                    continue
                hits.append(
                    {
                        "segment_id": seg_id,
                        "document_id": str(row.get("document_id") or ""),
                        "text": text,
                        "metadata": payload,
                        "bm25_score": float(score),
                    }
                )
            hits.sort(key=lambda x: x.get("bm25_score", 0.0), reverse=True)
            return hits[:query_keyword_k], len(raw_hits)

        def _merge_dense_results(
            results: list[tuple[list, int]],
        ) -> tuple[list, int, dict[str, list[str]]]:
            total_raw = 0
            merged: dict[str, dict[str, Any]] = {}
            ranked_lists: dict[str, list[str]] = {}
            for (hits, raw_count), query_text in zip(results, dense_queries, strict=False):
                total_raw += raw_count
                ranked_ids: list[str] = []
                ranked_seen: set[str] = set()
                for h in hits:
                    payload = dict(h.get("payload") or {})
                    seg_id = str(payload.get("segment_id") or h.get("point_id") or "")
                    if not seg_id:
                        continue
                    if seg_id not in ranked_seen:
                        ranked_seen.add(seg_id)
                        ranked_ids.append(seg_id)
                    score = float(h.get("score") or 0.0)
                    if seg_id not in merged or score > merged[seg_id]["score"]:
                        merged[seg_id] = {
                            "payload": payload,
                            "score": score,
                            "point_id": h.get("point_id"),
                        }
                ranked_lists[query_text] = ranked_ids

            dense_hits = list(merged.values())
            dense_hits.sort(key=lambda x: x.get("score", 0.0), reverse=True)
            if len(results) == 1:
                dense_hits = dense_hits[: min(vector_k, candidate_k)]
            return dense_hits, total_raw, ranked_lists

        def _merge_bm25_results(
            results: list[tuple[list, int]],
        ) -> tuple[list, int, dict[str, list[str]]]:
            total_raw = 0
            merged: dict[str, dict[str, Any]] = {}
            ranked_lists: dict[str, list[str]] = {}
            for (hits, raw_count), query_text in zip(results, bm25_queries, strict=False):
                total_raw += raw_count
                ranked_ids: list[str] = []
                ranked_seen: set[str] = set()
                for h in hits:
                    seg_id = str(h.get("segment_id") or "")
                    if not seg_id:
                        continue
                    if seg_id not in ranked_seen:
                        ranked_seen.add(seg_id)
                        ranked_ids.append(seg_id)
                    score = float(h.get("bm25_score") or 0.0)
                    if seg_id not in merged or score > float(
                        merged[seg_id].get("bm25_score") or 0.0
                    ):
                        merged[seg_id] = h
                ranked_lists[query_text] = ranked_ids

            bm25_hits = list(merged.values())
            bm25_hits.sort(key=lambda x: x.get("bm25_score", 0.0), reverse=True)
            if len(results) == 1:
                bm25_hits = bm25_hits[: min(keyword_k, candidate_k)]
            return bm25_hits, total_raw, ranked_lists

        recall_semaphore = asyncio.Semaphore(retrieval_query_concurrency)
        embedding_semaphore = asyncio.Semaphore(retrieval_query_concurrency)

        async def _run_dense_multi() -> tuple[list, int, dict[str, list[str]]]:
            if not dense_queries:
                return [], 0, {}

            async def _run(query_text: str) -> tuple[list, int]:
                async with recall_semaphore:
                    started = time.perf_counter()
                    try:
                        return await _dense_search(query_text)
                    finally:
                        stage_timings["dense_search_ms"] += (time.perf_counter() - started) * 1000

            gathered = await asyncio.gather(
                *[_run(dq) for dq in dense_queries],
                return_exceptions=is_multi_query,
            )
            results: list[tuple[list, int]] = []
            for query_text, result in zip(dense_queries, gathered, strict=False):
                if isinstance(result, BaseException):
                    if isinstance(result, asyncio.CancelledError):
                        raise result
                    if isinstance(result, ValidationFailedError):
                        raise result
                    recall_errors.setdefault(query_text, {})["dense"] = str(result)
                    results.append(([], 0))
                else:
                    results.append(result)
            return _merge_dense_results(results)

        async def _run_bm25_multi() -> tuple[list, int, dict[str, list[str]]]:
            if not bm25_queries:
                return [], 0, {}

            async def _run(query_text: str) -> tuple[list, int]:
                async with recall_semaphore:
                    started = time.perf_counter()
                    try:
                        return await _bm25_search(query_text)
                    finally:
                        stage_timings["bm25_search_ms"] += (time.perf_counter() - started) * 1000

            gathered = await asyncio.gather(
                *[_run(bq) for bq in bm25_queries],
                return_exceptions=is_multi_query,
            )
            results: list[tuple[list, int]] = []
            for query_text, result in zip(bm25_queries, gathered, strict=False):
                if isinstance(result, BaseException):
                    if isinstance(result, asyncio.CancelledError):
                        raise result
                    if lexical_config.reads_bm25_v2:
                        raise result
                    recall_errors.setdefault(query_text, {})["bm25"] = str(result)
                    results.append(([], 0))
                else:
                    results.append(result)
            return _merge_bm25_results(results)

        # Decide if we need query embedding (dense/hybrid, or MMR without rerank).
        need_query_vector = effective_mode in {"dense", "hybrid"} or (
            mmr_enabled and not rerank_enabled
        )

        qvec: list[float] | None = None
        embedder: BaseEmbedding | None = None
        dense_prepare_started = time.perf_counter()
        if need_query_vector and dense_queries:
            try:
                # Use cached embedder to reduce first-call latency (connection reuse)
                if is_multimodal:
                    # Use UnifiedMultimodalEmbedding for cross-modal retrieval
                    logger.debug(
                        f"Using UnifiedMultimodalEmbedding for retrieval on multimodal dataset {dataset_id}"
                    )
                    embedder = await maybe_await(
                        self._ks._get_unified_multimodal_embedder(dataset, embedding_config)
                    )
                else:
                    econf = await maybe_await(
                        self._ks._resolve_embedding_config(
                            provider=embedding_provider,
                            model=embedding_model,
                            embedding_config=embedding_config,
                            tenant_id=str(dataset.get("tenant_id") or ""),
                        )
                    )
                    # Use cached embedder for better performance (connection reuse)
                    embedder = await get_cached_embedder(econf, dimension=dim)

                async def _embed_query(query_text: str) -> list[float]:
                    async with embedding_semaphore:
                        return await embedder.embed_query(query_text)

                embedded = await asyncio.gather(
                    *[_embed_query(query_text) for query_text in dense_queries],
                    return_exceptions=is_multi_query,
                )
                for query_text, result in zip(dense_queries, embedded, strict=False):
                    if isinstance(result, BaseException):
                        if isinstance(result, asyncio.CancelledError):
                            raise result
                        recall_errors.setdefault(query_text, {})["dense_prepare"] = str(result)
                        continue
                    query_vectors[query_text] = result
                qvec = query_vectors.get(q)
                if not query_vectors:
                    raise ValidationFailedError("dense retrieval requires query embedding")

                # Dataset creation and ingestion already ensure persisted collections.
                # Keep a compatibility fallback only for legacy rows missing the name.
                if not collection:
                    collection = await self.vector_store.ensure_collection(
                        dataset_id=dataset_id,
                        dimension=embedder.dimension,
                        tenant_id=dataset_tenant_id,
                        **({"lexical_config": lexical_config} if lexical_config.configured else {}),
                    )
                # Note: Don't close cached embedder - it's reused across requests

            except Exception as vec_prep_err:
                dense_disabled_reason = str(vec_prep_err)
                logger.warning(f"Vector retrieval preparation failed: {vec_prep_err}")
                if effective_mode == "dense" and not is_multi_query:
                    raise ValidationFailedError(
                        f"Dense retrieval preparation failed: {vec_prep_err}"
                    )

                for query_text in dense_queries:
                    recall_errors.setdefault(query_text, {}).setdefault(
                        "dense_prepare", str(vec_prep_err)
                    )

                # HYBRID mode: degrade to BM25-only (skip vector retrieval path).
                dense_queries = []
                query_vectors.clear()
                qvec = None
                embedder = None
                collection = ""
        if need_query_vector:
            stage_timings["dense_prepare_ms"] = (time.perf_counter() - dense_prepare_started) * 1000

        # Fast path: one Qdrant batch request, with one dense+sparse RRF per query.
        native_hybrid_used = False
        native_hybrid_error: str | None = None
        native_prefetch_count = 0
        native_rrf_count = 0
        native_rrf_ms = 0.0
        native_result_sets: list[list[Any]] = []
        native_hybrid_enabled = bool(retrieval_defaults.get("native_hybrid", True))
        if (
            native_hybrid_enabled
            and effective_mode == "hybrid"
            and effective_fusion_method == "rrf"
            and collection
            and not dense_disabled_reason
            and not filters_vary_by_query
            and all(query_modes[query_text] == "hybrid" for query_text in queries_to_run)
        ):
            native_routes = []
            for query_text in queries_to_run:
                query_vector = query_vectors.get(query_text)
                sparse_indices, sparse_values = query_to_sparse_vector(query_text)
                if not query_vector or (not lexical_config.reads_bm25_v2 and not sparse_indices):
                    native_routes = []
                    break
                native_routes.append(
                    {
                        "query_vector": query_vector,
                        "sparse_indices": sparse_indices,
                        "sparse_values": sparse_values,
                        "query_text": query_text,
                        "dense_limit": max(
                            int(_query_option(query_text, "vector_top_k", vector_k)), 1
                        ),
                        "sparse_limit": max(
                            int(_query_option(query_text, "keyword_top_k", keyword_k)), 1
                        ),
                        "document_id": _query_option(query_text, "document_id", document_id),
                        "source_type": _query_option(
                            query_text, "source_type_filter", source_type_filter
                        ),
                        "language": _query_option(query_text, "language_filter", language_filter),
                        "metadata_filter": _query_option(
                            query_text, "metadata_filter", metadata_filter
                        ),
                    }
                )
            if native_routes:
                native_started = time.perf_counter()
                try:
                    native_kwargs: dict[str, Any] = {
                        "collection_name": collection,
                        "routes": native_routes,
                        "top_k": candidate_k,
                        "with_payload": True,
                        "rrf_k": rrf_k_value,
                        "dataset_id": dataset_id,
                        "tenant_id": dataset_tenant_id,
                    }
                    if weighted_rrf_enabled:
                        native_kwargs.update(
                            dense_weight=effective_dense_weight,
                            sparse_weight=effective_bm25_weight,
                        )
                    if lexical_config.reads_bm25_v2:
                        native_kwargs["lexical_config"] = lexical_config
                        native_kwargs["authority_content_revision"] = dataset_content_revision
                    native_result_sets = await self.vector_store.hybrid_search_multi_native(
                        **native_kwargs,
                    )
                    if len(native_result_sets) != len(native_routes):
                        raise RuntimeError(
                            "Qdrant native batch returned an unexpected result count"
                        )
                    native_hybrid_used = True
                    native_prefetch_count = len(native_routes) * 2
                    native_rrf_count = len(native_routes)
                except CollectionReadAuthorityError as exc:
                    raise ValidationFailedError(
                        f"dataset collection is not readable: {exc}"
                    ) from exc
                except Exception as exc:
                    native_hybrid_error = str(exc)
                    if lexical_config.reads_bm25_v2:
                        raise ValidationFailedError(
                            f"bm25_v2 native hybrid search failed: {exc}"
                        ) from exc
                    logger.warning("Native hybrid search failed, falling back: %s", exc)
                finally:
                    native_rrf_ms = (time.perf_counter() - native_started) * 1000

        if native_hybrid_used:
            dense_hits, dense_hits_raw_count, dense_ranked_lists = [], 0, {}
            bm25_hits, bm25_hits_raw_count, bm25_ranked_lists = [], 0, {}
        else:
            dense_result, bm25_result = await asyncio.gather(
                _run_dense_multi(), _run_bm25_multi(), return_exceptions=True
            )

            if isinstance(dense_result, BaseException):
                if (
                    isinstance(
                        dense_result,
                        (asyncio.CancelledError, ValidationFailedError),
                    )
                    or effective_mode == "dense"
                ):
                    raise dense_result
                logger.warning("Dense multi-retrieval failed: %s", dense_result)
                dense_hits, dense_hits_raw_count, dense_ranked_lists = [], 0, {}
            else:
                dense_hits, dense_hits_raw_count, dense_ranked_lists = dense_result

            if isinstance(bm25_result, BaseException):
                if (
                    isinstance(bm25_result, asyncio.CancelledError)
                    or effective_mode == "bm25"
                    or lexical_config.reads_bm25_v2
                ):
                    raise bm25_result
                logger.warning("BM25 multi-retrieval failed: %s", bm25_result)
                bm25_hits, bm25_hits_raw_count, bm25_ranked_lists = [], 0, {}
            else:
                bm25_hits, bm25_hits_raw_count, bm25_ranked_lists = bm25_result

        # --- Merge candidates with clear score tracking ---
        candidates: dict[str, dict[str, Any]] = {}

        def upsert_candidate(
            segment_id: str,
            document_id: str,
            text: str,
            metadata: dict[str, Any],
            *,
            source: str,
            dense_score: float | None = None,
            bm25_score: float | None = None,
        ) -> None:
            seg_id = str(segment_id or "").strip()
            if not seg_id:
                return
            cand = candidates.get(seg_id)
            if cand is None:
                cand = {
                    "segment_id": seg_id,
                    "document_id": str(document_id or ""),
                    "text": str(text or ""),
                    "metadata": dict(metadata or {}),
                    "_sources": set(),
                    # Stage 1: Raw scores (None = N/A)
                    "_dense_score": None,
                    "_bm25_score": None,
                    # Stage 2: Normalized scores
                    "_dense_score_norm": None,
                    "_bm25_score_norm": None,
                    # Stage 3: Fusion score
                    "_fusion_score": None,
                    # Stage 4: MMR score
                    "_mmr_score": None,
                    "_mmr_relevance": None,
                    "_mmr_max_sim": None,
                    # Stage 5: Rerank score
                    "_rerank_score": None,
                    # Final score for display
                    "_final_score": 0.0,
                }
                candidates[seg_id] = cand
            if document_id and not cand.get("document_id"):
                cand["document_id"] = str(document_id)
            if text and not cand.get("text"):
                cand["text"] = str(text)
            if isinstance(metadata, dict) and metadata:
                merged = _ensure_dict(cand.get("metadata"))
                for k, v in metadata.items():
                    merged.setdefault(k, v)
                cand["metadata"] = merged

            cand["_sources"].add(source)
            if dense_score is not None:
                cand["_dense_score"] = float(dense_score)
            if bm25_score is not None:
                cand["_bm25_score"] = float(bm25_score)

        # Add dense hits
        for h in dense_hits:
            payload = dict(h.get("payload") or {})
            seg_id = str(payload.get("segment_id") or h.get("point_id") or "")
            if not seg_id:
                continue
            doc_id = str(payload.get("document_id") or "")
            text = str(payload.get("text") or "")
            upsert_candidate(
                seg_id,
                doc_id,
                text,
                payload,
                source="dense",
                dense_score=float(h.get("score") or 0.0),
            )

        # Add BM25 hits
        for h in bm25_hits:
            seg_id = str(h.get("segment_id") or "")
            upsert_candidate(
                seg_id,
                str(h.get("document_id") or ""),
                str(h.get("text") or ""),
                dict(h.get("metadata") or {}),
                source="bm25",
                bm25_score=float(h.get("bm25_score") or 0.0),
            )

        if native_hybrid_used:
            for hits in native_result_sets:
                for hit in hits:
                    payload = dict(getattr(hit, "payload", None) or {})
                    text = str(payload.get("text") or "").strip()
                    if not text:
                        continue
                    seg_id = str(payload.get("segment_id") or getattr(hit, "point_id", "") or "")
                    if not seg_id:
                        continue
                    upsert_candidate(
                        seg_id,
                        str(payload.get("document_id") or ""),
                        text,
                        payload,
                        source="qdrant_rrf",
                    )
                    score = float(getattr(hit, "score", 0.0) or 0.0)
                    candidate = candidates[seg_id]
                    current_score = candidate.get("_rrf_score_raw")
                    if current_score is None or score > float(current_score):
                        candidate["_rrf_score_raw"] = score

            native_score_max = (
                max(
                    (
                        float(candidate.get("_rrf_score_raw") or 0.0)
                        for candidate in candidates.values()
                    ),
                    default=1.0,
                )
                or 1.0
            )
            for candidate in candidates.values():
                score = float(candidate.get("_rrf_score_raw") or 0.0)
                normalized_score = score / native_score_max
                candidate["_rrf_score"] = normalized_score
                candidate["_fusion_score"] = normalized_score
                candidate["_final_score"] = normalized_score

        candidate_ids = list(candidates)
        active_candidate_ids = await self._active_segment_ids(
            dataset_id=dataset_id,
            tenant_id=dataset_tenant_id,
            segment_ids=candidate_ids,
        )
        inactive_candidate_count = len(candidate_ids) - len(active_candidate_ids)
        if inactive_candidate_count:
            candidates = {
                segment_id: candidate
                for segment_id, candidate in candidates.items()
                if segment_id in active_candidate_ids
            }

        legacy_image_candidate_count = sum(
            1 for candidate in candidates.values() if _candidate_is_image(candidate)
        )
        if legacy_image_candidate_count:
            candidates = {
                segment_id: candidate
                for segment_id, candidate in candidates.items()
                if not _candidate_is_image(candidate)
            }

        # --- Stage 2: Normalize scores to [0, 1] using robust normalization ---
        # Build score dicts for normalization
        dense_scores_dict = {
            cid: float(c.get("_dense_score") or 0)
            for cid, c in candidates.items()
            if c.get("_dense_score") is not None
        }
        bm25_scores_dict = {
            cid: float(c.get("_bm25_score") or 0)
            for cid, c in candidates.items()
            if c.get("_bm25_score") is not None
        }

        # Use robust normalization (clips outliers at 5th/95th percentile)
        # This is more stable than min-max for hybrid search
        dense_norm_dict = ScoreNormalization.robust_normalize(dense_scores_dict)
        bm25_norm_dict = ScoreNormalization.robust_normalize(bm25_scores_dict)

        # Apply normalized scores to candidates.
        for cid, cand in candidates.items():
            if cid in dense_norm_dict:
                cand["_dense_score_norm"] = dense_norm_dict[cid]
            if cid in bm25_norm_dict:
                cand["_bm25_score_norm"] = bm25_norm_dict[cid]

        # --- Compute text match info (for display only, not scoring) ---
        for cand in candidates.values():
            text = str(cand.get("text") or "")
            match_score, match_info = compute_text_match_score(q, text)
            cand["_text_match_score"] = match_score
            cand["_exact_match"] = match_info["exact_match"]
            cand["_term_matches"] = match_info["term_matches"]
            cand["_term_ratio"] = match_info.get("term_ratio", 0.0)

        # --- Stage 3: Fusion (combine dense and BM25 scores) ---
        rrf_scores: dict[str, float] | None = None
        rrf_max = 1.0
        rrf_query_count = 0
        rrf_ranked_lists = {
            **{
                f"dense:{query_text}": ranked_ids
                for query_text, ranked_ids in dense_ranked_lists.items()
            },
            **{
                f"bm25:{query_text}": ranked_ids
                for query_text, ranked_ids in bm25_ranked_lists.items()
            },
        }
        use_rrf = (
            not native_hybrid_used
            and effective_fusion_method == "rrf"
            and (effective_mode == "hybrid" or len(rrf_ranked_lists) > 1)
        )
        if use_rrf:
            rrf_scores = {}
            for query_text in queries_to_run:
                query_ranked_lists = {
                    source: ranked_lists[query_text]
                    for source, ranked_lists in (
                        ("dense", dense_ranked_lists),
                        ("bm25", bm25_ranked_lists),
                    )
                    if ranked_lists.get(query_text)
                }
                if not query_ranked_lists:
                    continue
                rrf_kwargs: dict[str, Any] = {"k": rrf_k_value}
                if weighted_rrf_enabled:
                    rrf_kwargs.update(
                        weights={
                            source: (
                                effective_dense_weight
                                if source == "dense"
                                else effective_bm25_weight
                            )
                            for source in query_ranked_lists
                        },
                        qdrant_weighted=True,
                    )
                query_scores = reciprocal_rank_fusion(
                    query_ranked_lists,
                    **rrf_kwargs,
                )
                rrf_query_count += 1
                for cid, score in query_scores.items():
                    rrf_scores[cid] = max(rrf_scores.get(cid, 0.0), score)
            rrf_max = max(rrf_scores.values()) if rrf_scores else 1.0

        weighted_dense_weight = None
        weighted_bm25_weight = None
        if effective_mode == "hybrid" and effective_fusion_method != "rrf":
            total_w = effective_dense_weight + effective_bm25_weight
            weighted_dense_weight = effective_dense_weight / total_w if total_w > 0 else 0.5
            weighted_bm25_weight = effective_bm25_weight / total_w if total_w > 0 else 0.5

        for cid, cand in candidates.items():
            if native_hybrid_used:
                continue
            dense_norm = cand.get("_dense_score_norm")
            bm25_norm = cand.get("_bm25_score_norm")

            if use_rrf:
                rrf_score_raw = float((rrf_scores or {}).get(cid, 0.0))
                rrf_score = rrf_score_raw / (rrf_max or 1.0)
                cand["_rrf_score_raw"] = rrf_score_raw
                cand["_rrf_score"] = rrf_score
                cand["_fusion_score"] = rrf_score

            elif effective_mode == "dense":
                # Dense only: use dense score
                cand["_fusion_score"] = dense_norm if dense_norm is not None else 0.0

            elif effective_mode == "bm25":
                # BM25 only: use BM25 score
                cand["_fusion_score"] = bm25_norm if bm25_norm is not None else 0.0

            else:
                # Hybrid mode: fuse scores
                if effective_fusion_method == "rrf":
                    # RRF fusion
                    rrf_score_raw = float((rrf_scores or {}).get(cid, 0.0))
                    rrf_score = rrf_score_raw / (rrf_max or 1.0)
                    cand["_rrf_score_raw"] = rrf_score_raw
                    cand["_rrf_score"] = rrf_score
                    cand["_fusion_score"] = rrf_score
                else:
                    # Weighted average fusion
                    d_val = dense_norm if dense_norm is not None else 0.0
                    b_val = bm25_norm if bm25_norm is not None else 0.0
                    d_weight = weighted_dense_weight if weighted_dense_weight is not None else 0.5
                    b_weight = weighted_bm25_weight if weighted_bm25_weight is not None else 0.5

                    # If only one source, penalize the missing score
                    sources = cand.get("_sources", set())
                    if "dense" in sources and "bm25" not in sources:
                        cand["_fusion_score"] = d_val * d_weight
                    elif "bm25" in sources and "dense" not in sources:
                        cand["_fusion_score"] = b_val * b_weight
                    else:
                        cand["_fusion_score"] = d_val * d_weight + b_val * b_weight

            # Set initial final score to fusion score
            cand["_final_score"] = cand.get("_fusion_score") or 0.0

        # T9: summary siblings fold into their original block with
        # max(block, summary) before anything else consumes the fusion score
        # space; structural routing then applies its bounded breadcrumb bonus.
        # Both mutate scores in place, so the single sort below stays the only
        # ordering pass over this score space.
        summary_index_stats: dict[str, Any] | None = None
        if summary_index_settings is not None and candidates:
            summary_index_stats = merge_summary_siblings(
                candidates,
                settings=summary_index_settings,
            )
        structural_routing_stats: dict[str, Any] | None = None
        if structural_settings is not None and candidates:
            structural_routing_stats = apply_structural_routing(
                candidates,
                q,
                settings=structural_settings,
            )

        require_recall_result(recall_errors, candidates)
        # Sort by fusion score
        ranked = sorted(
            candidates.values(), key=lambda c: float(c.get("_final_score") or 0.0), reverse=True
        )
        metadata_filter_original_count = len(ranked)
        filter_started = time.perf_counter()
        if (source_type_filter or language_filter or metadata_filter) and not filters_vary_by_query:
            ranked = self._ks._filter_candidates_by_metadata(
                ranked, source_type_filter, language_filter, metadata_filter
            )
            stage_timings["filter_ms"] = (time.perf_counter() - filter_started) * 1000
        metadata_filter_removed_count = metadata_filter_original_count - len(ranked)
        ranked = ranked[:candidate_k]
        if native_hybrid_used:
            for global_rank, candidate in enumerate(ranked, 1):
                candidate["_global_rank"] = global_rank

        bm25_v2_readiness_receipt = None
        if lexical_config.reads_bm25_v2 and bm25_queries and collection:
            receipt_getter = getattr(
                self.vector_store,
                "latest_bm25_v2_readiness_receipt",
                None,
            )
            if callable(receipt_getter):
                bm25_v2_readiness_receipt = receipt_getter(
                    collection,
                    lexical_config,
                )

        meta: dict[str, Any] = {
            "dataset_id": dataset_id,
            "mode": effective_mode,
            "top_k": int(top_k),
            "queries": queries_to_run,
            "query_count": len(queries_to_run),
            "query_modes": query_modes,
            "recall_max_parallel": retrieval_query_concurrency,
            "document_id": document_id,
            "enforce_config": retrieval_enforce,
            "lexical_version": lexical_config.active_version,
            "lexical_field": lexical_config.active_field,
            "bm25_v2_schema_fingerprint": (
                lexical_config.bm25_v2.fingerprint if lexical_config.reads_bm25_v2 else None
            ),
            "bm25_v2_filtering_profile_fingerprint": (
                lexical_config.filtering.fingerprint if lexical_config.reads_bm25_v2 else None
            ),
            "bm25_v2_readiness_verified": bm25_v2_readiness_receipt is not None,
            "bm25_v2_readiness_receipt": (
                {
                    "runtime_revision": bm25_v2_readiness_receipt.runtime_revision,
                    "point_count": bm25_v2_readiness_receipt.point_count,
                    "point_ids_sha256": bm25_v2_readiness_receipt.point_ids_sha256,
                    "source_text_sha256": (bm25_v2_readiness_receipt.source_text_sha256),
                    "backfill_receipt_sha256": (bm25_v2_readiness_receipt.backfill_receipt_sha256),
                }
                if bm25_v2_readiness_receipt is not None
                else None
            ),
            # Retrieval counts (for backward compatibility with frontend)
            "vector_hits_count": None
            if native_hybrid_used
            else (len(dense_hits) if effective_mode in {"dense", "hybrid"} else None),
            "keyword_hits_count": None
            if native_hybrid_used
            else (len(bm25_hits) if effective_mode in {"bm25", "hybrid"} else None),
            "dense_hits_count": None
            if native_hybrid_used
            else (len(dense_hits) if effective_mode in {"dense", "hybrid"} else None),
            "dense_hits_raw_count": dense_hits_raw_count
            if effective_mode in {"dense", "hybrid"} and not native_hybrid_used
            else None,
            "bm25_hits_count": None
            if native_hybrid_used
            else (len(bm25_hits) if effective_mode in {"bm25", "hybrid"} else None),
            "bm25_hits_raw_count": bm25_hits_raw_count
            if effective_mode in {"bm25", "hybrid"} and not native_hybrid_used
            else None,
            # Top K settings
            "dense_top_k": int(vector_k) if effective_mode in {"dense", "hybrid"} else None,
            "bm25_top_k": int(keyword_k) if effective_mode in {"bm25", "hybrid"} else None,
            "candidate_top_k": int(candidate_k),
            # Fusion config
            "fusion_method": effective_fusion_method
            if (effective_mode == "hybrid" or use_rrf)
            else None,
            "dense_weight": effective_dense_weight if effective_mode == "hybrid" else None,
            "bm25_weight": effective_bm25_weight if effective_mode == "hybrid" else None,
            "rrf_k": int(rrf_k_value) if effective_fusion_method == "rrf" else None,
            "rrf_ranked_list_count": native_prefetch_count
            if native_hybrid_used
            else (len(rrf_ranked_lists) if use_rrf else 0),
            "rrf_query_count": native_rrf_count if native_hybrid_used else rrf_query_count,
            "cross_query_fusion": "max"
            if effective_fusion_method == "rrf" and len(queries_to_run) > 1
            else None,
            "native_hybrid": native_hybrid_used,
            "native_prefetch_count": native_prefetch_count,
            "native_batch_request_count": 1 if native_hybrid_used else 0,
            "native_rrf_ms": round(native_rrf_ms, 2),
            "fusion_applied_by": "qdrant" if native_hybrid_used else "python",
            "fusion_semantics": (
                QDRANT_WEIGHTED_RRF_SEMANTICS if weighted_rrf_enabled else LEGACY_RRF_SEMANTICS
            )
            if effective_fusion_method == "rrf" and (effective_mode == "hybrid" or use_rrf)
            else None,
            # Post-processing config
            "rerank": bool(rerank_enabled),
            "rerank_provider": effective_rerank_provider if rerank_enabled else None,
            "rerank_model": effective_rerank_model if rerank_enabled else None,
            "mmr": bool(mmr_enabled),
            "mmr_lambda": float(effective_mmr_lambda) if mmr_enabled else None,
            "mmr_threshold": float(effective_mmr_threshold)
            if (mmr_enabled and effective_mmr_threshold is not None)
            else None,
            "mmr_fill_policy": effective_mmr_fill_policy if mmr_enabled else None,
            "score_threshold": float(effective_score_threshold)
            if effective_score_threshold > 0
            else None,
            # Embedding info
            "collection_name": collection or None,
            "embedding_provider": embedding_provider,
            "embedding_model": embedding_model,
            # Total candidates after merge
            "total_candidates": len(candidates),
            "inactive_candidates_filtered": inactive_candidate_count,
            "legacy_image_candidates_filtered": legacy_image_candidate_count,
            # Pipeline stages
            "pipeline_stages": [],
        }
        if dense_disabled_reason:
            meta["dense_disabled_reason"] = dense_disabled_reason[:500]
        if recall_errors:
            meta["recall_errors"] = recall_errors
        if fts_ilike_fallback_queries:
            meta["bm25_fts_ilike_fallback_queries"] = sorted(fts_ilike_fallback_queries)
        if native_hybrid_error:
            meta["native_hybrid_error"] = native_hybrid_error
        # T9 feature reports (only present when the dataset switch is on).
        # parent_child_report is mutated in place again after the fold stage.
        if parent_child_report is not None:
            meta["parent_child"] = parent_child_report
        if summary_index_stats is not None:
            meta["summary_index"] = summary_index_stats
        if structural_routing_stats is not None:
            meta["structural_routing"] = structural_routing_stats
        # T2-8 echo: configured-but-inert switches surface here so the admin
        # UI and telemetry can distinguish "off" from "on, gate pending".
        if query_preset_report is not None:
            meta["query_presets"] = query_preset_report

        # Log pipeline stages with details
        if native_hybrid_used:
            meta["pipeline_stages"].append(
                f"Qdrant native batch: {sum(len(hits) for hits in native_result_sets)} "
                f"route hits from {native_prefetch_count} prefetches"
            )
        elif effective_mode in {"dense", "hybrid"}:
            meta["pipeline_stages"].append(
                f"Dense retrieval: {len(dense_hits)}/{dense_hits_raw_count} results"
            )
            if dense_disabled_reason:
                meta["pipeline_stages"].append(
                    f"Dense retrieval disabled (fallback to BM25): {dense_disabled_reason[:120]}"
                )
        if not native_hybrid_used and effective_mode in {"bm25", "hybrid"}:
            meta["pipeline_stages"].append(
                f"BM25 retrieval: {len(bm25_hits)}/{bm25_hits_raw_count} results"
            )
        meta["pipeline_stages"].append(f"Merged candidates: {len(candidates)}")
        if native_hybrid_used:
            rrf_semantics = (
                QDRANT_WEIGHTED_RRF_SEMANTICS if weighted_rrf_enabled else LEGACY_RRF_SEMANTICS
            )
            meta["pipeline_stages"].append(
                f"Fusion ({rrf_semantics}, qdrant): "
                f"queries={native_rrf_count}, "
                f"cross_query=max, k={rrf_k_value}, "
                f"dense_w={effective_dense_weight:.2f}, "
                f"bm25_w={effective_bm25_weight:.2f}"
            )
        elif use_rrf:
            rrf_semantics = (
                QDRANT_WEIGHTED_RRF_SEMANTICS if weighted_rrf_enabled else LEGACY_RRF_SEMANTICS
            )
            meta["pipeline_stages"].append(
                f"Fusion ({rrf_semantics}, python): "
                f"queries={rrf_query_count}, "
                f"ranked_lists={len(rrf_ranked_lists)}, cross_query=max, "
                f"dense_w={effective_dense_weight:.2f}, "
                f"bm25_w={effective_bm25_weight:.2f}"
            )
        elif effective_mode == "hybrid":
            meta["pipeline_stages"].append(
                f"Fusion ({effective_fusion_method}): dense_w={effective_dense_weight:.2f}, bm25_w={effective_bm25_weight:.2f}"
            )
        if filters_vary_by_query:
            meta["pipeline_stages"].append("Per-query filters applied during candidate recall")
            meta["query_filters"] = [
                {
                    "query": query_text,
                    "source_type_filter": config[0],
                    "language_filter": config[1],
                    "metadata_filter": config[2],
                }
                for query_text, config in zip(queries_to_run, query_filter_configs, strict=False)
            ]
        elif source_type_filter or language_filter or metadata_filter:
            if metadata_filter_removed_count:
                meta["pipeline_stages"].append(
                    f"Metadata filter: filtered {metadata_filter_removed_count} candidates"
                )
            if source_type_filter:
                meta["source_type_filter"] = source_type_filter
            if language_filter:
                meta["language_filter"] = language_filter
            if metadata_filter:
                meta["metadata_filter"] = dict(metadata_filter)

        # Prefetch vectors for MMR in parallel with rerank to reduce latency
        mmr_vectors_task = None
        if mmr_enabled and ranked and len(ranked) > top_k and collection:
            ids_for_mmr = [
                str(c.get("segment_id") or "") for c in ranked if str(c.get("segment_id") or "")
            ]
            if ids_for_mmr:
                mmr_vectors_task = asyncio.create_task(
                    self.vector_store.retrieve_vectors(
                        collection_name=collection,
                        point_ids=ids_for_mmr,
                        tenant_id=dataset_tenant_id,
                        dataset_id=dataset_id,
                    )
                )

        # --- Stage 4: Optional rerank ---
        if rerank_enabled and ranked:
            rerank_started = time.perf_counter()
            try:

                def _resolve_dashscope_rerank_api_key() -> str | None:
                    # Unprefixed fallbacks resolve through Settings passthrough
                    # (Phase 0 getenv consolidation); ``aliyun_key`` covers the
                    # historical "Aliyun_KEY" spelling case-insensitively.
                    env = _live_settings()
                    return (
                        getattr(self.settings.knowledge.dashscope, "api_key", None)
                        or env.dashscope_api_key
                        or env.aliyun_key
                    )

                def _resolve_cohere_rerank_api_key() -> str | None:
                    return _live_settings().cohere_api_key or None

                if effective_rerank_top_n is None:
                    effective_rerank_top_n = min(len(ranked), max(top_k * 3, 20))

                api_key = None
                if effective_rerank_provider == "dashscope":
                    api_key = _resolve_dashscope_rerank_api_key()
                    if not api_key:
                        raise ValidationFailedError("dashscope api_key is required for rerank")
                elif effective_rerank_provider == "cohere":
                    api_key = _resolve_cohere_rerank_api_key()
                    if not api_key:
                        raise ValidationFailedError("cohere api_key is required for rerank")

                # Use provider-specific async reranker with caching/connection pooling
                reranker = create_reranker(
                    provider=effective_rerank_provider,
                    api_key=api_key,
                    model=effective_rerank_model,
                )
                applied_rerank_provider = effective_rerank_provider
                applied_rerank_model = effective_rerank_model
                docs = [str(c.get("text") or "") for c in ranked]

                def _rerank_budget_seconds() -> float:
                    # Couple rerank to the entrypoint's interactive deadline
                    # (PRD T2-3): the reranker's own HTTP timeout (~30s) sits
                    # outside the 3s budget, so the pipeline enforces what
                    # remains of the budget here. No active deadline (direct
                    # pipeline calls outside an entrypoint) falls back to the
                    # vector store's configured deadline.
                    remaining = remaining_interactive_budget_seconds()
                    if remaining is None:
                        remaining = getattr(
                            self.vector_store,
                            "interactive_deadline_seconds",
                            3.0,
                        )
                    return float(remaining or 0.0)

                async def _rerank_within_budget(rk: Any) -> Any:
                    budget = _rerank_budget_seconds()
                    if budget < _RERANK_MIN_BUDGET_SECONDS:
                        raise _RerankBudgetExhausted(
                            f"rerank skipped: only {budget:.3f}s of the interactive budget remains"
                        )
                    return await asyncio.wait_for(
                        rk.rerank(
                            query=q,
                            documents=docs,
                            top_n=effective_rerank_top_n,
                        ),
                        timeout=budget,
                    )

                try:
                    rerank_results = await _rerank_within_budget(reranker)
                except RuntimeError as fallback_exc:
                    # Local BGE dependency missing: fallback to DashScope rerank if key is available.
                    if effective_rerank_provider == "bge" and "FlagEmbedding" in str(fallback_exc):
                        fallback_api_key = _resolve_dashscope_rerank_api_key()
                        if not fallback_api_key:
                            raise
                        fallback_model = normalize_rerank_model("dashscope", None)
                        logger.warning(
                            "BGE reranker unavailable (%s), fallback to DashScope model=%s",
                            fallback_exc,
                            fallback_model,
                        )
                        reranker = create_reranker(
                            provider="dashscope",
                            api_key=fallback_api_key,
                            model=fallback_model,
                        )
                        applied_rerank_provider = "dashscope"
                        applied_rerank_model = fallback_model
                        rerank_results = await _rerank_within_budget(reranker)
                        meta["rerank_fallback"] = {
                            "from_provider": "bge",
                            "to_provider": "dashscope",
                            "to_model": fallback_model,
                        }
                    else:
                        raise

                reranked: list[dict[str, Any]] = []
                for r in rerank_results:
                    idx = r.index
                    score = r.relevance_score
                    if 0 <= idx < len(ranked):
                        c = ranked[idx]
                        c["_rerank_score"] = score
                        c["_final_score"] = score  # Rerank score becomes final score
                        reranked.append(c)

                # Preserve reranker order, then append untouched fallback candidates.
                if reranked:
                    reranked_ids = {id(c) for c in reranked}
                    ranked = reranked + [c for c in ranked if id(c) not in reranked_ids]
                    meta["pipeline_stages"].append(
                        f"Rerank ({applied_rerank_provider}/{applied_rerank_model}): {len(reranked)} results"
                    )
                meta["rerank_applied_provider"] = applied_rerank_provider
                meta["rerank_applied_model"] = applied_rerank_model
                meta["rerank_top_n"] = effective_rerank_top_n
                if tenant_score_threshold > 0.0:
                    # Provider relevance scores are absolute: re-arm the
                    # tenant threshold against them post-rerank (PRD T2-4).
                    effective_score_threshold = tenant_score_threshold
                    meta["score_threshold"] = float(tenant_score_threshold)
            except Exception as exc:
                # str(TimeoutError()) is empty; keep the free text meaningful.
                meta["rerank_error"] = str(exc) or type(exc).__name__
                # Machine-readable degrade signal on top of the free-text
                # rerank_error (PRD T2-3). Fusion order keeps serving.
                if isinstance(exc, _RerankBudgetExhausted):
                    meta["rerank_degraded"] = "budget_exhausted"
                elif isinstance(exc, TimeoutError):
                    meta["rerank_degraded"] = "timeout"
                else:
                    meta["rerank_degraded"] = "error"
                _metrics.record_rerank_degraded(meta["rerank_degraded"])
            finally:
                stage_timings["rerank_ms"] = (time.perf_counter() - rerank_started) * 1000

        # --- Stage 5: Optional MMR diversification ---
        final: list[dict[str, Any]] = ranked
        if mmr_enabled and ranked and len(ranked) <= top_k:
            meta["mmr_skipped"] = "candidate_count<=top_k"
            mmr_enabled = False
        if mmr_enabled and ranked:
            mmr_started = time.perf_counter()
            if not collection:
                meta["mmr_error"] = "dataset collection_name is missing"
            else:
                try:
                    ids = [
                        str(c.get("segment_id") or "")
                        for c in ranked
                        if str(c.get("segment_id") or "")
                    ]
                    if mmr_vectors_task is not None:
                        vectors = await mmr_vectors_task
                    else:
                        vectors = await self.vector_store.retrieve_vectors(
                            collection_name=collection,
                            point_ids=ids,
                            tenant_id=dataset_tenant_id,
                            dataset_id=dataset_id,
                        )

                    relevance: dict[str, float] = {}
                    for c in ranked:
                        cid = str(c.get("segment_id") or "")
                        if not cid:
                            continue
                        # Use the best available relevance score
                        if c.get("_rerank_score") is not None:
                            relevance[cid] = float(c.get("_rerank_score") or 0.0)
                        elif c.get("_fusion_score") is not None:
                            relevance[cid] = float(c.get("_fusion_score") or 0.0)
                        elif qvec is not None and cid in vectors:
                            relevance[cid] = cosine_similarity(qvec, vectors[cid])
                        else:
                            relevance[cid] = float(c.get("_final_score") or 0.0)

                    ordered_ids = sorted(
                        ids, key=lambda x: float(relevance.get(x, 0.0)), reverse=True
                    )
                    # B5 (PRD T1-8): fill-remaining used to live here and
                    # re-added candidates that the diversity pass had
                    # rejected, silently undoing similarity_threshold. The
                    # decision now belongs to mmr_select's fill_policy:
                    # "strict" (default) returns fewer than top_k rather
                    # than resurrecting rejected candidates; "fill" restores
                    # the legacy append-by-relevance behaviour explicitly.
                    selected_ids, picks = mmr_select(
                        ordered_ids,
                        relevance,
                        vectors,
                        top_k=top_k,
                        lambda_mult=effective_mmr_lambda,
                        similarity_threshold=effective_mmr_threshold,
                        fill_policy=effective_mmr_fill_policy,
                    )

                    if (
                        effective_mmr_fill_policy == "strict"
                        and len(selected_ids) < int(top_k)
                        and len(ordered_ids) > len(selected_ids)
                    ):
                        meta["mmr_diversity_shortfall"] = {
                            "requested": int(top_k),
                            "selected": len(selected_ids),
                            "candidates": len(ordered_ids),
                        }

                    cand_by_id = {str(c.get("segment_id") or ""): c for c in ranked}
                    out: list[dict[str, Any]] = []
                    for cid in selected_ids[:top_k]:
                        c = cand_by_id.get(cid)
                        if not c:
                            continue
                        pick = picks.get(cid)
                        if pick is not None:
                            c["_mmr_score"] = float(pick.mmr_score)
                            c["_mmr_relevance"] = float(pick.relevance)
                            c["_mmr_max_sim"] = float(pick.max_sim_to_selected)
                            # MMR relevance becomes final score (mmr_score can be negative)
                            c["_final_score"] = float(pick.relevance)
                        else:
                            c["_mmr_relevance"] = float(relevance.get(cid, 0.0))
                            c["_final_score"] = float(relevance.get(cid, 0.0))
                        out.append(c)
                    final = out
                    meta["pipeline_stages"].append(
                        f"MMR diversification: {len(out)} results "
                        f"(lambda={effective_mmr_lambda}, "
                        f"fill_policy={effective_mmr_fill_policy})"
                    )
                except CollectionReadAuthorityError as exc:
                    raise ValidationFailedError(
                        f"dataset collection is not readable: {exc}"
                    ) from exc
                except Exception as exc:
                    meta["mmr_error"] = str(exc)
            stage_timings["mmr_ms"] = (time.perf_counter() - mmr_started) * 1000

        # --- Build response ---
        # ``final`` is already ordered by fusion, reranker, or MMR. Do not mix
        # their incompatible score spaces with another sort.
        final_sorted = list(final or [])

        # T9 (PRD T9-2): fold child hits to their parents AFTER rerank/MMR
        # and before the threshold + top_k truncation — the cut is taken at
        # parent level, never at child level (the Dify truncation bug this
        # design rejects). Parent text comes from the scoped, active-only
        # segment authority.
        if parent_child_settings is not None and final_sorted:
            final_sorted, parent_child_fold_stats = await fold_candidates_to_parents(
                final_sorted,
                settings=parent_child_settings,
                db=self.db,
                dataset_id=dataset_id,
                tenant_id=dataset_tenant_id,
                max_concurrency=retrieval_query_concurrency,
            )
            if parent_child_report is not None:
                parent_child_report["fold"] = parent_child_fold_stats
            meta["pipeline_stages"].append(
                "Parent-child fold: "
                f"{parent_child_fold_stats.get('child_hits', 0)} child hits -> "
                f"{parent_child_fold_stats.get('parents', 0)} parent results "
                f"(mode={parent_child_fold_stats.get('mode')}, "
                f"unresolved_parents={parent_child_fold_stats.get('unresolved_parents', 0)})"
            )

        # Apply the score threshold to final results (PRD T2-4). The tenant
        # threshold is absolute-scale calibrated, so it only governs finals
        # on a calibrated scale: dense cosine, or rerank relevance after a
        # rerank was served (re-armed above). Untouched rerank-tail
        # candidates keep the fusion-order fallback; fusion-only finals
        # (hybrid/bm25 without a served rerank) skip the filter explicitly
        # instead of mis-scaling it (runbook §4.6-6 score boundary,
        # Dify-addendum T2-6).
        if tenant_score_threshold > 0.0 and effective_score_threshold == 0.0:
            meta["score_threshold_skipped"] = "uncalibrated_final_score"
        elif effective_score_threshold > 0.0:
            rerank_served = meta.get("rerank_applied_provider") is not None
            original_count = len(final_sorted)
            final_sorted = [
                c
                for c in final_sorted
                if (
                    rerank_served and "_rerank_score" not in c  # tail never saw the rerank scale
                )
                or float(c.get("_final_score") or 0.0) >= effective_score_threshold
            ]
            if len(final_sorted) < original_count:
                meta["pipeline_stages"].append(
                    f"Score threshold ({effective_score_threshold}): filtered {original_count - len(final_sorted)} low-score results"
                )

        final_sorted = final_sorted[:top_k]

        # Normalize final scores for display (keep raw for debugging)
        if final_sorted:
            raw_score_map = {
                str(c.get("segment_id") or idx): float(c.get("_final_score") or 0.0)
                for idx, c in enumerate(final_sorted)
            }
            norm_map = ScoreNormalization.robust_normalize(raw_score_map)
            for idx, c in enumerate(final_sorted):
                key = str(c.get("segment_id") or idx)
                raw_score = float(c.get("_final_score") or 0.0)
                c["_final_score_raw"] = raw_score
                c["_final_score_norm"] = float(norm_map.get(key, 0.0))

        # Build result candidates first (to collect image URLs for presigned generation)
        result_candidates: list[dict[str, Any]] = []
        for rank, c in enumerate(final_sorted, 1):
            seg_id = str(c.get("segment_id") or "")
            payload = dict(c.get("metadata") or {})

            # Attach sources - convert set to sorted list
            sources = c.get("_sources") or set()
            if isinstance(sources, set):
                # Keep original source names for frontend compatibility
                payload["_sources"] = sorted(str(s) for s in sources)
            elif isinstance(sources, list):
                payload["_sources"] = sources
            else:
                payload["_sources"] = []

            # Ensure source_type reflects post-processed classification
            if c.get("source_type"):
                payload["source_type"] = c.get("source_type")

            # Stage 1: Raw scores (keep both new and old field names for compatibility)
            dense_raw = c.get("_dense_score")
            bm25_raw = c.get("_bm25_score")

            # New field names
            payload["_dense_score"] = round(dense_raw, 4) if dense_raw is not None else "N/A"
            payload["_bm25_score"] = round(bm25_raw, 4) if bm25_raw is not None else "N/A"

            # OLD field names for backward compatibility
            if dense_raw is not None:
                payload["_vector_score"] = round(dense_raw, 4)
            if bm25_raw is not None:
                payload["_keyword_score"] = round(bm25_raw, 4)

            # Stage 2: Normalized scores
            dense_norm = c.get("_dense_score_norm")
            bm25_norm = c.get("_bm25_score_norm")
            payload["_dense_score_norm"] = round(dense_norm, 4) if dense_norm is not None else "N/A"
            payload["_bm25_score_norm"] = round(bm25_norm, 4) if bm25_norm is not None else "N/A"

            # Stage 3: Fusion score
            fusion = c.get("_fusion_score")
            payload["_fusion_score"] = round(fusion, 4) if fusion is not None else "N/A"
            if c.get("_rrf_score") is not None:
                payload["_rrf_score"] = round(c.get("_rrf_score"), 4)
            if c.get("_rrf_score_raw") is not None:
                payload["_rrf_score_raw"] = round(c.get("_rrf_score_raw"), 6)
            if c.get("_global_rank") is not None:
                payload["global_rank"] = int(c["_global_rank"])

            # Stage 4: Rerank score
            rerank = c.get("_rerank_score")
            payload["_rerank_score"] = round(rerank, 4) if rerank is not None else "N/A"

            # Stage 5: MMR scores
            mmr = c.get("_mmr_score")
            mmr_rel = c.get("_mmr_relevance")
            mmr_max = c.get("_mmr_max_sim")
            payload["_mmr_score"] = round(mmr, 4) if mmr is not None else "N/A"
            payload["_mmr_relevance"] = round(mmr_rel, 4) if mmr_rel is not None else "N/A"
            payload["_mmr_max_sim"] = round(mmr_max, 4) if mmr_max is not None else "N/A"

            # Also keep old name for compatibility
            if mmr_rel is not None:
                payload["_relevance_score"] = round(mmr_rel, 4)

            # Text match info
            payload["_text_match_score"] = c.get("_text_match_score")
            payload["_exact_match"] = c.get("_exact_match")
            payload["_term_matches"] = c.get("_term_matches")
            payload["_term_ratio"] = c.get("_term_ratio")

            # Pre-formatted citation/source metadata when supplied by ingestion.
            if c.get("citation_text"):
                payload["citation_text"] = c["citation_text"]

            # Rank
            payload["_rank"] = rank

            # Final score for display
            score = float(c.get("_final_score_norm") or 0.0)
            payload["_final_score_raw"] = round(float(c.get("_final_score_raw") or 0.0), 6)
            payload["_final_score"] = round(score, 6)

            # Extract multimodal fields from payload/metadata
            content_type = payload.get("content_type", "text")
            raw_image_url = payload.get("image_url")
            vlm_description = payload.get("vlm_description")

            result_candidates.append(
                {
                    "seg_id": seg_id,
                    "document_id": str(c.get("document_id") or ""),
                    "score": score,
                    "text": str(c.get("text") or ""),
                    "payload": payload,
                    "content_type": content_type,
                    "raw_image_url": raw_image_url,
                    "vlm_description": vlm_description,
                }
            )

        # Generate presigned URLs for image results (Text-First RAG)
        async def get_presigned_url_for_result(cand: dict[str, Any]) -> str | None:
            """Generate presigned URL for an image result."""
            content_type = cand.get("content_type")
            raw_url = cand.get("raw_image_url")
            seg_id = cand.get("seg_id")

            if content_type == "image" and raw_url:
                # Use presigned URL for S3/OSS, API endpoint for local
                return await self._ks._get_presigned_image_url(raw_url, seg_id)
            elif raw_url:
                # For non-image content with image URLs, use simple normalization
                return self._ks._normalize_local_image_url(raw_url, seg_id)
            return None

        # Generate presigned URLs in parallel
        presigned_tasks = [get_presigned_url_for_result(c) for c in result_candidates]
        presigned_urls = await asyncio.gather(*presigned_tasks, return_exceptions=True)

        # Build final results with presigned URLs
        results: list[RetrieveResult] = []
        for cand, presigned_url in zip(result_candidates, presigned_urls, strict=False):
            if isinstance(presigned_url, asyncio.CancelledError):
                raise presigned_url
            if isinstance(presigned_url, BaseException):
                logger.debug("Presigned URL generation failed: %s", presigned_url)
                presigned_url = None
            payload = cand["payload"]
            image_url = presigned_url or cand.get("raw_image_url")

            # Update payload with normalized/presigned URL
            if image_url and image_url != cand.get("raw_image_url"):
                payload["image_url"] = image_url
                # Also add presigned_url field for clarity
                if cand.get("content_type") == "image":
                    payload["image_presigned_url"] = image_url

            results.append(
                RetrieveResult(
                    segment_id=cand["seg_id"],
                    document_id=cand["document_id"],
                    score=cand["score"],
                    text=cand["text"],
                    metadata=payload,
                    content_type=cand["content_type"],
                    image_url=image_url,
                    vlm_description=cand["vlm_description"],
                )
            )

        stage_timings["total_ms"] = (time.perf_counter() - retrieval_started) * 1000
        meta["timings_ms"] = {key: round(value, 2) for key, value in stage_timings.items()}
        await self._require_unchanged_retrieval_generation(
            user,
            dataset_id,
            retrieval_generation,
        )
        return results, meta

    # ========================================================================
    # Multimodal Retrieval v1
    # ========================================================================

    @_with_interactive_qdrant_budget
    async def retrieve_with_images(
        self,
        user: UserContext,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        include_images: bool = True,
        content_type_filter: str | None = None,
        multimodal_rerank: bool = False,
        # Advanced multimodal parameters
        image_search_enabled: bool = True,
        vlm_rerank_weight: float | None = None,
        image_boost: float | None = None,
        image_score_threshold: float | None = None,
        use_separate_thresholds: bool = False,
        **kwargs: Any,
    ) -> tuple[list[RetrieveResult], dict[str, Any]]:
        """
        Retrieve with associated images attached to results.

        This is the multimodal-aware retrieval method that:
        1. Performs standard retrieval (dense/bm25/hybrid) with unified embedding
        2. Applies separate score thresholds for text vs image content
        3. Optionally boosts image results
        4. Attaches associated images to text segments
        5. Optionally performs multimodal reranking via VLM
        """
        _require_bounded_retrieval_request(
            query=query,
            top_k=top_k,
            vector_top_k=kwargs.get("vector_top_k"),
            keyword_top_k=kwargs.get("keyword_top_k"),
            candidate_top_k=kwargs.get("candidate_top_k"),
            keyword_candidate_k=kwargs.get("keyword_candidate_k"),
            rerank_top_n=kwargs.get("rerank_top_n"),
            rrf_k=kwargs.get("rrf_k"),
            dense_weight=kwargs.get("dense_weight"),
            bm25_weight=kwargs.get("bm25_weight"),
            alpha=kwargs.get("alpha"),
            score_threshold=kwargs.get("score_threshold"),
            mmr_lambda=kwargs.get("mmr_lambda"),
            mmr_threshold=kwargs.get("mmr_threshold"),
            rrf_weights=kwargs.get("rrf_weights"),
        )
        raise ValidationFailedError("multimodal retrieval is unavailable in this release")

        _ = image_search_enabled

        # Fetch more results if filtering to ensure we get enough after filter
        # Also fetch more if we're applying separate thresholds or boosting
        effective_top_k = (
            top_k * 3 if (content_type_filter or use_separate_thresholds) else top_k * 2
        )

        # Filter out kwargs that retrieve() doesn't support
        # These are multimodal-specific or UI-specific parameters
        unsupported_kwargs = {
            "image_search_enabled",
            "vlm_rerank_weight",
            "image_boost",
            "image_score_threshold",
            "use_separate_thresholds",
        }
        filtered_kwargs = {k: v for k, v in kwargs.items() if k not in unsupported_kwargs}

        # Perform standard retrieval (now with unified multimodal embedding)
        dataset = await self._ks.require_dataset_access(
            user,
            dataset_id,
            required="viewer",
        )
        retrieval_generation = dataset_retrieval_generation(dataset)
        results, meta = await self._retrieve_queries(
            user=user,
            dataset_id=dataset_id,
            query=query,
            top_k=effective_top_k,
            _dataset=dataset,
            **filtered_kwargs,
        )

        # Debug: Log content types from base retrieve
        content_types_before = {}
        for r in results:
            ct = r.metadata.get("content_type", getattr(r, "content_type", "text"))
            content_types_before[ct] = content_types_before.get(ct, 0) + 1
        logger.info(
            f"[retrieve_with_images] Base retrieve returned {len(results)} results: {content_types_before}"
        )

        # Apply separate thresholds for text vs image content if requested
        if use_separate_thresholds and results:
            # Handle None values explicitly - kwargs.get returns None if key exists with None value
            raw_text_threshold = kwargs.get("score_threshold")
            text_threshold = raw_text_threshold if raw_text_threshold is not None else 0.3
            img_threshold = image_score_threshold if image_score_threshold is not None else 0.2

            filtered_results = []
            for r in results:
                content_type = r.metadata.get("content_type", getattr(r, "content_type", "text"))
                threshold = img_threshold if content_type == "image" else text_threshold
                if r.score >= threshold:
                    filtered_results.append(r)
            results = filtered_results
            meta["separate_thresholds"] = True
            meta["text_threshold"] = text_threshold
            meta["image_threshold"] = img_threshold

        # Apply image boost if specified
        if image_boost and image_boost != 1.0 and results:
            for r in results:
                content_type = r.metadata.get("content_type", getattr(r, "content_type", "text"))
                if content_type == "image":
                    # Create new result with boosted score
                    min(r.score * image_boost, 1.0)
                    # Update the result's score (RetrieveResult is mutable via metadata)
                    r.metadata["_original_score"] = r.score
                    r.metadata["_boosted"] = True
                    # Note: RetrieveResult score is set at creation, so we track in metadata
            # Re-sort by effective score (original for text, boosted for images)
            results.sort(
                key=lambda r: (
                    min(r.score * image_boost, 1.0)
                    if r.metadata.get("content_type", getattr(r, "content_type", "text")) == "image"
                    else r.score
                ),
                reverse=True,
            )
            meta["image_boost"] = image_boost

        # Apply content_type_filter if specified
        if content_type_filter and content_type_filter in ("text", "image"):
            filtered_results = []
            for r in results:
                segment_content_type = r.metadata.get(
                    "content_type", getattr(r, "content_type", "text")
                )
                if segment_content_type == content_type_filter:
                    filtered_results.append(r)
            results = filtered_results[:top_k]
            meta["content_type_filter"] = content_type_filter
            meta["filtered_count"] = len(filtered_results)

        if not include_images or not results:
            await self._require_unchanged_retrieval_generation(
                user,
                dataset_id,
                retrieval_generation,
            )
            return results, meta

        # Get segment IDs that might have associated images
        segment_ids = [r.segment_id for r in results]

        # Batch fetch associated images
        associations = await self.db.get_segment_associations_batch(
            segment_ids,
            dataset_id=dataset_id,
            tenant_id=str(dataset.get("tenant_id") or ""),
        )

        # Enhance results with associated images
        enhanced_results: list[RetrieveResult] = []
        for r in results:
            # Create enhanced metadata with images
            enhanced_meta = dict(r.metadata)

            # Build associated images list
            associated_imgs: list[dict[str, Any]] = []
            if r.segment_id in associations and associations[r.segment_id]:
                associated_imgs = [
                    {
                        "image_segment_id": img["image_segment_id"],
                        "storage_url": self._ks._normalize_local_image_url(
                            img.get("storage_url", ""),
                            img.get("image_segment_id"),
                        ),
                        "filename": img.get("filename", ""),
                        "vlm_description": img.get("vlm_description"),
                        "proximity_score": float(img.get("proximity_score", 1.0)),
                        "media_type": img.get("media_type", "image/png"),
                    }
                    for img in associations[r.segment_id]
                ]
                enhanced_meta["has_images"] = True
                enhanced_meta["image_count"] = len(associated_imgs)
            else:
                enhanced_meta["has_images"] = False
                enhanced_meta["image_count"] = 0

            # Get content_type from metadata or original result
            content_type = r.metadata.get("content_type", getattr(r, "content_type", "text"))
            image_url = self._ks._normalize_local_image_url(
                r.metadata.get("image_url", getattr(r, "image_url", None)),
                r.segment_id,
            )
            vlm_description = r.metadata.get("vlm_description", getattr(r, "vlm_description", None))

            enhanced_results.append(
                RetrieveResult(
                    segment_id=r.segment_id,
                    document_id=r.document_id,
                    score=r.score,
                    text=r.text,
                    metadata=enhanced_meta,
                    # P3: Multimodal fields
                    content_type=content_type,
                    image_url=image_url,
                    vlm_description=vlm_description,
                    associated_images=tuple(associated_imgs),
                )
            )

        # Update meta to indicate multimodal retrieval
        meta["multimodal"] = True
        meta["include_images"] = include_images

        # Count segments with images
        segments_with_images = sum(
            1 for r in enhanced_results if r.metadata.get("has_images", False)
        )
        meta["segments_with_images"] = segments_with_images

        # Apply multimodal reranking if requested
        if multimodal_rerank and self._ks.vlm_service:
            try:
                from .multimodal_reranker import MultimodalReranker, RerankCandidate

                # Use configurable VLM rerank weight (default 0.4)
                effective_vlm_weight = vlm_rerank_weight if vlm_rerank_weight is not None else 0.4

                # Create reranker instance with configurable weight
                reranker = MultimodalReranker(
                    vlm_service=self._ks.vlm_service,
                    max_concurrent=3,
                    timeout_seconds=30.0,
                    image_weight=effective_vlm_weight,
                )
                meta["vlm_rerank_weight"] = effective_vlm_weight

                # Convert results to rerank candidates
                rerank_candidates: list[RerankCandidate] = []
                for r in enhanced_results:
                    # Determine media type
                    media_type = "image" if r.content_type == "image" else "text"

                    # For image segments, we need to load image bytes
                    image_bytes = None
                    if media_type == "image" and r.image_url:
                        try:
                            # Try to load from storage service if available
                            if self._ks.image_storage_service:
                                # Extract storage key from URL or use image_url directly
                                # For now, try downloading from URL
                                async with httpx.AsyncClient(timeout=10.0) as client:
                                    response = await client.get(r.image_url)
                                    response.raise_for_status()
                                    image_bytes = response.content
                        except Exception as load_err:
                            logger.debug(f"Could not load image for reranking: {load_err}")

                    candidate = RerankCandidate(
                        segment_id=r.segment_id,
                        text=r.text if media_type == "text" else None,
                        image_url=r.image_url,
                        image_bytes=image_bytes,
                        media_type=media_type,
                        original_score=r.score,
                        metadata=r.metadata,
                    )
                    rerank_candidates.append(candidate)

                # Perform reranking
                logger.info(f"Applying multimodal reranking to {len(rerank_candidates)} candidates")
                reranked = await reranker.rerank(
                    query=query,
                    candidates=rerank_candidates,
                    top_k=top_k,
                    rerank_images_only=False,
                    score_threshold=0.0,
                )

                # Map reranked results back to RetrieveResult format
                {c.segment_id: c for c in reranked}
                reranked_results: list[RetrieveResult] = []

                for candidate in reranked:
                    # Find original result
                    original = next(
                        (r for r in enhanced_results if r.segment_id == candidate.segment_id), None
                    )
                    if not original:
                        continue

                    # Update score with rerank score
                    reranked_results.append(
                        RetrieveResult(
                            segment_id=original.segment_id,
                            document_id=original.document_id,
                            score=candidate.rerank_score,  # Use reranked score
                            text=original.text,
                            metadata=original.metadata,
                            content_type=original.content_type,
                            image_url=original.image_url,
                            vlm_description=original.vlm_description,
                            associated_images=original.associated_images,
                        )
                    )

                enhanced_results = reranked_results
                meta["multimodal_rerank"] = True
                meta["multimodal_rerank_count"] = len(reranked_results)
                logger.info(f"Multimodal reranking completed: {len(reranked_results)} results")

            except Exception as rerank_err:
                logger.warning(f"Multimodal reranking failed: {rerank_err}")
                meta["multimodal_rerank"] = False
                meta["multimodal_rerank_error"] = str(rerank_err)
        elif multimodal_rerank and not self._ks.vlm_service:
            logger.warning("Multimodal reranking requested but VLM service not available")
            meta["multimodal_rerank"] = False
            meta["multimodal_rerank_message"] = "VLM service not configured"

        # Truncate to original top_k (effective_top_k was expanded for filtering headroom)
        enhanced_results = enhanced_results[:top_k]
        await self._require_unchanged_retrieval_generation(
            user,
            dataset_id,
            retrieval_generation,
        )
        return enhanced_results, meta

    # ========================================================================
    # Multimodal Retrieval v2 — hierarchical with intent-aware VLM reranking
    # ========================================================================

    @_with_interactive_qdrant_budget
    async def retrieve_with_images_v2(
        self,
        user: UserContext,
        dataset_id: str,
        query: str,
        top_k: int = 5,
        intent: str = "general",  # "general" | "find_image" | "find_document"
        vlm_rerank: bool = True,  # Whether to enable VLM reranking
        include_images: bool = True,  # Whether to attach associated images
        **kwargs: Any,
    ) -> tuple[list[RetrieveResult], dict[str, Any]]:
        """
        Hierarchical multimodal retrieval v2 with intent-aware VLM reranking.

        This enhanced retrieval method implements a two-stage pipeline:
        1. Expanded recall phase: Retrieve `top_k * 2.5` candidates using hybrid search
        2. VLM reranking phase: Apply VLM-based reranking for image results (conditional)
        """
        _require_bounded_retrieval_request(
            query=query,
            top_k=top_k,
            vector_top_k=kwargs.get("vector_top_k"),
            keyword_top_k=kwargs.get("keyword_top_k"),
            candidate_top_k=kwargs.get("candidate_top_k"),
            keyword_candidate_k=kwargs.get("keyword_candidate_k"),
            rerank_top_n=kwargs.get("rerank_top_n"),
            rrf_k=kwargs.get("rrf_k"),
            dense_weight=kwargs.get("dense_weight"),
            bm25_weight=kwargs.get("bm25_weight"),
            alpha=kwargs.get("alpha"),
            score_threshold=kwargs.get("score_threshold"),
            mmr_lambda=kwargs.get("mmr_lambda"),
            mmr_threshold=kwargs.get("mmr_threshold"),
            rrf_weights=kwargs.get("rrf_weights"),
        )
        if bool(include_images) or bool(vlm_rerank) or str(intent).lower() == "find_image":
            raise ValidationFailedError("multimodal retrieval is unavailable in this release")

        # Validate intent parameter
        valid_intents = {"general", "find_image", "find_document"}
        if intent not in valid_intents:
            logger.warning(f"Invalid intent '{intent}', defaulting to 'general'")
            intent = "general"

        # Stage 1: Expanded recall - fetch more candidates for better reranking pool
        # Use 2.5x expansion for general/find_image, less for find_document
        expansion_factor = 2.5 if intent != "find_document" else 2.0
        expanded_top_k = int(top_k * expansion_factor)

        # Configure retrieval mode - use hybrid search (Dense + BM25 + RRF) by default
        retrieve_kwargs = {
            "mode": kwargs.get("mode", "hybrid"),
            "fusion_method": kwargs.get("fusion_method", "rrf"),
            **{k: v for k, v in kwargs.items() if k not in ("mode", "fusion_method")},
        }
        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        require_shadow_only_dataset(dataset)
        retrieval_generation = dataset_retrieval_generation(dataset)
        await self._require_collection_readable(dataset, dataset_id)
        # The cache is safe only when it is bound to the authoritative content
        # revision and retrieval-effective Dataset configuration. This existing
        # fingerprint also includes the versioned lexical profile, so a
        # lexical_v1 -> bm25_v2 cutover cannot reuse legacy results.
        from .dataset_service import _dataset_revision_fingerprint

        dataset_revision_fingerprint = _dataset_revision_fingerprint(dataset)
        normalized_query = " ".join((query or "").strip().split())
        cache_fingerprint_payload = {
            "user_id": user.user_id,
            "dataset_id": dataset_id,
            "dataset_revision_fingerprint": dataset_revision_fingerprint,
            "query": normalized_query,
            "intent": intent,
            "top_k": int(top_k),
            "expanded_top_k": int(expanded_top_k),
            "include_images": bool(include_images),
            "vlm_rerank": bool(vlm_rerank),
            "retrieve_kwargs": retrieve_kwargs,
            "strict_section_traceability": bool(
                retrieve_kwargs.get("strict_section_traceability")
                or retrieve_kwargs.get("strict_traceability")
                or False
            ),
        }
        retrieval_query_fingerprint = self._ks._compute_retrieval_query_fingerprint(
            cache_fingerprint_payload
        )
        retrieval_cache_key = (
            f"{user.user_id}:{dataset_id}:{retrieval_query_fingerprint}:intent={intent}"
        )
        if dataset_revision_fingerprint is not None:
            cached_response = await self._ks._get_cached_retrieval(retrieval_cache_key)
            if cached_response is not None:
                cached_results, cached_meta = cached_response
                cached_ids = [
                    str(getattr(result, "segment_id", "") or "").strip()
                    for result in cached_results
                    if str(getattr(result, "segment_id", "") or "").strip()
                ]
                active_cached_ids = await self._active_segment_ids(
                    dataset_id=dataset_id,
                    tenant_id=str(dataset.get("tenant_id") or ""),
                    segment_ids=cached_ids,
                )
                if active_cached_ids != set(cached_ids):
                    cached_response = None
                else:
                    cached_meta["retrieval_cache_hit"] = True
                    cached_meta["retrieval_query_fingerprint"] = retrieval_query_fingerprint
                    cached_meta["dataset_revision_fingerprint"] = dataset_revision_fingerprint
                    # Same contract as the standard retrieve() cache path:
                    # a cache hit performed no rerank, so it never reports a
                    # rerank-degraded state (see retrieve() for the rationale).
                    cached_meta.pop("rerank_degraded", None)
                    cached_meta.pop("rerank_error", None)
                    await self._require_unchanged_retrieval_generation(
                        user,
                        dataset_id,
                        retrieval_generation,
                    )
                    return cached_results, cached_meta

        # Perform base retrieval with expanded top_k
        results, meta = await self._retrieve_queries(
            user=user,
            dataset_id=dataset_id,
            query=query,
            top_k=expanded_top_k,
            _dataset=dataset,
            **retrieve_kwargs,
        )

        # Add v2 metadata
        meta["retrieval_version"] = "v2"
        meta["intent"] = intent
        meta["expanded_top_k"] = expanded_top_k
        meta["original_top_k"] = top_k
        meta["retrieval_cache_hit"] = False
        meta["retrieval_query_fingerprint"] = retrieval_query_fingerprint
        meta["dataset_revision_fingerprint"] = dataset_revision_fingerprint

        # Log retrieval statistics
        content_type_counts: dict[str, int] = {}
        for r in results:
            ct = r.metadata.get("content_type", getattr(r, "content_type", "text"))
            content_type_counts[ct] = content_type_counts.get(ct, 0) + 1
        logger.info(f"[retrieve_v2] Stage 1 returned {len(results)} results: {content_type_counts}")
        meta["stage1_content_types"] = content_type_counts

        if not results:
            # Same no-degraded-rerank-to-cache rule as retrieve().
            if dataset_revision_fingerprint is not None and not meta.get("rerank_degraded"):
                await self._ks._set_cached_retrieval(retrieval_cache_key, results, meta)
            await self._require_unchanged_retrieval_generation(
                user,
                dataset_id,
                retrieval_generation,
            )
            return results, meta

        # Stage 2: VLM reranking (conditional)
        # Skip VLM reranking if:
        # - vlm_rerank is False
        # - intent is "find_document" (user wants text content, not images)
        # - VLM service is not available
        should_vlm_rerank = (
            vlm_rerank and intent != "find_document" and self._ks.vlm_service is not None
        )

        if should_vlm_rerank:
            try:
                from .multimodal_reranker import MultimodalReranker, RerankCandidate

                # Configure reranker based on intent
                # find_image: Higher image weight (0.5) for aggressive image prioritization
                # general: Balanced weight (0.4)
                image_weight = 0.5 if intent == "find_image" else 0.4
                assert 0.0 <= image_weight <= 1.0, (
                    f"image_weight must be in [0.0, 1.0], got {image_weight}"
                )

                reranker = MultimodalReranker(
                    vlm_service=self._ks.vlm_service,
                    max_concurrent=3,
                    timeout_seconds=30.0,
                    image_weight=image_weight,
                    image_storage_service=self._ks.image_storage_service,
                )

                # Separate results by content type
                image_results: list[RetrieveResult] = []
                text_results: list[RetrieveResult] = []

                for r in results:
                    content_type = r.metadata.get(
                        "content_type", getattr(r, "content_type", "text")
                    )
                    if content_type == "image":
                        image_results.append(r)
                    else:
                        text_results.append(r)

                logger.info(
                    f"[retrieve_v2] Stage 2: {len(image_results)} images, "
                    f"{len(text_results)} text candidates for VLM reranking"
                )

                # Only rerank image results if there are any
                reranked_image_results: list[RetrieveResult] = []
                if image_results:
                    # Convert image results to RerankCandidate format
                    rerank_candidates: list[RerankCandidate] = []
                    for r in image_results:
                        # Load image bytes if we have a URL
                        image_bytes = None
                        if r.image_url and self._ks.image_storage_service:
                            try:
                                # Try to load image bytes for VLM analysis
                                async with httpx.AsyncClient(timeout=10.0) as client:
                                    response = await client.get(r.image_url)
                                    response.raise_for_status()
                                    image_bytes = response.content
                            except Exception as load_err:
                                logger.debug(f"Could not load image for reranking: {load_err}")

                        candidate = RerankCandidate(
                            segment_id=r.segment_id,
                            text=r.vlm_description,  # Use VLM description for context
                            image_url=r.image_url,
                            image_bytes=image_bytes,
                            media_type="image",
                            original_score=r.score,
                            metadata=r.metadata,
                        )
                        rerank_candidates.append(candidate)

                    # Perform VLM reranking on image candidates
                    reranked_candidates = await reranker.rerank(
                        query=query,
                        candidates=rerank_candidates,
                        top_k=len(rerank_candidates),  # Keep all for merging
                        rerank_images_only=True,
                        score_threshold=0.0,
                    )

                    # Convert back to RetrieveResult format with updated scores
                    candidate_map = {c.segment_id: c for c in reranked_candidates}
                    for r in image_results:
                        if r.segment_id in candidate_map:
                            reranked_score = candidate_map[r.segment_id].rerank_score
                            # Create new result with updated score
                            reranked_image_results.append(
                                RetrieveResult(
                                    segment_id=r.segment_id,
                                    document_id=r.document_id,
                                    score=reranked_score,
                                    text=r.text,
                                    metadata={
                                        **r.metadata,
                                        "_original_score": r.score,
                                        "_vlm_reranked": True,
                                    },
                                    content_type=r.content_type,
                                    image_url=r.image_url,
                                    vlm_description=r.vlm_description,
                                    associated_images=r.associated_images,
                                )
                            )

                    meta["vlm_rerank_applied"] = True
                    meta["vlm_rerank_count"] = len(reranked_image_results)
                    meta["vlm_image_weight"] = image_weight

                # Merge text and reranked image results
                all_results = text_results + reranked_image_results
                # Sort by score descending
                all_results.sort(key=lambda x: x.score, reverse=True)
                results = all_results

                logger.info(f"[retrieve_v2] After VLM reranking: {len(results)} merged results")

            except Exception as rerank_err:
                logger.warning(f"[retrieve_v2] VLM reranking failed: {rerank_err}")
                meta["vlm_rerank_applied"] = False
                meta["vlm_rerank_error"] = str(rerank_err)
        else:
            # Log why VLM reranking was skipped
            if not vlm_rerank:
                meta["vlm_rerank_skipped"] = "disabled"
            elif intent == "find_document":
                meta["vlm_rerank_skipped"] = "intent_is_find_document"
            elif not self._ks.vlm_service:
                meta["vlm_rerank_skipped"] = "vlm_service_unavailable"

        # Truncate to final top_k
        results = results[:top_k]

        # Stage 3: Attach associated images (same as retrieve_with_images)
        if include_images and results:
            segment_ids = [r.segment_id for r in results]
            associations = await self.db.get_segment_associations_batch(
                segment_ids,
                dataset_id=dataset_id,
                tenant_id=str(dataset.get("tenant_id") or ""),
            )

            enhanced_results: list[RetrieveResult] = []
            for r in results:
                enhanced_meta = dict(r.metadata)

                # Build associated images list
                associated_imgs: list[dict[str, Any]] = []
                if r.segment_id in associations and associations[r.segment_id]:
                    associated_imgs = [
                        {
                            "image_segment_id": img["image_segment_id"],
                            "storage_url": self._ks._normalize_local_image_url(
                                img.get("storage_url", ""),
                                img.get("image_segment_id"),
                            ),
                            "filename": img.get("filename", ""),
                            "vlm_description": img.get("vlm_description"),
                            "proximity_score": float(img.get("proximity_score", 1.0)),
                            "media_type": img.get("media_type", "image/png"),
                        }
                        for img in associations[r.segment_id]
                    ]
                    enhanced_meta["has_images"] = True
                    enhanced_meta["image_count"] = len(associated_imgs)
                else:
                    enhanced_meta["has_images"] = False
                    enhanced_meta["image_count"] = 0

                # Get content_type from metadata or original result
                content_type = r.metadata.get("content_type", getattr(r, "content_type", "text"))
                image_url = self._ks._normalize_local_image_url(
                    r.metadata.get("image_url", getattr(r, "image_url", None)),
                    r.segment_id,
                )
                vlm_description = r.metadata.get(
                    "vlm_description", getattr(r, "vlm_description", None)
                )

                enhanced_results.append(
                    RetrieveResult(
                        segment_id=r.segment_id,
                        document_id=r.document_id,
                        score=r.score,
                        text=r.text,
                        metadata=enhanced_meta,
                        content_type=content_type,
                        image_url=image_url,
                        vlm_description=vlm_description,
                        associated_images=tuple(associated_imgs),
                    )
                )

            results = enhanced_results

            # Update metadata
            segments_with_images = sum(1 for r in results if r.metadata.get("has_images", False))
            meta["segments_with_images"] = segments_with_images
            meta["include_images"] = True

        # Final statistics
        final_content_types: dict[str, int] = {}
        for r in results:
            ct = r.metadata.get("content_type", getattr(r, "content_type", "text"))
            final_content_types[ct] = final_content_types.get(ct, 0) + 1
        meta["final_content_types"] = final_content_types
        meta["final_count"] = len(results)

        logger.info(
            f"[retrieve_v2] Final: {len(results)} results, content_types={final_content_types}"
        )

        # Same no-degraded-rerank-to-cache rule as retrieve().
        if dataset_revision_fingerprint is not None and not meta.get("rerank_degraded"):
            await self._ks._set_cached_retrieval(retrieval_cache_key, results, meta)
        await self._require_unchanged_retrieval_generation(
            user,
            dataset_id,
            retrieval_generation,
        )
        return results, meta

    # ========================================================================
    # Batch Retrieval
    # ========================================================================

    @_with_interactive_qdrant_budget
    async def retrieve_batch(
        self,
        user: UserContext,
        dataset_id: str,
        queries: list[Any],
        top_k: int | None = None,
        mode: str = "hybrid",
        document_id: str | None = None,
        dense_weight: float | None = None,
        bm25_weight: float | None = None,
        fusion_method: str | None = None,
        alpha: float | None = None,
        score_threshold: float | None = None,
        source_type_filter: str | None = None,
        language_filter: str | None = None,
        vector_top_k: int | None = None,
        keyword_top_k: int | None = None,
        candidate_top_k: int | None = None,
        keyword_candidate_k: int | None = None,
        fusion: str | None = None,
        rrf_k: int | None = None,
        rrf_weights: dict[str, float] | None = None,
        rerank: bool | None = None,
        rerank_model: str | None = None,
        rerank_top_n: int | None = None,
        mmr: bool | None = None,
        mmr_lambda: float | None = None,
        mmr_threshold: float | None = None,
        include_images: bool = True,
        include_associated_images: bool = True,
        max_parallel: int = 10,
        dedupe_results: bool = False,
    ) -> tuple[list[dict[str, Any]], dict[str, Any]]:
        """Retrieve one global result set from multiple recall queries.

        Args:
            queries: Original query first, followed by optional rewrites.
            max_parallel: Maximum concurrent dense/BM25 recall operations.
            dedupe_results: Retained for compatibility; global dedupe is always enabled.
            ... (same params as retrieve)

        Returns:
            Tuple of (batch_results, meta). ``batch_results`` contains one
            globally fused {query, results, meta} result group.
        """
        _require_bounded_batch_request(queries, max_parallel=max_parallel)
        _require_bounded_retrieval_request(
            query="batch",
            top_k=top_k,
            vector_top_k=vector_top_k,
            keyword_top_k=keyword_top_k,
            candidate_top_k=candidate_top_k,
            keyword_candidate_k=keyword_candidate_k,
            rerank_top_n=rerank_top_n,
            rrf_k=rrf_k,
            dense_weight=dense_weight,
            bm25_weight=bm25_weight,
            alpha=alpha,
            score_threshold=score_threshold,
            mmr_lambda=mmr_lambda,
            mmr_threshold=mmr_threshold,
            rrf_weights=rrf_weights,
            scope="retrieval batch request",
        )
        _ = include_images, include_associated_images
        start_time = time.time()

        def _normalize_query_spec(item: Any) -> dict[str, Any] | None:
            if isinstance(item, str):
                query_text = item.strip()
                return {"query": query_text} if query_text else None
            if isinstance(item, dict):
                query_text = str(item.get("query") or "").strip()
                if not query_text:
                    return None
                normalized = {"query": query_text}
                for key in (
                    "document_id",
                    "mode",
                    "dense_weight",
                    "bm25_weight",
                    "fusion_method",
                    "alpha",
                    "score_threshold",
                    "source_type_filter",
                    "language_filter",
                    "vector_top_k",
                    "keyword_top_k",
                    "candidate_top_k",
                    "keyword_candidate_k",
                    "fusion",
                    "rrf_k",
                    "rerank",
                    "rerank_model",
                    "rerank_top_n",
                    "mmr",
                    "mmr_lambda",
                    "mmr_threshold",
                    "include_images",
                    "include_associated_images",
                    "metadata_filter",
                ):
                    if item.get(key) is not None:
                        normalized[key] = item[key]
                return normalized
            return None

        valid_specs = [spec for spec in (_normalize_query_spec(q) for q in queries) if spec]
        if not valid_specs:
            return [], {"error": "No valid queries provided"}

        unique_specs: list[dict[str, Any]] = []
        seen_queries: set[str] = set()
        for spec in valid_specs:
            query_text = str(spec["query"])
            if query_text in seen_queries:
                continue
            seen_queries.add(query_text)
            unique_specs.append(spec)

        primary_spec = unique_specs[0]
        primary_query = str(primary_spec["query"])
        resolved_top_k = (
            max(int(top_k), 1)
            if top_k is not None
            else MULTI_QUERY_TOP_K.get(min(len(unique_specs), 5), 10)
        )

        def _primary_option(key: str, default: Any) -> Any:
            value = primary_spec.get(key)
            return default if value is None else value

        dataset = await self._ks.require_dataset_access(user, dataset_id, required="viewer")
        require_shadow_only_dataset(dataset)
        retrieval_generation = dataset_retrieval_generation(dataset)
        await self._require_collection_readable(dataset, dataset_id)
        retrieve_started = time.perf_counter()
        try:
            results, pipeline_meta = await self._retrieve_queries(
                user=user,
                dataset_id=dataset_id,
                query=primary_query,
                top_k=resolved_top_k,
                mode=_primary_option("mode", mode),
                document_id=document_id,
                dense_weight=_primary_option("dense_weight", dense_weight),
                bm25_weight=_primary_option("bm25_weight", bm25_weight),
                fusion_method=_primary_option("fusion_method", fusion_method),
                alpha=_primary_option("alpha", alpha),
                score_threshold=_primary_option("score_threshold", score_threshold),
                source_type_filter=source_type_filter,
                language_filter=language_filter,
                vector_top_k=vector_top_k,
                keyword_top_k=keyword_top_k,
                candidate_top_k=_primary_option("candidate_top_k", candidate_top_k),
                keyword_candidate_k=keyword_candidate_k,
                fusion=_primary_option("fusion", fusion),
                rrf_k=_primary_option("rrf_k", rrf_k),
                rrf_weights=rrf_weights,
                rerank=_primary_option("rerank", rerank),
                rerank_model=_primary_option("rerank_model", rerank_model),
                rerank_top_n=_primary_option("rerank_top_n", rerank_top_n),
                mmr=_primary_option("mmr", mmr),
                mmr_lambda=_primary_option("mmr_lambda", mmr_lambda),
                mmr_threshold=_primary_option("mmr_threshold", mmr_threshold),
                _query_specs=unique_specs,
                _recall_max_parallel=max(int(max_parallel), 1),
                _dataset=dataset,
            )
        except (ValidationFailedError, PermissionDeniedError, IndexLeaseUnavailableError):
            raise
        except _RetrievalGenerationChanged:
            raise
        except CollectionReadAuthorityError as exc:
            raise ValidationFailedError(f"dataset collection is not readable: {exc}") from exc
        except Exception as exc:
            raise_batch_failure(exc)

        retrieve_time_ms = (time.perf_counter() - retrieve_started) * 1000
        pipeline_meta = dict(pipeline_meta or {})
        pipeline_meta.update(
            {
                "queries": [str(spec["query"]) for spec in unique_specs],
                "input_query_count": len(valid_specs),
                "unique_query_count": len(unique_specs),
                "duplicate_query_count": len(valid_specs) - len(unique_specs),
                "final_top_k": resolved_top_k,
                "queue_wait_ms": 0.0,
                "retrieve_time_ms": round(retrieve_time_ms, 2),
            }
        )

        serialized_results = [
            {
                "segment_id": result.segment_id,
                "document_id": result.document_id,
                "score": result.score,
                "text": result.text,
                "metadata": result.metadata,
                "content_type": getattr(result, "content_type", "text"),
                "image_url": getattr(result, "image_url", None),
                "vlm_description": getattr(result, "vlm_description", None),
                "associated_images": list(getattr(result, "associated_images", ()) or ()),
            }
            for result in results[:resolved_top_k]
        ]
        batch_results = [
            {
                "query": primary_query,
                "results": serialized_results,
                "meta": pipeline_meta,
            }
        ]

        execution_time_ms = (time.time() - start_time) * 1000
        meta = {
            "total_queries": len(valid_specs),
            "unique_queries": len(unique_specs),
            "final_top_k": resolved_top_k,
            "total_results": len(serialized_results),
            "execution_time_ms": round(execution_time_ms, 2),
            "max_parallel": int(
                pipeline_meta.get("recall_max_parallel") or max(int(max_parallel), 1)
            ),
            "dedupe_results": True,
            "dedupe_results_requested": dedupe_results,
            "avg_queue_wait_ms": 0.0,
            "max_queue_wait_ms": 0.0,
        }

        observation = new_query_observation(primary_query)
        meta["trace_id"] = observation.trace_id
        meta["query_fingerprint"] = observation.query_fingerprint
        pipeline_meta["trace_id"] = observation.trace_id
        pipeline_meta["query_fingerprint"] = observation.query_fingerprint
        self._record_retrieval_telemetry(
            user=user,
            dataset_id=dataset_id,
            query=primary_query,
            mode=str(_primary_option("mode", mode)),
            top_k=resolved_top_k,
            results=serialized_results,
            meta={**pipeline_meta, **meta},
            source="retrieve_batch",
        )

        await self._require_unchanged_retrieval_generation(
            user,
            dataset_id,
            retrieval_generation,
        )
        return batch_results, meta
