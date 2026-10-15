"""Immutable source/toolchain identity parsing for the hosted Rust gate."""

from __future__ import annotations

import hashlib
import json
import re
import stat
import subprocess
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any

LOCK_SCHEMA = "ai-platform/agent-runtime-source-lock/v2"
CANONICAL_UPSTREAM_URL = "https://github.com/openai/codex.git"
OVERLAY_MANIFEST_REL = Path("rust/agent-runtime-overlay/manifest.json")
OVERLAY_SCHEMA_V1 = "ai-platform/agent-runtime-overlay/v1"
OVERLAY_SCHEMA_V2 = "ai-platform/agent-runtime-overlay/v2"
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")
TOOL_VERSION = re.compile(
    r"^(?P<tool>cargo|rustc) (?P<version>[0-9]+\.[0-9]+\.[0-9]+)(?:\s|$)"
)


class GateError(RuntimeError):
    """The Rust gate cannot prove or execute its declared contract."""


@dataclass(frozen=True)
class LockAuthority:
    upstream_sha: str
    upstream_url: str
    cargo_version: str
    rustc_version: str
    toolchain_version: str
    overlay_manifest: str
    overlay_sha256: str
    overlay_file_count: int
    overlay_cargo_lock_sha256: str
    lock_sha256: str


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    try:
        with path.open("rb") as handle:
            for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(chunk)
    except OSError as exc:
        raise GateError(f"cannot hash {path}: {exc}") from exc
    return digest.hexdigest()


def load_json_object(path: Path, *, label: str) -> dict[str, Any]:
    def no_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
        result: dict[str, Any] = {}
        for key, value in pairs:
            if key in result:
                raise GateError(f"{label} has duplicate key {key!r}")
            result[key] = value
        return result

    try:
        value = json.loads(
            path.read_text(encoding="utf-8"), object_pairs_hook=no_duplicate_keys
        )
    except (OSError, json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise GateError(f"{label} is unreadable: {path}: {exc}") from exc
    if not isinstance(value, dict):
        raise GateError(f"{label} must be a JSON object: {path}")
    return value


def _required_hex(value: Any, *, length: int, label: str) -> str:
    pattern = HEX_40 if length == 40 else HEX_64
    if not isinstance(value, str) or pattern.fullmatch(value) is None:
        raise GateError(f"{label} must be a lowercase {length}-character hash")
    return value


def toolchain_version(value: Any, *, tool: str) -> str:
    if not isinstance(value, str):
        raise GateError(f"build.{tool} must be the exact {tool} --version output")
    match = TOOL_VERSION.match(value)
    if match is None or match.group("tool") != tool:
        raise GateError(f"build.{tool} is not a valid {tool} --version output")
    return match.group("version")


def validate_public_upstream_url(url: str) -> str:
    """Allow only the canonical credential-free HTTPS public upstream."""
    if url != CANONICAL_UPSTREAM_URL:
        raise GateError("source.upstream_url must be the canonical public HTTPS upstream")
    return url


def verify_overlay_upstream_base(*, repo_root: Path, source: Path, upstream_sha: str) -> None:
    """Verify every v2 overlay file was reviewed against the exact upstream blob."""
    overlay_root = repo_root / OVERLAY_MANIFEST_REL.parent
    manifest = load_json_object(overlay_root / "manifest.json", label="overlay manifest")
    schema = manifest.get("schema_version")
    if schema not in (OVERLAY_SCHEMA_V1, OVERLAY_SCHEMA_V2):
        raise GateError(f"unsupported overlay manifest schema: {schema!r}")
    _required_hex(upstream_sha, length=40, label="upstream_sha")
    if manifest.get("upstream_sha") != upstream_sha:
        raise GateError("overlay manifest upstream_sha does not match the locked upstream")
    if schema == OVERLAY_SCHEMA_V1:
        # Historical manifests predate per-file provenance; callers pin their
        # required schema in the release identity to prevent a v2 downgrade.
        return

    declared = manifest.get("upstream_blobs")
    if not isinstance(declared, dict):
        raise GateError("v2 overlay manifest requires an upstream_blobs object")
    for relative, blob in declared.items():
        path = PurePosixPath(relative)
        if path.is_absolute() or ".." in path.parts or path.as_posix() != relative or relative == ".":
            raise GateError(f"invalid overlay upstream_blobs path: {relative!r}")
        if blob is not None:
            _required_hex(blob, length=40, label=f"upstream_blobs[{relative!r}]")

    workspace = overlay_root / "kernel-rs"
    payload: set[str] = set()
    try:
        if overlay_root.is_symlink() or workspace.is_symlink() or not workspace.is_dir():
            raise GateError("overlay workspace must be a real directory")
        if {entry.name for entry in overlay_root.iterdir()} != {"kernel-rs", "manifest.json"}:
            raise GateError("v2 overlay payload must be entirely under kernel-rs")
        directories = [workspace]
        while directories:
            directory = directories.pop()
            for entry in directory.iterdir():
                mode = entry.lstat().st_mode
                if stat.S_ISDIR(mode):
                    directories.append(entry)
                elif stat.S_ISREG(mode):
                    payload.add(entry.relative_to(workspace).as_posix())
                else:
                    raise GateError(f"overlay payload must be a regular file or directory: {entry}")
    except OSError as exc:
        raise GateError(f"cannot inspect overlay payload: {exc}") from exc
    if not payload or set(declared) != payload:
        raise GateError(
            "overlay upstream_blobs must match the exact payload file set: "
            f"missing={sorted(payload - set(declared))}, extra={sorted(set(declared) - payload)}"
        )

    try:
        result = subprocess.run(
            [
                "git", "--no-replace-objects", "-C", str(source), "ls-tree",
                "-r", "-t", "-z", "--full-tree", upstream_sha, "--", "codex-rs",
            ],
            capture_output=True,
            text=True,
            encoding="utf-8",
            timeout=30,
            check=False,
        )
    except (OSError, UnicodeDecodeError, subprocess.TimeoutExpired) as exc:
        raise GateError(f"cannot read locked upstream Git tree: {exc}") from exc
    if result.returncode != 0:
        raise GateError(f"cannot read locked upstream Git tree: {result.stderr.strip()}")
    tree: dict[str, tuple[str, str, str]] = {}
    try:
        for record in result.stdout.split("\0"):
            if record:
                metadata, path = record.split("\t", 1)
                mode, kind, blob = metadata.split()
                tree[path] = (mode, kind, blob)
    except ValueError as exc:
        raise GateError("locked upstream Git tree returned invalid entries") from exc
    if tree.get("codex-rs", (None, None, None))[:2] != ("040000", "tree"):
        raise GateError("locked upstream Git tree has no codex-rs workspace")
    for relative in sorted(payload):
        path = PurePosixPath("codex-rs") / relative
        for parent in path.parents:
            entry = tree.get(parent.as_posix())
            if entry is not None and entry[:2] != ("040000", "tree"):
                raise GateError(f"overlay path crosses an upstream non-directory: {relative}")
        entry = tree.get(path.as_posix())
        if entry is not None and (entry[0] not in {"100644", "100755"} or entry[1] != "blob"):
            raise GateError(f"overlay target is not an upstream regular file: {relative}")
        actual = entry[2] if entry is not None else None
        if declared[relative] != actual:
            raise GateError(
                f"overlay upstream blob mismatch for {relative}: "
                f"declared={declared[relative]!r}, actual={actual!r}"
            )


def load_lock_authority(lock_path: Path) -> LockAuthority:
    lock = load_json_object(lock_path, label="Agent Runtime source lock")
    if lock.get("schema_version") != LOCK_SCHEMA:
        raise GateError(f"unsupported source lock schema: {lock.get('schema_version')!r}")
    source = lock.get("source")
    build = lock.get("build")
    if not isinstance(source, dict) or not isinstance(build, dict):
        raise GateError("source lock requires object-valued source and build sections")

    upstream_sha = _required_hex(
        source.get("upstream_sha"), length=40, label="source.upstream_sha"
    )
    upstream_url = source.get("upstream_url")
    if not isinstance(upstream_url, str):
        raise GateError("source.upstream_url is required")
    validate_public_upstream_url(upstream_url)

    cargo_version = build.get("cargo")
    rustc_version = build.get("rustc")
    cargo_toolchain = toolchain_version(cargo_version, tool="cargo")
    rustc_toolchain = toolchain_version(rustc_version, tool="rustc")
    if cargo_toolchain != rustc_toolchain:
        raise GateError(
            f"lock cargo/rustc versions disagree: {cargo_toolchain} != {rustc_toolchain}"
        )
    assert isinstance(cargo_version, str)
    assert isinstance(rustc_version, str)

    overlay_manifest = build.get("overlay_manifest")
    if overlay_manifest != OVERLAY_MANIFEST_REL.as_posix():
        raise GateError(
            f"build.overlay_manifest must be {OVERLAY_MANIFEST_REL.as_posix()!r}"
        )
    overlay_file_count = build.get("overlay_file_count")
    if (
        isinstance(overlay_file_count, bool)
        or not isinstance(overlay_file_count, int)
        or overlay_file_count <= 0
    ):
        raise GateError("build.overlay_file_count must be a positive integer")

    return LockAuthority(
        upstream_sha=upstream_sha,
        upstream_url=upstream_url,
        cargo_version=cargo_version,
        rustc_version=rustc_version,
        toolchain_version=cargo_toolchain,
        overlay_manifest=overlay_manifest,
        overlay_sha256=_required_hex(
            build.get("overlay_sha256"), length=64, label="build.overlay_sha256"
        ),
        overlay_file_count=overlay_file_count,
        overlay_cargo_lock_sha256=_required_hex(
            build.get("overlay_cargo_lock_sha256"),
            length=64,
            label="build.overlay_cargo_lock_sha256",
        ),
        lock_sha256=sha256_file(lock_path),
    )
