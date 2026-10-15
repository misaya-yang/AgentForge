#!/usr/bin/env python3
"""Import a manifest-pinned golden JSONL into the versioned Postgres store.

PRD T0-#2 storage path: the manifest-hashed git JSONL candidate is projected
one way into ``kb_eval_golden`` (migration 104) so admin surfaces can cite a
server-pinned version, split into the frozen regression set and the growth set.
``make kb-golden-gate`` checks structure only; release readiness is decided by
``make kb-release-evidence-gate`` after human review and real-baseline binding.

Fail-closed contract: every case passes ``validate_rag_cases`` (the same
gateway-side contract the eval gate enforces) before a single row is written;
rows arriving with a metadata version disagree with --version/each other
abort the import.  Promotion to ``frozen`` is never a side effect here —
import lands rows as ``growth`` (or their metadata.split) and review promotes
via KbEvalGoldenStore.set_split.

Usage (dev Postgres resolved from .env, never printed):

    ENV_FILE=/path/to/.env uv run --all-packages python scripts/import_kb_eval_golden.py \
        tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl --dry-run
    ... --pin-release current
"""

from __future__ import annotations

import argparse
import json
import os
import sys
from pathlib import Path
from typing import Any

from dotenv import dotenv_values
from knowledge_service.persistence.kb_eval_golden_store import (
    DEFAULT_RELEASE_KEY,
    KbEvalGoldenStore,
)

from src.services.eval.rag_regression import validate_rag_cases


def _load_cases(path: Path) -> list[dict[str, Any]]:
    text = path.read_text(encoding="utf-8")
    rows: list[dict[str, Any]] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as exc:
            raise ValueError(f"Invalid JSONL at line {line_no}: {exc}") from exc
        if not isinstance(row, dict):
            raise ValueError(f"JSONL row {line_no} must be an object")
        rows.append(row)
    return rows


def resolve_version(cases: list[dict[str, Any]], requested: str | None) -> str:
    """The import version: --version wins; else all rows must agree on metadata.version."""
    versions = {
        str(case.get("metadata", {}).get("version") or "")
        for case in cases
    }
    versions.discard("")
    if requested:
        if versions - {requested}:
            raise ValueError(
                f"--version {requested!r} contradicts row metadata versions {sorted(versions)}"
            )
        return requested
    if len(versions) == 1:
        return versions.pop()
    raise ValueError(
        f"rows carry {len(versions)} distinct metadata versions {sorted(versions)}; pass --version"
    )


def _postgres_config() -> dict[str, Any]:
    root = Path(__file__).resolve().parents[1]
    file_values = dotenv_values(os.environ.get("ENV_FILE") or root / ".env")
    required = ("POSTGRES_USER", "POSTGRES_PASSWORD", "POSTGRES_DB", "POSTGRES_PORT")
    values = {key: os.environ.get(key) or file_values.get(key) for key in required}
    missing = [key for key, value in values.items() if not value]
    if missing:
        raise RuntimeError(
            f"PostgreSQL configuration missing keys: {', '.join(missing)} (set ENV_FILE)"
        )
    return {
        "host": os.environ.get("POSTGRES_HOST") or file_values.get("POSTGRES_HOST") or "127.0.0.1",
        "port": int(str(values["POSTGRES_PORT"])),
        "user": str(values["POSTGRES_USER"]),
        "password": str(values["POSTGRES_PASSWORD"]),
        "database": str(values["POSTGRES_DB"]),
    }


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "expectations",
        nargs="?",
        default="tests/fixtures/eval/rag/golden/kb_golden_qa_v1.jsonl",
        help="golden JSONL in the validate_rag_cases shape",
    )
    parser.add_argument("--version", default=None, help="pin the import version (default: metadata.version)")
    parser.add_argument(
        "--split",
        default="growth",
        choices=("frozen", "growth"),
        help="default split for rows without metadata.split",
    )
    parser.add_argument("--pin-release", action="store_true", help="point the release key at this version")
    parser.add_argument("--release-key", default=DEFAULT_RELEASE_KEY)
    parser.add_argument("--note", default="", help="note recorded with --pin-release")
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="validate and print the plan without touching the database",
    )
    args = parser.parse_args(argv)

    path = Path(args.expectations)
    try:
        cases = _load_cases(path)
        report = validate_rag_cases(cases)
        if not report["valid"]:
            for entry in report["errors"]:
                print(f"invalid golden case {entry['case_id']}: {'; '.join(entry['errors'])}", file=sys.stderr)
            return 2
        version = resolve_version(cases, args.version)
    except (ValueError, RuntimeError, FileNotFoundError, OSError) as exc:
        print(f"golden import refused: {exc}", file=sys.stderr)
        return 2

    splits = {"frozen": 0, "growth": 0}
    for case in cases:
        split = str(case.get("metadata", {}).get("split") or args.split)
        splits[split] = splits.get(split, 0) + 1
    plan = {"file": str(path), "version": version, "cases": len(cases), **splits}
    if args.dry_run:
        plan["mode"] = "dry-run"
        print(json.dumps(plan, ensure_ascii=False, sort_keys=True))
        return 0

    import asyncio

    async def _run() -> dict[str, Any]:
        import asyncpg

        pool = await asyncpg.create_pool(**_postgres_config(), min_size=1, max_size=2)
        try:
            store = KbEvalGoldenStore(pool)
            counts = await store.import_cases(
                cases, version=version, default_split=args.split
            )
            if args.pin_release:
                await store.pin_release(version, release_key=args.release_key, note=args.note)
                counts["pinned"] = 1
            return counts
        finally:
            await pool.close()

    try:
        counts = asyncio.run(_run())
    except Exception as exc:  # fail closed: any write-side error aborts the command
        print(f"golden import failed: {type(exc).__name__}: {exc}", file=sys.stderr)
        return 1
    plan.update(counts, mode="imported", pinned=bool(args.pin_release))
    print(json.dumps(plan, ensure_ascii=False, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
