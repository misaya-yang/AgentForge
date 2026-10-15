"""Golden tests for KB RAGAS judge prompt versioning (PRD T0-#6).

JUDGE_PROMPTS_SHA256_PIN is a literal on purpose: it pins the canonical
serialization of every judge prompt surface (version number, system prompt,
data-section header, and the five metric prompts). Any reword of a prompt
changes the computed hash and fails test_judge_prompt_hash_is_pinned. The only
sanctioned recovery is a deliberate revision: bump JUDGE_PROMPT_VERSION in
ragas_eval_service.py (the version number is itself part of the bundle, so the
hash moves with it), update this literal, and say so in the commit. Rows scored
before and after then carry different evaluator_version strings and stay
distinguishable in the get_kb_ragas_summary dedup CTE, instead of being
silently collapsed into one partition.
"""

from __future__ import annotations

import hashlib
import json
import re
from typing import Any

import pytest
from knowledge_service.services.eval.ragas_eval_service import (
    _JUDGE_PROMPT_BUNDLE,
    CONTEXT_PRECISION_JUDGE_PROMPT,
    FAITHFULNESS_JUDGE_PROMPT,
    JUDGE_DATA_SECTION_HEADER,
    JUDGE_EVALUATOR_VERSION,
    JUDGE_PROMPT_VERSION,
    JUDGE_PROMPTS_SHA256,
    JUDGE_SYSTEM_PROMPT,
    KBRagasEvalService,
)
from knowledge_service.services.knowledge.qa_service import LLMConfig

# Pinned literal — see the module docstring for the bump discipline.
JUDGE_PROMPTS_SHA256_PIN = (
    "c9deccd8f10d93ee1e48b29d1e5d88c36731771d1a1509ceedda516c88288a9b"
)

# Recorded judge JSON: a realistic faithfulness verdict matching the parse
# contract of _claim_support_ratio — two of four claims supported.
FAITHFULNESS_JUDGE_JSON = json.dumps(
    {
        "claims": [
            {"claim": "Refunds are allowed within 30 days of purchase.", "supported": True},
            {"claim": "Refunds require the original receipt.", "supported": True},
            {"claim": "Gift cards are refundable in cash.", "supported": False},
            {"claim": "The policy applies to digital goods.", "supported": False},
        ],
        "explanation": "Two of four claims are entailed by the retrieved context.",
    },
    ensure_ascii=False,
)


class _FakeLLMClient:
    def __init__(self, response: str) -> None:
        self.messages: list[dict[str, str]] = []
        self.response = response

    async def chat_completion(self, **kwargs: Any) -> tuple[str, int]:
        self.messages = kwargs.get("messages") or []
        return (self.response, 25)

    async def close(self) -> None:
        return None


class _RaisingLLMClient(_FakeLLMClient):
    async def chat_completion(self, **_kwargs: Any) -> tuple[str, int]:
        raise RuntimeError("judge unavailable")


def _service_with_transport(
    monkeypatch: pytest.MonkeyPatch,
    client: _FakeLLMClient,
) -> KBRagasEvalService:
    monkeypatch.setattr(
        "knowledge_service.services.eval.ragas_eval_service.LLMClient",
        lambda _config: client,
    )
    return KBRagasEvalService(LLMConfig(model="golden-judge-model"))


def test_judge_prompt_hash_is_pinned() -> None:
    recomputed = hashlib.sha256(
        json.dumps(
            _JUDGE_PROMPT_BUNDLE, ensure_ascii=False, separators=(",", ":")
        ).encode("utf-8")
    ).hexdigest()
    # The module value must equal an independent recomputation of the canonical
    # serialization (fixed bundle order, compact separators, utf-8, sha256)...
    assert recomputed == JUDGE_PROMPTS_SHA256
    # ...and that serialization must equal the deliberate pin. A mismatch means
    # a prompt surface or the version number changed — see module docstring.
    assert JUDGE_PROMPTS_SHA256 == JUDGE_PROMPTS_SHA256_PIN
    assert JUDGE_PROMPT_VERSION == 1


def test_judge_evaluator_version_format() -> None:
    assert re.fullmatch(
        r"ragas-judge-prompt-v1:[0-9a-f]{12}", JUDGE_EVALUATOR_VERSION
    )
    assert (
        f"ragas-judge-prompt-v{JUDGE_PROMPT_VERSION}:{JUDGE_PROMPTS_SHA256[:12]}"
        == JUDGE_EVALUATOR_VERSION
    )


@pytest.mark.asyncio
async def test_faithfulness_golden_judge_json_metric_math_and_metadata(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = _FakeLLMClient(FAITHFULNESS_JUDGE_JSON)
    service = _service_with_transport(monkeypatch, client)

    results = await service.evaluate_retrieval(
        query="refund policy",
        contexts=["Refunds are allowed within 30 days with the original receipt."],
        answer="Refunds take 30 days and gift cards refund in cash.",
        metrics=["faithfulness"],
    )

    assert len(results) == 1
    row = results[0]
    # Judge JSON -> metric math golden: supported_claims / total_claims = 2 / 4.
    assert row.metric == "faithfulness"
    assert row.score == 0.5
    assert row.label == "fail"
    assert row.failure_kind is None
    # Version metadata rides on the emitted row: evaluator_version of the form
    # downstream persists, plus judge model id and full prompt hash in metadata.
    assert row.evaluator_version == JUDGE_EVALUATOR_VERSION
    assert row.metadata == {
        "judge_prompt_version": JUDGE_PROMPT_VERSION,
        "judge_prompt_sha256": JUDGE_PROMPTS_SHA256,
        "judge_model": "golden-judge-model",
    }
    assert row.judge_model == "golden-judge-model"
    # The judge saw exactly the pinned prompt surface.
    assert client.messages[0]["content"] == JUDGE_SYSTEM_PROMPT
    assert client.messages[1]["content"].startswith(
        FAITHFULNESS_JUDGE_PROMPT + JUDGE_DATA_SECTION_HEADER
    )


@pytest.mark.asyncio
async def test_rendered_prompt_is_pinned_template_plus_bounded_data(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    client = _FakeLLMClient('{"verdicts": [true], "explanation": "useful"}')
    service = _service_with_transport(monkeypatch, client)

    await service.evaluate_retrieval(
        query="q",
        contexts=["c"],
        metrics=["context_precision"],
        ground_truth="reference",
    )

    user_prompt = client.messages[1]["content"]
    prefix = CONTEXT_PRECISION_JUDGE_PROMPT + JUDGE_DATA_SECTION_HEADER
    assert user_prompt.startswith(prefix)
    assert user_prompt.endswith("\n")
    assert '"question": "q"' in user_prompt


@pytest.mark.asyncio
async def test_infrastructure_failure_row_keeps_semantics_and_carries_version(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service = _service_with_transport(monkeypatch, _RaisingLLMClient(""))

    results = await service.evaluate_retrieval(query="q", contexts=["c"])

    row = results[0]
    # Existing failure semantics: unchanged (score=0, review, infrastructure).
    assert row.label == "review"
    assert row.failure_kind == "infrastructure"
    assert row.score == 0.0
    # New: the row is attributable to the exact judge prompt revision.
    assert row.evaluator_version == JUDGE_EVALUATOR_VERSION
    assert row.metadata == {
        "judge_prompt_version": JUDGE_PROMPT_VERSION,
        "judge_prompt_sha256": JUDGE_PROMPTS_SHA256,
        "judge_model": "golden-judge-model",
    }


@pytest.mark.asyncio
async def test_semantic_review_skip_row_carries_version_metadata(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    service = _service_with_transport(monkeypatch, _FakeLLMClient('{"score": 1.0}'))

    results = await service.evaluate_retrieval(
        query="q",
        contexts=["c"],
        metrics=["faithfulness"],  # no answer -> prerequisite skip, no LLM call
    )

    row = results[0]
    assert row.failure_kind == "semantic_review"
    assert row.score == 0.0
    assert row.evaluator_version == JUDGE_EVALUATOR_VERSION
    assert row.metadata["judge_prompt_sha256"] == JUDGE_PROMPTS_SHA256
