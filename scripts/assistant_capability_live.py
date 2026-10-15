#!/usr/bin/env python3
"""Gate result-level Assistant×KB live receipts.

The runner that exercises a real environment may be pytest, Playwright, or an
operator script. This CLI deliberately accepts only bounded assertion receipts,
not raw prompts, provider payloads, credentials, or model output.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from src.services.eval.assistant_capability import (
    CapabilityCasePolicy,
    CapabilityTrialReceipt,
    evaluate_capability_suite,
)


def _load_jsonl(path: str | Path) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for line_number, line in enumerate(
        Path(path).read_text(encoding="utf-8").splitlines(), start=1
    ):
        if not line.strip():
            continue
        try:
            value = json.loads(line)
        except json.JSONDecodeError as exc:
            raise ValueError(f"invalid JSONL at line {line_number}: {exc}") from exc
        if not isinstance(value, dict):
            raise ValueError(f"JSONL line {line_number} must be an object")
        rows.append(value)
    return rows


def gate(
    manifest_path: str | Path,
    receipts_path: str | Path,
    *,
    minimum_case_pass_rate: float,
) -> dict[str, Any]:
    policies = [CapabilityCasePolicy.from_dict(row) for row in _load_jsonl(manifest_path)]
    receipts = [CapabilityTrialReceipt.from_dict(row) for row in _load_jsonl(receipts_path)]
    return evaluate_capability_suite(
        policies,
        receipts,
        minimum_case_pass_rate=minimum_case_pass_rate,
    )


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Gate Assistant×KB result-level receipts.")
    parser.add_argument("manifest")
    parser.add_argument("receipts")
    parser.add_argument("--minimum-case-pass-rate", type=float, default=0.9)
    parser.add_argument("--output")
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        result = gate(
            args.manifest,
            args.receipts,
            minimum_case_pass_rate=args.minimum_case_pass_rate,
        )
        rendered = json.dumps(result, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
        if args.output:
            Path(args.output).write_text(rendered, encoding="utf-8")
        else:
            print(rendered, end="")
        return 0 if result["passed"] else 1
    except Exception as exc:  # noqa: BLE001 - CLI must fail closed with concise evidence
        print(f"assistant_capability_live failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
