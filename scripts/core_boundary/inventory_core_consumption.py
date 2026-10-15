#!/usr/bin/env python3
"""ARC-04 import/data-access inventory for ``ai-gateway-core`` consumers.

Generates the machine-readable inventory required by PRD §ARC-04 goal 1:

- which ``ai_gateway_core`` modules the Gateway (``src/``) and Knowledge
  (``apps/knowledge-service/``) services actually import;
- which of those modules are I/O-free protocol candidates consumed by at
  least two owners (cross-boundary stable protocols);
- per-module direct third-party imports (I/O dependency evidence);
- table-level SQL access of the core persistence modules (read vs write);
- Rust-side schema-version markers sharing a wire contract with Python.

The committed JSON doubles as the no-growth baseline for
``check_core_boundary``. Verification is the safe default; a reviewed refresh
must name the exact clean source commit::

    uv run python scripts/core_boundary/inventory_core_consumption.py
    uv run python scripts/core_boundary/inventory_core_consumption.py \
        --write --source-rev <full-clean-HEAD>

Output: ``reports/inventory/core-import-inventory.json`` (stable key order).
"""

from __future__ import annotations

import argparse
import ast
import contextlib
import json
import re
import subprocess
import sys
import tarfile
import tempfile
from collections.abc import Iterable
from pathlib import Path, PurePosixPath

SCHEMA_VERSION = "arc04-core-inventory/v3"
PROVENANCE_SCHEMA = "arc04-core-inventory-provenance/v1"
GENERATOR_PATH = "scripts/core_boundary/inventory_core_consumption.py"

GIT_SOURCE_PATHS: tuple[str, ...] = (
    "src",
    "apps/knowledge-service",
    "tests",
    "packages/ai-gateway-core",
    "packages/ai-gateway-contracts",
    "scripts",
    "sdk/python",
    "database",
    "rust",
)

CORE_PKG_DIR = Path("packages/ai-gateway-core/src/ai_gateway_core")
CONTRACTS_PKG_DIR = Path("packages/ai-gateway-contracts/src/ai_gateway_contracts")

# Docstring marker every ARC-04 compatibility shim carries.  The inventory
# and the boundary gate use it to tell true shims apart from core modules
# that merely import a contracts helper (e.g. auth/gateway_secret.py).
SHIM_MARKER = "Compatibility shim — implementation moved to ``ai_gateway_contracts``"

# Mixed core modules that still own concrete I/O/business behavior while
# re-exporting selected contracts objects.  They are not compatibility shims
# and must not carry SHIM_MARKER, but their exported symbols and consumers need
# the same machine-readable lifetime discipline.
MIXED_CORE_EXPORTS: dict[str, dict[str, object]] = {
    "ai_gateway_core.auth.gateway_secret": {
        "contracts_module": "ai_gateway_contracts.replay",
        "symbols": ("InMemoryReplayStore", "ReplayStore"),
        "replacement": "import from ai_gateway_contracts.replay",
        "deletion_condition": (
            "Remove these two re-exports after every direct and facade consumer imports "
            "ai_gateway_contracts.replay; gateway_secret continues to own GatewaySecret "
            "and RedisReplayStore."
        ),
    }
}

# Directories scanned for consumers, mapped to an owner label.  Order matters:
# first match wins (e.g. packages/*/tests before packages/**).  ``core``
# tracks intra-package imports so the boundary gate can see exactly which
# core modules still reach into ai_gateway_contracts (shims + helpers).
CONSUMER_SCOPES: tuple[tuple[str, str], ...] = (
    ("src", "gateway"),
    ("apps/knowledge-service", "knowledge"),
    ("tests", "tests"),
    ("packages/ai-gateway-core/tests", "tests"),
    ("packages/ai-gateway-contracts/tests", "tests"),
    ("scripts", "scripts"),
    ("sdk/python", "sdk"),
    ("packages/ai-gateway-core/src", "core"),
)

# Third-party imports that prove a module performs I/O or embeds service
# configuration (i.e. is NOT an I/O-free protocol candidate).
IO_MARKER_IMPORTS: frozenset[str] = frozenset(
    {
        "asyncpg",
        "redis",
        "httpx",
        "aiohttp",
        "requests",
        "urllib",
        "fastapi",
        "starlette",
        "oss2",
        "aioboto3",
        "boto3",
        "aiofiles",
        "PIL",
        "numpy",
        "cryptography",
        "opentelemetry",
        "pydantic_settings",
        "dotenv",
        "dashscope",
        "qdrant_client",
        "yaml",
        "tiktoken",
        "websockets",
        "sse_starlette",
    }
)

# Schema-version strings shared between Python and Rust; presence in rust/
# proves the Rust side is a co-owner of the wire contract.
RUST_MARKERS: tuple[str, ...] = (
    "ai-platform-capability-proof/v1",
    "agent-runtime-model-lease/v1",
    "agent-runtime/v1",
    "agent-runtime-envelope/v1",
    "usage.recorded.v1",
)

_SQL_WRITE = re.compile(
    r"\b(INSERT\s+INTO|UPDATE|DELETE\s+FROM|MERGE\s+INTO)\s+(?:ONLY\s+)?"
    r"([a-z_][a-z0-9_.]*)",
    re.IGNORECASE,
)
_SQL_READ = re.compile(
    r"\b(FROM|JOIN)\s+(?:ONLY\s+)?([a-z_][a-z0-9_.]*)",
    re.IGNORECASE,
)
_SQL_NOISE = frozenset(
    {"select", "where", "set", "values", "returning", "lateral", "unnest", "generate_series"}
)


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


class InventoryProvenanceError(RuntimeError):
    """The inventory is not bound to one immutable Git source object."""


def _git_text(root: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args],
        cwd=root,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise InventoryProvenanceError(
            f"git {' '.join(args)} failed: {detail or f'exit {result.returncode}'}"
        )
    return result.stdout.strip()


def resolve_source_commit(root: Path, raw_revision: object) -> str:
    """Resolve only a full, lowercase commit SHA without accepting aliases."""
    if not isinstance(raw_revision, str) or not re.fullmatch(
        r"[0-9a-f]{40}", raw_revision
    ):
        raise InventoryProvenanceError(
            "inventory source revision must be a full lowercase 40-character Git SHA"
        )
    resolved = _git_text(root, "rev-parse", "--verify", f"{raw_revision}^{{commit}}")
    if resolved != raw_revision:
        raise InventoryProvenanceError(
            f"inventory source revision did not resolve exactly: {raw_revision} -> {resolved}"
        )
    return resolved


def source_tree_sha(root: Path, source_commit: str) -> str:
    tree = _git_text(root, "rev-parse", "--verify", f"{source_commit}^{{tree}}")
    if not re.fullmatch(r"[0-9a-f]{40}", tree):
        raise InventoryProvenanceError(
            f"inventory source tree is not a full Git object id: {tree!r}"
        )
    return tree


def clean_source_revision(root: Path, expected_revision: str | None = None) -> str:
    """Require a stable clean HEAD before any formal verify/write workflow."""
    head = _git_text(root, "rev-parse", "--verify", "HEAD")
    if expected_revision is not None and head != expected_revision:
        raise InventoryProvenanceError(
            f"--source-rev {expected_revision!r} does not equal clean HEAD {head}"
        )
    status = _git_text(root, "status", "--porcelain=v1", "--untracked-files=all")
    if status:
        raise InventoryProvenanceError(
            "formal inventory verification/generation requires a clean working tree, "
            "including no untracked files"
        )
    return resolve_source_commit(root, head)


def _archive_paths_at_revision(root: Path, source_commit: str) -> list[str]:
    paths: list[str] = []
    for path in GIT_SOURCE_PATHS:
        exists = subprocess.run(
            ["git", "cat-file", "-e", f"{source_commit}:{path}"],
            cwd=root,
            capture_output=True,
            timeout=30,
            check=False,
        )
        if exists.returncode == 0:
            paths.append(path)
    return paths


@contextlib.contextmanager
def materialized_git_source(root: Path, source_commit: str) -> Iterable[Path]:
    """Yield a temporary tree extracted from ``source_commit``, never the worktree."""
    commit = resolve_source_commit(root, source_commit)
    with tempfile.TemporaryDirectory(prefix="arc04-core-source-") as tmp:
        snapshot = Path(tmp) / "source"
        snapshot.mkdir()
        archive_path = Path(tmp) / "source.tar"
        paths = _archive_paths_at_revision(root, commit)
        with archive_path.open("wb") as archive_file:
            result = subprocess.run(
                ["git", "archive", "--format=tar", commit, "--", *paths],
                cwd=root,
                stdout=archive_file,
                stderr=subprocess.PIPE,
                timeout=120,
                check=False,
            )
        if result.returncode != 0:
            detail = result.stderr.decode("utf-8", errors="replace").strip()
            raise InventoryProvenanceError(
                f"cannot materialize inventory source {commit}: "
                f"{detail or f'exit {result.returncode}'}"
            )
        destination = snapshot.resolve()
        with tarfile.open(archive_path, "r:") as archive:
            members = archive.getmembers()
            for member in members:
                member_path = PurePosixPath(member.name)
                if (
                    member_path.is_absolute()
                    or ".." in member_path.parts
                    or member.issym()
                    or member.islnk()
                    or not (member.isfile() or member.isdir())
                ):
                    raise InventoryProvenanceError(
                        f"unsafe path in Git archive: {member.name!r}"
                    )
                target = (destination / member.name).resolve()
                if not target.is_relative_to(destination):
                    raise InventoryProvenanceError(
                        f"Git archive path escapes snapshot: {member.name!r}"
                    )
            archive.extractall(snapshot, members=members)
        yield snapshot


def iter_python_files(root: Path, rel_dir: str) -> Iterable[Path]:
    base = root / rel_dir
    if not base.is_dir():
        return
    for path in sorted(base.rglob("*.py")):
        if "__pycache__" in path.parts:
            continue
        yield path


def owner_for(rel_path: Path) -> str | None:
    rel = rel_path.as_posix()
    for prefix, owner in CONSUMER_SCOPES:
        if rel == prefix or rel.startswith(prefix + "/"):
            return owner
    return None


def parse_imports(path: Path) -> tuple[set[str], set[str]]:
    """Return ``(plain_imports, from_modules)`` of dotted module paths.

    - ``plain_imports``: targets of ``import a.b`` statements.
    - ``from_modules``: source modules of ``from a.b import ...`` plus, for
      ``from pkg import name``, the deeper ``pkg.name`` when the imported
      symbol is itself a submodule (resolved by the caller against the known
      module catalog).
    """
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (SyntaxError, UnicodeDecodeError):
        return set(), set()
    plain: set[str] = set()
    from_modules: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            plain.update(alias.name for alias in node.names)
        elif isinstance(node, ast.ImportFrom):
            if node.level or not node.module:
                continue
            if node.module == "ai_gateway_core":
                # `from ai_gateway_core import submodule_or_symbol` — resolve
                # per-symbol below against the known catalog.
                for alias in node.names:
                    if alias.name != "*":
                        from_modules.add(f"ai_gateway_core.{alias.name}")
            else:
                from_modules.add(node.module)
    return plain, from_modules


def _resolve_from_module(
    current_module: str,
    *,
    is_package: bool,
    node: ast.ImportFrom,
) -> str | None:
    """Resolve one absolute or relative ``from`` import to a dotted module."""

    if node.level == 0:
        return node.module
    package = current_module.split(".") if is_package else current_module.split(".")[:-1]
    climb = node.level - 1
    if climb > len(package):
        return None
    parts = package[: len(package) - climb]
    if node.module:
        parts.extend(node.module.split("."))
    return ".".join(parts) or None


def shim_facade_exports(
    modules: dict[str, Path],
    shim_modules: set[str],
) -> dict[str, dict[str, str]]:
    """Map package-facade symbols back to the compatibility shim that owns them."""

    facades: dict[str, dict[str, str]] = {}
    for dotted, path in modules.items():
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        except (SyntaxError, UnicodeDecodeError):
            continue
        for node in tree.body:
            if not isinstance(node, ast.ImportFrom):
                continue
            source = _resolve_from_module(
                dotted,
                is_package=path.name == "__init__.py",
                node=node,
            )
            if source not in shim_modules:
                continue
            exports = facades.setdefault(dotted, {})
            for alias in node.names:
                if alias.name != "*":
                    exports[alias.asname or alias.name] = source
    return facades


def shim_targets_for_imports(
    path: Path,
    *,
    known_submodules: set[str],
    shim_modules: set[str],
    facade_exports: dict[str, dict[str, str]],
) -> set[str]:
    """Return shims consumed directly or through a package facade.

    ``from ai_gateway_core.agents import RuntimeModelLeaseSigner`` consumes the
    ``runtime_lease`` shim even though its textual import stops at the
    ``ai_gateway_core.agents`` facade.  The no-growth ledger must retain that
    origin or it can report zero consumers while production still relies on
    the compatibility path.
    """

    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (SyntaxError, UnicodeDecodeError):
        return set()

    def reduce(dotted: str) -> str | None:
        parts = dotted.split(".")
        for depth in range(len(parts), 0, -1):
            candidate = ".".join(parts[:depth])
            if candidate in known_submodules:
                return candidate
        return None

    targets: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                target = reduce(alias.name)
                if target in shim_modules:
                    targets.add(target)
                if target in facade_exports:
                    targets.update(facade_exports[target].values())
            continue
        if not isinstance(node, ast.ImportFrom) or node.level or not node.module:
            continue
        source = reduce(node.module)
        if source in shim_modules:
            targets.add(source)
        exports = facade_exports.get(source or "", {})
        for alias in node.names:
            if alias.name == "*":
                targets.update(exports.values())
                continue
            origin = exports.get(alias.name)
            if origin:
                targets.add(origin)
                continue
            # ``from ai_gateway_core import agents`` and
            # ``from ai_gateway_core.agents import runtime`` name a deeper
            # module rather than a symbol re-export.
            candidate = reduce(f"{node.module}.{alias.name}")
            if candidate in shim_modules:
                targets.add(candidate)
            if candidate in facade_exports:
                targets.update(facade_exports[candidate].values())
    return targets


def mixed_export_facade_exports(
    modules: dict[str, Path],
) -> dict[str, dict[str, tuple[str, str]]]:
    """Map facade names to ``(mixed module, original symbol)`` origins."""

    facades: dict[str, dict[str, tuple[str, str]]] = {}
    for dotted, path in modules.items():
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
        except (SyntaxError, UnicodeDecodeError):
            continue
        for node in tree.body:
            if not isinstance(node, ast.ImportFrom):
                continue
            source = _resolve_from_module(
                dotted,
                is_package=path.name == "__init__.py",
                node=node,
            )
            spec = MIXED_CORE_EXPORTS.get(source or "")
            if spec is None:
                continue
            symbols = set(spec["symbols"])
            exports = facades.setdefault(dotted, {})
            for alias in node.names:
                if alias.name in symbols:
                    exports[alias.asname or alias.name] = (source, alias.name)
    return facades


def _attribute_paths(tree: ast.AST) -> set[tuple[str, ...]]:
    paths: set[tuple[str, ...]] = set()
    for node in ast.walk(tree):
        if not isinstance(node, ast.Attribute):
            continue
        parts: list[str] = []
        current: ast.expr = node
        while isinstance(current, ast.Attribute):
            parts.append(current.attr)
            current = current.value
        if isinstance(current, ast.Name):
            parts.append(current.id)
            paths.add(tuple(reversed(parts)))
    return paths


def mixed_export_targets_for_imports(
    path: Path,
    *,
    facade_exports: dict[str, dict[str, tuple[str, str]]],
) -> set[tuple[str, str]]:
    """Return mixed-export symbols actually referenced by one consumer file."""

    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (SyntaxError, UnicodeDecodeError):
        return set()
    attributes = _attribute_paths(tree)
    targets: set[tuple[str, str]] = set()

    def record_bound(prefix: tuple[str, ...], source: str) -> None:
        symbols = set(MIXED_CORE_EXPORTS[source]["symbols"])
        for attribute in attributes:
            if attribute[: len(prefix)] == prefix and len(attribute) == len(prefix) + 1:
                symbol = attribute[-1]
                if symbol in symbols:
                    targets.add((source, symbol))

    def record_facade_bound(prefix: tuple[str, ...], facade: str) -> None:
        exports = facade_exports[facade]
        for attribute in attributes:
            if attribute[: len(prefix)] == prefix and len(attribute) == len(prefix) + 1:
                origin = exports.get(attribute[-1])
                if origin:
                    targets.add(origin)

    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.name in MIXED_CORE_EXPORTS:
                    prefix = (alias.asname,) if alias.asname else tuple(alias.name.split("."))
                    record_bound(prefix, alias.name)
                if alias.name in facade_exports:
                    prefix = (alias.asname,) if alias.asname else tuple(alias.name.split("."))
                    record_facade_bound(prefix, alias.name)
            continue
        if not isinstance(node, ast.ImportFrom) or node.level or not node.module:
            continue
        direct = MIXED_CORE_EXPORTS.get(node.module)
        facade = facade_exports.get(node.module)
        for alias in node.names:
            if alias.name == "*":
                if direct is not None:
                    targets.update((node.module, symbol) for symbol in direct["symbols"])
                if facade is not None:
                    targets.update(facade.values())
                continue
            if direct is not None and alias.name in set(direct["symbols"]):
                targets.add((node.module, alias.name))
            if facade is not None and alias.name in facade:
                targets.add(facade[alias.name])
            candidate = f"{node.module}.{alias.name}"
            binding = (alias.asname or alias.name,)
            if candidate in MIXED_CORE_EXPORTS:
                record_bound(binding, candidate)
            if candidate in facade_exports:
                record_facade_bound(binding, candidate)
    return targets


def package_modules(root: Path, pkg_dir: Path) -> dict[str, Path]:
    """Dotted module path -> file for every module of a workspace package."""
    modules: dict[str, Path] = {}
    src_root = root / pkg_dir.parent
    for path in iter_python_files(root, pkg_dir.as_posix()):
        rel = path.relative_to(src_root)
        parts = list(rel.with_suffix("").parts)
        if parts[-1] == "__init__":
            parts = parts[:-1]
        if not parts:
            continue
        modules[".".join(parts)] = path
    return modules


def core_modules(root: Path) -> dict[str, Path]:
    """Dotted module path -> file for every ai_gateway_core module."""
    return package_modules(root, CORE_PKG_DIR)


def direct_imports(path: Path) -> set[str]:
    """Third-party/stdlib top-level imports declared directly in this file."""
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (SyntaxError, UnicodeDecodeError):
        return set()
    tops: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            tops.update(alias.name.split(".")[0] for alias in node.names)
        elif isinstance(node, ast.ImportFrom) and node.level == 0 and node.module:
            tops.add(node.module.split(".")[0])
    return tops


def sql_tables(path: Path) -> dict[str, list[str]]:
    """Extract table names referenced by SQL string constants in ``path``."""
    try:
        tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
    except (SyntaxError, UnicodeDecodeError):
        return {"write": [], "read": []}
    write: set[str] = set()
    read: set[str] = set()
    for node in ast.walk(tree):
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            text = node.value
            if len(text) < 12 or not re.search(r"\b(SELECT|INSERT|UPDATE|DELETE)\b", text, re.I):
                continue
            for match in _SQL_WRITE.finditer(text):
                table = match.group(2).lower()
                if table not in _SQL_NOISE:
                    write.add(table)
            for match in _SQL_READ.finditer(text):
                table = match.group(2).lower()
                if table not in _SQL_NOISE and "." not in table:
                    read.add(table)
    return {"write": sorted(write), "read": sorted(read)}


def build_inventory(root: Path) -> dict:
    modules = core_modules(root)
    known_submodules = set(modules)
    shim_modules = {
        dotted
        for dotted, path in modules.items()
        if SHIM_MARKER in path.read_text(encoding="utf-8")
    }
    facade_exports = shim_facade_exports(modules, shim_modules)
    mixed_facade_exports = mixed_export_facade_exports(modules)
    contracts = package_modules(root, CONTRACTS_PKG_DIR)
    known_contracts_submodules = set(contracts)

    consumption: dict[str, dict[str, dict[str, object]]] = {
        owner: {} for _, owner in CONSUMER_SCOPES
    }
    consumption.setdefault("other", {})
    contracts_consumption: dict[str, dict[str, dict[str, object]]] = {
        owner: {} for _, owner in CONSUMER_SCOPES
    }
    contracts_consumption.setdefault("other", {})
    shim_consumption: dict[str, dict[str, dict[str, object]]] = {
        owner: {} for _, owner in CONSUMER_SCOPES
    }
    shim_consumption.setdefault("other", {})
    mixed_consumption: dict[str, dict[str, dict[str, object]]] = {
        owner: {} for _, owner in CONSUMER_SCOPES
    }
    mixed_consumption.setdefault("other", {})
    core_import_paths: set[str] = set()
    contracts_import_paths: set[str] = set()

    def reduce(dotted: str, catalog: set[str]) -> str | None:
        """Reduce a dotted path to the longest known submodule in ``catalog``."""
        parts = dotted.split(".")
        for depth in range(len(parts), 0, -1):
            candidate = ".".join(parts[:depth])
            if candidate in catalog:
                return candidate
        return None

    def record(
        bucket_map: dict[str, dict[str, object]], owner: str, target: str, rel_posix: str
    ) -> None:
        bucket = bucket_map.setdefault(target, {"files": [], "count": 0})
        files = bucket["files"]
        assert isinstance(files, list)
        if rel_posix not in files:
            files.append(rel_posix)
            bucket["count"] = len(files)

    # A facade is itself a real shim consumer.  Seed it explicitly because
    # ``parse_imports`` intentionally ignores relative imports.
    for facade, exports in facade_exports.items():
        facade_path = modules[facade].relative_to(root).as_posix()
        owner = owner_for(Path(facade_path)) or "other"
        for shim in set(exports.values()):
            record(shim_consumption[owner], owner, shim, facade_path)

    # Mixed-module facades are real consumers of the exported contracts symbol
    # even though the owning module retains unrelated concrete behavior.
    for facade, exports in mixed_facade_exports.items():
        facade_path = modules[facade].relative_to(root).as_posix()
        owner = owner_for(Path(facade_path)) or "other"
        for mixed_module, symbol in set(exports.values()):
            record(
                mixed_consumption[owner],
                owner,
                f"{mixed_module}:{symbol}",
                facade_path,
            )

    scan_dirs = [prefix for prefix, _ in CONSUMER_SCOPES] + ["database"]
    for rel_dir in scan_dirs:
        for path in iter_python_files(root, rel_dir):
            rel = path.relative_to(root)
            owner = owner_for(rel) or "other"
            plain, from_modules = parse_imports(path)
            imported_shims = shim_targets_for_imports(
                path,
                known_submodules=known_submodules,
                shim_modules=shim_modules,
                facade_exports=facade_exports,
            )
            imported_mixed_exports = mixed_export_targets_for_imports(
                path,
                facade_exports=mixed_facade_exports,
            )
            targets: set[str] = set()
            contracts_targets: set[str] = set()
            for dotted in plain:
                if dotted.startswith("ai_gateway_core"):
                    target = reduce(dotted, known_submodules)
                    if target:
                        targets.add(target)
                        core_import_paths.add(dotted)
                elif dotted.startswith("ai_gateway_contracts"):
                    target = reduce(dotted, known_contracts_submodules)
                    if target:
                        contracts_targets.add(target)
                        contracts_import_paths.add(dotted)
            for dotted in from_modules:
                if dotted.startswith("ai_gateway_contracts"):
                    target = reduce(dotted, known_contracts_submodules)
                    if target is not None:
                        if target == "ai_gateway_contracts" and dotted != "ai_gateway_contracts":
                            continue
                        contracts_targets.add(target)
                        contracts_import_paths.add(dotted)
                    continue
                target = reduce(dotted, known_submodules)
                if target is None:
                    continue
                # `from ai_gateway_core import symbol` where symbol is not a
                # submodule collapses to the bare package — only keep it when
                # that is genuinely what was imported.
                if target == "ai_gateway_core" and dotted != "ai_gateway_core":
                    continue
                targets.add(target)
                core_import_paths.add(dotted)
            rel_posix = rel.as_posix()
            for target in targets:
                record(consumption[owner], owner, target, rel_posix)
            for target in contracts_targets:
                record(contracts_consumption[owner], owner, target, rel_posix)
            for target in imported_shims:
                record(shim_consumption[owner], owner, target, rel_posix)
            for mixed_module, symbol in imported_mixed_exports:
                record(
                    mixed_consumption[owner],
                    owner,
                    f"{mixed_module}:{symbol}",
                    rel_posix,
                )

    # Also catch bare `import ai_gateway_core` consumers.
    module_info: dict[str, dict] = {}
    for dotted in sorted(modules):
        path = modules[dotted]
        tops = direct_imports(path)
        io_deps = sorted(tops & IO_MARKER_IMPORTS)
        text = path.read_text(encoding="utf-8")
        is_shim = SHIM_MARKER in text
        owners = {
            owner: sorted(data[dotted]["files"])
            for owner, data in consumption.items()
            if dotted in data
        }
        module_info[dotted] = {
            "file": path.relative_to(root).as_posix(),
            "kind": "package" if path.name == "__init__.py" else "module",
            "loc": text.count("\n") + (0 if text.endswith("\n") else 1),
            "direct_imports": sorted(tops),
            "io_deps": io_deps,
            "io_free": not io_deps,
            "contracts_shim": is_shim,
            "consumer_owners": owners,
        }

    # Table access for persistence-adjacent modules.
    table_access: dict[str, dict[str, list[str]]] = {}
    for dotted, path in modules.items():
        if dotted.startswith(("ai_gateway_core.persistence", "ai_gateway_core.session")):
            tables = sql_tables(path)
            if tables["write"] or tables["read"]:
                table_access[dotted] = tables

    # Contracts package module info — the allowlist gate compares against this.
    contracts_info: dict[str, dict] = {}
    for dotted in sorted(contracts):
        path = contracts[dotted]
        tops = direct_imports(path)
        io_deps = sorted(tops & IO_MARKER_IMPORTS)
        text = path.read_text(encoding="utf-8")
        contracts_info[dotted] = {
            "file": path.relative_to(root).as_posix(),
            "kind": "package" if path.name == "__init__.py" else "module",
            "loc": text.count("\n") + (0 if text.endswith("\n") else 1),
            "direct_imports": sorted(tops),
            "io_deps": io_deps,
            "io_free": not io_deps,
        }

    knowledge_modules = sorted(consumption["knowledge"])
    rust_markers = {marker: sorted(_rust_marker_files(root, marker)) for marker in RUST_MARKERS}
    # PRD §ARC-04 goal 1: only I/O-free protocols consumed by at least two
    # owners qualify for ai-gateway-contracts.  Leaf modules are protocol
    # candidates; package __init__ entries (logging/proxy/tracing/…) are
    # shared infrastructure and stay in core per goal 3.
    cross_boundary: list[dict[str, object]] = []
    shared_infra: list[str] = []
    for dotted, info in sorted(module_info.items()):
        service_owners = sorted(
            owner for owner in info["consumer_owners"] if owner in ("gateway", "knowledge")
        )
        if len(service_owners) < 2 or not info["io_free"]:
            continue
        if info["kind"] == "package":
            shared_infra.append(dotted)
            continue
        marker = dotted_to_marker(dotted)
        rust_files = rust_markers.get(marker, []) if marker else []
        cross_boundary.append(
            {
                "module": dotted,
                "service_owners": service_owners,
                "rust_marker": marker if rust_files else None,
                "rust_files": rust_files,
                "in_contracts": bool(core_to_contracts(dotted)),
            }
        )

    # Shim consumption baseline: files importing through compat paths that
    # re-export from ai_gateway_contracts (only meaningful post-migration).
    sorted_shim_modules = sorted(shim_modules)
    shim_consumers = {
        m: {
            "files": sorted(
                f
                for owner in ("gateway", "knowledge", "tests", "scripts", "sdk", "core", "other")
                for f in shim_consumption[owner].get(m, {}).get("files", [])
            )
        }
        for m in sorted_shim_modules
    }
    for m in sorted_shim_modules:
        shim_consumers[m]["count"] = len(shim_consumers[m]["files"])

    mixed_export_consumers: dict[str, dict[str, object]] = {}
    for module, spec in sorted(MIXED_CORE_EXPORTS.items()):
        symbol_rows: dict[str, dict[str, object]] = {}
        all_files: set[str] = set()
        for symbol in sorted(spec["symbols"]):
            key = f"{module}:{symbol}"
            files = sorted(
                {
                    file
                    for owner in (
                        "gateway",
                        "knowledge",
                        "tests",
                        "scripts",
                        "sdk",
                        "core",
                        "other",
                    )
                    for file in mixed_consumption[owner].get(key, {}).get("files", [])
                }
            )
            all_files.update(files)
            symbol_rows[symbol] = {"files": files, "count": len(files)}
        facades = {
            facade: {
                exposed: origin_symbol
                for exposed, (origin_module, origin_symbol) in sorted(exports.items())
                if origin_module == module
            }
            for facade, exports in sorted(mixed_facade_exports.items())
            if any(origin_module == module for origin_module, _symbol in exports.values())
        }
        mixed_export_consumers[module] = {
            "contracts_module": spec["contracts_module"],
            "symbols": symbol_rows,
            "files": sorted(all_files),
            "count": len(all_files),
            "facades": facades,
            "replacement": spec["replacement"],
            "deletion_condition": spec["deletion_condition"],
        }

    return {
        "schema_version": SCHEMA_VERSION,
        "base_sha": None,
        "provenance": {
            "schema_version": PROVENANCE_SCHEMA,
            "source_kind": "working-tree",
        },
        "core_package_dir": CORE_PKG_DIR.as_posix(),
        "contracts_package_dir": CONTRACTS_PKG_DIR.as_posix(),
        "modules": module_info,
        "contracts_modules": contracts_info,
        "consumption": {
            owner: {m: consumption[owner][m] for m in sorted(consumption[owner])}
            for owner in consumption
        },
        "contracts_consumption": {
            owner: {
                m: contracts_consumption[owner][m] for m in sorted(contracts_consumption[owner])
            }
            for owner in contracts_consumption
        },
        "knowledge_core_modules": knowledge_modules,
        "knowledge_core_module_count": len(knowledge_modules),
        "gateway_core_module_count": len(consumption["gateway"]),
        "cross_boundary_protocol_candidates": cross_boundary,
        "shared_infra_packages": shared_infra,
        "rust_markers": rust_markers,
        "table_access": table_access,
        "shim_consumers": shim_consumers,
        "shim_facade_exports": {
            facade: dict(sorted(exports.items()))
            for facade, exports in sorted(facade_exports.items())
        },
        "mixed_export_consumers": mixed_export_consumers,
        "core_import_paths_seen": sorted(core_import_paths),
        "contracts_import_paths_seen": sorted(contracts_import_paths),
    }


def build_inventory_from_git(root: Path, source_revision: str) -> dict:
    """Build the formal inventory solely from one verified Git commit object."""
    source_commit = resolve_source_commit(root, source_revision)
    tree = source_tree_sha(root, source_commit)
    with materialized_git_source(root, source_commit) as snapshot:
        inventory = build_inventory(snapshot)
    inventory["base_sha"] = source_commit
    inventory["provenance"] = {
        "schema_version": PROVENANCE_SCHEMA,
        "source_kind": "git-commit",
        "source_commit": source_commit,
        "source_tree": tree,
        "generator": GENERATOR_PATH,
    }
    return inventory


def verify_inventory_provenance(root: Path, baseline: dict) -> dict[str, str]:
    """Rebuild v3 from its Git object and reject any self-certified payload."""
    if baseline.get("schema_version") != SCHEMA_VERSION:
        raise InventoryProvenanceError(
            f"core inventory schema must be exactly {SCHEMA_VERSION!r}, got "
            f"{baseline.get('schema_version')!r}"
        )
    provenance = baseline.get("provenance")
    if not isinstance(provenance, dict):
        raise InventoryProvenanceError("core inventory v3 provenance is missing")
    expected_metadata = {
        "schema_version": PROVENANCE_SCHEMA,
        "source_kind": "git-commit",
        "generator": GENERATOR_PATH,
    }
    for field, expected in expected_metadata.items():
        if provenance.get(field) != expected:
            raise InventoryProvenanceError(
                f"core inventory provenance {field} must be {expected!r}, got "
                f"{provenance.get(field)!r}"
            )
    source_commit = resolve_source_commit(root, provenance.get("source_commit"))
    ancestor = subprocess.run(
        ["git", "merge-base", "--is-ancestor", source_commit, "HEAD"],
        cwd=root,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    if ancestor.returncode != 0:
        raise InventoryProvenanceError(
            f"core inventory source commit is not an ancestor of HEAD: {source_commit}"
        )
    tree = source_tree_sha(root, source_commit)
    if provenance.get("source_tree") != tree:
        raise InventoryProvenanceError(
            f"core inventory provenance source_tree does not match {source_commit}: "
            f"recorded {provenance.get('source_tree')!r}, Git object {tree!r}"
        )
    if baseline.get("base_sha") != source_commit:
        raise InventoryProvenanceError(
            f"core inventory base_sha must equal provenance source_commit {source_commit}"
        )
    rebuilt = build_inventory_from_git(root, source_commit)
    if baseline != rebuilt:
        raise InventoryProvenanceError(
            "core inventory payload does not match its declared Git source object"
        )
    return {"source_commit": source_commit, "source_tree": tree}


def dotted_to_marker(dotted: str) -> str | None:
    """Map a core module to its shared schema-version marker, when known."""
    mapping = {
        "ai_gateway_core.auth.capability_proof": "ai-platform-capability-proof/v1",
        "ai_gateway_core.agents.runtime_lease": "agent-runtime-model-lease/v1",
        "ai_gateway_core.agents.runtime": "agent-runtime-envelope/v1",
        "ai_gateway_core.events.envelope": "usage.recorded.v1",
    }
    return mapping.get(dotted)


# Core shim module -> contracts module owning the implementation.  Keep in
# sync with check_core_boundary.CONTRACTS_ALLOWLIST: the gate uses this map
# as the authoritative shim list.
CORE_TO_CONTRACTS: dict[str, str] = {
    "ai_gateway_core.auth.capability_proof": "ai_gateway_contracts.capability_proof",
    "ai_gateway_core.agents.runtime": "ai_gateway_contracts.agent_runtime",
    "ai_gateway_core.agents.runtime_lease": "ai_gateway_contracts.agent_runtime_lease",
    "ai_gateway_core.events.envelope": "ai_gateway_contracts.event_envelope",
    "ai_gateway_core.events.errors": "ai_gateway_contracts.event_errors",
}


def core_to_contracts(dotted: str) -> str | None:
    """Return the contracts module a migrated core shim re-exports, if any."""
    return CORE_TO_CONTRACTS.get(dotted)


def _rust_marker_files(root: Path, marker: str) -> Iterable[str]:
    rust_dir = root / "rust"
    if not rust_dir.is_dir():
        return
    needle = marker.encode("utf-8")
    for path in sorted(rust_dir.rglob("*.rs")):
        try:
            if needle in path.read_bytes():
                yield path.relative_to(root).as_posix()
        except OSError:
            continue


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument(
        "--verify",
        action="store_true",
        help="verify the committed inventory against its Git object (default)",
    )
    mode.add_argument(
        "--write",
        action="store_true",
        help="write a reviewed refresh from --source-rev",
    )
    parser.add_argument(
        "--source-rev",
        help="full clean HEAD required with --write",
    )
    parser.add_argument(
        "--output",
        default="reports/inventory/core-import-inventory.json",
        help="inventory JSON destination (repo-relative)",
    )
    args = parser.parse_args(argv)
    root = repo_root()
    out_path = root / args.output
    try:
        clean_head = clean_source_revision(root)
        if args.write:
            if args.source_rev is None:
                parser.error("--write requires --source-rev with the full clean HEAD")
            source_revision = resolve_source_commit(root, args.source_rev)
            if source_revision != clean_head:
                raise InventoryProvenanceError(
                    f"--source-rev {source_revision} does not equal clean HEAD {clean_head}"
                )
            inventory = build_inventory_from_git(root, source_revision)
            out_path.parent.mkdir(parents=True, exist_ok=True)
            out_path.write_text(
                json.dumps(
                    inventory,
                    indent=2,
                    sort_keys=True,
                    ensure_ascii=False,
                )
                + "\n",
                encoding="utf-8",
            )
            print(
                f"wrote {out_path.relative_to(root)} from Git commit {source_revision}; "
                "review and commit this diff before running the independent gate"
            )
            return 0
        if args.source_rev is not None:
            parser.error("--source-rev is only valid with --write")
        if not out_path.is_file():
            raise InventoryProvenanceError(
                f"committed core inventory is missing: {out_path.relative_to(root)}"
            )
        baseline = json.loads(out_path.read_text(encoding="utf-8"))
        provenance = verify_inventory_provenance(root, baseline)
        print(
            f"verified {out_path.relative_to(root)} against Git commit "
            f"{provenance['source_commit']} (no files written)"
        )
        return 0
    except (
        InventoryProvenanceError,
        json.JSONDecodeError,
        OSError,
        subprocess.SubprocessError,
    ) as exc:
        print(f"PROVENANCE ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
