#!/usr/bin/env python3
"""Emit the bilingual reranker bake-off case JSONL (PRD T2-2c).

The T0 golden fixtures (tests/fixtures/eval/rag/golden/) are retrieval
*expectations* — a query joined to graded segment ids — but they carry no
candidate text, which the reranker bake-off needs (each case must ship a fixed
candidate list so the comparison isolates rerank quality from recall). This
script authors a self-contained bilingual case set over the same six topic
families, reusing the seeded queries so provenance stays reviewable.

Corpus is intentionally imperfect at baseline: candidates are listed in an
order that puts distractors ahead of the grade-3 primary, so the identity
baseline (no rerank) scores below 1.0 on the Chinese slice and a genuinely
better reranker has headroom to win the gate. Relevance follows graded nDCG
convention (grade 3 primary, grade 1 supporting, unlisted = grade 0).

Read-only w.r.t. T0: it imports nothing from the golden store, and it never
rewrites any T0 fixture. Output is deterministic (stable key order, fixed
corpus), so it is safe to re-run and diff.

    uv run --all-packages python scripts/build_rerank_bakeoff_cases.py \
        --out tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from src.services.eval.rerank_bakeoff import load_cases  # noqa: E402

CASE_VERSION = "rerank-bakeoff-v1"
DEFAULT_OUT = Path("tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl")

# id -> (text, ) for the whole bilingual corpus. Every segment carries a
# Chinese or English surface form; primaries mirror the seeded answers and are
# written bilingual so a single-language query must still match them.
CORPUS: dict[str, str] = {
    # refund
    "refund-primary": "年付订阅的退款资格以公示的退款窗口期为准。Annual refunds follow "
    "the published eligibility window.",
    "refund-window": "退款窗口在订阅续期前 7 天开启，逾期不可退。The refund window "
    "opens 7 days before renewal and closes at renewal.",
    "refund-near-monthly": "月付订阅可随时取消，不产生退款。Monthly plans can be "
    "cancelled anytime and are never refunded.",
    "refund-near-coupon": "优惠券一经使用不予退还。Used coupons are non-refundable.",
    "refund-near-invoice": "发票金额与实付一致。Invoice amounts match the charged total.",
    # password
    "password-primary": "重置链接会发送到已验证的账户渠道。A reset link is sent to "
    "the verified account channel.",
    "password-window": "已验证渠道包括注册邮箱与绑定手机号。Verified channels are "
    "the registered email and the bound phone number.",
    "password-near-2fa": "双因素验证码在登录时输入，与密码重置无关。Two-factor "
    "codes are for login, not password reset.",
    "password-near-change": "修改密码需先登录并进入安全设置。Changing a password "
    "requires signing in first.",
    "password-near-session": "会话令牌过期后需重新登录。Session tokens expire and "
    "require re-login.",
    # invoice
    "invoice-primary": "最新发票可在账单历史中查看。The latest invoice is available "
    "in billing history.",
    "invoice-window": "账单历史保留最近 24 个月的发票记录。Billing history keeps the "
    "last 24 months of invoices.",
    "invoice-near-receipt": "支付即时回执显示在付款确认页。Immediate payment "
    "receipts appear on the confirmation page.",
    "invoice-near-tax": "税额按注册地区规则自动计算。Tax is calculated by region.",
    "invoice-near-refund": "退款不会生成新发票。Refunds do not create a new invoice.",
    # approval
    "approval-primary": "仅在存在当前且匹配的有效审批后，工具才会执行。A high-risk "
    "tool executes only after a current matching approval.",
    "approval-window": "审批绑定具体工具与参数，过期即失效。Approvals are bound to a "
    "specific tool and expire.",
    "approval-near-log": "审计日志记录每次工具调用结果。Audit logs record each tool call outcome.",
    "approval-near-lowrisk": "低风险工具无需审批即可运行。Low-risk tools run without approval.",
    "approval-near-scope": "工具权限范围在安装时确定。Tool scope is fixed at install.",
    # compaction
    "compaction-primary": "压缩须保留目标、约束、决策以及检查点的连续性。Compaction "
    "retains goal, constraints, decisions, and checkpoint continuity.",
    "compaction-window": "被压缩的原始轮次仍可回溯到检查点。Compacted turns stay "
    "traceable to a checkpoint.",
    "compaction-near-summary": "摘要只是压缩的副产物，不保证决策连续。A summary is a "
    "byproduct and need not preserve decisions.",
    "compaction-near-truncate": "直接截断会丢弃早期上下文，不属于压缩。Hard truncation "
    "drops early context; it is not compaction.",
    "compaction-near-token": "令牌计数用于成本估算。Token counting is for cost estimates.",
    # tenant
    "tenant-primary": "检索严格限定在当前已认证租户范围内。Retrieval is restricted to "
    "the authenticated tenant.",
    "tenant-window": "跨租户命中会在授权层被过滤丢弃。Cross-tenant hits are filtered "
    "at the authorization layer.",
    "tenant-near-cache": "缓存键含租户命名空间。Cache keys carry a tenant namespace.",
    "tenant-near-share": "共享链接需显式授权才可访问。Shared links require explicit "
    "grant to access.",
    "tenant-near-metric": "指标按租户维度聚合。Metrics are aggregated per tenant.",
}

# case_id, family, query, language, cross_lingual, [candidate ids in baseline
# order], relevance overrides. Baseline order lists near-miss distractors
# first so identity (fusion order) is imperfect and rerank has headroom.
_CASE_SPECS: list[dict[str, Any]] = [
    {
        "family": "refund",
        "language": "zh",
        "query": "年付订阅如何申请退款",
        "cross_lingual": True,
        "candidates": [
            "refund-near-monthly",
            "refund-primary",
            "refund-window",
            "refund-near-coupon",
        ],
        "relevance": {"refund-primary": 3, "refund-window": 1},
    },
    {
        "family": "refund",
        "language": "en",
        "query": "how to get a refund for an annual subscription",
        "cross_lingual": False,
        "candidates": [
            "refund-near-coupon",
            "refund-window",
            "refund-primary",
            "refund-near-invoice",
        ],
        "relevance": {"refund-primary": 3, "refund-window": 1},
    },
    {
        "family": "password",
        "language": "zh",
        "query": "密码重置链接会发送到哪里",
        "cross_lingual": True,
        "candidates": [
            "password-near-2fa",
            "password-window",
            "password-primary",
            "password-near-session",
        ],
        "relevance": {"password-primary": 3, "password-window": 1},
    },
    {
        "family": "password",
        "language": "en",
        "query": "where is the password reset link delivered",
        "cross_lingual": False,
        "candidates": [
            "password-near-change",
            "password-primary",
            "password-window",
            "password-near-2fa",
        ],
        "relevance": {"password-primary": 3, "password-window": 1},
    },
    {
        "family": "invoice",
        "language": "zh",
        "query": "在哪里可以下载最近一期发票",
        "cross_lingual": True,
        "candidates": [
            "invoice-near-tax",
            "invoice-primary",
            "invoice-window",
            "invoice-near-refund",
        ],
        "relevance": {"invoice-primary": 3, "invoice-window": 1},
    },
    {
        "family": "invoice",
        "language": "en",
        "query": "where can I download my most recent invoice",
        "cross_lingual": False,
        "candidates": [
            "invoice-near-receipt",
            "invoice-window",
            "invoice-primary",
            "invoice-near-tax",
        ],
        "relevance": {"invoice-primary": 3, "invoice-window": 1},
    },
    {
        "family": "approval",
        "language": "zh",
        "query": "高风险工具需要什么审批才能执行",
        "cross_lingual": True,
        "candidates": [
            "approval-near-lowrisk",
            "approval-window",
            "approval-primary",
            "approval-near-scope",
        ],
        "relevance": {"approval-primary": 3, "approval-window": 1},
    },
    {
        "family": "approval",
        "language": "en",
        "query": "when is a high-risk tool allowed to execute",
        "cross_lingual": False,
        "candidates": [
            "approval-near-log",
            "approval-primary",
            "approval-window",
            "approval-near-lowrisk",
        ],
        "relevance": {"approval-primary": 3, "approval-window": 1},
    },
    {
        "family": "compaction",
        "language": "zh",
        "query": "对话压缩必须保留哪些内容",
        "cross_lingual": True,
        "candidates": [
            "compaction-near-summary",
            "compaction-primary",
            "compaction-window",
            "compaction-near-token",
        ],
        "relevance": {"compaction-primary": 3, "compaction-window": 1},
    },
    {
        "family": "compaction",
        "language": "en",
        "query": "what must conversation compaction preserve",
        "cross_lingual": False,
        "candidates": [
            "compaction-near-truncate",
            "compaction-window",
            "compaction-primary",
            "compaction-near-summary",
        ],
        "relevance": {"compaction-primary": 3, "compaction-window": 1},
    },
    {
        "family": "tenant",
        "language": "zh",
        "query": "检索结果会不会越权访问其他租户的数据",
        "cross_lingual": True,
        "candidates": [
            "tenant-near-share",
            "tenant-window",
            "tenant-primary",
            "tenant-near-metric",
        ],
        "relevance": {"tenant-primary": 3, "tenant-window": 1},
    },
    {
        "family": "tenant",
        "language": "en",
        "query": "can retrieval results cross tenant boundaries",
        "cross_lingual": False,
        "candidates": ["tenant-near-cache", "tenant-primary", "tenant-window", "tenant-near-share"],
        "relevance": {"tenant-primary": 3, "tenant-window": 1},
    },
]


def build_rows() -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for spec in _CASE_SPECS:
        lang = spec["language"]
        case_id = f"kb.bakeoff.{spec['family']}.{lang}"
        candidate_ids = spec["candidates"]
        unknown = [c for c in candidate_ids if c not in CORPUS]
        if unknown:
            raise ValueError(f"{case_id}: candidates missing from corpus: {unknown}")
        rows.append(
            {
                "case_id": case_id,
                "query": spec["query"],
                "language": lang,
                "cross_lingual": bool(spec["cross_lingual"]),
                "relevance": spec["relevance"],
                "candidates": [{"id": cid, "text": CORPUS[cid]} for cid in candidate_ids],
                "metadata": {
                    "version": CASE_VERSION,
                    "family": spec["family"],
                    "owner": "kb-eval",
                    "provenance": "bake-off corpus, human-review-pending",
                },
            }
        )
    return rows


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--out", default=str(DEFAULT_OUT), help=f"output JSONL (default {DEFAULT_OUT})"
    )
    parser.add_argument("--stdout", action="store_true", help="print instead of writing")
    args = parser.parse_args(argv)

    rows = build_rows()
    payload = "\n".join(
        json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":")) for row in rows
    )
    text = payload + "\n"

    if args.stdout:
        sys.stdout.write(text)
        return 0

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(text, "utf-8")

    # Fail-closed self-check: the file we just wrote must load under the same
    # contract the bake-off enforces at run time.
    cases = load_cases(out)
    zh = sum(1 for c in cases if c.language == "zh")
    en = sum(1 for c in cases if c.language == "en")
    print(f"wrote {len(cases)} cases ({zh} zh / {en} en) -> {out}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
