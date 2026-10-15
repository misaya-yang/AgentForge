#!/usr/bin/env python3
"""ARC-00 fact-baseline runner.

Regenerates every machine-readable baseline for the ``2026-08-post-rag``
baseline id into ``docs/architecture/baselines/2026-08-post-rag/``:

* ``service-topology.json``       — deployment units, bounded contexts, routes;
* ``data-access-inventory.json``  — PostgreSQL objects, table readers/writers,
                                    Qdrant/Redis/object-store namespaces;
* ``loc-baseline.json``           — line counts and the oversized-file ledger;
* ``dependency-baseline.json``    — declared deps and cross-unit import edges;
* ``skip-baseline.json``          — every statically discoverable skip/xfail;
* ``contract-freeze.json``        — SHA-256 digests of offline-computable
                                    public-contract artifacts.

Stdlib-only, like every generator it calls: any machine with Python 3.10+
and a checkout can rebuild the baselines.

Determinism contract: at one Git revision the output is byte-identical on
every run (sorted output, no wall-clock fields, identity derived from Git and
file contents only). ``--verify`` recomputes every baseline in memory and
fails closed if the committed copy drifts from the current tree.

Usage:
    python3 scripts/inventory/generate_baselines.py             # verify (safe default)
    python3 scripts/inventory/generate_baselines.py --verify    # verify explicitly
    python3 scripts/inventory/generate_baselines.py --write \
        --source-rev <full-clean-HEAD>                           # reviewed refresh
"""

from __future__ import annotations

import argparse
import importlib
import json
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from _common import (  # noqa: E402
    BASELINE_INPUT_PATHS,
    OUTPUT_DIR,
    BaselineProvenanceError,
    baseline_source_revision,
    clean_git_head,
    require_payload_revision,
    require_source_tree,
)

# (module, output file) in a fixed order; contract_freeze last because it
# consumes data_access and service_topology results.
GENERATORS = (
    ("service_topology", "service-topology.json"),
    ("data_access", "data-access-inventory.json"),
    ("loc_baseline", "loc-baseline.json"),
    ("dependency_baseline", "dependency-baseline.json"),
    ("skip_baseline", "skip-baseline.json"),
    ("contract_freeze", "contract-freeze.json"),
)


def _serialized(payload: dict) -> str:
    return json.dumps(payload, indent=2, sort_keys=True, ensure_ascii=False) + "\n"


def _committed_source_revision() -> str:
    revisions: set[str] = set()
    for _module_name, output_name in GENERATORS:
        target = OUTPUT_DIR / output_name
        if not target.is_file():
            raise BaselineProvenanceError(f"committed baseline is missing: {output_name}")
        try:
            payload = json.loads(target.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            raise BaselineProvenanceError(
                f"committed baseline is unreadable: {output_name}: {exc}"
            ) from exc
        revision = payload.get("base_git_sha")
        if not isinstance(revision, str):
            raise BaselineProvenanceError(
                f"committed baseline has no source revision: {output_name}"
            )
        revisions.add(revision)
    if len(revisions) != 1:
        raise BaselineProvenanceError(
            f"committed baselines declare inconsistent source revisions: {sorted(revisions)}"
        )
    return revisions.pop()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--verify",
        action="store_true",
        help="recompute every baseline and fail if it differs (the default mode)",
    )
    mode.add_argument(
        "--write",
        action="store_true",
        help="refresh baselines after an explicit clean source revision is reviewed",
    )
    parser.add_argument(
        "--source-rev",
        help="full 40-character clean HEAD required with --write",
    )
    args = parser.parse_args()

    verify = not args.write
    try:
        current_head = clean_git_head()
    except (BaselineProvenanceError, subprocess.SubprocessError) as exc:
        print(f"PROVENANCE ERROR: {exc}", file=sys.stderr)
        return 2
    if args.write:
        if args.source_rev is None:
            parser.error("--write requires --source-rev with the full clean HEAD")
        if args.source_rev != current_head:
            print(
                f"PROVENANCE ERROR: --source-rev {args.source_rev!r} "
                f"does not equal clean HEAD {current_head}",
                file=sys.stderr,
            )
            return 2
        source_sha = current_head
    elif args.source_rev is not None:
        parser.error("--source-rev is only valid with --write")
    else:
        try:
            source_sha = _committed_source_revision()
        except BaselineProvenanceError as exc:
            print(f"PROVENANCE ERROR: {exc}", file=sys.stderr)
            return 2

    try:
        require_source_tree(source_sha, included_paths=BASELINE_INPUT_PATHS)
    except (BaselineProvenanceError, subprocess.SubprocessError) as exc:
        print(f"PROVENANCE ERROR: {exc}", file=sys.stderr)
        return 2

    drift = 0
    generated: list[tuple[str, str]] = []
    with baseline_source_revision(source_sha):
        for module_name, output_name in GENERATORS:
            module = importlib.import_module(module_name)
            payload = module.build()
            try:
                require_payload_revision(payload, source_sha, name=output_name)
            except BaselineProvenanceError as exc:
                print(f"PROVENANCE ERROR: {exc}", file=sys.stderr)
                return 2
            text = _serialized(payload)
            # Fail closed on non-serializable content before touching the tree.
            json.loads(text)
            generated.append((output_name, text))

    # A concurrent commit or file edit during the scans invalidates the whole
    # batch.  Check again before comparing or writing any output.
    try:
        clean_git_head(expected_sha=current_head)
        require_source_tree(source_sha, included_paths=BASELINE_INPUT_PATHS)
    except (BaselineProvenanceError, subprocess.SubprocessError) as exc:
        print(f"PROVENANCE ERROR: {exc}", file=sys.stderr)
        return 2

    for output_name, text in generated:
        target = OUTPUT_DIR / output_name
        if verify:
            if not target.is_file():
                print(f"DRIFT  {output_name}: missing on disk")
                drift += 1
            elif target.read_text(encoding="utf-8") != text:
                print(f"DRIFT  {output_name}: tree no longer matches the committed baseline")
                drift += 1
            else:
                print(f"OK     {output_name}")
        else:
            OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")
            print(f"wrote  {target}")

    if verify and drift:
        print(
            f"\n{drift} baseline(s) drifted. At the intended clean source revision, "
            "re-run with `--write --source-rev <full-clean-HEAD>`, review the diff, "
            "and commit the refreshed baseline."
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
