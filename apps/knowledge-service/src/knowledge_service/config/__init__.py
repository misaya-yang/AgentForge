"""Knowledge Service configuration.

Settings are read from environment variables with the ``KNOWLEDGE_`` prefix
and double-underscore nesting (e.g. ``KNOWLEDGE_QDRANT__URL``). The
env-passthrough fields on ``Settings`` below additionally pin the unprefixed
legacy environment names that replaced bare ``os.getenv`` call sites (Phase 0
of ``docs/plans/rag-upgrade-prd-2026-08.md`` §7); those names are frozen for
compatibility and must never be renamed.
"""

from __future__ import annotations

from typing import Any, Literal

from pydantic import AliasChoices, BaseModel, Field, SecretStr, field_validator, model_validator
from pydantic_settings import BaseSettings, SettingsConfigDict

# ---------------------------------------------------------------------------
# Sub-models
# ---------------------------------------------------------------------------

class CORSSettings(BaseModel):
    """CORS origin whitelist. Set via KNOWLEDGE_CORS__ALLOW_ORIGINS (comma-separated)."""

    allow_origins: list[str] = Field(default_factory=lambda: ["http://localhost:80", "http://localhost:3000"])

    @field_validator("allow_origins", mode="before")
    @classmethod
    def parse_origins(cls, v: Any) -> list[str]:
        if isinstance(v, str):
            return [o.strip() for o in v.split(",") if o.strip()]
        return v


class AppSettings(BaseModel):
    """General application settings."""

    host: str = "0.0.0.0"
    port: int = 8092
    log_level: str = "INFO"
    allow_anonymous: bool = False


class ConfluenceSettings(BaseModel):
    """Manual saved-binding transport only; never starts a polling scheduler."""

    manual_sync_enabled: bool = True
    request_timeout: float = Field(default=30.0, ge=1.0, le=120.0)
    max_retries: int = Field(default=3, ge=0, le=5)
    client_cache_ttl_seconds: int = Field(default=300, ge=0, le=3600)


class DatabaseSettings(BaseModel):
    """PostgreSQL connection pool settings."""

    dsn: str = "postgresql://localhost:5432/gateway"
    pool_min_size: int = 2
    pool_max_size: int = 10


class RedisSettings(BaseModel):
    """Optional Redis cache for retrieval results."""

    enabled: bool = False
    url: str = "redis://localhost:6379/2"


class QdrantSettings(BaseModel):
    """Qdrant vector database settings."""

    url: str = "http://localhost:6333"
    api_key: str | None = None
    prefer_grpc: bool = False
    timeout_seconds: float = 120.0
    max_retries: int = 5
    retry_base_delay: float = 2.0
    # Global emergency gate. Dataset config alone cannot enable native BM25.
    bm25_v2_enabled: bool = False
    bm25_v2_capability_ttl_seconds: float = Field(default=300.0, ge=0.0)
    bm25_v2_readiness_ttl_seconds: float = Field(default=5.0, ge=0.0)
    # Comma-separated tenant ids allowed to exercise the pre-release cutover.
    # Empty is fail-closed; rollback remains available independently.
    bm25_v2_cutover_test_tenants: str = ""


class EmbeddingSettings(BaseModel):
    """Text embedding provider settings."""

    provider: str = "dashscope"  # dashscope | gemini | siliconflow | openai_compatible
    api_key: str = ""
    model: str = "text-embedding-v4"
    dimension: int = 1024
    batch_size: int = 50
    max_concurrent: int = 5
    base_url: str | None = None
    timeout_seconds: float = 30.0

    # Per-provider API keys (for multi-model support)
    google_api_key: str = ""
    dashscope_api_key: str = ""
    siliconflow_api_key: str = ""
    siliconflow_base_url: str = "https://api.siliconflow.cn/v1"
    # Self-hosted OpenAI-compatible embedding server (vLLM / TEI) — the
    # Qwen3-Embedding upgrade path. No base-URL default on purpose: a
    # half-configured deployment must fail closed rather than silently
    # embed against a public cloud endpoint.
    openai_compatible_api_key: str = ""
    openai_compatible_base_url: str = ""

    def get_api_key_for_provider(self, provider: str) -> str:
        """Get the correct API key for a given embedding provider."""
        provider_key = {
            "gemini": self.google_api_key,
            "dashscope": self.dashscope_api_key,
            "siliconflow": self.siliconflow_api_key,
            "openai_compatible": self.openai_compatible_api_key,
        }.get(provider)
        return provider_key or self.api_key


class MultimodalEmbeddingSettings(BaseModel):
    """Multimodal (image+text) embedding settings."""

    provider: str = "dashscope"
    api_key: str = ""
    model: str = "tongyi-embedding-vision-plus"
    dimension: int = 1024
    max_concurrent: int = 5


class OCRSettings(BaseModel):
    """OCR and VLM-based document recognition settings."""

    enabled: bool = True
    strategy: str = "hybrid"  # tesseract | vlm | hybrid
    languages: str = "eng+ara"
    render_dpi: int = 200
    page_concurrency: int = 3
    tesseract_timeout_seconds: int = 60
    min_text_chars_for_ocr: int = 200

    # Qwen-OCR is the primary document-recognition provider. ``hybrid`` keeps
    # the local Tesseract fallback for provider outages; it does not replace
    # Qwen for the normal scanned-document path.
    vlm_provider: str = "dashscope"  # gemini | dashscope | siliconflow | auto
    vlm_model: str = "qwen-vl-ocr"
    vlm_task: str = "document_parsing"
    vlm_min_pixels: int = Field(default=3_072, ge=1_024, le=30_720_000)
    vlm_max_pixels: int = Field(default=8_388_608, ge=3_072, le=30_720_000)
    vlm_max_tokens: int = Field(default=8_192, ge=256, le=8_192)
    vlm_enable_rotate: bool = True
    vlm_api_keys: str = ""  # comma-separated keys for multi-key providers (siliconflow)
    vlm_base_url: str | None = None  # custom API URL override
    vlm_concurrency: int = 4
    vlm_timeout_seconds: int = 30
    vlm_batch_size: int = 5
    vlm_max_concurrent: int = 8
    vlm_quality_threshold: float = 0.5

    @field_validator("vlm_task")
    @classmethod
    def validate_vlm_task(cls, v: str) -> str:
        allowed = {
            "text_recognition",
            "advanced_recognition",
            "key_information_extraction",
            "table_parsing",
            "document_parsing",
            "formula_recognition",
            "multi_lan",
        }
        value = str(v or "document_parsing").strip()
        if value not in allowed:
            raise ValueError(f"Invalid Qwen-OCR task: {value}")
        return value

    @model_validator(mode="after")
    def validate_vlm_pixel_range(self) -> OCRSettings:
        if self.vlm_min_pixels > self.vlm_max_pixels:
            raise ValueError("vlm_min_pixels must not exceed vlm_max_pixels")
        return self

    @field_validator("languages")
    @classmethod
    def validate_ocr_languages(cls, v: str) -> str:
        ALLOWED_SINGLE = frozenset(
            {
                "eng", "ara", "chi_sim", "chi_tra", "fra", "deu",
                "spa", "rus", "jpn", "kor", "por", "ita", "nld",
                "tur", "vie", "tha",
            }
        )
        ALLOWED_COMBOS = frozenset(
            {"eng+ara", "ara+eng", "chi_sim+eng", "jpn+eng", "kor+eng"}
        )
        if not v:
            return "eng+ara"
        if v in ALLOWED_COMBOS or v in ALLOWED_SINGLE:
            return v
        if "+" in v:
            parts = v.split("+")
            if all(p.strip() in ALLOWED_SINGLE for p in parts):
                return v
            raise ValueError(f"Invalid OCR language combination: {v}")
        raise ValueError(f"Invalid OCR language: {v}")


class MetadataLLMSettings(BaseModel):
    """LLM-based metadata extraction settings (P1)."""

    enabled: bool = False
    provider: str = "gemini"  # gemini | dashscope
    api_key: str = ""  # Provider API key; falls back to env var
    model: str = "gemini-2.0-flash"  # gemini-2.0-flash | qwen3.6-plus
    base_url: str = "https://dashscope.aliyuncs.com/compatible-mode/v1"
    max_concurrent: int = 5
    batch_size: int = 10  # Chunks per LLM call
    timeout_seconds: float = 30.0


class ProcessingSettings(BaseModel):
    """Document processing and worker settings."""

    worker_concurrency: int = 2
    document_worker_concurrency: int = 3

    # Large file handling
    large_file_threshold: int = 50 * 1024 * 1024  # 50 MB
    max_memory_processing_size: int = 100 * 1024 * 1024  # 100 MB
    streaming_batch_size: int = 20
    streaming_min_batch_size: int = 5
    streaming_max_batch_size: int = 50

    # Chunking / hierarchical indexing
    hierarchical_l2_chunk_size: int = 8000
    hierarchical_l2_chunk_overlap: int = 400
    hierarchical_l3_chunk_size: int = 2000
    hierarchical_l3_chunk_overlap: int = 200
    hierarchical_l1_top_k: int = 5
    hierarchical_l2_top_k: int = 10
    hierarchical_l3_top_k: int = 5

    # Retrieval concurrency
    dataset_fanout_max_concurrency: int = 6
    retrieval_query_max_concurrency: int = 8
    retrieval_cache_ttl_seconds: int = 300

    # Document type detection
    detection_sample_pages: int = 5
    detection_native_pdf_threshold: float = 0.8
    detection_scanned_pdf_threshold: float = 0.2
    detection_min_chars_per_page: int = 50

    # Scanned PDF
    scanned_min_images_for_image_only: int = 5
    image_position_offset: int = 1_000_000

    # PDF auto-split
    pdf_split_enabled: bool = True
    pdf_split_max_size_bytes: int = 20 * 1024 * 1024  # 20 MB
    pdf_split_min_pages_per_part: int = 5


class StorageS3Settings(BaseModel):
    """S3 / S3-compatible storage settings."""

    bucket: str = ""
    region: str = "us-east-1"
    access_key: str = ""
    secret_key: str = ""
    endpoint_url: str | None = None


class StorageSettings(BaseModel):
    """Object storage for uploaded files and extracted images."""

    backend: str = "local"  # local | s3
    local_base_path: str = "./data/files"
    url_expiry_seconds: int = 3600
    key_prefix: str = "dev"
    signing_key: SecretStr = Field(default_factory=lambda: SecretStr(""))
    s3: StorageS3Settings = Field(default_factory=StorageS3Settings)


class RagasEvalSettings(BaseModel):
    """Server-owned judge selection and request budgets for KB RAGAS evals."""

    enabled: bool = False
    provider: str = Field(default="dashscope", min_length=1, max_length=32)
    model: str = Field(default="qwen3.7-plus", min_length=1, max_length=128)
    base_url: str | None = Field(default=None, max_length=2048)
    timeout_seconds: float = Field(default=60.0, ge=1.0, le=180.0)
    request_timeout_seconds: float = Field(default=180.0, ge=1.0, le=600.0)
    allowed_providers: list[str] = Field(default_factory=lambda: ["dashscope"])
    allowed_models: list[str] = Field(default_factory=lambda: ["qwen3.7-plus"])
    max_contexts: int = Field(default=32, ge=1, le=128)
    max_context_chars: int = Field(default=8_000, ge=256, le=100_000)
    max_total_context_chars: int = Field(default=64_000, ge=1_000, le=500_000)
    max_metrics: int = Field(default=5, ge=1, le=5)

    @field_validator("provider")
    @classmethod
    def normalize_provider(cls, value: str) -> str:
        return value.strip().lower()

    @field_validator("model")
    @classmethod
    def normalize_model(cls, value: str) -> str:
        return value.strip()

    @field_validator("allowed_providers")
    @classmethod
    def normalize_allowed_providers(cls, values: list[str]) -> list[str]:
        normalized = list(
            dict.fromkeys(str(value).strip().lower() for value in values if str(value).strip())
        )
        if not normalized:
            raise ValueError("allowed_providers must not be empty")
        return normalized

    @field_validator("allowed_models")
    @classmethod
    def normalize_allowed_models(cls, values: list[str]) -> list[str]:
        normalized = list(
            dict.fromkeys(str(value).strip() for value in values if str(value).strip())
        )
        if not normalized:
            raise ValueError("allowed_models must not be empty")
        return normalized

    @model_validator(mode="after")
    def validate_default_judge_is_allowlisted(self) -> RagasEvalSettings:
        if self.provider not in self.allowed_providers:
            raise ValueError("ragas eval provider must be allowlisted")
        if self.model not in self.allowed_models:
            raise ValueError("ragas eval model must be allowlisted")
        return self


# ---------------------------------------------------------------------------
# Root Settings
# ---------------------------------------------------------------------------

class Settings(BaseSettings):
    """Root settings loaded from environment variables prefixed ``KNOWLEDGE_``."""

    model_config = SettingsConfigDict(
        env_prefix="KNOWLEDGE_",
        env_nested_delimiter="__",
        env_file=".env",
        env_file_encoding="utf-8",
        extra="ignore",
        populate_by_name=True,
    )

    runtime_role: Literal["all", "api", "worker"] = "all"
    cors: CORSSettings = Field(default_factory=CORSSettings)
    app: AppSettings = Field(default_factory=AppSettings)
    confluence: ConfluenceSettings = Field(default_factory=ConfluenceSettings)
    database: DatabaseSettings = Field(default_factory=DatabaseSettings)
    redis: RedisSettings = Field(default_factory=RedisSettings)
    qdrant: QdrantSettings = Field(default_factory=QdrantSettings)
    qdrant_interactive_deadline_seconds: float = Field(default=3.0, ge=0.1, le=30.0)
    embeddings: EmbeddingSettings = Field(default_factory=EmbeddingSettings)
    multimodal: MultimodalEmbeddingSettings = Field(
        default_factory=MultimodalEmbeddingSettings,
    )
    ocr: OCRSettings = Field(default_factory=OCRSettings)
    metadata_llm: MetadataLLMSettings = Field(default_factory=MetadataLLMSettings)
    ragas_eval: RagasEvalSettings = Field(default_factory=RagasEvalSettings)
    processing: ProcessingSettings = Field(default_factory=ProcessingSettings)
    storage: StorageSettings = Field(default_factory=StorageSettings)

    # -----------------------------------------------------------------------
    # Env passthrough: unprefixed legacy names (Phase 0 getenv consolidation)
    #
    # Each field replaces one or more bare ``os.getenv``/``os.environ.get``
    # reads at call sites across the service. Hard compatibility contract:
    #   * ``validation_alias`` is the EXACT original env name (aliases bypass
    #     the ``KNOWLEDGE_`` prefix) — renaming one breaks live deployments.
    #   * The field default equals the old ``os.getenv(name, default)`` value.
    #   * Legacy fallback chains stay as ordered per-name fields so callers
    #     reproduce the original first-truthy-value ``or``-chains exactly
    #     (pydantic-settings reports an env var as present even when it is set
    #     to the empty string, which ``or``-chains but not ``AliasChoices``
    #     handle with the original semantics).
    # -----------------------------------------------------------------------
    log_format: str = Field(default="", validation_alias=AliasChoices("LOG_FORMAT"))
    environment: str = Field(default="", validation_alias=AliasChoices("ENVIRONMENT"))
    gateway_encryption_key: str = Field(
        default="", validation_alias=AliasChoices("GATEWAY_ENCRYPTION_KEY")
    )
    redis_url: str = Field(default="", validation_alias=AliasChoices("REDIS_URL"))
    internal_idempotency_backend: str = Field(
        default="redis", validation_alias=AliasChoices("INTERNAL_IDEMPOTENCY_BACKEND")
    )
    internal_idempotency_ttl_seconds: int = Field(
        default=86400, validation_alias=AliasChoices("INTERNAL_IDEMPOTENCY_TTL_SECONDS")
    )
    internal_comm_redis_url: str = Field(
        default="", validation_alias=AliasChoices("INTERNAL_COMM_REDIS_URL")
    )
    internal_comm_state_backend: str = Field(
        default="redis", validation_alias=AliasChoices("INTERNAL_COMM_STATE_BACKEND")
    )
    internal_auth_version: str = Field(
        default="v2", validation_alias=AliasChoices("INTERNAL_AUTH_VERSION")
    )
    ai_platform_internal_token: str = Field(
        default="", validation_alias=AliasChoices("AI_PLATFORM_INTERNAL_TOKEN")
    )
    ai_platform_capability_proof_secret: str = Field(
        default="", validation_alias=AliasChoices("AI_PLATFORM_CAPABILITY_PROOF_SECRET")
    )

    # QA-service LLM selection (services/knowledge/qa_service.py).
    llm_model: str = Field(
        default="gemini-2.0-flash", validation_alias=AliasChoices("LLM_MODEL")
    )
    llm_base_url: str = Field(default="", validation_alias=AliasChoices("LLM_BASE_URL"))
    llm_api_key: str = Field(default="", validation_alias=AliasChoices("LLM_API_KEY"))
    deepseek_api_key: str = Field(default="", validation_alias=AliasChoices("DEEPSEEK_API_KEY"))
    deepseek_key: str = Field(default="", validation_alias=AliasChoices("DEEPSEEK_KEY"))
    gemini_api_key: str = Field(default="", validation_alias=AliasChoices("GEMINI_API_KEY"))
    google_api_key: str = Field(default="", validation_alias=AliasChoices("GOOGLE_API_KEY"))
    dashscope_api_key: str = Field(
        default="", validation_alias=AliasChoices("DASHSCOPE_API_KEY")
    )
    # ALIYUN_KEY also covers the historical "Aliyun_KEY" spelling because
    # pydantic-settings matches env var names case-insensitively.
    aliyun_key: str = Field(default="", validation_alias=AliasChoices("ALIYUN_KEY"))

    # Gemini/Vertex embedding env fallbacks (services/knowledge/embedding_manager.py).
    vertex_embedding_api_key: str = Field(
        default="", validation_alias=AliasChoices("VERTEX_EMBEDDING_API_KEY")
    )
    google_embedding_backend: str = Field(
        default="", validation_alias=AliasChoices("GOOGLE_EMBEDDING_BACKEND")
    )

    # DashScope rerank endpoint/schema overrides (services/knowledge/text_reranker.py).
    dashscope_rerank_base_url: str = Field(
        default="", validation_alias=AliasChoices("DASHSCOPE_RERANK_BASE_URL")
    )
    dashscope_base_url: str = Field(
        default="", validation_alias=AliasChoices("DASHSCOPE_BASE_URL")
    )
    dashscope_rerank_request_schema: str = Field(
        default="", validation_alias=AliasChoices("DASHSCOPE_RERANK_REQUEST_SCHEMA")
    )
    dashscope_rerank_instruct: str = Field(
        default="", validation_alias=AliasChoices("DASHSCOPE_RERANK_INSTRUCT")
    )

    # Cohere rerank key fallback (services/knowledge/retrieval_service.py).
    cohere_api_key: str = Field(
        default="", validation_alias=AliasChoices("COHERE_API_KEY")
    )

    # Upload / PDF-split limits (api/routes/knowledge.py). Existing defaults
    # preserve the historical ``os.getenv(name, default)`` values. The page
    # and expanded-output budgets are new safety fences for the 512 MiB
    # knowledge-service profile; route-level hard caps prevent unsafe raises.
    kb_max_file_size_mb: int = Field(
        default=16, validation_alias=AliasChoices("KB_MAX_FILE_SIZE_MB")
    )
    kb_max_batch_size_mb: int = Field(
        default=32, validation_alias=AliasChoices("KB_MAX_BATCH_SIZE_MB")
    )
    kb_pdf_split_max_size_mb: int = Field(
        default=20, validation_alias=AliasChoices("KB_PDF_SPLIT_MAX_SIZE_MB")
    )
    kb_pdf_split_pages_per_part: int = Field(
        default=500, validation_alias=AliasChoices("KB_PDF_SPLIT_PAGES_PER_PART")
    )
    kb_pdf_max_pages: int = Field(
        default=2000, validation_alias=AliasChoices("KB_PDF_MAX_PAGES")
    )
    kb_pdf_split_max_output_bytes: int = Field(
        default=96 * 1024 * 1024,
        validation_alias=AliasChoices("KB_PDF_SPLIT_MAX_OUTPUT_BYTES"),
    )

    def qa_llm_api_key(self, provider: str) -> str | None:
        """API key for QA LLM calls, mirroring the historical env fallback chain.

        Order (first truthy wins, exactly like the old per-name ``os.getenv``
        loop): ``LLM_API_KEY`` → provider chain
        (deepseek: ``DEEPSEEK_API_KEY`` → ``DEEPSEEK_KEY``; gemini:
        ``GEMINI_API_KEY`` → ``GOOGLE_API_KEY``; dashscope:
        ``DASHSCOPE_API_KEY`` → ``ALIYUN_KEY``/``Aliyun_KEY``; any other
        provider has no fallback).
        """
        provider_chain = {
            "deepseek": self.deepseek_api_key or self.deepseek_key,
            "gemini": self.gemini_api_key or self.google_api_key,
            "dashscope": self.dashscope_api_key or self.aliyun_key,
        }.get(provider, "")
        return self.llm_api_key or provider_chain or None


def get_settings() -> Settings:
    """Resolve :class:`Settings` from the live process environment only.

    This mirrors the semantics of the bare ``os.environ.get`` call sites it
    replaces: every call re-reads the current environment (so tests that
    monkeypatch env vars and code that resolves config at construction time
    behave identically), and a repository ``.env`` file is *not* consulted,
    exactly like ``os.getenv`` would ignore it. The application's startup
    config is the separate ``Settings()`` instance built in ``main.py``
    (which does read ``.env`` for the ``KNOWLEDGE_``-prefixed fields).
    """
    return Settings(_env_file=None)
