from __future__ import annotations

import json
import math
import uuid
from datetime import datetime
from enum import Enum
from typing import Annotated, Any, Literal

from pydantic import BaseModel, ConfigDict, Field, field_validator, model_validator
from typing_extensions import Self

# ============================================================
# Enums
# ============================================================


class HierarchicalStrategy(str, Enum):
    """Hierarchical retrieval strategies."""

    CASCADE = "cascade"
    PARALLEL = "parallel"
    ADAPTIVE = "adaptive"


# ============================================================
# Dataset Schemas
# ============================================================


class DatasetCreateSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    dataset_id: str | None = Field(default=None, min_length=1, max_length=128)
    name: str = Field(min_length=1, max_length=200)
    description: str | None = Field(default="", max_length=8_000)
    visibility: str = "private"  # private|tenant|public

    # KB type and use case (multimodal support)
    kb_type: str = "document"  # document|data|image|audio_video
    use_case: str = "basic_qa"  # basic_qa|rich_text_response

    # The open-source quickstart is DashScope-first.  Optional providers remain
    # selectable, but an omitted provider must work with the only required key.
    embedding_provider: str = Field(default="dashscope", min_length=1, max_length=64)
    embedding_model: str = Field(default="text-embedding-v4", min_length=1, max_length=256)
    embedding_dimension: int | None = Field(default=1024, ge=1, le=8192)
    embedding_config: dict[str, Any] = Field(default_factory=dict)

    index_config: dict[str, Any] = Field(default_factory=dict)
    collection_name: str | None = Field(default=None, min_length=1, max_length=255)
    indexing_technique: str = "high_quality"  # high_quality|economy

    # Optional process rules
    process_rule: dict[str, Any] | None = None


class DatasetUpdateSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    name: str | None = Field(default=None, min_length=1, max_length=200)
    description: str | None = Field(default=None, max_length=8_000)
    visibility: str | None = None
    kb_type: str | None = None  # document|data|image|audio_video
    use_case: str | None = None  # basic_qa|rich_text_response
    embedding_provider: str | None = Field(default=None, min_length=1, max_length=64)
    embedding_model: str | None = Field(default=None, min_length=1, max_length=256)
    embedding_dimension: int | None = Field(default=None, ge=1, le=8192)
    embedding_config: dict[str, Any] | None = None
    index_config: dict[str, Any] | None = None
    indexing_technique: str | None = None


class DatasetPermissionGrantSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    subject_type: str  # user|role
    subject_id: str
    permission: str  # owner|editor|viewer


class DatasetDeleteSchema(BaseModel):
    """Knowledge dataset delete confirmation payload."""

    model_config = ConfigDict(extra="allow")

    confirmation: dict[str, Any] | None = Field(default=None, alias="_gateway_delete_confirmation")
    reason: str | None = Field(default=None, max_length=500)


# ============================================================
# Process Rule Schemas (Dify-style)
# ============================================================


class PreProcessingRuleSchema(BaseModel):
    id: str  # remove_extra_spaces|remove_urls_emails|remove_stopwords
    enabled: bool = True


class SegmentationConfigSchema(BaseModel):
    separator: str = "\n"
    max_tokens: int = 500  # token limit for embedding models
    chunk_overlap: int = 50  # legacy overlap (tokens)


class ProcessRuleSchema(BaseModel):
    """Processing rule configuration - matches Dify's rule format"""

    model_config = ConfigDict(extra="allow")

    mode: str = "automatic"  # automatic|custom|hierarchical
    pre_processing_rules: list[PreProcessingRuleSchema] = Field(default_factory=list)
    segmentation: SegmentationConfigSchema = Field(default_factory=SegmentationConfigSchema)

    # Hierarchical mode specific
    parent_mode: str | None = None  # paragraph|full_doc
    child_chunk_size: int | None = None


# ============================================================
# Document Schemas
# ============================================================


class DocumentCreateTextSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    title: str = Field(min_length=1, max_length=512)
    content: str = Field(min_length=1, max_length=200_000)
    metadata: dict[str, Any] = Field(default_factory=dict, max_length=64)

    # Optional process rule override
    process_rule: ProcessRuleSchema | None = None
    doc_form: str = "text_model"  # text_model|qa_model
    doc_language: str | None = Field(default=None, max_length=64)


class DocumentCreateUrlSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    url: str = Field(min_length=1, max_length=2_048)
    title: str | None = Field(default=None, max_length=512)
    metadata: dict[str, Any] = Field(default_factory=dict, max_length=64)

    # Optional process rule override
    process_rule: ProcessRuleSchema | None = None


class DocumentBatchCreateSchema(BaseModel):
    """Batch document creation schema"""

    model_config = ConfigDict(extra="allow")

    documents: list[DocumentCreateTextSchema] = Field(min_length=1, max_length=50)
    process_rule: ProcessRuleSchema | None = None
    batch_name: str | None = Field(default=None, max_length=256)

    @model_validator(mode="after")
    def _validate_aggregate_text_size(self) -> Self:
        if sum(len(document.content) for document in self.documents) > 2_000_000:
            raise ValueError("batch document content exceeds 2000000 characters")
        return self


class DocumentUpdateSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    title: str | None = Field(default=None, min_length=1, max_length=512)
    metadata: dict[str, Any] | None = Field(default=None, max_length=64)
    metadata_patch: dict[str, Any] | None = Field(default=None, max_length=32)
    metadata_remove: list[str] | None = Field(default=None, max_length=32)
    metadata_schema_revision: int | None = Field(default=None, ge=0)
    doc_type: str | None = Field(default=None, max_length=64)
    doc_language: str | None = Field(default=None, max_length=64)

    @model_validator(mode="after")
    def _validate_metadata_edit_shape(self) -> Self:
        uses_patch = self.metadata_patch is not None or self.metadata_remove is not None
        if uses_patch and self.metadata is not None:
            raise ValueError("metadata replacement and metadata merge-patch are mutually exclusive")
        if uses_patch and any(
            value is not None for value in (self.title, self.doc_type, self.doc_language)
        ):
            raise ValueError("metadata merge-patch must be submitted separately")
        if uses_patch and self.metadata_schema_revision is None:
            raise ValueError("metadata_schema_revision is required for metadata merge-patch")
        if self.metadata_schema_revision is not None and not uses_patch:
            raise ValueError("metadata_schema_revision requires metadata_patch or metadata_remove")
        return self


class DocumentMetadataFieldSchema(BaseModel):
    model_config = ConfigDict(extra="forbid")

    name: str = Field(pattern=r"^[a-z][a-z0-9_]{0,63}$")
    label: str = Field(min_length=1, max_length=128)
    type: str = Field(pattern=r"^(string|number|datetime)$")
    description: str | None = Field(default=None, max_length=512)


class DocumentMetadataRegistryUpdateSchema(BaseModel):
    model_config = ConfigDict(extra="forbid")

    expected_revision: int = Field(ge=0)
    fields: list[DocumentMetadataFieldSchema] = Field(default_factory=list, max_length=32)


class DocumentMetadataBatchUpdateSchema(BaseModel):
    model_config = ConfigDict(extra="forbid")

    document_ids: list[Annotated[str, Field(min_length=1, max_length=255)]] = Field(
        min_length=1,
        max_length=500,
    )
    metadata_patch: dict[str, Any] = Field(default_factory=dict, max_length=32)
    metadata_remove: list[str] = Field(default_factory=list, max_length=32)
    metadata_schema_revision: int = Field(ge=0)

    @model_validator(mode="after")
    def _validate_non_empty_patch(self) -> Self:
        if not self.metadata_patch and not self.metadata_remove:
            raise ValueError("metadata_patch or metadata_remove is required")
        if len(set(self.document_ids)) != len(self.document_ids):
            raise ValueError("document_ids must be unique")
        return self


class DocumentEnableDisableSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    enabled: bool


class DocumentArchiveSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    archived: bool
    # Matches database/schema.sql archived_reason VARCHAR(255) and the web
    # counter; reject before persistence rather than surfacing a database 500.
    reason: str | None = Field(default=None, max_length=255)


def _validate_retrieval_fusion_parameters(value: Any) -> None:
    """Validate request-level fusion knobs before either retrieval path sees them."""

    rrf_k = getattr(value, "rrf_k", None)
    if rrf_k is not None and (
        isinstance(rrf_k, bool) or not 1 <= int(rrf_k) <= 10_000
    ):
        raise ValueError("rrf_k must be between 1 and 10000")

    resolved_weights: list[tuple[str, float]] = []
    for field_name in ("dense_weight", "bm25_weight"):
        field_value = getattr(value, field_name, None)
        if field_value is None:
            continue
        numeric = float(field_value)
        if not math.isfinite(numeric) or numeric < 0.0:
            raise ValueError(f"{field_name} must be finite and non-negative")
        resolved_weights.append((field_name, numeric))
    if len(resolved_weights) == 2 and not any(weight > 0.0 for _, weight in resolved_weights):
        raise ValueError("at least one dense_weight or bm25_weight must be positive")

    alpha = getattr(value, "alpha", None)
    if alpha is not None:
        numeric_alpha = float(alpha)
        if not math.isfinite(numeric_alpha) or not 0.0 <= numeric_alpha <= 1.0:
            raise ValueError("alpha must be finite and between 0 and 1")

    rrf_weights = getattr(value, "rrf_weights", None)
    if isinstance(rrf_weights, dict) and rrf_weights:
        normalized_weights: list[float] = []
        for weight in rrf_weights.values():
            numeric = float(weight)
            if not math.isfinite(numeric) or numeric < 0.0:
                raise ValueError("rrf_weights values must be finite and non-negative")
            normalized_weights.append(numeric)
        if not any(weight > 0.0 for weight in normalized_weights):
            raise ValueError("at least one rrf_weights value must be positive")


def _reject_unreleased_multimodal_options(value: Any) -> None:
    """Keep every public retrieval contract text-only for this release."""

    if (
        bool(getattr(value, "include_images", False))
        or bool(getattr(value, "include_associated_images", False))
        or bool(getattr(value, "multimodal_rerank", False))
        or bool(getattr(value, "use_separate_thresholds", False))
        or str(getattr(value, "content_type_filter", "") or "").strip().lower()
        == "image"
        or getattr(value, "vlm_rerank_weight", None) is not None
        or getattr(value, "image_boost", None) is not None
        or getattr(value, "image_score_threshold", None) is not None
    ):
        raise ValueError("multimodal retrieval is not enabled for this release")

    supplied_fields = getattr(value, "model_fields_set", set())
    if (
        "image_search_enabled" in supplied_fields
        and bool(getattr(value, "image_search_enabled", False))
    ):
        raise ValueError("multimodal retrieval is not enabled for this release")


class RetrieveRequestSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    query: str = Field(min_length=1, max_length=4096)
    top_k: int = Field(default=5, ge=1, le=100)
    mode: str = "hybrid"  # dense|bm25|hybrid (also accepts: keyword|vector for backwards compat)
    document_id: str | None = Field(default=None, min_length=1, max_length=256)

    # Fusion weights for hybrid mode (0-1, sum to 1)
    dense_weight: float | None = None  # Weight for dense (vector) scores
    bm25_weight: float | None = None  # Weight for BM25 (keyword) scores
    fusion_method: str | None = None  # "weighted" or "rrf"

    # Legacy alpha parameter (converted to weights: alpha = dense_weight)
    alpha: float | None = None

    # Score threshold to filter out low-relevance results (0.0 - 1.0)
    # Default 0.0 means no filtering. Recommended: 0.3-0.5 for keyword, 0.5-0.7 for vector
    score_threshold: float | None = None

    # Advanced request overrides; dataset defaults live in index_config.retrieval.
    vector_top_k: int | None = Field(default=None, ge=1, le=1000)
    keyword_top_k: int | None = Field(default=None, ge=1, le=1000)
    candidate_top_k: int | None = Field(default=None, ge=1, le=2000)
    keyword_candidate_k: int | None = Field(default=None, ge=1, le=500)

    fusion: str | None = None  # Legacy: rrf|alpha (hybrid only)
    rrf_k: int | None = Field(default=None, ge=1, le=10_000)
    rrf_weights: dict[str, float] = Field(default_factory=dict, max_length=16)  # Legacy

    rerank: bool | None = None
    rerank_model: str | None = Field(default=None, min_length=1, max_length=256)
    rerank_top_n: int | None = Field(default=None, ge=1, le=1000)

    mmr: bool | None = None
    mmr_lambda: float | None = None
    mmr_threshold: float | None = None

    # Multimodal retrieval options
    include_images: bool = False  # Include image segments in results
    include_associated_images: bool = False  # Attach associated images to text segments
    multimodal_rerank: bool = False  # Use VLM for multimodal reranking
    content_type_filter: str | None = None  # Filter by content type: text|image|None (all)

    # Advanced multimodal parameters (Phase 2 optimization)
    image_search_enabled: bool = False  # Reserved until multimodal serving is released
    vlm_rerank_weight: float | None = None  # Weight of VLM score (0.0-1.0), default 0.4
    image_boost: float | None = None  # Boost factor for image results (>1 = prefer images)
    image_score_threshold: float | None = None  # Score threshold for images (lower than text)
    use_separate_thresholds: bool = False  # Use different thresholds for text vs image

    # Source/metadata filters
    source_type_filter: str | None = Field(default=None, max_length=128)
    language_filter: str | None = Field(default=None, max_length=64)
    metadata_filter: dict[str, Any] | None = Field(default=None, max_length=64)

    # Hierarchical retrieval options (for large document collections)
    hierarchical: bool = False  # Enable hierarchical 3-level retrieval
    hierarchical_strategy: HierarchicalStrategy = HierarchicalStrategy.CASCADE
    l1_top_k: int = Field(default=5, ge=1, le=100)
    l2_top_k: int = Field(default=10, ge=1, le=200)
    include_context: bool = True  # Include parent context in L3 results

    @model_validator(mode="after")
    def _validate_production_fusion_parameters(self) -> Self:
        _validate_retrieval_fusion_parameters(self)
        _reject_unreleased_multimodal_options(self)
        return self


RetrievalEvalGrade = Annotated[
    float,
    Field(ge=0.0, le=5.0, allow_inf_nan=False),
]
RetrievalEvalK = Annotated[int, Field(ge=1, le=100)]


class RetrievalEvalCaseSchema(BaseModel):
    """One retrieval-evaluation case: a query plus its ground-truth relevance.

    ``relevant_segment_ids`` (binary) and ``relevance`` (graded id->score in
    [0, 5]) may both be supplied; they are merged, with graded values taking
    precedence. At least one relevant segment should be provided for the case to
    contribute meaningfully to the metrics.
    """

    model_config = ConfigDict(extra="forbid")

    query: str = Field(min_length=1, max_length=4096)
    case_id: str | None = Field(default=None, min_length=1, max_length=128)
    relevant_segment_ids: list[str] = Field(default_factory=list, max_length=500)
    relevance: dict[str, RetrievalEvalGrade] = Field(default_factory=dict, max_length=500)

    @field_validator("query", "case_id", mode="before")
    @classmethod
    def _strip_eval_text(cls, value: Any) -> Any:
        if isinstance(value, str):
            return value.strip()
        return value

    @field_validator("relevant_segment_ids")
    @classmethod
    def _validate_relevant_segment_ids(cls, values: list[str]) -> list[str]:
        normalized = [str(value).strip() for value in values]
        if any(not value or len(value) > 256 for value in normalized):
            raise ValueError("relevant segment IDs must contain 1-256 characters")
        if len(normalized) != len(set(normalized)):
            raise ValueError("relevant segment IDs must be unique within a case")
        return normalized

    @field_validator("relevance")
    @classmethod
    def _validate_relevance_ids(
        cls,
        values: dict[str, RetrievalEvalGrade],
    ) -> dict[str, RetrievalEvalGrade]:
        normalized: dict[str, RetrievalEvalGrade] = {}
        for raw_segment_id, grade in values.items():
            segment_id = str(raw_segment_id).strip()
            if not segment_id or len(segment_id) > 256:
                raise ValueError("relevance segment IDs must contain 1-256 characters")
            normalized[segment_id] = grade
        return normalized


class RetrievalEvalRequestSchema(RetrieveRequestSchema):
    """Run the retrieval pipeline against a labelled test set and score it.

    Inherits every retrieval knob from ``RetrieveRequestSchema`` (mode, fusion,
    weights, rerank, mmr, thresholds, ...) so that two different configurations
    can be evaluated and A/B-compared on the exact same queries. The ``query``
    field is ignored here; ``cases`` supplies the queries instead.
    """

    model_config = ConfigDict(extra="forbid")

    query: str = Field(default="", max_length=4096)  # ignored; cases supply queries
    top_k: int = Field(default=5, ge=1, le=100)
    cases: list[RetrievalEvalCaseSchema] = Field(..., min_length=1, max_length=20)
    k_values: list[RetrievalEvalK] = Field(
        default_factory=lambda: [1, 3, 5, 10],
        min_length=1,
        max_length=8,
    )
    return_retrieved: bool = True  # include per-case ranked lists in the response

    @model_validator(mode="before")
    @classmethod
    def _flatten_retrieval_preset(cls, value: Any) -> Any:
        """Accept ``RetrievalConfig.to_dict()`` without weakening flat callers.

        The preset endpoint intentionally returns the canonical nested retrieval
        config used by dataset settings. Evaluation requests historically inherit
        the flat ``RetrieveRequestSchema``. Normalize known nested preset fields at
        this boundary so the preset response can be posted back verbatim while the
        execution service continues receiving its stable flat contract.
        """

        if not isinstance(value, dict):
            return value
        data = dict(value)

        def consume_nested(name: str, allowed: set[str]) -> dict[str, Any] | None:
            nested = data.get(name)
            if not isinstance(nested, dict):
                return None
            unknown = set(nested).difference(allowed)
            if unknown:
                unknown_fields = ", ".join(sorted(str(field) for field in unknown))
                raise ValueError(f"unsupported {name} preset fields: {unknown_fields}")
            data.pop(name)
            return nested

        vector = consume_nested("vector", {"enabled", "top_k", "score_threshold"})
        if vector is not None:
            if vector.get("top_k") is not None:
                data.setdefault("vector_top_k", vector["top_k"])
            if data.get("score_threshold") is None and vector.get("score_threshold") is not None:
                data["score_threshold"] = vector["score_threshold"]

        keyword = consume_nested(
            "keyword",
            {"enabled", "top_k", "candidate_pool_size", "bm25_k1", "bm25_b"},
        )
        if keyword is not None:
            if keyword.get("top_k") is not None:
                data.setdefault("keyword_top_k", keyword["top_k"])
            if keyword.get("candidate_pool_size") is not None:
                data.setdefault("keyword_candidate_k", keyword["candidate_pool_size"])

        fusion = consume_nested("fusion", {"strategy", "rrf_k", "rrf_weights", "alpha"})
        if fusion is not None:
            if fusion.get("strategy") is not None:
                data.setdefault("fusion_method", fusion["strategy"])
            for source, target in (
                ("rrf_k", "rrf_k"),
                ("rrf_weights", "rrf_weights"),
                ("alpha", "alpha"),
            ):
                if fusion.get(source) is not None:
                    data.setdefault(target, fusion[source])

        rerank = consume_nested(
            "rerank",
            {"enabled", "provider", "model", "top_n", "score_threshold"},
        )
        if rerank is not None:
            if rerank.get("enabled") is not None:
                data.setdefault("rerank", rerank["enabled"])
            if rerank.get("model") is not None:
                data.setdefault("rerank_model", rerank["model"])
            if rerank.get("top_n") is not None:
                data.setdefault("rerank_top_n", rerank["top_n"])

        mmr = consume_nested("mmr", {"enabled", "lambda", "similarity_threshold"})
        if mmr is not None:
            if mmr.get("enabled") is not None:
                data.setdefault("mmr", mmr["enabled"])
            if mmr.get("lambda") is not None:
                data.setdefault("mmr_lambda", mmr["lambda"])
            if mmr.get("similarity_threshold") is not None:
                data.setdefault("mmr_threshold", mmr["similarity_threshold"])

        multimodal = consume_nested(
            "multimodal",
            {
                "enabled",
                "image_search_enabled",
                "image_score_threshold",
                "text_score_threshold",
                "use_separate_thresholds",
                "image_boost",
                "vlm_rerank_enabled",
                "vlm_rerank_weight",
                "content_type_filter",
            },
        )
        if multimodal is not None:
            # Disabled preset blocks retain dormant tuning values for backward
            # compatible serialization, but those values must not be promoted
            # into an executable public request. Any enabled block or active
            # boolean/content selector is still surfaced to the after-validator
            # and rejected by the release-wide text-only gate.
            multimodal_enabled = bool(multimodal.get("enabled", False))
            if multimodal_enabled:
                data.setdefault("include_images", True)
            mappings = {
                "image_search_enabled": "image_search_enabled",
                "use_separate_thresholds": "use_separate_thresholds",
                "vlm_rerank_enabled": "multimodal_rerank",
                "content_type_filter": "content_type_filter",
            }
            if multimodal_enabled:
                mappings.update(
                    {
                        "image_score_threshold": "image_score_threshold",
                        "image_boost": "image_boost",
                        "vlm_rerank_weight": "vlm_rerank_weight",
                    }
                )
            for source, target in mappings.items():
                if multimodal.get(source) is not None:
                    data.setdefault(target, multimodal[source])

        return data

    @model_validator(mode="after")
    def _validate_eval_bounds_and_identity(self) -> Self:
        self.k_values = sorted(set(self.k_values))

        seen_case_ids: set[str] = set()
        for index, case in enumerate(self.cases):
            resolved_case_id = case.case_id or f"case_{index}"
            if resolved_case_id in seen_case_ids:
                raise ValueError(f"duplicate case_id: {resolved_case_id}")
            seen_case_ids.add(resolved_case_id)
            case.case_id = resolved_case_id

        integer_limits = {
            "vector_top_k": 1000,
            "keyword_top_k": 1000,
            "candidate_top_k": 2000,
            "keyword_candidate_k": 500,
            "rerank_top_n": 1000,
            "rrf_k": 10000,
            "l1_top_k": 100,
            "l2_top_k": 200,
        }
        for field_name, upper_bound in integer_limits.items():
            field_value = getattr(self, field_name, None)
            if field_value is not None and not 1 <= int(field_value) <= upper_bound:
                raise ValueError(f"{field_name} must be between 1 and {upper_bound}")

        unit_interval_fields = (
            "dense_weight",
            "bm25_weight",
            "alpha",
            "score_threshold",
            "mmr_lambda",
            "mmr_threshold",
            "vlm_rerank_weight",
            "image_score_threshold",
        )
        for field_name in unit_interval_fields:
            field_value = getattr(self, field_name, None)
            if field_value is None:
                continue
            numeric = float(field_value)
            if not math.isfinite(numeric) or not 0.0 <= numeric <= 1.0:
                raise ValueError(f"{field_name} must be finite and between 0 and 1")

        if self.image_boost is not None:
            image_boost = float(self.image_boost)
            if not math.isfinite(image_boost) or not 0.0 <= image_boost <= 10.0:
                raise ValueError("image_boost must be finite and between 0 and 10")

        if self.rrf_weights:
            if len(self.rrf_weights) > 16:
                raise ValueError("rrf_weights supports at most 16 entries")
            for name, weight in self.rrf_weights.items():
                if not name or len(name) > 64:
                    raise ValueError("rrf_weights keys must contain 1-64 characters")
                numeric = float(weight)
                if not math.isfinite(numeric) or not 0.0 <= numeric <= 100.0:
                    raise ValueError("rrf_weights values must be finite and between 0 and 100")

        text_limits = {
            "document_id": 256,
            "mode": 32,
            "fusion_method": 32,
            "fusion": 32,
            "rerank_model": 256,
            "content_type_filter": 64,
            "source_type_filter": 128,
            "language_filter": 64,
        }
        for field_name, maximum_length in text_limits.items():
            field_value = getattr(self, field_name, None)
            if field_value is not None and len(str(field_value)) > maximum_length:
                raise ValueError(f"{field_name} must not exceed {maximum_length} characters")

        if self.metadata_filter is not None:
            serialized_filter = json.dumps(
                self.metadata_filter,
                ensure_ascii=False,
                allow_nan=False,
                default=str,
            )
            if len(serialized_filter.encode("utf-8")) > 16_384:
                raise ValueError("metadata_filter must not exceed 16 KiB")

        return self


class AssociatedImageSchema(BaseModel):
    """Schema for associated image in retrieval results."""

    image_segment_id: str
    storage_url: str
    filename: str = ""
    vlm_description: str | None = None
    proximity_score: float = 1.0
    media_type: str = "image/png"


class RetrieveHitSchema(BaseModel):
    segment_id: str
    document_id: str
    score: float
    text: str
    metadata: dict[str, Any] = Field(default_factory=dict)

    # P3: Multimodal fields
    content_type: str = "text"  # "text" | "image"
    image_url: str | None = None  # For image segments
    vlm_description: str | None = None  # VLM-generated description for images
    associated_images: list[AssociatedImageSchema] = Field(
        default_factory=list
    )  # Associated images for text segments

    # Source traceability fields
    source_type: str | None = None
    citation_text: str | None = None  # Pre-formatted citation string
    source_reference: dict[str, Any] = Field(default_factory=dict)  # Structured reference data


class RetrieveResponseSchema(BaseModel):
    results: list[RetrieveHitSchema] = Field(default_factory=list)
    metadata: dict[str, Any] = Field(default_factory=dict)
    trace_id: str | None = None
    query_fingerprint: str | None = None


class SegmentUpdateSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    text: str = Field(min_length=1, max_length=200_000)
    answer: str | None = Field(default=None, max_length=200_000)
    keywords: list[str] | None = Field(default=None, max_length=100)

    @field_validator("keywords")
    @classmethod
    def _validate_update_keywords(cls, values: list[str] | None) -> list[str] | None:
        if values is not None and any(not value or len(value) > 256 for value in values):
            raise ValueError("keywords must contain 1-256 characters")
        return values


class SegmentCreateSchema(BaseModel):
    """Create a new segment manually"""

    model_config = ConfigDict(extra="allow")

    content: str = Field(min_length=1, max_length=200_000)
    answer: str | None = Field(default=None, max_length=200_000)
    keywords: list[str] | None = Field(default=None, max_length=100)

    @field_validator("keywords")
    @classmethod
    def _validate_create_keywords(cls, values: list[str] | None) -> list[str] | None:
        if values is not None and any(not value or len(value) > 256 for value in values):
            raise ValueError("keywords must contain 1-256 characters")
        return values


class SegmentEnableDisableSchema(BaseModel):
    model_config = ConfigDict(extra="allow")

    enabled: bool


class SegmentBatchEnableDisableSchema(BaseModel):
    """Batch enable/disable segments"""

    model_config = ConfigDict(extra="allow")

    segment_ids: list[str] = Field(min_length=1, max_length=500)
    enabled: bool

    @field_validator("segment_ids")
    @classmethod
    def _validate_segment_ids(cls, values: list[str]) -> list[str]:
        normalized = [value.strip() for value in values]
        if any(not value or len(value) > 256 for value in normalized):
            raise ValueError("segment IDs must contain 1-256 characters")
        return normalized


# ============================================================
# Statistics/Info Schemas
# ============================================================


class DatasetStatisticsSchema(BaseModel):
    """Dataset statistics response"""

    dataset_id: str
    document_count: int = 0
    segment_count: int = 0
    word_count: int = 0
    available_document_count: int = 0
    available_segment_count: int = 0
    hit_count: int = 0


class DocumentStatisticsSchema(BaseModel):
    """Document statistics response"""

    document_id: str
    segment_count: int = 0
    word_count: int = 0
    hit_count: int = 0


# ============================================================
# Query History Schemas
# ============================================================


class QueryHistorySchema(BaseModel):
    """Query history entry"""

    id: str
    dataset_id: str
    content: str
    source: str
    source_app_id: str | None = None
    created_by_role: str | None = None
    created_by: str | None = None
    metadata: dict[str, Any] = Field(default_factory=dict)
    trace_id: uuid.UUID | None = None
    query_fingerprint: str | None = None
    mode: str | None = None
    top_k: int | None = None
    hit_count: int | None = None
    stage_timings: dict[str, Any] = Field(default_factory=dict)
    created_at: datetime


class QueryHistoryListSchema(BaseModel):
    """Query history list response"""

    queries: list[QueryHistorySchema] = Field(default_factory=list)
    next_cursor: str | None = None
    has_more: bool = False


class QueryFeedbackUpsertSchema(BaseModel):
    model_config = ConfigDict(extra="forbid")

    trace_id: uuid.UUID
    query_fingerprint: str = Field(pattern=r"^[0-9a-f]{64}$")
    target_type: Literal["retrieval_hit", "qa_answer"]
    segment_id: str | None = Field(default=None, min_length=1, max_length=255)
    rating: Literal["positive", "negative"]
    reason_code: Literal[
        "relevant",
        "helpful",
        "well_cited",
        "irrelevant",
        "incorrect",
        "missing_context",
        "bad_citation",
        "stale",
        "unsafe",
        "other",
    ]
    comment: str | None = Field(default=None, max_length=2000)

    @model_validator(mode="after")
    def _validate_feedback_shape(self) -> Self:
        if self.target_type == "retrieval_hit" and not self.segment_id:
            raise ValueError("retrieval-hit feedback requires segment_id")
        if self.target_type == "qa_answer" and self.segment_id is not None:
            raise ValueError("qa-answer feedback must not include segment_id")
        positive = {"relevant", "helpful", "well_cited", "other"}
        negative = {
            "irrelevant",
            "incorrect",
            "missing_context",
            "bad_citation",
            "stale",
            "unsafe",
            "other",
        }
        allowed = positive if self.rating == "positive" else negative
        if self.reason_code not in allowed:
            raise ValueError("reason_code is incompatible with rating")
        return self


class QueryFeedbackSchema(BaseModel):
    feedback_id: uuid.UUID
    tenant_id: str
    dataset_id: str
    trace_id: uuid.UUID
    query_fingerprint: str
    target_type: Literal["retrieval_hit", "qa_answer"]
    target_id: str
    rating: Literal["positive", "negative"]
    reason_code: str
    comment: str | None = None
    created_by: str
    query_content: str | None = None
    created_at: datetime
    updated_at: datetime


class QueryFeedbackListSchema(BaseModel):
    feedback: list[QueryFeedbackSchema] = Field(default_factory=list)
    next_cursor: str | None = None
    has_more: bool = False


# ============================================================
# Batch Operation Schemas
# ============================================================


class BatchReindexSchema(BaseModel):
    """Batch reindex documents"""

    model_config = ConfigDict(extra="allow")

    document_ids: list[Annotated[str, Field(min_length=1, max_length=255)]] = Field(
        default_factory=list,
        max_length=1000,
    )
    all_documents: bool = False  # If true, reindex all documents in dataset

    @model_validator(mode="after")
    def _validate_selector(self) -> Self:
        if self.all_documents == bool(self.document_ids):
            raise ValueError("choose exactly one of all_documents or document_ids")
        if len(set(self.document_ids)) != len(self.document_ids):
            raise ValueError("document_ids must be unique")
        return self


class BatchDeleteSchema(BaseModel):
    """Batch delete documents"""

    model_config = ConfigDict(extra="allow")

    document_ids: list[Annotated[str, Field(min_length=1, max_length=255)]] = Field(
        min_length=1,
        max_length=1000,
    )

    @model_validator(mode="after")
    def _validate_unique_ids(self) -> Self:
        if len(set(self.document_ids)) != len(self.document_ids):
            raise ValueError("document_ids must be unique")
        return self


class BatchOperationResultSchema(BaseModel):
    """Batch operation result"""

    success_count: int = 0
    failed_count: int = 0
    failed_ids: list[str] = Field(default_factory=list)
    errors: dict[str, str] = Field(default_factory=dict)


# ============================================================
# QA Testing Schemas
# ============================================================


class LLMConfigSchema(BaseModel):
    """Non-secret QA generation preferences.

    Provider credentials and endpoints are server-owned.  Allowing a viewer to
    pair an arbitrary endpoint with a server credential turns the QA test route
    into an SSRF and credential-exfiltration primitive.
    """

    model_config = ConfigDict(extra="forbid")

    provider: str | None = None
    model: str | None = None
    temperature: float | None = Field(default=None, ge=0.0, le=2.0)
    max_tokens: int | None = Field(default=None, ge=1, le=4096)
    system_prompt: str | None = Field(default=None, max_length=4_000)


class QAQuerySchema(BaseModel):
    """Request schema for QA query"""

    model_config = ConfigDict(extra="forbid")

    query: str = Field(min_length=1, max_length=4_096)
    top_k: int = Field(default=5, ge=1, le=100)
    mode: str = "hybrid"
    fusion_method: str | None = None
    dense_weight: float | None = Field(default=None, ge=0.0, le=1.0)
    bm25_weight: float | None = Field(default=None, ge=0.0, le=1.0)
    score_threshold: float | None = Field(default=None, ge=0.0, le=1.0)
    document_id: str | None = None
    rerank: bool = False
    rerank_top_n: int | None = Field(default=None, ge=1, le=200)
    mmr: bool = False
    mmr_lambda: float | None = Field(default=None, ge=0.0, le=1.0)

    # LLM config override
    llm_config: LLMConfigSchema | None = None

    # Include raw retrieval results
    include_raw_results: bool = False


class QAResultSchema(BaseModel):
    """Response schema for QA query"""

    query: str
    answer: str
    context_segments: list[dict[str, Any]] = Field(default_factory=list)
    retrieval_metadata: dict[str, Any] = Field(default_factory=dict)
    timing: dict[str, int] = Field(default_factory=dict)
    model: str
    tokens_used: int | None = None
    trace_id: str | None = None
    query_fingerprint: str | None = None


class QATestCaseSchema(BaseModel):
    """Test case for QA evaluation"""

    model_config = ConfigDict(extra="forbid")

    query: str = Field(min_length=1, max_length=4_096)
    expected_answer: str | None = Field(default=None, max_length=20_000)
    expected_segments: list[str] | None = Field(
        default=None,
        max_length=200,
    )  # segment_ids


class QABatchTestSchema(BaseModel):
    """Batch QA test request"""

    model_config = ConfigDict(extra="forbid")

    test_cases: list[QATestCaseSchema] = Field(min_length=1, max_length=10)
    top_k: int = Field(default=5, ge=1, le=100)
    mode: str = "hybrid"
    rerank: bool = False
    mmr: bool = False

    # LLM config
    llm_config: LLMConfigSchema | None = None


class QATestResultSchema(BaseModel):
    """Single test result"""

    test_case: QATestCaseSchema
    result: QAResultSchema
    answer_correct: bool | None = None
    retrieval_recall: float | None = None
    retrieval_precision: float | None = None


class QABatchTestResultSchema(BaseModel):
    """Batch test results"""

    results: list[QATestResultSchema] = Field(default_factory=list)
    summary: dict[str, Any] = Field(default_factory=dict)


# ============================================================
# Batch Retrieval Schemas
# ============================================================


class BatchRetrieveQuerySchema(BaseModel):
    """Per-query overrides for batch retrieval."""

    model_config = ConfigDict(extra="allow")

    query: str = Field(min_length=1, max_length=4096)
    document_id: str | None = Field(default=None, min_length=1, max_length=256)
    mode: str | None = None
    fusion_method: str | None = None
    alpha: float | None = None
    dense_weight: float | None = None
    bm25_weight: float | None = None
    rrf_k: int | None = Field(default=None, ge=1, le=10_000)
    vector_top_k: int | None = Field(default=None, ge=1, le=1000)
    keyword_top_k: int | None = Field(default=None, ge=1, le=1000)
    candidate_top_k: int | None = Field(default=None, ge=1, le=2000)
    keyword_candidate_k: int | None = Field(default=None, ge=1, le=500)
    rerank: bool | None = None
    rerank_model: str | None = Field(default=None, min_length=1, max_length=256)
    rerank_top_n: int | None = Field(default=None, ge=1, le=1000)
    mmr: bool | None = None
    mmr_lambda: float | None = None
    mmr_threshold: float | None = None
    score_threshold: float | None = None
    source_type_filter: str | None = Field(default=None, max_length=128)
    language_filter: str | None = Field(default=None, max_length=64)
    metadata_filter: dict[str, Any] | None = Field(default=None, max_length=64)
    include_images: bool | None = None
    include_associated_images: bool | None = None

    @model_validator(mode="after")
    def _validate_query_fusion_parameters(self) -> Self:
        _validate_retrieval_fusion_parameters(self)
        _reject_unreleased_multimodal_options(self)
        return self


class BatchRetrieveRequestSchema(BaseModel):
    """Multi-query retrieval request with one global result set.

    Usage:
    - queries: Original query first, followed by recall-only rewrites
    - Or use query with comma-separated queries: "query1,query2,query3"

    Returns one globally fused result group capped by ``top_k``.
    """

    model_config = ConfigDict(extra="allow")

    queries: list[str | BatchRetrieveQuerySchema] | None = Field(
        default=None,
        min_length=1,
        max_length=20,
    )
    query: str | None = Field(default=None, min_length=1, max_length=81_939)
    top_k: int | None = Field(default=None, ge=1, le=100)
    mode: str = "hybrid"
    document_id: str | None = Field(default=None, min_length=1, max_length=256)

    # Fusion weights
    dense_weight: float | None = None
    bm25_weight: float | None = None
    fusion_method: str | None = None
    alpha: float | None = None
    score_threshold: float | None = None
    source_type_filter: str | None = Field(default=None, max_length=128)
    language_filter: str | None = Field(default=None, max_length=64)

    # Advanced options
    vector_top_k: int | None = Field(default=None, ge=1, le=1000)
    keyword_top_k: int | None = Field(default=None, ge=1, le=1000)
    candidate_top_k: int | None = Field(default=None, ge=1, le=2000)
    keyword_candidate_k: int | None = Field(default=None, ge=1, le=500)
    fusion: str | None = None
    rrf_k: int | None = Field(default=None, ge=1, le=10_000)
    rrf_weights: dict[str, float] = Field(default_factory=dict, max_length=16)

    # Post-processing
    rerank: bool | None = None
    rerank_model: str | None = Field(default=None, min_length=1, max_length=256)
    rerank_top_n: int | None = Field(default=None, ge=1, le=1000)
    mmr: bool | None = None
    mmr_lambda: float | None = None
    mmr_threshold: float | None = None
    # Multimodal options
    include_images: bool = False
    include_associated_images: bool = False

    # Batch-specific options
    max_parallel: int = Field(default=10, ge=1, le=10)
    dedupe_results: bool = False  # Compatibility flag; global segment dedupe is always applied

    @model_validator(mode="after")
    def _validate_batch_fusion_parameters(self) -> Self:
        _validate_retrieval_fusion_parameters(self)
        _reject_unreleased_multimodal_options(self)
        raw_queries: list[str] = []
        if self.queries:
            raw_queries.extend(
                item.query if isinstance(item, BatchRetrieveQuerySchema) else item
                for item in self.queries
            )
        elif self.query:
            raw_queries.extend(part.strip() for part in self.query.split(",") if part.strip())
        if not 1 <= len(raw_queries) <= 20:
            raise ValueError("batch retrieval requires between 1 and 20 queries")
        if any(not query.strip() or len(query.strip()) > 4096 for query in raw_queries):
            raise ValueError("each batch query must contain 1-4096 characters")
        return self


class BatchRetrieveResultSchema(BaseModel):
    """Globally fused result group keyed by the original query."""

    query: str
    results: list[dict[str, Any]] = Field(default_factory=list)
    meta: dict[str, Any] = Field(default_factory=dict)


class BatchRetrieveResponseSchema(BaseModel):
    """Multi-query response containing one globally fused result group."""

    batch_results: list[BatchRetrieveResultSchema] = Field(default_factory=list)
    total_queries: int = 0
    total_results: int = 0
    execution_time_ms: float = 0.0
    trace_id: str | None = None
    query_fingerprint: str | None = None


# ============================================================
# Chunking Configuration Schemas
# ============================================================


class ChunkingSegmentationSchema(BaseModel):
    """Safe Dify-compatible segmentation override."""

    model_config = ConfigDict(extra="forbid")

    max_tokens: int = Field(ge=1, le=100_000)


class ChunkingConfigSchema(BaseModel):
    """Chunking configuration schema

    Supports multiple chunking modes:
    - automatic: Smart detection of best strategy
    - fixed_size: Fixed character count with overlap
    - paragraph: Split by paragraphs
    - heading: Split by document headings
    - hierarchical: Parent-child dual-layer chunking
    - separator: Split by custom separators
    - regex: Split by regex pattern
    - recursive: Recursive multi-level splitting
    - qa: Question-answer pair format
    """

    model_config = ConfigDict(extra="forbid")

    mode: str = Field(default="automatic", min_length=1, max_length=32)
    chunk_size: int = Field(default=2000, ge=50, le=100_000)
    chunk_overlap: int = Field(default=300, ge=0, le=50_000)
    overlap: int | None = Field(default=None, ge=0, le=50_000)
    max_chunk_size: int | None = Field(default=None, ge=50, le=100_000)
    min_chunk_size: int | None = Field(default=None, ge=50, le=100_000)

    # Token-based (preferred for production)
    use_token_count: bool = True
    token_limit: int | None = Field(default=None, ge=1, le=100_000)
    max_tokens: int | None = Field(default=None, ge=1, le=100_000)
    min_chunk_tokens: int | None = Field(default=None, ge=1, le=100_000)
    max_chunk_tokens: int | None = Field(default=None, ge=1, le=100_000)
    parent_token_limit: int | None = Field(default=None, ge=1, le=100_000)
    child_token_limit: int | None = Field(default=None, ge=1, le=100_000)

    # Separator mode
    separators: list[str] | None = Field(default=None, max_length=32)
    separator: str = Field(default="\n", max_length=1_024)
    primary_separator: str | None = Field(default=None, max_length=1_024)
    keep_separator: bool | None = None

    # Regex mode
    regex_pattern: str | None = Field(default=None, max_length=256)

    # Heading mode
    heading_level: str | None = Field(default=None, max_length=32)  # h1 | h2 | h3 etc
    heading_patterns: list[str] | None = Field(default=None, max_length=20)

    # Paragraph mode
    min_paragraph_length: int | None = Field(default=None, ge=1, le=100_000)
    merge_short_paragraphs: bool | None = None

    # Hierarchical mode
    parent_mode: str | None = Field(default=None, max_length=32)
    parent_chunk_size: int | None = Field(default=None, ge=50, le=100_000)
    parent_overlap: int | None = Field(default=None, ge=0, le=50_000)
    parent_chunk_overlap: int | None = Field(default=None, ge=0, le=50_000)
    child_chunk_size: int | None = Field(default=None, ge=10, le=100_000)
    child_overlap: int | None = Field(default=None, ge=0, le=50_000)
    child_chunk_overlap: int | None = Field(default=None, ge=0, le=50_000)

    # QA mode
    question_prefix: str | None = Field(default=None, max_length=128)
    answer_prefix: str | None = Field(default=None, max_length=128)

    # Pre-processing
    remove_extra_spaces: bool = True
    remove_urls_emails: bool = False
    normalize_whitespace: bool | None = None
    strip_html: bool | None = None
    extract_metadata: bool | None = None
    metadata_fields: list[str] | None = Field(default=None, max_length=32)
    page_marker: str | None = Field(default=None, max_length=16)
    strict_section_traceability: bool | None = None

    # Optional image and compatibility knobs consumed by the runtime parser.
    preserve_images: bool | None = None
    image_context_chars: int | None = Field(default=None, ge=0, le=1_000_000)
    segmentation: ChunkingSegmentationSchema | None = None

    @model_validator(mode="after")
    def _validate_safe_chunking_contract(self) -> Self:
        mode = self.mode.strip().lower()
        if mode not in {
            "automatic",
            "auto",
            "fixed_size",
            "fixed",
            "custom",
            "paragraph",
            "page",
            "heading",
            "section",
            "regex",
            "separator",
            "recursive",
            "hierarchical",
            "parent_child",
            "qa",
        }:
            raise ValueError("unsupported chunking mode")
        if mode == "regex" or self.regex_pattern:
            raise ValueError("custom regex chunking is disabled")
        safe_heading_patterns = (
            r"^#{1,6}\s+.+$",
            r"^第[一二三四五六七八九十\d]+[章节条款]",
            r"^[A-Z][A-Z \t]{4,}:?$",
        )
        if self.heading_patterns and tuple(self.heading_patterns) != safe_heading_patterns:
            raise ValueError("custom heading regex patterns are disabled")
        if self.page_marker not in (None, "", r"\f", "\f"):
            raise ValueError("custom page marker regex is disabled")
        if self.separators is not None and (
            not self.separators
            or any(
                not separator or len(separator) > 1_024
                for separator in self.separators
            )
        ):
            raise ValueError("separators must contain non-empty strings up to 1024 chars")
        if self.metadata_fields is not None and any(
            not field_name or len(field_name) > 64 for field_name in self.metadata_fields
        ):
            raise ValueError("metadata_fields must contain non-empty names up to 64 chars")
        effective_overlap = self.overlap if self.overlap is not None else self.chunk_overlap
        if effective_overlap >= self.chunk_size:
            raise ValueError("chunk_overlap must be smaller than chunk_size")
        if (
            self.min_chunk_size is not None
            and self.max_chunk_size is not None
            and self.min_chunk_size > self.max_chunk_size
        ):
            raise ValueError("min_chunk_size must not exceed max_chunk_size")
        child_overlap = (
            self.child_overlap
            if self.child_overlap is not None
            else self.child_chunk_overlap
        )
        if (
            child_overlap is not None
            and self.child_chunk_size is not None
            and child_overlap >= self.child_chunk_size
        ):
            raise ValueError("child_overlap must be smaller than child_chunk_size")
        parent_overlap = (
            self.parent_overlap
            if self.parent_overlap is not None
            else self.parent_chunk_overlap
        )
        if (
            parent_overlap is not None
            and self.parent_chunk_size is not None
            and parent_overlap >= self.parent_chunk_size
        ):
            raise ValueError("parent_overlap must be smaller than parent_chunk_size")
        if (
            self.min_chunk_tokens is not None
            and self.max_chunk_tokens is not None
            and self.min_chunk_tokens > self.max_chunk_tokens
        ):
            raise ValueError("min_chunk_tokens must not exceed max_chunk_tokens")
        return self


class RetrievalConfigSchema(BaseModel):
    """Retrieval pipeline configuration schema"""

    model_config = ConfigDict(extra="allow")

    mode: str = "hybrid"  # vector | keyword | hybrid
    top_k: int = Field(default=5, ge=1, le=100)
    score_threshold: float | None = Field(default=None, ge=0.0, le=1.0, allow_inf_nan=False)

    # Vector search
    vector_enabled: bool = True
    vector_top_k: int = Field(default=20, ge=1, le=1000)

    # Keyword search
    keyword_enabled: bool = True
    keyword_top_k: int = Field(default=20, ge=1, le=1000)

    # Fusion
    fusion_strategy: str = "rrf"  # rrf | weighted
    rrf_k: int = Field(default=60, ge=1, le=10_000)
    alpha: float = Field(default=0.75, ge=0.0, le=1.0, allow_inf_nan=False)

    # Rerank
    rerank_enabled: bool = False
    rerank_model: str = Field(default="qwen3-rerank", min_length=1, max_length=256)
    rerank_top_n: int | None = Field(default=None, ge=1, le=1000)

    # MMR
    mmr_enabled: bool = False
    mmr_lambda: float = Field(default=0.5, ge=0.0, le=1.0, allow_inf_nan=False)


class DatasetConfigUpdateSchema(BaseModel):
    """Update dataset configuration (chunking + retrieval)"""

    model_config = ConfigDict(extra="allow")

    chunking_config: ChunkingConfigSchema | None = None
    retrieval_config: RetrievalConfigSchema | None = None
    embedding_provider: str | None = Field(default=None, min_length=1, max_length=64)
    embedding_model: str | None = Field(default=None, min_length=1, max_length=256)
    embedding_dimension: int | None = Field(default=None, ge=1, le=8192)


class ChunkPreviewRequestSchema(BaseModel):
    """Request schema for chunk preview"""

    model_config = ConfigDict(extra="allow")

    text: str = Field(min_length=1, max_length=200_000)
    config: ChunkingConfigSchema | None = None
    document_id: str | None = Field(default=None, min_length=1, max_length=256)

    @model_validator(mode="after")
    def _reject_unsafe_regex_preview(self) -> Self:
        config = self.config.model_dump(exclude_none=True) if self.config else {}
        if (
            str(config.get("mode") or "automatic").strip().lower() == "regex"
            or bool(config.get("regex_pattern"))
            or bool(config.get("regex"))
            or bool(config.get("heading_patterns"))
            or bool(config.get("page_marker"))
        ):
            raise ValueError("custom regex chunk preview is disabled")
        return self


class ChunkPreviewItemSchema(BaseModel):
    """Single chunk item in preview"""

    content: str
    token_count: int
    char_count: int
    metadata: dict[str, Any] = Field(default_factory=dict)


class ChunkPreviewResponseSchema(BaseModel):
    """Response schema for chunk preview"""

    chunks: list[ChunkPreviewItemSchema] = Field(default_factory=list)
    total_chunks: int = 0
