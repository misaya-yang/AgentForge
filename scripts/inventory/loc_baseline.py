"""LOC baseline.

Produces ``loc-baseline.json``: recomputed line counts for the whole tree,
plus the oversized-file ledger (Python > 800 lines, TypeScript/TSX > 500
lines) that ARC-07/AC-M22 holds at no-growth.

The PRD's historical numbers are explicitly not reusable; this file is the
post-RAG truth at the pinned Git revision. Test files are counted separately
so production and test size pressure are not conflated.
"""

from __future__ import annotations

import re
import subprocess
import tarfile
import tempfile
from pathlib import Path

from _common import PRUNED_DIRS, REPO_ROOT, base_envelope, git_head_sha, unit_for_path

PY_THRESHOLD = 800
TS_THRESHOLD = 500

# Scan roots for source facts. tmp/, logs/, .venv etc. are pruned in common.
SCAN_ROOTS = ("src", "apps", "packages", "scripts", "database", "web", "sdk", "tests")


class LocGenerationError(RuntimeError):
    """The LOC generator could not read its declared Git source object."""


def _resolve_source_revision(root: Path, raw_revision: object) -> str:
    if not isinstance(raw_revision, str) or not re.fullmatch(
        r"[0-9a-f]{40}", raw_revision
    ):
        raise LocGenerationError(
            "LOC source revision must be a full lowercase 40-character Git SHA"
        )
    result = subprocess.run(
        ["git", "rev-parse", "--verify", f"{raw_revision}^{{commit}}"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0 or result.stdout.strip() != raw_revision:
        raise LocGenerationError(
            f"cannot resolve LOC source revision as an exact commit: {raw_revision}"
        )
    return raw_revision


def _git_line_counts(
    root: Path,
    source_revision: str,
) -> tuple[dict[Path, int], dict[Path, int], dict[Path, int]]:
    """Read every relevant count from one Git archive, never the worktree."""
    top_level = subprocess.run(
        ["git", "ls-tree", "-d", "--name-only", source_revision],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if top_level.returncode != 0:
        raise LocGenerationError(
            f"cannot resolve LOC source revision {source_revision}: "
            f"{top_level.stderr.strip() or f'exit {top_level.returncode}'}"
        )
    available = set(top_level.stdout.splitlines())
    archive_roots = sorted(
        available & {path.split("/", 1)[0] for path in (*SCAN_ROOTS, "rust")}
    )
    py_counts: dict[Path, int] = {}
    ts_counts: dict[Path, int] = {}
    rs_counts: dict[Path, int] = {}
    with tempfile.TemporaryDirectory(prefix="loc-baseline-source-") as tmp:
        archive_path = Path(tmp) / "source.tar"
        with archive_path.open("wb") as archive_file:
            result = subprocess.run(
                [
                    "git",
                    "archive",
                    "--format=tar",
                    source_revision,
                    "--",
                    *archive_roots,
                ],
                cwd=root,
                stdout=archive_file,
                stderr=subprocess.PIPE,
                check=False,
            )
        if result.returncode != 0:
            detail = result.stderr.decode("utf-8", errors="replace").strip()
            raise LocGenerationError(
                f"cannot archive LOC source {source_revision}: "
                f"{detail or f'exit {result.returncode}'}"
            )
        with tarfile.open(archive_path, "r:") as archive:
            for member in archive:
                if not member.isfile():
                    continue
                rel = Path(member.name)
                if any(part in PRUNED_DIRS for part in rel.parts):
                    continue
                target_counts: dict[Path, int] | None = None
                if rel.suffix == ".py" and rel.parts[0] in SCAN_ROOTS:
                    target_counts = py_counts
                elif rel.suffix in {".ts", ".tsx"} and rel.parts[0] in SCAN_ROOTS:
                    target_counts = ts_counts
                elif rel.suffix == ".rs" and rel.parts[0] == "rust":
                    target_counts = rs_counts
                if target_counts is None:
                    continue
                file_obj = archive.extractfile(member)
                if file_obj is None:
                    raise LocGenerationError(
                        f"cannot read LOC Git archive member: {member.name}"
                    )
                with file_obj:
                    try:
                        lines = len(file_obj.read().decode("utf-8").splitlines())
                    except UnicodeDecodeError as exc:
                        raise LocGenerationError(
                            f"LOC Git blob is not UTF-8: {source_revision}:{rel}"
                        ) from exc
                target_counts[rel] = lines
    return py_counts, ts_counts, rs_counts


def _is_test_path(rel: str) -> bool:
    base = rel.rsplit("/", 1)[-1]
    return (
        rel.startswith("tests/")
        or "/tests/" in rel
        or "/e2e/" in rel
        or base.startswith("test_")
        or base.endswith((".test.ts", ".test.tsx", ".spec.ts", ".spec.tsx"))
    )


def _oversized(
    counts: dict[Path, int],
    threshold: int,
) -> list[dict]:
    rows = []
    for rel, lines in counts.items():
        if lines > threshold:
            rows.append(
                {
                    "file": str(rel),
                    "lines": lines,
                    "unit": unit_for_path(rel),
                    "is_test": _is_test_path(str(rel)),
                }
            )
    rows.sort(key=lambda item: (-item["lines"], item["file"]))
    return rows


def build(
    root: Path = REPO_ROOT,
    *,
    source_revision: str | None = None,
) -> dict:
    revision = _resolve_source_revision(root, source_revision or git_head_sha())
    py_counts, ts_counts, rs_counts = _git_line_counts(root, revision)
    py_files = sorted(py_counts)
    ts_files = sorted(ts_counts)
    rs_files = sorted(rs_counts)

    py_oversized = _oversized(py_counts, PY_THRESHOLD)
    ts_oversized = _oversized(ts_counts, TS_THRESHOLD)
    rs_large = _oversized(rs_counts, 2000)  # informational only; pinned upstream, no LOC churn

    def totals(files: list[Path], counts: dict[Path, int]) -> dict:
        return {"files": len(files), "lines": sum(counts[rel] for rel in files)}

    production_py = [f for f in py_files if not _is_test_path(str(f))]
    production_ts = [f for f in ts_files if not _is_test_path(str(f))]

    def by_unit(rows: list[dict]) -> dict[str, int]:
        counts: dict[str, int] = {}
        for row in rows:
            counts[row["unit"]] = counts.get(row["unit"], 0) + 1
        return dict(sorted(counts.items()))

    envelope = base_envelope("loc-baseline")
    envelope["base_git_sha"] = revision
    return {
        **envelope,
        "thresholds": {
            "python_new_file_max": PY_THRESHOLD,
            "typescript_new_file_max": TS_THRESHOLD,
            "policy": "PRD AC-M22 / ARC-07E: oversized files are no-growth; new files stay under threshold; exceptions need owner + reason + expiry.",
        },
        "totals": {
            "python": totals(py_files, py_counts),
            "python_production": totals(production_py, py_counts),
            "typescript": totals(ts_files, ts_counts),
            "typescript_production": totals(production_ts, ts_counts),
            "rust_overlay": totals(rs_files, rs_counts),
        },
        "oversized_python": {
            "threshold_lines": PY_THRESHOLD,
            "count": len(py_oversized),
            "production_count": sum(1 for row in py_oversized if not row["is_test"]),
            "by_unit": by_unit(py_oversized),
            "files": py_oversized,
        },
        "oversized_typescript": {
            "threshold_lines": TS_THRESHOLD,
            "count": len(ts_oversized),
            "production_count": sum(1 for row in ts_oversized if not row["is_test"]),
            "by_unit": by_unit(ts_oversized),
            "files": ts_oversized,
        },
        "rust_large_informational": {
            "threshold_lines": 2000,
            "note": (
                "Pinned upstream Rust is not churned for LOC targets (PRD ARC-07E); "
                "listed only so reviewers know the shape of the overlay."
            ),
            "count": len(rs_large),
            "files": rs_large,
        },
        "methodology": [
            "Line counts are read from Git blobs at base_git_sha, never working-tree files.",
            "Test files are classified by path (tests/, e2e/, *.test.ts, *.spec.ts, test_*.py).",
            "Re-running this generator at the same SHA must yield byte-identical output.",
        ],
    }


if __name__ == "__main__":
    from _common import write_json

    path = write_json("loc-baseline.json", build())
    print(f"wrote {path}")
