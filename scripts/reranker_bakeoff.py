#!/usr/bin/env python3
"""Bilingual reranker bake-off CLI (PRD T2-#2).

Report-only: runs the contestants over a fixed-candidate case file and writes
JSON + markdown evidence. It never switches serving defaults — promotion of a
winner goes through the T0 evaluation gate afterwards (只跑评测，不切默认).

Usage:

    uv run --all-packages python scripts/reranker_bakeoff.py \
        --cases <file>.jsonl \
        --adapters identity,bge,dashscope:qwen3-rerank \
        --allow-live --out reports/rerank-bakeoff/<date>-run1

Live API adapters (``dashscope:*``, ``cohere:*``, ``jina``) require
``--allow-live`` plus their env key (DASHSCOPE_API_KEY / COHERE_API_KEY /
JINA_API_KEY); keys are read from the environment and never printed.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from dotenv import dotenv_values  # noqa: E402

from src.services.eval.rerank_bakeoff import (  # noqa: E402
    HttpRerankAdapter,
    IdentityAdapter,
    RerankerAdapter,
    bake_off,
    load_cases,
    render_markdown,
)

REPO_ROOT = Path(__file__).resolve().parents[1]
_FILE_VALUES = dotenv_values(os.environ.get("ENV_FILE") or REPO_ROOT / ".env")


def _secret(env_name: str) -> str:
    """Resolve a key from the process env or the ENV_FILE/repo .env; never printed."""
    return (os.environ.get(env_name) or _FILE_VALUES.get(env_name) or "").strip()


JINA_RERANK_URL = "https://api.jina.ai/v1/rerank"
JINA_DEFAULT_MODEL = "jina-reranker-v3"
LIVE_PROVIDERS = {"dashscope", "cohere", "jina"}
PROVIDER_KEY_ENV = {
    "dashscope": "DASHSCOPE_API_KEY",
    "cohere": "COHERE_API_KEY",
    "jina": "JINA_API_KEY",
}
# Non-secret endpoint/schema config the reranker factory reads through
# get_settings(), which only consults the process env. Serving gets these from
# the container environment; the bake-off injects them from the same ENV_FILE
# so live runs exercise the same resolution serving would.
PROVIDER_EXTRA_ENV = {
    "dashscope": (
        "DASHSCOPE_BASE_URL",
        "DASHSCOPE_RERANK_BASE_URL",
        "DASHSCOPE_RERANK_REQUEST_SCHEMA",
        "DASHSCOPE_RERANK_INSTRUCT",
    ),
}


def _parse_adapter(spec: str):
    provider, _, model = spec.partition(":")
    provider = provider.strip().lower()
    model = model.strip() or None
    if provider == "identity":
        return IdentityAdapter()
    if provider in {"bge", "dashscope", "cohere"}:
        from knowledge_service.services.knowledge.text_reranker import create_reranker

        return RerankerAdapter(provider=provider, model=model, reranker_factory=create_reranker)
    if provider == "jina":
        return HttpRerankAdapter(
            name=f"jina:{model or JINA_DEFAULT_MODEL}",
            url=JINA_RERANK_URL,
            model=model or JINA_DEFAULT_MODEL,
            api_key="",  # filled by _require_live_keys
        )
    raise ValueError(f"unknown adapter provider: {provider!r}")


def _require_live_keys(adapters, specs: list[str], allow_live: bool) -> None:
    live_needed = [s for s in specs if s.partition(":")[0].lower() in LIVE_PROVIDERS]
    if live_needed and not allow_live:
        raise SystemExit(f"live adapters {live_needed} require --allow-live (paid API calls)")
    for adapter, spec in zip(adapters, specs, strict=True):
        provider = spec.partition(":")[0].lower()
        env_name = PROVIDER_KEY_ENV.get(provider)
        if not env_name:
            continue
        key = _secret(env_name)
        if not key:
            raise SystemExit(f"{env_name} is not set (required by {spec})")
        # Both live adapter kinds carry the key as an ``api_key`` attribute.
        adapter.api_key = key
        os.environ.setdefault(env_name, key)
        for extra in PROVIDER_EXTRA_ENV.get(provider, ()):
            value = _secret(extra)
            if value:
                os.environ.setdefault(extra, value)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--cases", required=True, help="bake-off case JSONL")
    parser.add_argument(
        "--adapters",
        default="identity,bge",
        help="comma-separated specs: identity | bge[:model] | "
        "dashscope[:model] | cohere[:model] | jina[:model]",
    )
    parser.add_argument("--top-k", type=int, default=10)
    parser.add_argument("--allow-live", action="store_true")
    parser.add_argument(
        "--out",
        required=True,
        help="output path stem; .json and .md are written next to it",
    )
    args = parser.parse_args(argv)

    cases = load_cases(args.cases)
    specs = [s.strip() for s in args.adapters.split(",") if s.strip()]
    if not specs or "identity" not in [s.partition(":")[0] for s in specs]:
        raise SystemExit("the identity baseline must be part of --adapters (gate)")
    adapters = [_parse_adapter(spec) for spec in specs]
    _require_live_keys(adapters, specs, allow_live=args.allow_live)

    report = asyncio.run(bake_off(cases, adapters, k=args.top_k))

    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.with_suffix(".json").write_text(json.dumps(report, ensure_ascii=False, indent=2), "utf-8")
    out.with_suffix(".md").write_text(render_markdown(report), "utf-8")
    gate = report["gate"]
    print(f"winner={gate.get('winner')} promotable={gate.get('promotable')}")
    print(f"report: {out.with_suffix('.json')} / {out.with_suffix('.md')}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
