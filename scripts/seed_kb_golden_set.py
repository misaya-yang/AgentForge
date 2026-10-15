#!/usr/bin/env python3
"""Seed the versioned bilingual KB golden QA starter set (PRD T0-#2 machinery).

Storage investigation (2026-08-28): the platform already runs a generic golden
store — ``eval_datasets`` / ``eval_examples`` (migration 061_trace_eval_sota_core.sql)
served by ``POST /api/v1/eval/datasets/{dataset_id}/examples:import``
(src/api/v1/_eval_dataset_routes.py), validated by validate_case in
src/services/eval/golden_validation.py.  It accepts JSON payloads **without a
schema change**, but its semantics are the Assistant trace contract: a RAG
golden case's ``track``/``relevance``/KB-dataset binding would have to ride in
unvalidated metadata, and assistant experiments would treat the rows as agent
runs.  A dedicated retrieval-golden table is the honest home; this is a new DB
table, so per the task contract this script does NOT create one.  Instead it
emits versioned JSONL in the exact ``validate_rag_cases`` shape — consumable
today by scripts/eval_rag.py and tomorrow by a ``kb_eval_golden``-style store —
plus an ``--emit eval-import`` payload for the existing endpoint if the team
later decides metadata-smuggling is acceptable.

Follow-up (delivered 2026-08-28): the dedicated store exists — migration
``database/migrations/104_kb_eval_golden.sql`` +
``knowledge_service.persistence.kb_eval_golden_store`` +
``scripts/import_kb_eval_golden.py`` project this JSONL into a version-pinned
Postgres table with the frozen/growth split.  This script remains the git
development candidate (manifest-hashed by make kb-golden-gate); the structure
gate is not release evidence. Promotion to ``frozen`` happens only after human
review and the separate release-evidence gate, never at seed time.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from src.services.eval.rag_regression import validate_rag_cases

SEED_VERSION = "2026-08-28.1"
PROVENANCE = "seed-machine-generated, human-review-pending"
DEFAULT_OUTPUT = Path("tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl")
CORPUS_LANGUAGE = "en"

# Topic families and graded-relevance segment pairs mirror the structure of the
# existing golden fixture corpus (tests/fixtures/eval/rag/golden/
# rag_regression_v1.jsonl): one primary (grade 3) plus one supporting window
# (grade 1) per family, and the answer-track rows bound to the parallel
# ``answer-*`` segment ids.  Segment ids are synthetic placeholders until the
# set is bound to a real dataset.
_FAMILIES: list[dict[str, Any]] = [
    {
        "family": "refund",
        "relevance": {"refund-primary": 3, "refund-window": 1},
        "answer_relevance": {"answer-refund-primary": 3, "answer-refund-window": 1},
        "query_en": "how to get a refund for an annual subscription",
        "query_zh": "年付订阅如何申请退款",
        "answer_zh": "年付订阅的退款资格以公示的退款窗口期为准。",
        "answer_en": "Annual refunds follow the published eligibility window.",
    },
    {
        "family": "password",
        "relevance": {"password-primary": 3, "password-security": 1},
        "answer_relevance": {"answer-password-primary": 3, "answer-password-security": 1},
        "query_en": "where is the password reset link delivered",
        "query_zh": "密码重置链接会发送到哪里",
        "answer_zh": "重置链接会发送到已验证的账户渠道。",
        "answer_en": "A reset link is sent to the verified account channel.",
    },
    {
        "family": "invoice",
        "relevance": {"invoice-primary": 3, "invoice-history": 1},
        "answer_relevance": {"answer-invoice-primary": 3, "answer-invoice-history": 1},
        "query_en": "where can I download my most recent invoice",
        "query_zh": "在哪里可以下载最近一期发票",
        "answer_zh": "最新发票可在账单历史中查看。",
        "answer_en": "The latest invoice is available in billing history.",
    },
    {
        "family": "approval",
        "relevance": {"approval-primary": 3, "approval-audit": 1},
        "answer_relevance": {"answer-approval-primary": 3, "answer-approval-audit": 1},
        "query_en": "when is a high-risk tool allowed to execute",
        "query_zh": "高风险工具需要什么审批才能执行",
        "answer_zh": "仅在存在当前且匹配的有效审批后，工具才会执行。",
        "answer_en": "It executes only after a current matching approval.",
    },
    {
        "family": "compaction",
        "relevance": {"compaction-primary": 3, "compaction-checkpoint": 1},
        "answer_relevance": {"answer-compaction-primary": 3, "answer-compaction-checkpoint": 1},
        "query_en": "what must conversation compaction preserve",
        "query_zh": "对话压缩必须保留哪些内容",
        "answer_zh": "压缩须保留目标、约束、决策以及检查点的连续性。",
        "answer_en": "Compaction retains goal, constraints, decisions, and checkpoint continuity.",
    },
    {
        "family": "tenant",
        "relevance": {"tenant-primary": 3, "tenant-authz": 1},
        "answer_relevance": {"answer-tenant-primary": 3, "answer-tenant-authz": 1},
        "query_en": "can retrieval results cross tenant boundaries",
        "query_zh": "检索结果会不会越权访问其他租户的数据",
        "answer_zh": "检索严格限定在当前已认证租户范围内。",
        "answer_en": "Retrieval is restricted to the authenticated tenant.",
    },
]


def build_seed_rows(version: str = SEED_VERSION) -> list[dict[str, Any]]:
    """Return the curated starter rows in the canonical RAG expectations shape."""

    rows: list[dict[str, Any]] = []
    for spec in _FAMILIES:
        family = str(spec["family"])
        base = {
            "owner": "kb-eval",
            "version": version,
            "provenance": PROVENANCE,
            "family": family,
            "corpus_language": CORPUS_LANGUAGE,
        }
        rows.append(
            {
                "case_id": f"kb.seed.{family}.retrieval.en",
                "track": "retrieval_only",
                "query": spec["query_en"],
                "relevance": dict(spec["relevance"]),
                "metadata": {**base, "language": "en", "cross_lingual": False},
            }
        )
        rows.append(
            {
                "case_id": f"kb.seed.{family}.retrieval.zh",
                "track": "retrieval_only",
                "query": spec["query_zh"],
                "relevance": dict(spec["relevance"]),
                "metadata": {**base, "language": "zh", "cross_lingual": True},
            }
        )
        rows.append(
            {
                "case_id": f"kb.seed.{family}.answer.xl",
                "track": "answer_aware",
                "query": spec["query_zh"],
                "relevance": dict(spec["answer_relevance"]),
                "reference_answer": spec["answer_en"],
                "metadata": {
                    **base,
                    "language": "zh",
                    "answer_language": "en",
                    "cross_lingual": True,
                },
            }
        )
    return rows


def render_golden_jsonl(rows: list[dict[str, Any]]) -> bytes:
    """Serialize rows deterministically (sorted keys, compact, trailing newline)."""

    encoded = "\n".join(
        json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
        for row in rows
    ) + "\n"
    return encoded.encode("utf-8")


def build_eval_import_payload(rows: list[dict[str, Any]]) -> dict[str, Any]:
    """Map golden rows onto the existing eval-examples import contract.

    ``validate_case`` (Assistant trace contract) is satisfied with empty
    assistant fields; the RAG semantics ride in metadata and therefore remain
    unvalidated — hence the recommended dedicated table.
    """

    examples = []
    for row in rows:
        metadata = dict(row["metadata"])
        metadata.update(
            {
                "track": row["track"],
                "relevance": dict(row["relevance"]),
                "golden_schema": "kb-golden-qa/v1",
            }
        )
        example: dict[str, Any] = {
            "case_id": row["case_id"],
            "split": "regression",
            "input": {"query": row["query"]},
            "expected_output": {},
            "expected_trajectory": {},
            "assertions": [],
            "metadata": metadata,
        }
        if row.get("reference_answer"):
            example["expected_output"] = {"reference": row["reference_answer"]}
        examples.append(example)
    return {"examples": examples, "mode": "skip_duplicates"}


def _validate_or_fail(rows: list[dict[str, Any]]) -> None:
    if not rows:
        raise ValueError("seed set produced zero rows")
    validation = validate_rag_cases(rows)
    if not validation["valid"]:
        raise ValueError(
            "seed rows fail validate_rag_cases: "
            + json.dumps(validation["errors"], ensure_ascii=False, sort_keys=True)
        )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Emit the versioned bilingual KB golden QA starter set."
    )
    parser.add_argument("--output", default=str(DEFAULT_OUTPUT))
    parser.add_argument(
        "--emit",
        choices=("golden", "eval-import"),
        default="golden",
        help="golden = RAG expectations JSONL for scripts/eval_rag.py; "
        "eval-import = JSON payload for POST /api/v1/eval/datasets/{id}/examples:import",
    )
    parser.add_argument(
        "--version",
        default=SEED_VERSION,
        help="version stamped into every row's metadata",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="compare the rendered bytes against --output and fail on drift; never writes",
    )
    parser.add_argument("--force", action="store_true")
    args = parser.parse_args(argv)
    try:
        rows = build_seed_rows(version=args.version)
        _validate_or_fail(rows)
        if args.emit == "eval-import":
            payload = json.dumps(
                build_eval_import_payload(rows), ensure_ascii=False, indent=2, sort_keys=True
            )
            print(payload)
            return 0
        rendered = render_golden_jsonl(rows)
        target = Path(args.output)
        if args.check:
            if not target.is_file():
                print(f"seed check failed: {target} is missing", file=sys.stderr)
                return 1
            if target.read_bytes() != rendered:
                print(f"seed check failed: {target} drifted from the seed source", file=sys.stderr)
                return 1
            print(json.dumps({"check": "ok", "output": str(target), "case_count": len(rows)}))
            return 0
        if target.exists() and not args.force:
            print(
                f"refusing to overwrite {target} without --force",
                file=sys.stderr,
            )
            return 1
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(rendered)
        print(json.dumps({"written": str(target), "case_count": len(rows)}))
        return 0
    except Exception as exc:  # noqa: BLE001 - CLI must fail closed with concise evidence
        print(f"seed_kb_golden_set failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
