"""Phase 0 (PRD §7) pins for the bare-``os.getenv`` → ``Settings`` consolidation.

Guarantees, per the compatibility contract:

1. every consolidated env var still resolves from the environment under its
   ORIGINAL name (no renames);
2. defaults are unchanged when the env var is absent;
3. legacy aliases / fallback chains keep working (``Aliyun_KEY``,
   ``DEEPSEEK_KEY``, ``GOOGLE_API_KEY``, ``VERTEX_EMBEDDING_API_KEY``, ...);
4. call-site behavior matches what the bare ``os.getenv`` reads did (first
   truthy value wins, empty-string envs fall through, whitespace is stripped
   only where the old call site stripped it).
"""

from __future__ import annotations

from pathlib import Path
from types import SimpleNamespace

import pytest
from knowledge_service.config import Settings, get_settings
from knowledge_service.services.knowledge.embedding_manager import EmbeddingManager
from knowledge_service.services.knowledge.qa_service import LLMConfig, LLMProvider
from knowledge_service.services.knowledge.text_reranker import (
    AsyncTextReranker,
    _resolve_dashscope_rerank_url,
)

# (env_name, Settings field, old os.getenv default)
CONSOLIDATED_VARS: tuple[tuple[str, str, object], ...] = (
    ("LOG_FORMAT", "log_format", ""),
    ("ENVIRONMENT", "environment", ""),
    ("GATEWAY_ENCRYPTION_KEY", "gateway_encryption_key", ""),
    ("REDIS_URL", "redis_url", ""),
    ("INTERNAL_IDEMPOTENCY_BACKEND", "internal_idempotency_backend", "redis"),
    ("INTERNAL_IDEMPOTENCY_TTL_SECONDS", "internal_idempotency_ttl_seconds", 86400),
    ("INTERNAL_COMM_REDIS_URL", "internal_comm_redis_url", ""),
    ("INTERNAL_COMM_STATE_BACKEND", "internal_comm_state_backend", "redis"),
    ("INTERNAL_AUTH_VERSION", "internal_auth_version", "v2"),
    ("AI_PLATFORM_INTERNAL_TOKEN", "ai_platform_internal_token", ""),
    ("AI_PLATFORM_CAPABILITY_PROOF_SECRET", "ai_platform_capability_proof_secret", ""),
    ("LLM_MODEL", "llm_model", "gemini-2.0-flash"),
    ("LLM_BASE_URL", "llm_base_url", ""),
    ("LLM_API_KEY", "llm_api_key", ""),
    ("DEEPSEEK_API_KEY", "deepseek_api_key", ""),
    ("DEEPSEEK_KEY", "deepseek_key", ""),
    ("GEMINI_API_KEY", "gemini_api_key", ""),
    ("GOOGLE_API_KEY", "google_api_key", ""),
    ("DASHSCOPE_API_KEY", "dashscope_api_key", ""),
    ("ALIYUN_KEY", "aliyun_key", ""),
    ("VERTEX_EMBEDDING_API_KEY", "vertex_embedding_api_key", ""),
    ("GOOGLE_EMBEDDING_BACKEND", "google_embedding_backend", ""),
    ("DASHSCOPE_RERANK_BASE_URL", "dashscope_rerank_base_url", ""),
    ("DASHSCOPE_BASE_URL", "dashscope_base_url", ""),
    ("DASHSCOPE_RERANK_REQUEST_SCHEMA", "dashscope_rerank_request_schema", ""),
    ("DASHSCOPE_RERANK_INSTRUCT", "dashscope_rerank_instruct", ""),
    ("COHERE_API_KEY", "cohere_api_key", ""),
    ("KB_MAX_FILE_SIZE_MB", "kb_max_file_size_mb", 16),
    ("KB_MAX_BATCH_SIZE_MB", "kb_max_batch_size_mb", 32),
    ("KB_PDF_SPLIT_MAX_SIZE_MB", "kb_pdf_split_max_size_mb", 20),
    ("KB_PDF_SPLIT_PAGES_PER_PART", "kb_pdf_split_pages_per_part", 500),
    ("KB_PDF_MAX_PAGES", "kb_pdf_max_pages", 2000),
    (
        "KB_PDF_SPLIT_MAX_OUTPUT_BYTES",
        "kb_pdf_split_max_output_bytes",
        96 * 1024 * 1024,
    ),
)


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch: pytest.MonkeyPatch) -> None:
    for env_name, _field, _default in CONSOLIDATED_VARS:
        monkeypatch.delenv(env_name, raising=False)


# ---------------------------------------------------------------------------
# (a) resolves from env under the ORIGINAL name / (b) defaults unchanged
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(("env_name", "field", "default"), CONSOLIDATED_VARS)
def test_env_resolves_under_original_name(
    monkeypatch: pytest.MonkeyPatch, env_name: str, field: str, default: object
) -> None:
    sentinel = "123" if isinstance(default, int) else "sentinel-value"
    monkeypatch.setenv(env_name, sentinel)
    value = getattr(get_settings(), field)
    assert value == (int(sentinel) if isinstance(default, int) else sentinel)


@pytest.mark.parametrize(("env_name", "field", "default"), CONSOLIDATED_VARS)
def test_default_unchanged_when_env_absent(
    env_name: str, field: str, default: object
) -> None:
    assert getattr(get_settings(), field) == default


def test_get_settings_reflects_env_changes_per_call(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Call-time resolution matches the old ``os.getenv``-at-call-site semantics."""
    assert get_settings().ai_platform_internal_token == ""
    monkeypatch.setenv("AI_PLATFORM_INTERNAL_TOKEN", "late-token")
    assert get_settings().ai_platform_internal_token == "late-token"


def test_get_settings_ignores_dotenv_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """Bare ``os.getenv`` never consulted ``.env``; the consolidated reads must not either."""
    (tmp_path / ".env").write_text("GOOGLE_API_KEY=from-dotenv\n", encoding="utf-8")
    monkeypatch.chdir(tmp_path)
    # The startup Settings() keeps its pre-existing dotenv behavior for the
    # unprefixed aliases too, but get_settings() must stay env-only.
    assert Settings().google_api_key == "from-dotenv"
    assert get_settings().google_api_key == ""


# ---------------------------------------------------------------------------
# (c) legacy aliases and fallback chains
# ---------------------------------------------------------------------------


def test_aliyun_key_mixed_case_legacy_alias_is_honored(monkeypatch: pytest.MonkeyPatch) -> None:
    # The historical QA chain listed both "ALIYUN_KEY" and "Aliyun_KEY";
    # pydantic-settings' case-insensitive env lookup covers either spelling.
    monkeypatch.setenv("Aliyun_KEY", "mixed-case-legacy")
    assert LLMConfig(provider=LLMProvider.DASHSCOPE).get_api_key() == "mixed-case-legacy"


def test_qa_dashscope_key_precedence_and_empty_fallthrough(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("DASHSCOPE_API_KEY", "primary")
    monkeypatch.setenv("ALIYUN_KEY", "legacy")
    assert LLMConfig(provider=LLMProvider.DASHSCOPE).get_api_key() == "primary"

    # An env present-but-empty must fall through to the legacy key, exactly
    # like the old first-truthy ``os.getenv`` loop did.
    monkeypatch.setenv("DASHSCOPE_API_KEY", "")
    assert LLMConfig(provider=LLMProvider.DASHSCOPE).get_api_key() == "legacy"


def test_qa_llm_api_key_generic_override(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LLM_API_KEY", "generic")
    monkeypatch.setenv("DASHSCOPE_API_KEY", "primary")
    assert LLMConfig(provider=LLMProvider.DASHSCOPE).get_api_key() == "generic"


@pytest.mark.parametrize(
    ("provider", "fallback_env"),
    [
        (LLMProvider.DEEPSEEK, "DEEPSEEK_KEY"),
        (LLMProvider.GEMINI, "GOOGLE_API_KEY"),
    ],
)
def test_qa_provider_specific_fallback_chains(
    monkeypatch: pytest.MonkeyPatch, provider: LLMProvider, fallback_env: str
) -> None:
    monkeypatch.setenv(fallback_env, "fallback-key")
    assert LLMConfig(provider=provider).get_api_key() == "fallback-key"


def test_qa_custom_provider_has_no_fallback() -> None:
    assert LLMConfig(provider=LLMProvider.CUSTOM).get_api_key() is None


def test_qa_other_provider_isolation(monkeypatch: pytest.MonkeyPatch) -> None:
    # A dashscope key must not leak into the gemini provider chain and vice
    # versa (the old loop only tried the current provider's names).
    monkeypatch.setenv("DASHSCOPE_API_KEY", "ds")
    assert LLMConfig(provider=LLMProvider.GEMINI).get_api_key() is None


def test_qa_llm_model_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("LLM_MODEL", "qwen3.7-plus")
    assert LLMConfig().model == "qwen3.7-plus"
    monkeypatch.setenv("LLM_MODEL", "")
    assert LLMConfig().model == ""


def test_qa_llm_base_url_env(monkeypatch: pytest.MonkeyPatch) -> None:
    config = LLMConfig(provider=LLMProvider.DASHSCOPE)
    config.base_url = None
    monkeypatch.setenv("LLM_BASE_URL", "https://example.test/v1/")
    assert config.get_base_url() == "https://example.test/v1"


@pytest.mark.asyncio
async def test_embedding_manager_vertex_and_gemini_chain(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manager = EmbeddingManager(
        settings=SimpleNamespace(knowledge=SimpleNamespace(gemini=SimpleNamespace(api_key="")))
    )

    # Default (non-vertex) backend: GEMINI_API_KEY wins over GOOGLE_API_KEY.
    monkeypatch.setenv("GEMINI_API_KEY", "gem")
    monkeypatch.setenv("GOOGLE_API_KEY", "goog")
    resolved = await manager.resolve_embedding_config("gemini", "m", {})
    assert resolved.api_key == "gem"

    # Whitespace-only GEMINI_API_KEY strips to falsy and falls through.
    monkeypatch.setenv("GEMINI_API_KEY", "   ")
    resolved = await manager.resolve_embedding_config("gemini", "m", {})
    assert resolved.api_key == "goog"

    # Vertex backend switches the credential source.
    monkeypatch.setenv("GOOGLE_EMBEDDING_BACKEND", "Vertex")
    monkeypatch.setenv("VERTEX_EMBEDDING_API_KEY", "vtx")
    resolved = await manager.resolve_embedding_config("gemini", "m", {})
    assert resolved.api_key == "vtx"


def test_metadata_extractor_google_env(monkeypatch: pytest.MonkeyPatch) -> None:
    from knowledge_service.services.knowledge.metadata_extractor import MetadataExtractor

    monkeypatch.setenv("GOOGLE_API_KEY", "meta-goog")
    assert MetadataExtractor(provider="gemini").api_key == "meta-goog"
    monkeypatch.delenv("GOOGLE_API_KEY")
    assert MetadataExtractor(provider="gemini").api_key == ""


def test_text_reranker_env_reads(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("DASHSCOPE_RERANK_BASE_URL", "https://ws.example.com/x/compatible-api/v1/reranks/")
    assert _resolve_dashscope_rerank_url("qwen3-rerank") == (
        "https://ws.example.com/x/compatible-api/v1/reranks"
    )

    monkeypatch.delenv("DASHSCOPE_RERANK_BASE_URL")
    # qwen3-rerank fails closed with no endpoint env (original behavior).
    with pytest.raises(ValueError, match="qwen3-rerank requires"):
        _resolve_dashscope_rerank_url("qwen3-rerank")
    # Other models keep the legacy default endpoint.
    assert _resolve_dashscope_rerank_url("gte-rerank") == AsyncTextReranker.DASHSCOPE_RERANK_URL

    monkeypatch.setenv("DASHSCOPE_BASE_URL", "https://dashscope-intl.aliyuncs.com")
    assert _resolve_dashscope_rerank_url("gte-rerank") == (
        "https://dashscope-intl.aliyuncs.com/api/v1/services/rerank/text-rerank/text-rerank"
    )

    reranker = AsyncTextReranker(api_key="k", base_url="https://example.com/rerank")
    assert reranker.request_schema == "legacy"
    assert reranker.instruct is None

    monkeypatch.setenv("DASHSCOPE_RERANK_REQUEST_SCHEMA", "flat")
    monkeypatch.setenv("DASHSCOPE_RERANK_INSTRUCT", "prioritize exact answers")
    reranker = AsyncTextReranker(api_key="k", base_url="https://example.com/rerank")
    assert reranker.request_schema == "flat"
    assert reranker.instruct == "prioritize exact answers"


# ---------------------------------------------------------------------------
# .env.example completeness for the consolidated variables
# ---------------------------------------------------------------------------


def test_env_example_documents_all_consolidated_vars() -> None:
    root = Path(__file__).resolve().parents[3]
    names = {
        line.split("=", 1)[0].strip()
        for line in (root / ".env.example").read_text(encoding="utf-8").splitlines()
        if "=" in line and not line.lstrip().startswith("#")
    }
    missing = [env_name for env_name, _field, _default in CONSOLIDATED_VARS if env_name not in names]
    assert not missing, f".env.example is missing consolidated env vars: {missing}"


def test_env_examples_default_idempotency_to_redis() -> None:
    root = Path(__file__).resolve().parents[3]

    def values(path: Path) -> dict[str, str]:
        parsed: dict[str, str] = {}
        for line in path.read_text(encoding="utf-8").splitlines():
            if "=" not in line or line.lstrip().startswith("#"):
                continue
            name, value = line.split("=", 1)
            parsed[name.strip()] = value.strip()
        return parsed

    assert values(root / ".env.example")["INTERNAL_IDEMPOTENCY_BACKEND"] == "redis"
    assert (
        values(root / "apps" / "knowledge-service" / ".env.example")[
            "INTERNAL_IDEMPOTENCY_BACKEND"
        ]
        == "redis"
    )

    compose = (root / "docker-compose.yml").read_text(encoding="utf-8")
    assert compose.count('INTERNAL_IDEMPOTENCY_BACKEND: "redis"') == 2
    assert 'INTERNAL_IDEMPOTENCY_BACKEND: "${INTERNAL_IDEMPOTENCY_BACKEND' not in compose
