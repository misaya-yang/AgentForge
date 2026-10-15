"""Internal evaluation routes for knowledge-base RAGAS scoring."""

from __future__ import annotations

import asyncio

from ai_gateway_core.config.endpoints import resolve_dashscope
from fastapi import APIRouter, Body, Depends, HTTPException, Request
from pydantic import BaseModel, ConfigDict, Field, field_validator

from ...config import RagasEvalSettings, Settings
from ...services.eval import KBRagasEvalService, MetricResult
from ...services.knowledge.embedding import EmbeddingConfig, create_embedding
from ...services.knowledge.qa_service import LLMConfig, LLMProvider
from ..deps import get_settings

router = APIRouter(tags=["kb-eval"])


class RagasJudgeSelector(BaseModel):
    """The only caller-selectable judge fields; credentials stay server-owned."""

    model_config = ConfigDict(extra="forbid")

    provider: str | None = Field(default=None, min_length=1, max_length=32)
    model: str | None = Field(default=None, min_length=1, max_length=128)


class RagasEvalRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")

    query: str = Field(..., min_length=1, max_length=4_000)
    contexts: list[str] = Field(..., min_length=1, max_length=128)
    answer: str | None = Field(default=None, max_length=8_000)
    metrics: list[str] | None = Field(default=None, max_length=5)
    ground_truth: str | None = Field(default=None, max_length=8_000)
    llm_config: RagasJudgeSelector | None = None

    @field_validator("contexts")
    @classmethod
    def reject_blank_or_oversized_contexts(cls, contexts: list[str]) -> list[str]:
        if any(not str(context).strip() for context in contexts):
            raise ValueError("contexts must not contain blank entries")
        if any(len(context) > 100_000 for context in contexts):
            raise ValueError("each context must contain at most 100000 characters")
        return contexts


class RagasMetricResponse(BaseModel):
    metric: str
    score: float
    explanation: str
    label: str
    failure_kind: str | None = None


class RagasEvalResponse(BaseModel):
    results: list[RagasMetricResponse]
    judge_model: str


def _to_response(result: MetricResult) -> RagasMetricResponse:
    return RagasMetricResponse(
        metric=result.metric,
        score=result.score,
        explanation=result.explanation,
        label=result.label,
        failure_kind=result.failure_kind,
    )


def require_verified_gateway(request: Request) -> None:
    """Keep the internal judge endpoint closed even in anonymous dev mode."""

    if getattr(request.state, "gateway_secret_verified", False) is not True:
        raise HTTPException(
            status_code=401,
            detail={
                "code": "AUTH_DENIED",
                "message": "Verified gateway signature required",
            },
        )


def require_ragas_eval_enabled(
    settings: Settings = Depends(get_settings),
) -> None:
    """Keep provider-paid judging disabled until its caller identity is signed."""

    if settings.ragas_eval.enabled is not True:
        raise HTTPException(status_code=403, detail="KB RAGAS evaluation is disabled")


def _build_server_llm_config(
    selector: RagasJudgeSelector | None,
    config: RagasEvalSettings,
) -> LLMConfig:
    provider_name = str(selector.provider if selector and selector.provider else config.provider)
    provider_name = provider_name.strip().lower()
    model = str(selector.model if selector and selector.model else config.model).strip()
    if provider_name not in config.allowed_providers:
        raise ValueError(f"KB RAGAS provider is not allowlisted: {provider_name}")
    if model not in config.allowed_models:
        raise ValueError(f"KB RAGAS model is not allowlisted: {model}")
    try:
        provider = LLMProvider(provider_name)
    except ValueError as exc:
        raise ValueError(f"Unsupported KB RAGAS provider: {provider_name}") from exc
    if provider is LLMProvider.CUSTOM:
        raise ValueError("Custom KB RAGAS providers are not allowed")

    api_key = None
    base_url = config.base_url
    if provider is LLMProvider.DASHSCOPE:
        resolved_key, resolved_base_url = resolve_dashscope("chat")
        api_key = resolved_key or None
        if not base_url:
            base_url = f"{resolved_base_url.rstrip('/')}/v1"

    return LLMConfig(
        provider=provider,
        model=model,
        api_key=api_key,
        base_url=base_url,
        temperature=0.0,
        max_tokens=512,
        timeout_seconds=config.timeout_seconds,
    )


def _validate_request_budget(body: RagasEvalRequest, config: RagasEvalSettings) -> None:
    if len(body.contexts) > config.max_contexts:
        raise ValueError(f"KB RAGAS accepts at most {config.max_contexts} contexts")
    if any(len(context) > config.max_context_chars for context in body.contexts):
        raise ValueError(
            f"each KB RAGAS context must contain at most {config.max_context_chars} characters"
        )
    if sum(len(context) for context in body.contexts) > config.max_total_context_chars:
        raise ValueError(
            "KB RAGAS contexts exceed the configured total character budget"
        )
    if body.metrics is not None and len(body.metrics) > config.max_metrics:
        raise ValueError(f"KB RAGAS accepts at most {config.max_metrics} metrics")


@router.post(
    "/internal/eval/ragas",
    response_model=RagasEvalResponse,
    dependencies=[
        Depends(require_verified_gateway),
        Depends(require_ragas_eval_enabled),
    ],
)
async def evaluate_kb_ragas(
    body: RagasEvalRequest = Body(...),
    settings: Settings = Depends(get_settings),
) -> RagasEvalResponse:
    require_ragas_eval_enabled(settings)

    try:
        _validate_request_budget(body, settings.ragas_eval)
        llm_config = _build_server_llm_config(body.llm_config, settings.ragas_eval)
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc

    embedding = None
    if any(
        metric in {"answer_relevancy", "response_relevancy"}
        for metric in body.metrics or []
    ):
        embedding_settings = settings.embeddings
        embedding = create_embedding(
            EmbeddingConfig(
                provider=embedding_settings.provider,
                model=embedding_settings.model,
                api_key=embedding_settings.get_api_key_for_provider(
                    embedding_settings.provider
                ),
                base_url=embedding_settings.base_url,
                timeout_seconds=embedding_settings.timeout_seconds,
            ),
            dimension=embedding_settings.dimension,
        )
    service = KBRagasEvalService(llm_config, embedding=embedding)
    try:
        async with asyncio.timeout(settings.ragas_eval.request_timeout_seconds):
            results = await service.evaluate_retrieval(
                query=body.query,
                contexts=body.contexts,
                answer=body.answer,
                metrics=body.metrics,
                ground_truth=body.ground_truth,
            )
    except TimeoutError as exc:
        raise HTTPException(status_code=504, detail="KB RAGAS evaluation timed out") from exc
    except ValueError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    finally:
        await service.close()
    return RagasEvalResponse(
        results=[_to_response(item) for item in results],
        judge_model=llm_config.model,
    )
