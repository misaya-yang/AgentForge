from __future__ import annotations

import pytest
from ai_gateway_core.eval.kb_ragas_sample import build_kb_ragas_sample, kb_ragas_sample_from_target


def test_build_kb_ragas_sample_extracts_contexts_from_retriever_span() -> None:
    detail = {
        "trace": {
            "trace_id": "trace-rag-1",
            "trace_family": "rag",
            "input_preview": "refund policy",
            "output_preview": "Refunds are available for 30 days.",
            "metadata": {
                "gen_ai.retrieval.query.text": "refund policy",
                "dataset_id": "dataset-9",
            },
        },
        "spans": [
            {
                "span_kind": "lifecycle",
                "name": "retrieve",
            },
            {
                "span_kind": "retriever",
                "attributes": {
                    "retrieval": {
                        "documents": [
                            {"content_eval": "Refunds are allowed within 30 days."},
                            {"content_preview": "Contact support for exceptions."},
                        ]
                    }
                },
            },
        ],
    }

    sample = build_kb_ragas_sample(detail)

    assert sample is not None
    assert sample.question == "refund policy"
    assert sample.contexts == [
        "Refunds are allowed within 30 days.",
        "Contact support for exceptions.",
    ]
    assert sample.dataset_id == "dataset-9"
    assert sample.trace_id == "trace-rag-1"
    assert sample.answer == "Refunds are available for 30 days."


def test_build_kb_ragas_sample_returns_none_without_contexts() -> None:
    detail = {
        "trace": {
            "trace_family": "rag",
            "input_preview": "hello",
            "metadata": {},
        },
        "spans": [{"span_kind": "retriever", "attributes": {}}],
    }

    assert build_kb_ragas_sample(detail) is None


def test_retrieval_only_trace_does_not_expose_document_count_as_answer() -> None:
    detail = {
        "trace": {
            "trace_id": "trace-retrieval-only",
            "trace_family": "rag",
            "input_preview": "refund policy",
            "output_preview": "2 retrieved documents",
            "metadata": {"answer_source": "retrieval_only"},
        },
        "spans": [
            {
                "span_kind": "retriever",
                "attributes": {
                    "retrieval": {"documents": [{"content_eval": "Refund policy chunk"}]}
                },
            }
        ],
    }

    sample = build_kb_ragas_sample(detail)

    assert sample is not None
    assert sample.answer_source == "retrieval_only"
    assert sample.answer is None


def test_contexts_preserve_duplicate_rank_slots() -> None:
    detail = {
        "trace": {
            "trace_family": "rag",
            "input_preview": "ranked query",
            "output_preview": "generated answer",
            "metadata": {},
        },
        "spans": [
            {
                "span_kind": "retriever",
                "attributes": {
                    "retrieval": {
                        "documents": [
                            {"content_eval": "dup"},
                            {"content_eval": "dup"},
                            {"content_eval": "later"},
                        ]
                    }
                },
            }
        ],
    }

    sample = build_kb_ragas_sample(detail)

    assert sample is not None
    assert sample.contexts == ["dup", "dup", "later"]


@pytest.mark.parametrize(
    "trace_identity",
    [
        {"workflow_kind": "rag_retrieval_chain"},
        {"source_adapter": "gateway.knowledge_proxy"},
    ],
)
def test_historical_retrieval_chain_infers_retrieval_only_from_count_output(
    trace_identity: dict[str, str],
) -> None:
    detail = {
        "trace": {
            "trace_family": "rag",
            **trace_identity,
            "input_preview": "refund policy",
            "output_preview": "2 retrieved documents",
            "metadata": {},
        },
        "spans": [
            {
                "span_kind": "retriever",
                "attributes": {
                    "retrieval": {"documents": [{"content_eval": "refund chunk"}]}
                },
            }
        ],
    }

    sample = build_kb_ragas_sample(detail)

    assert sample is not None
    assert sample.answer_source == "retrieval_only"
    assert sample.answer is None


def test_generated_rag_is_not_inferred_retrieval_only_from_count_like_answer() -> None:
    detail = {
        "trace": {
            "trace_family": "rag",
            "workflow_kind": "ai_assistant_chat",
            "input_preview": "how many documents",
            "output_preview": "2 retrieved documents",
            "metadata": {},
        },
        "spans": [
            {
                "span_kind": "retriever",
                "attributes": {
                    "retrieval": {"documents": [{"content_eval": "document count"}]}
                },
            }
        ],
    }

    sample = build_kb_ragas_sample(detail)

    assert sample is not None
    assert sample.answer_source is None
    assert sample.answer == "2 retrieved documents"


def test_kb_ragas_sample_from_target_uses_ground_truth() -> None:
    target = {
        "trace_id": "trace-rag-2",
        "trace_family": "rag",
        "input_preview": "pricing",
        "output_preview": "The starter plan costs $10.",
        "metadata": {},
        "spans": [
            {
                "span_kind": "retriever",
                "attributes": {
                    "retrieval.documents": [{"text": "Plans start at $10/month."}],
                },
            }
        ],
    }

    sample = kb_ragas_sample_from_target(target, ground_truth="Starter plan is $10")

    assert sample is not None
    assert sample.ground_truth == "Starter plan is $10"
    assert sample.answer == "The starter plan costs $10."
    assert sample.contexts == ["Plans start at $10/month."]
