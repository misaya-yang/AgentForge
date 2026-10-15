"""Shared helpers for the ARC-00 fact-baseline generators.

Stdlib-only on purpose (same rule as ``scripts/harness/check_harness.py``):
the baseline must be regenerable on any machine that has Python 3.10+ and a
checkout of this repository, without installing the uv workspace.

Determinism contract: every generator sorts its output, never embeds
wall-clock time, and derives its identity fields from the Git revision and
file contents only. Re-running the generator at the same Git revision must
produce byte-identical JSON.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import re
import subprocess
from pathlib import Path

# scripts/inventory/_common.py -> parents[2] is the repository root.
REPO_ROOT = Path(__file__).resolve().parents[2]

OUTPUT_DIR = REPO_ROOT / "docs" / "architecture" / "baselines" / "2026-08-post-rag"

# Complete union of files read by the six generators.  Receipt/runbook/doc
# commits made after the baseline artifact commit do not change these facts and
# therefore must not invalidate provenance.
BASELINE_INPUT_PATHS = tuple(
    REPO_ROOT / rel
    for rel in (
        "src",
        "apps",
        "packages",
        "scripts",
        "database",
        "web",
        "sdk",
        "tests",
        "rust",
        "reports/performance",
        "pyproject.toml",
        "docker-compose.yml",
        "docker-compose.dev.yml",
        "docker-compose.build.yml",
        "docker-compose.kbms.yml",
        "docker-compose.capability.yml",
    )
)

BASELINE_ID = "2026-08-post-rag"
_PINNED_SOURCE_SHA: str | None = None

# Directories that never contain first-party source facts.
PRUNED_DIRS = {
    ".git",
    ".venv",
    "venv",
    "node_modules",
    "dist",
    "build",
    "__pycache__",
    ".mypy_cache",
    ".ruff_cache",
    ".pytest_cache",
    ".playwright",
    ".claude",
    ".worktrees",
    "tmp",
    "temp",
    "logs",
    "uploads",
    "test-results",
    "playwright-report",
    "htmlcov",
    "target",  # Rust build output
}

# Source units that own Python code. Maps top-level path -> unit id.
PYTHON_UNITS = {
    "src": "gateway",
    "apps/knowledge-service": "knowledge-service",
    "apps/local-node": "local-node",
    "packages/ai-gateway-core": "ai-gateway-core",
    "packages/ai-gateway-contracts": "ai-gateway-contracts",
    "sdk/python": "sdk-python",
    "scripts": "scripts",
    "tests": "tests",
    "database": "database-tools",
}

# Importable module name -> owning unit (for cross-package import detection).
MODULE_UNITS = {
    "src": "gateway",
    "knowledge_service": "knowledge-service",
    "local_node": "local-node",
    "ai_gateway_core": "ai-gateway-core",
    "ai_gateway_contracts": "ai-gateway-contracts",
}


def git_head_sha() -> str:
    """Current Git revision; the baseline's identity anchor."""
    if _PINNED_SOURCE_SHA is not None:
        return _PINNED_SOURCE_SHA
    out = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=REPO_ROOT,
        capture_output=True,
        text=True,
        check=True,
    )
    return out.stdout.strip()


class BaselineProvenanceError(RuntimeError):
    """Raised when a baseline cannot be bound to one clean Git revision."""


def clean_git_head(
    root: Path = REPO_ROOT,
    *,
    expected_sha: str | None = None,
) -> str:
    """Return HEAD only when ``root`` is a clean, stable Git checkout.

    Baseline generators inspect working-tree files, not Git blobs.  Recording
    ``git rev-parse HEAD`` while tracked or untracked files differ would bind
    facts to a revision that never contained them.  Fail closed instead.
    """

    head = subprocess.run(
        ["git", "rev-parse", "--verify", "HEAD"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()
    if expected_sha is not None and head != expected_sha:
        raise BaselineProvenanceError(
            f"baseline source revision changed during generation: {expected_sha} -> {head}"
        )
    status = subprocess.run(
        ["git", "status", "--porcelain=v1", "--untracked-files=all"],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout
    if status:
        raise BaselineProvenanceError(
            "baseline generation requires a clean working tree, including no untracked files"
        )
    return head


@contextlib.contextmanager
def baseline_source_revision(source_sha: str):
    """Pin envelopes to the clean pre-baseline source revision for one batch.

    A baseline file cannot contain the SHA of the commit that contains that
    file (the SHA would be self-referential).  The declared revision is the
    clean source commit immediately before the baseline artifact commit.
    Verification proves every generator input still matches that source
    revision; later receipt/runbook commits are outside the fact-input set.
    """

    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise BaselineProvenanceError(
            f"baseline source revision must be a full lowercase Git SHA: {source_sha!r}"
        )
    global _PINNED_SOURCE_SHA
    previous = _PINNED_SOURCE_SHA
    _PINNED_SOURCE_SHA = source_sha
    try:
        yield
    finally:
        _PINNED_SOURCE_SHA = previous


def require_source_tree(
    source_sha: str,
    root: Path = REPO_ROOT,
    *,
    included_paths: tuple[Path, ...] | None = None,
    excluded_paths: tuple[Path, ...] = (OUTPUT_DIR,),
) -> None:
    """Prove selected current fact inputs equal ``source_sha``."""

    if not re.fullmatch(r"[0-9a-f]{40}", source_sha):
        raise BaselineProvenanceError(
            f"baseline source revision must be a full lowercase Git SHA: {source_sha!r}"
        )
    ancestor = subprocess.run(
        ["git", "merge-base", "--is-ancestor", source_sha, "HEAD"],
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if ancestor.returncode != 0:
        raise BaselineProvenanceError(
            f"baseline source revision {source_sha} is not an ancestor of HEAD"
        )
    command = ["git", "diff", "--quiet", source_sha, "--"]
    selected = included_paths or (root,)
    for path in selected:
        try:
            rel = path.relative_to(root).as_posix()
        except ValueError as exc:
            raise BaselineProvenanceError(
                f"baseline input path is outside the repository: {path}"
            ) from exc
        command.append(rel or ".")
    if included_paths is None:
        for path in excluded_paths:
            try:
                rel = path.relative_to(root).as_posix()
            except ValueError as exc:
                raise BaselineProvenanceError(
                    f"baseline output path is outside the repository: {path}"
                ) from exc
            command.append(f":(exclude){rel}/**")
    diff = subprocess.run(
        command,
        cwd=root,
        capture_output=True,
        text=True,
        check=False,
    )
    if diff.returncode == 1:
        raise BaselineProvenanceError(
            "working tree facts differ from the declared baseline source revision"
        )
    if diff.returncode != 0:
        detail = (diff.stderr or diff.stdout).strip()
        raise BaselineProvenanceError(
            f"cannot compare baseline source revision: {detail or f'exit {diff.returncode}'}"
        )


def require_payload_revision(payload: dict, source_sha: str, *, name: str) -> None:
    """Require a generated payload to declare the clean source revision."""

    observed = payload.get("base_git_sha")
    if observed != source_sha:
        raise BaselineProvenanceError(
            f"{name} declares base_git_sha={observed!r}, expected source revision {source_sha}"
        )


def walk_files(suffixes: tuple[str, ...], roots: tuple[str, ...] | None = None):
    """Yield repo-relative paths with the given suffixes, pruned and sorted.

    Paths are relative to the repository root so baselines are portable and
    never embed the checkout location or the user name.
    """
    scan_roots = [REPO_ROOT / r for r in (roots or tuple(PYTHON_UNITS))]
    found: set[Path] = set()
    for root in scan_roots:
        if not root.is_dir():
            continue
        stack = [root]
        while stack:
            current = stack.pop()
            try:
                entries = sorted(current.iterdir())
            except (PermissionError, OSError):
                continue
            for entry in entries:
                if entry.is_symlink():
                    continue
                if entry.is_dir():
                    if entry.name not in PRUNED_DIRS:
                        stack.append(entry)
                elif entry.suffix in suffixes:
                    found.add(entry.relative_to(REPO_ROOT))
    return sorted(found)


def unit_for_path(path: Path) -> str:
    """Map a repo-relative path to its owning source unit."""
    rel = str(path)
    for prefix in sorted(PYTHON_UNITS, key=len, reverse=True):
        if rel == prefix or rel.startswith(prefix + "/"):
            return PYTHON_UNITS[prefix]
    return "other"


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def sha256_text(text: str) -> str:
    return sha256_bytes(text.encode("utf-8"))


def sha256_file(path: Path) -> str:
    return sha256_bytes(path.read_bytes())


def canonical_sha256(obj) -> str:
    """SHA-256 of the canonical JSON encoding of ``obj`` (stable across runs)."""
    return sha256_text(json.dumps(obj, sort_keys=True, ensure_ascii=False, separators=(",", ":")))


def write_json(name: str, payload: dict) -> Path:
    source_sha = clean_git_head()
    require_payload_revision(payload, source_sha, name=name)
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    path = OUTPUT_DIR / name
    text = json.dumps(payload, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
    path.write_text(text, encoding="utf-8")
    return path


def base_envelope(kind: str) -> dict:
    """Common identity block shared by every baseline file."""
    return {
        "schema": f"ai-gateway/baseline/{kind}/v1",
        "baseline_id": BASELINE_ID,
        "base_git_sha": git_head_sha(),
        "generator": "scripts/inventory/generate_baselines.py",
    }


_SQL_TABLE_REF = re.compile(
    r"\b(?:FROM|JOIN|INTO|UPDATE|TABLE|TRUNCATE)\s+(?:ONLY\s+)?"
    r"((?:[a-z_][a-z0-9_]*\.)?[a-z_][a-z0-9_]*)",
    re.IGNORECASE,
)

_SQL_KEYWORDS = {
    "select", "where", "group", "order", "limit", "offset", "returning", "set",
    "values", "distinct", "lateral", "generate_series", "unnest", "jsonb",
    "json", "information_schema", "pg_catalog",
}


def extract_sql_table_refs(sql_text: str) -> set[str]:
    """Best-effort table references from a SQL fragment (static scan)."""
    refs: set[str] = set()
    for match in _SQL_TABLE_REF.finditer(sql_text):
        name = match.group(1).lower().strip(".")
        leaf = name.rsplit(".", 1)[-1]
        if leaf in _SQL_KEYWORDS or name in _SQL_KEYWORDS:
            continue
        refs.add(name)
    return refs
