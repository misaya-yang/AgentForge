#!/usr/bin/env python3
"""Record RAG observations from the live KS retrieval plane (T0 companion gate).

Companion to ``scripts/eval_rag.py``: the deterministic gate consumes
pre-recorded observation JSONL, while this script regenerates that evidence by
issuing strictly read-only ``POST /api/v1/knowledge/{dataset_id}/retrieve``
calls against a running knowledge service, and verifies the versioned golden
manifest.  It never fabricates or partially records observations: every case
must succeed before any byte is written.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

import httpx

from src.services.eval.rag_regression import validate_rag_cases, validate_rag_observations

DEFAULT_URL = "http://localhost:8092"
DEFAULT_TOP_K = 5
DEFAULT_MANIFEST = Path("tests/fixtures/eval/rag/golden/manifest.json")
RETRIEVE_PATH = "/api/v1/knowledge/{dataset_id}/retrieve"
IDENTITY_HEADER_FLAGS = (
    ("X-User-Id", "user_id"),
    ("X-Tenant-Id", "tenant_id"),
    ("X-User-Tier", "user_tier"),
    ("X-User-Type", "user_type"),
    ("X-User-Roles", "user_roles"),
)


class RecordingError(ValueError):
    """Fail-closed recording/verification error carrying precise CLI evidence."""


def _load_jsonl_snapshot(path: str | Path) -> tuple[list[dict[str, Any]], str]:
    """Parse and hash the same immutable byte snapshot (mirrors eval_rag)."""

    raw = Path(path).read_bytes()
    digest = hashlib.sha256(raw).hexdigest()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise RecordingError(f"Invalid UTF-8 JSONL: {path}") from exc
    rows: list[dict[str, Any]] = []
    for line_no, line in enumerate(text.splitlines(), start=1):
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except json.JSONDecodeError as exc:
            raise RecordingError(f"Invalid JSONL at line {line_no}: {exc}") from exc
        if not isinstance(row, dict):
            raise RecordingError(f"JSONL row {line_no} must be an object")
        rows.append(row)
    return rows, digest


def canonical_row_json(row: dict[str, Any]) -> str:
    """Serialize one JSONL row deterministically (fixture byte-for-byte style)."""

    return json.dumps(row, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def serialize_jsonl(rows: list[dict[str, Any]]) -> bytes:
    """Serialize rows in the given order with a single trailing newline."""

    if not rows:
        raise RecordingError("refusing to emit an empty observations file")
    encoded = "\n".join(canonical_row_json(row) for row in rows) + "\n"
    return encoded.encode("utf-8")


def select_recordable_cases(
    cases: list[dict[str, Any]],
    *,
    retrieval_only: bool,
) -> list[dict[str, Any]]:
    """Filter and gate the case set that the retrieval plane can honestly record."""

    selected = [case for case in cases if case.get("track") == "retrieval_only"] if (
        retrieval_only
    ) else list(cases)
    if not selected:
        raise RecordingError("zero cases to record after --retrieval-only filtering")
    answer_cases = [str(case["case_id"]) for case in selected if case.get("track") == "answer_aware"]
    if answer_cases:
        preview = ", ".join(answer_cases[:5])
        raise RecordingError(
            f"answer_aware cases cannot be recorded from the retrieval plane "
            f"({len(answer_cases)}: {preview}); /retrieve emits ranked segments only and "
            "generated answers belong to the recorded judge/answer channel — pass "
            "--retrieval-only to record the retrieval track"
        )
    return selected


def resolve_dataset_bindings(
    cases: list[dict[str, Any]],
    *,
    override: str | None,
) -> dict[str, str]:
    """Map every recorded case to a KS dataset id (flag override or row binding)."""

    bindings: dict[str, str] = {}
    missing: list[str] = []
    for case in cases:
        case_id = str(case.get("case_id"))
        if override is not None:
            bindings[case_id] = override
            continue
        metadata = case.get("metadata") if isinstance(case.get("metadata"), dict) else {}
        dataset_id = metadata.get("dataset_id")
        if isinstance(dataset_id, str) and dataset_id.strip():
            bindings[case_id] = dataset_id.strip()
        else:
            missing.append(case_id)
    if missing:
        preview = ", ".join(missing[:5])
        raise RecordingError(
            f"{len(missing)} case(s) carry no dataset binding ({preview}); pass "
            "--dataset-id or set metadata.dataset_id on every expectations row"
        )
    return bindings


def build_retrieval_replay(
    case: dict[str, Any],
    payload: Any,
) -> dict[str, Any]:
    """Map one KS /retrieve response to the exact replay shape the validator expects."""

    case_id = str(case.get("case_id"))
    if not isinstance(payload, dict):
        raise RecordingError(f"case {case_id!r}: KS response must be a JSON object")
    results = payload.get("results")
    if not isinstance(results, list):
        raise RecordingError(
            f"case {case_id!r}: KS response missing a 'results' list "
            "(unexpected response shape; aborting without recording)"
        )
    ranked: list[str] = []
    for index, item in enumerate(results, start=1):
        if not isinstance(item, dict):
            raise RecordingError(f"case {case_id!r}: results[{index}] is not an object")
        segment_id = item.get("segment_id")
        if not isinstance(segment_id, str) or not segment_id.strip():
            raise RecordingError(f"case {case_id!r}: results[{index}] has no usable segment_id")
        ranked.append(segment_id.strip())
    if not ranked:
        raise RecordingError(
            f"case {case_id!r}: KS returned zero results; refusing to emit an "
            "observation that violates the validator contract"
        )
    return {
        "status": "succeeded",
        "ranked_segment_ids": ranked,
        "answer_source": "retrieval_only",
        "answer": None,
    }


def record_observations(
    cases: list[dict[str, Any]],
    bindings: dict[str, str],
    fetch: Callable[[str, str, int], Any],
    *,
    top_k: int,
) -> list[dict[str, Any]]:
    """Fetch every case first; only then build rows (never partial evidence)."""

    replays: dict[str, dict[str, Any]] = {}
    for case in cases:
        case_id = str(case["case_id"])
        payload = fetch(bindings[case_id], str(case["query"]), top_k)
        replays[case_id] = build_retrieval_replay(case, payload)
    rows = [
        {"case_id": str(case["case_id"]), "replay": replays[str(case["case_id"])]}
        for case in cases
    ]
    observation_validation = validate_rag_observations(
        cases, {row["case_id"]: row["replay"] for row in rows}
    )
    if not observation_validation["valid"]:
        raise RecordingError(
            "recorded observations violate the validator contract: "
            + json.dumps(observation_validation["errors"], ensure_ascii=False, sort_keys=True)
        )
    return rows


GATEWAY_SECRET_ENV = "AI_PLATFORM_INTERNAL_TOKEN"
GATEWAY_SECRET_HEADER = "X-Gateway-Secret"


def _identity_headers(args: argparse.Namespace) -> dict[str, str]:
    headers: dict[str, str] = {}
    for header, flag in IDENTITY_HEADER_FLAGS:
        value = getattr(args, flag)
        if value is not None and str(value).strip():
            headers[header] = str(value).strip()
    # The live KS fronts the gateway-forwarded identity headers with a shared
    # secret (auth/gateway_secret_mw.py). The value comes only from the
    # environment and is never printed by this CLI.
    secret = os.environ.get(GATEWAY_SECRET_ENV, "").strip()
    if secret:
        headers[GATEWAY_SECRET_HEADER] = secret
    return headers


def _http_fetch_factory(client: Any, base_url: str) -> Callable[[str, str, int], Any]:
    def fetch(dataset_id: str, query: str, top_k: int) -> Any:
        url = base_url.rstrip("/") + RETRIEVE_PATH.format(dataset_id=dataset_id)
        try:
            response = client.post(url, json={"query": query, "top_k": top_k})
        except httpx.HTTPError as exc:
            raise RecordingError(
                f"retrieval plane unreachable at {url}: {exc.__class__.__name__}: {exc}"
            ) from exc
        if response.status_code != 200:
            detail = response.text[:400].replace("\n", " ")
            raise RecordingError(
                f"KS returned HTTP {response.status_code} for {url}; aborting without "
                f"recording. Response body (truncated): {detail}"
            )
        try:
            return response.json()
        except ValueError as exc:
            raise RecordingError(f"KS response for {url} is not valid JSON") from exc

    return fetch


def cmd_record(args: argparse.Namespace) -> int:
    cases, expectations_sha256 = _load_jsonl_snapshot(args.expectations)
    if not cases:
        raise RecordingError(f"expectations file {args.expectations} contains zero cases")
    case_validation = validate_rag_cases(cases)
    if not case_validation["valid"]:
        raise RecordingError(
            "expectations failed validate_rag_cases: "
            + json.dumps(case_validation["errors"], ensure_ascii=False, sort_keys=True)
        )
    selected = select_recordable_cases(cases, retrieval_only=args.retrieval_only)
    bindings = resolve_dataset_bindings(selected, override=args.dataset_id)
    plan = {
        "mode": "dry-run" if args.dry_run else "record",
        "expectations": {"source": str(Path(args.expectations)), "sha256": expectations_sha256},
        "endpoint": args.url.rstrip("/") + RETRIEVE_PATH.format(dataset_id="{dataset_id}"),
        "case_count": len(selected),
        "case_ids": [str(case["case_id"]) for case in selected],
        "datasets": sorted(set(bindings.values())),
        "top_k": args.top_k,
        "identity_headers": sorted(_identity_headers(args)),
        "output": None if args.dry_run else str(Path(args.output)),
    }
    if args.dry_run:
        _print(plan)
        return 0
    if Path(args.output).exists() and not args.force:
        raise RecordingError(f"refusing to overwrite {args.output} without --force")

    import httpx  # noqa: PLC0415 - network dependency only needed for a live record

    with httpx.Client(
        headers=_identity_headers(args),
        timeout=args.timeout,
    ) as client:
        rows = record_observations(
            selected,
            bindings,
            _http_fetch_factory(client, args.url),
            top_k=args.top_k,
        )
    payload_bytes = serialize_jsonl(rows)
    target = Path(args.output)
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(payload_bytes)
    _print(
        {
            "recorded_case_count": len(rows),
            "expectations_sha256": expectations_sha256,
            "observations": {"source": str(target), "sha256": hashlib.sha256(payload_bytes).hexdigest()},
            "endpoint": args.url.rstrip("/") + RETRIEVE_PATH.format(dataset_id="{dataset_id}"),
        }
    )
    return 0


def _is_sha256_hex(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def verify_manifest(manifest_path: str | Path) -> dict[str, Any]:
    """Recompute every manifest-listed file hash; pure and network-free."""

    path = Path(manifest_path)
    if not path.is_file():
        raise RecordingError(f"manifest not found: {path}")
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise RecordingError(f"manifest is not valid JSON: {path}: {exc}") from exc
    if not isinstance(manifest, dict):
        raise RecordingError(f"manifest must be a JSON object: {path}")
    version = manifest.get("version")
    if not isinstance(version, str) or not version.strip():
        raise RecordingError(f"manifest requires a non-empty 'version': {path}")
    files = manifest.get("files")
    if not isinstance(files, dict) or not files:
        raise RecordingError(f"manifest requires a non-empty 'files' object: {path}")
    entries: list[dict[str, Any]] = []
    for relpath in sorted(files):
        spec = files[relpath]
        if not isinstance(spec, dict) or not _is_sha256_hex(spec.get("sha256")):
            raise RecordingError(
                f"manifest entry {relpath!r} requires a lowercase sha256 hex digest"
            )
        target = (path.parent / relpath).resolve()
        entry: dict[str, Any] = {
            "file": relpath,
            "expected_sha256": spec["sha256"],
            "purpose": spec.get("purpose"),
        }
        if not target.is_file():
            entry["status"] = "missing"
        else:
            actual = hashlib.sha256(target.read_bytes()).hexdigest()
            entry["actual_sha256"] = actual
            entry["status"] = "ok" if actual == spec["sha256"] else "mismatch"
        entries.append(entry)
    return {
        "valid": all(entry["status"] == "ok" for entry in entries),
        "version": version,
        "frozen_at": manifest.get("frozen_at"),
        "manifest": str(path),
        "entries": entries,
    }


def cmd_verify(args: argparse.Namespace) -> int:
    report = verify_manifest(args.manifest)
    _print(report)
    return 0 if report["valid"] else 1


def _print(payload: dict[str, object]) -> None:
    print(json.dumps(payload, indent=2, sort_keys=True))


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Record live RAG observations and verify the golden manifest."
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    record = subparsers.add_parser(
        "record",
        help="regenerate observations JSONL from the live KS retrieval plane (read-only)",
    )
    record.add_argument("--expectations", required=True)
    record.add_argument("--output", default="tmp/eval-e1/observations-live.jsonl")
    record.add_argument("--url", default=DEFAULT_URL)
    record.add_argument("--dataset-id", default=None)
    record.add_argument("--user-id", default=None)
    record.add_argument("--tenant-id", default=None)
    record.add_argument("--user-tier", default=None)
    record.add_argument("--user-type", default=None)
    record.add_argument(
        "--user-roles",
        default=None,
        help="comma-separated roles forwarded as X-User-Roles",
    )
    record.add_argument("--top-k", type=int, default=DEFAULT_TOP_K)
    record.add_argument("--timeout", type=float, default=30.0)
    record.add_argument(
        "--retrieval-only",
        action="store_true",
        help="restrict recording to retrieval_only cases (the answer track needs the "
        "recorded judge/answer channel, which this script never fabricates)",
    )
    record.add_argument("--force", action="store_true")
    record.add_argument(
        "--dry-run",
        action="store_true",
        help="validate expectations and print the recording plan without network calls",
    )
    record.set_defaults(func=cmd_record)

    verify = subparsers.add_parser(
        "verify", help="recompute sha256 of every file listed in the golden manifest"
    )
    verify.add_argument("--manifest", default=str(DEFAULT_MANIFEST))
    verify.set_defaults(func=cmd_verify)
    return parser


def main(argv: list[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    try:
        return int(args.func(args))
    except Exception as exc:  # noqa: BLE001 - CLI must fail closed with concise evidence
        print(f"regen_rag_observations failed: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
