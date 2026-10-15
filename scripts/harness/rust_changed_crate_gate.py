#!/usr/bin/env python3
"""Run changed Rust crates in lock-pinned upstream + current overlay.

The sparse overlay is never run directly. CI may explicitly fetch the public
upstream into runner temp; local runs require a controlled source checkout.
Exit: 0 pass/N/A, 1 test failure, 2 gate error. Evidence is under tmp/.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import posixpath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path, PurePosixPath
from typing import Any

try:
    from . import rust_gate_identity as _identity
except ImportError:  # direct script execution
    import rust_gate_identity as _identity  # type: ignore[no-redef]

HEX_40 = _identity.HEX_40
CANONICAL_UPSTREAM_URL = _identity.CANONICAL_UPSTREAM_URL
LOCK_SCHEMA = _identity.LOCK_SCHEMA
GateError = _identity.GateError
LockAuthority = _identity.LockAuthority
_load_json_object = _identity.load_json_object
load_lock_authority = _identity.load_lock_authority
_sha256_file = _identity.sha256_file
_toolchain_version = _identity.toolchain_version
validate_public_upstream_url = _identity.validate_public_upstream_url

ROOT = Path(__file__).resolve().parents[2]
RUST_ROOT = "rust"
OVERLAY_ROOT_REL = Path("rust/agent-runtime-overlay")
OVERLAY_WORKSPACE_REL = OVERLAY_ROOT_REL / "kernel-rs"
OVERLAY_MANIFEST_REL = Path("rust/agent-runtime-overlay/manifest.json")
LOCK_REL = Path("deploy/agent-runtime-source/lock.json")
DEFAULT_EVIDENCE = ROOT / "tmp/gate-evidence/rust-changed-crate-gate.json"
PER_CRATE_TIMEOUT_S = 55 * 60
GATE_CONTROL_PATHS = {
    "scripts/harness/rust_changed_crate_gate.py",
    "scripts/harness/rust_gate_identity.py",
    "tests/harness/test_rust_changed_crate_gate.py",
    "tests/scripts/test_rust_build_tooling.py",
}
HOSTED_CI_OVERRIDE = "AI_PLATFORM_RUST_GATE_HOSTED_CI"
def require_hosted_ci(environment: dict[str, str] | None = None) -> None:
    """Refuse host Rust execution unless a hosted CI runner is explicit."""

    values = os.environ if environment is None else environment
    ci = values.get("CI", "").lower() == "true"
    github_actions = values.get("GITHUB_ACTIONS", "").lower() == "true"
    explicit_hosted = values.get(HOSTED_CI_OVERRIDE) == "1"
    if not ci or not (github_actions or explicit_hosted):
        raise GateError(
            "host Rust execution is prohibited; rust-changed-crate-gate requires "
            "CI=true plus GITHUB_ACTIONS=true or AI_PLATFORM_RUST_GATE_HOSTED_CI=1"
        )

def overlay_identity(root: Path) -> dict[str, Any]:
    """Hash only real files in the current overlay; symlinks fail closed."""
    if not root.is_dir():
        raise GateError(f"Agent Runtime overlay is missing: {root}")
    files: list[Path] = []
    stack = [root]
    while stack:
        current = stack.pop()
        try:
            entries = sorted(current.iterdir())
        except OSError as exc:
            raise GateError(f"cannot list overlay directory {current}: {exc}") from exc
        for entry in entries:
            try:
                mode = entry.lstat().st_mode
            except OSError as exc:
                raise GateError(f"cannot inspect overlay path {entry}: {exc}") from exc
            if stat.S_ISLNK(mode):
                raise GateError(f"overlay symlink is not allowed: {entry}")
            if stat.S_ISDIR(mode):
                stack.append(entry)
            elif stat.S_ISREG(mode):
                if entry.name != "manifest.json":
                    files.append(entry)
            else:
                raise GateError(f"unsupported overlay filesystem entry: {entry}")
    if not files:
        raise GateError("Agent Runtime overlay is empty")
    digest = hashlib.sha256()
    for path in sorted(files):
        relative = path.relative_to(root).as_posix().encode("utf-8")
        try:
            payload = path.read_bytes()
        except OSError as exc:
            raise GateError(f"cannot read overlay file {path}: {exc}") from exc
        digest.update(relative)
        digest.update(b"\0")
        digest.update(payload)
        digest.update(b"\0")
    return {"sha256": digest.hexdigest(), "file_count": len(files)}

def verify_current_overlay(repo_root: Path, authority: LockAuthority) -> dict[str, Any]:
    overlay_root = repo_root / OVERLAY_ROOT_REL
    workspace = repo_root / OVERLAY_WORKSPACE_REL
    manifest_path = repo_root / authority.overlay_manifest
    manifest = _load_json_object(manifest_path, label="Agent Runtime overlay manifest")
    identity = overlay_identity(overlay_root)
    cargo_lock_sha = _sha256_file(workspace / "Cargo.lock")
    expected = {
        "upstream_sha": authority.upstream_sha,
        "sha256": identity["sha256"],
        "file_count": identity["file_count"],
        "cargo_lock_sha256": cargo_lock_sha,
    }
    for field, value in expected.items():
        if manifest.get(field) != value:
            raise GateError(
                f"overlay manifest {field} does not match the current overlay: "
                f"declared {manifest.get(field)!r}, actual {value!r}"
            )
    if authority.overlay_sha256 != identity["sha256"]:
        raise GateError("source lock overlay_sha256 does not match the current overlay")
    if authority.overlay_file_count != identity["file_count"]:
        raise GateError("source lock overlay_file_count does not match the current overlay")
    if authority.overlay_cargo_lock_sha256 != cargo_lock_sha:
        raise GateError("source lock overlay Cargo.lock hash does not match current overlay")
    return {
        "path": OVERLAY_ROOT_REL.as_posix(),
        "sha256": identity["sha256"],
        "file_count": identity["file_count"],
        "cargo_lock_sha256": cargo_lock_sha,
        "manifest_sha256": _sha256_file(manifest_path),
        "upstream_sha": authority.upstream_sha,
    }

def _run_git(source: Path, *args: str, env: dict[str, str] | None = None) -> str:
    result = subprocess.run(
        ["git", "--no-replace-objects", "-C", str(source), *args],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise GateError(f"git {' '.join(args)} failed: {detail or f'exit {result.returncode}'}")
    return result.stdout.strip()

def validate_source_checkout(source: Path, authority: LockAuthority) -> dict[str, str]:
    try:
        mode = source.lstat().st_mode
    except OSError as exc:
        raise GateError(f"controlled Runtime source is missing: {source}: {exc}") from exc
    if stat.S_ISLNK(mode) or not stat.S_ISDIR(mode):
        raise GateError(f"controlled Runtime source is not a real directory: {source}")
    if _run_git(source, "rev-parse", "--is-inside-work-tree") != "true":
        raise GateError("controlled Runtime source is not a Git worktree")
    if _run_git(source, "status", "--porcelain=v1", "--untracked-files=all"):
        raise GateError("controlled Runtime source must be clean")
    resolved = _run_git(source, "rev-parse", f"{authority.upstream_sha}^{{commit}}")
    if resolved != authority.upstream_sha:
        raise GateError("controlled source did not resolve the exact locked upstream commit")
    tree = _run_git(source, "rev-parse", f"{authority.upstream_sha}^{{tree}}")
    if HEX_40.fullmatch(tree) is None:
        raise GateError("locked upstream Git tree identity is invalid")
    return {"upstream_sha": authority.upstream_sha, "upstream_tree_sha": tree}

def _public_fetch_environment() -> dict[str, str]:
    return {
        **os.environ,
        "GIT_ASKPASS": "/bin/false",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_TERMINAL_PROMPT": "0",
    }

def prepare_public_source(destination: Path, authority: LockAuthority) -> Path:
    """Fetch only the lock-pinned public commit into a new empty checkout."""
    validate_public_upstream_url(authority.upstream_url)
    if destination.exists():
        raise GateError(f"public fetch destination already exists: {destination}")
    try:
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.mkdir()
    except OSError as exc:
        raise GateError(f"cannot create public fetch destination {destination}: {exc}") from exc
    environment = _public_fetch_environment()
    _run_git(destination, "init", "--quiet", env=environment)
    _run_git(
        destination,
        "remote",
        "add",
        "upstream",
        authority.upstream_url,
        env=environment,
    )
    _run_git(
        destination,
        "-c",
        "credential.helper=",
        "-c",
        "protocol.file.allow=never",
        "-c",
        "protocol.ext.allow=never",
        "fetch",
        "--no-tags",
        "--depth=1",
        "--filter=blob:none",
        "upstream",
        authority.upstream_sha,
        env=environment,
    )
    validate_source_checkout(destination, authority)
    return destination

def changed_paths(base: str, *, repo_root: Path = ROOT) -> list[str]:
    def run(*args: str) -> str:
        result = subprocess.run(
            ["git", *args], cwd=repo_root, capture_output=True, text=True, check=False
        )
        if result.returncode != 0:
            raise GateError(f"git {' '.join(args)} failed: {result.stderr.strip()}")
        return result.stdout

    run("rev-parse", "--verify", "--quiet", f"{base}^{{commit}}")
    diff = run("diff", "--name-only", base, "--")
    untracked = run("ls-files", "--others", "--exclude-standard")
    return sorted({path.strip() for path in (diff + untracked).splitlines() if path.strip()})

def package_name(manifest: Path) -> str | None:
    try:
        text = manifest.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as exc:
        raise GateError(f"cannot read Cargo manifest {manifest}: {exc}") from exc
    in_package = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            in_package = stripped == "[package]"
            continue
        if in_package:
            match = re.match(r'^name\s*=\s*"([^"]+)"', stripped)
            if match:
                return match.group(1)
    return None

def crate_for_path(repo_root: Path, rel_path: str) -> tuple[str | None, Path | None]:
    if not rel_path.startswith(f"{OVERLAY_WORKSPACE_REL.as_posix()}/"):
        return None, None
    path = repo_root / rel_path
    current = path.parent if path.suffix or path.is_file() else path
    workspace = (repo_root / OVERLAY_WORKSPACE_REL).resolve()
    while True:
        manifest = current / "Cargo.toml"
        if manifest.is_file():
            name = package_name(manifest)
            return (name or "@workspace"), manifest
        if current.resolve() == workspace:
            return None, None
        parent = current.parent
        resolved_parent = parent.resolve()
        if (
            parent == current
            or (resolved_parent != workspace and workspace not in resolved_parent.parents)
        ):
            return None, None
        current = parent

def workspace_authority_changed(repo_root: Path, base: str | None, lock_path: Path) -> bool:
    """Narrow receipt refreshes only when the compiled inputs are provably unchanged."""
    current = load_lock_authority(lock_path)
    if base is None:
        return True
    try:
        baseline_text = _run_git(repo_root, "show", f"{base}:{LOCK_REL.as_posix()}")
        with tempfile.TemporaryDirectory(prefix="rust-gate-base-lock-") as tmp:
            baseline_path = Path(tmp) / "lock.json"
            baseline_path.write_text(baseline_text, encoding="utf-8")
            baseline = load_lock_authority(baseline_path)
    except (GateError, OSError):
        # Missing/older baseline identity cannot justify a partial test selection.
        return True
    compile_inputs = (
        "upstream_sha",
        "upstream_url",
        "cargo_version",
        "rustc_version",
        "overlay_manifest",
        "overlay_cargo_lock_sha256",
    )
    return any(getattr(current, key) != getattr(baseline, key) for key in compile_inputs)


def plan(
    repo_root: Path,
    changed: list[str],
    *,
    base: str | None = None,
    lock_path: Path | None = None,
) -> tuple[dict[str, list[str]], list[str]]:
    crates: dict[str, list[str]] = {}
    unmapped: list[str] = []
    identity_paths: list[str] = []
    workspace_controls = {
        (OVERLAY_WORKSPACE_REL / "Cargo.toml").as_posix(),
        (OVERLAY_WORKSPACE_REL / "Cargo.lock").as_posix(),
        (OVERLAY_WORKSPACE_REL / "rust-toolchain.toml").as_posix(),
    }
    for rel in changed:
        if rel in {LOCK_REL.as_posix(), OVERLAY_MANIFEST_REL.as_posix()}:
            identity_paths.append(rel)
            continue
        if rel in workspace_controls:
            crates.setdefault("@workspace", []).append(rel)
            continue
        if not rel.startswith(f"{RUST_ROOT}/"):
            continue
        name, _manifest = crate_for_path(repo_root, rel)
        if name is None:
            unmapped.append(rel)
            continue
        crates.setdefault(name, []).append(rel)
    # A new upstream/toolchain/dependency lock affects unmodified upstream crates
    # and all platform consumers. Only proven receipt refreshes retain a narrow
    # selector alongside real crate changes; receipt-only changes still run all.
    if identity_paths and (
        not crates
        or (
            "@workspace" not in crates
            and LOCK_REL.as_posix() in identity_paths
            and workspace_authority_changed(repo_root, base, lock_path or repo_root / LOCK_REL)
        )
    ):
        crates["@workspace"] = identity_paths
    if "@workspace" in crates:
        all_paths = sorted(path for paths in crates.values() for path in paths)
        crates = {"@workspace": all_paths}
    return crates, unmapped

def cargo_test_command(cargo: str, crate: str, workspace_manifest: Path) -> list[str]:
    command = [cargo, "test", "--locked"]
    if crate == "@workspace":
        command.append("--workspace")
    else:
        command.extend(("-p", crate))
    command.extend(("--manifest-path", str(workspace_manifest)))
    return command

def cargo_environment(toolchain_version: str | None = None) -> dict[str, str]:
    environment = dict(os.environ)
    environment["CARGO_BUILD_JOBS"] = "1"
    environment["CARGO_INCREMENTAL"] = "0"
    if toolchain_version is not None:
        environment["RUSTUP_TOOLCHAIN"] = toolchain_version
    return environment

def _safe_archive_path(raw: str) -> PurePosixPath:
    path = PurePosixPath(raw)
    if path.is_absolute() or ".." in path.parts or not path.parts or path.parts[0] != "codex-rs":
        raise GateError(f"upstream archive path escaped codex-rs: {raw!r}")
    return path

def _safe_symlink_target(member_path: PurePosixPath, raw_target: str) -> None:
    if PurePosixPath(raw_target).is_absolute():
        raise GateError(f"absolute upstream archive symlink: {member_path}")
    normalized = PurePosixPath(
        posixpath.normpath(f"{member_path.parent.as_posix()}/{raw_target}")
    )
    if not normalized.parts or normalized.parts[0] != "codex-rs":
        raise GateError(f"upstream archive symlink escapes codex-rs: {member_path}")

def _extract_upstream_archive(archive_path: Path, destination: Path) -> None:
    try:
        with tarfile.open(archive_path, "r:") as archive:
            names: set[str] = set()
            links: list[tarfile.TarInfo] = []
            regular: list[tarfile.TarInfo] = []
            for member in archive.getmembers():
                member_path = _safe_archive_path(member.name)
                if member_path.as_posix() in names:
                    raise GateError(f"duplicate upstream archive path: {member.name}")
                names.add(member_path.as_posix())
                if member.issym():
                    _safe_symlink_target(member_path, member.linkname)
                    links.append(member)
                elif member.isdir() or member.isreg():
                    regular.append(member)
                else:
                    raise GateError(f"unsupported upstream archive entry: {member.name}")
            # Symlinks are created last so no subsequent extraction can traverse them.
            for member in regular:
                archive.extract(member, destination)
            for member in links:
                target = destination / member.name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.symlink_to(member.linkname)
    except (OSError, tarfile.TarError) as exc:
        raise GateError(f"cannot extract pinned upstream archive: {exc}") from exc

def compose_workspace(
    *,
    repo_root: Path,
    source_root: Path,
    authority: LockAuthority,
    destination: Path,
) -> tuple[Path, dict[str, str]]:
    _identity.verify_overlay_upstream_base(
        repo_root=repo_root, source=source_root, upstream_sha=authority.upstream_sha
    )
    archive_path = destination / "upstream.tar"
    composed = destination / "source"
    composed.mkdir()
    archive_command = [
        "git", "--no-replace-objects", "-C", str(source_root), "archive",
        "--format=tar", f"--output={archive_path}", authority.upstream_sha,
        "--", "codex-rs",
    ]
    result = subprocess.run(
        archive_command,
        capture_output=True,
        text=True,
        check=False,
    )
    if result.returncode != 0:
        raise GateError(f"cannot archive locked upstream source: {result.stderr.strip()}")
    _extract_upstream_archive(archive_path, composed)
    upstream_workspace = composed / "codex-rs"
    if not (upstream_workspace / "Cargo.toml").is_file():
        raise GateError("locked upstream archive has no codex-rs/Cargo.toml")

    overlay_workspace = repo_root / OVERLAY_WORKSPACE_REL
    try:
        shutil.copytree(overlay_workspace, upstream_workspace, dirs_exist_ok=True)
    except OSError as exc:
        raise GateError(f"cannot apply current Rust overlay: {exc}") from exc
    manifest = upstream_workspace / "Cargo.toml"
    cargo_lock = upstream_workspace / "Cargo.lock"
    if not manifest.is_file() or not cargo_lock.is_file():
        raise GateError("composed Rust workspace is missing Cargo.toml or Cargo.lock")
    cargo_lock_sha = _sha256_file(cargo_lock)
    if cargo_lock_sha != authority.overlay_cargo_lock_sha256:
        raise GateError("composed workspace Cargo.lock does not match lock authority")
    return manifest, {
        "workspace": "<composed-source>/codex-rs",
        "cargo_manifest_sha256": _sha256_file(manifest),
        "cargo_lock_sha256": cargo_lock_sha,
    }

def _resolve_executable(value: str | None, *, label: str) -> str:
    if not value:
        raise GateError(f"{label} executable is required")
    candidate = shutil.which(value) if "/" not in value else value
    if candidate is None or not Path(candidate).is_file() or not os.access(candidate, os.X_OK):
        raise GateError(f"{label} executable is unavailable: {value}")
    # Rustup dispatches cargo/rustc from argv[0]. Resolving its proxy symlink to
    # the rustup binary would change the invoked tool and invalidate the check.
    return os.path.abspath(candidate)

def _actual_tool_version(
    executable: str,
    *,
    tool: str,
    environment: dict[str, str],
) -> str:
    result = subprocess.run(
        [executable, "--version"],
        capture_output=True,
        text=True,
        env=environment,
        timeout=30,
        check=False,
    )
    if result.returncode != 0:
        raise GateError(f"{tool} --version failed with exit {result.returncode}")
    actual = result.stdout.strip()
    _toolchain_version(actual, tool=tool)
    return actual

def _display_command(command: list[str], manifest: Path) -> list[str]:
    displayed = [Path(command[0]).name, *command[1:]]
    return [
        "<composed-source>/codex-rs/Cargo.toml" if value == str(manifest) else value
        for value in displayed
    ]

def _write_evidence(path: Path, evidence: dict[str, Any]) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(
            json.dumps(evidence, ensure_ascii=False, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    except OSError as exc:
        raise GateError(f"cannot write Rust gate evidence {path}: {exc}") from exc

def execute_gate(
    *,
    repo_root: Path,
    base: str,
    changed: list[str],
    lock_path: Path,
    evidence_path: Path,
    source_root: Path | None,
    fetch_public_source: Path | None,
    cargo: str | None,
    rustc: str | None,
    control_selftest_passed: bool = False,
) -> int:
    evidence: dict[str, Any] = {
        "schema_version": "ai-platform/rust-changed-crate-gate/v2",
        "gate": "rust-changed-crate-gate",
        "tier": "L1",
        "base": base,
        "changed_paths": changed,
        "crates": {},
        "control_selftest": "pass" if control_selftest_passed else "not-run",
    }
    try:
        crates, unmapped = plan(repo_root, changed, base=base, lock_path=lock_path)
        control_paths = sorted(set(changed) & GATE_CONTROL_PATHS)
        if not crates and not unmapped:
            if control_paths:
                if not control_selftest_passed:
                    raise GateError("gate control paths changed without a passing selftest")
                evidence["control_paths"] = control_paths
                evidence["mode"] = "control-only-selftest"
                evidence["result"] = "pass"
                _write_evidence(evidence_path, evidence)
                print("OK: Rust gate control-only diff passed the embedded selftest")
                return 0
            evidence["result"] = "not-applicable"
            _write_evidence(evidence_path, evidence)
            print("NOT APPLICABLE: no Rust overlay/source-lock paths in the diff")
            return 0
        if unmapped:
            raise GateError(f"Rust changes map to no composed Cargo crate: {unmapped}")

        authority = load_lock_authority(lock_path)
        evidence["authority"] = {
            "lock": LOCK_REL.as_posix(),
            "lock_sha256": authority.lock_sha256,
            "upstream_url": authority.upstream_url,
            "upstream_sha": authority.upstream_sha,
        }
        evidence["overlay"] = verify_current_overlay(repo_root, authority)

        if source_root is not None and fetch_public_source is not None:
            raise GateError("choose either a controlled source or public fetch destination")
        if fetch_public_source is not None:
            source_root = prepare_public_source(fetch_public_source, authority)
            source_mode = "pinned-public-fetch"
        elif source_root is not None:
            source_mode = "controlled-checkout"
        else:
            raise GateError(
                "controlled Runtime source is required; set "
                "AI_PLATFORM_AGENT_RUNTIME_SOURCE locally or explicit CI public-fetch path"
            )
        source_identity = validate_source_checkout(source_root, authority)
        evidence["source"] = {"mode": source_mode, **source_identity}

        environment = cargo_environment(authority.toolchain_version)
        cargo_executable = _resolve_executable(cargo, label="cargo")
        rustc_executable = _resolve_executable(rustc, label="rustc")
        actual_cargo = _actual_tool_version(
            cargo_executable, tool="cargo", environment=environment
        )
        actual_rustc = _actual_tool_version(
            rustc_executable, tool="rustc", environment=environment
        )
        evidence["toolchain"] = {
            "authority": "deploy/agent-runtime-source/lock.json:build",
            "version": authority.toolchain_version,
            "cargo": actual_cargo,
            "rustc": actual_rustc,
            "cargo_build_jobs": "1",
            "cargo_incremental": "0",
        }
        if actual_cargo != authority.cargo_version:
            raise GateError(
                f"cargo toolchain drift: lock={authority.cargo_version!r}, "
                f"actual={actual_cargo!r}"
            )
        if actual_rustc != authority.rustc_version:
            raise GateError(
                f"rustc toolchain drift: lock={authority.rustc_version!r}, "
                f"actual={actual_rustc!r}"
            )

        results: dict[str, dict[str, Any]] = {}
        exit_code = 0
        with tempfile.TemporaryDirectory(prefix="rust-changed-crate-composed-") as tmp:
            manifest, composed = compose_workspace(
                repo_root=repo_root,
                source_root=source_root,
                authority=authority,
                destination=Path(tmp),
            )
            evidence["composed_source"] = composed
            for crate in sorted(crates):
                command = cargo_test_command(cargo_executable, crate, manifest)
                displayed = _display_command(command, manifest)
                print(f"running: {' '.join(displayed)}")
                try:
                    process = subprocess.run(
                        command,
                        cwd=manifest.parent,
                        env=environment,
                        timeout=PER_CRATE_TIMEOUT_S,
                        check=False,
                    )
                    status = "pass" if process.returncode == 0 else "fail"
                    return_code: int | None = process.returncode
                except subprocess.TimeoutExpired:
                    status = "timeout"
                    return_code = None
                results[crate] = {
                    "status": status,
                    "paths": crates[crate],
                    "command": displayed,
                    "return_code": return_code,
                    "cargo_build_jobs": "1",
                }
                if status != "pass":
                    exit_code = 1
        evidence["crates"] = results
        evidence["result"] = "pass" if exit_code == 0 else "fail"
        _write_evidence(evidence_path, evidence)
        if exit_code == 0:
            print(f"OK: {len(crates)} changed crate selection(s) tested in pinned source")
        return exit_code
    except GateError as exc:
        evidence["error"] = str(exc)
        evidence["result"] = "error"
        try:
            _write_evidence(evidence_path, evidence)
        except GateError as evidence_error:
            print(f"GATE ERROR: {evidence_error}", file=sys.stderr)
        print(f"GATE ERROR: {exc}", file=sys.stderr)
        return 2

def _selftest() -> int:
    contract = f"{OVERLAY_WORKSPACE_REL.as_posix()}/ai-platform-capability-contract/src/lib.rs"
    crate, _ = crate_for_path(ROOT, contract)
    selected, unmapped = plan(ROOT, [contract, "src/main.py"])
    workspace, _ = plan(ROOT, [LOCK_REL.as_posix()])
    command = cargo_test_command("cargo", "crate", Path("Cargo.toml"))
    try:
        validate_public_upstream_url("file:///tmp/untrusted")
    except GateError:
        unsafe_rejected = True
    else:
        unsafe_rejected = False
    checks = (
        crate == "ai-platform-capability-contract",
        sorted(selected) == ["ai-platform-capability-contract"],
        unmapped == [],
        sorted(workspace) == ["@workspace"],
        command[2] == "--locked",
        cargo_environment()["CARGO_BUILD_JOBS"] == "1",
        unsafe_rejected,
    )
    if not all(checks):
        print("SELFTEST FAILED", file=sys.stderr)
        return 1
    print("SELFTEST OK: composed-source mapping and fail-closed controls verified")
    return 0

def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base")
    parser.add_argument("--lock", type=Path, default=ROOT / LOCK_REL)
    parser.add_argument("--source", type=Path)
    parser.add_argument("--fetch-public-source", type=Path)
    parser.add_argument("--cargo")
    parser.add_argument("--rustc")
    parser.add_argument("--evidence-out", type=Path, default=DEFAULT_EVIDENCE)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)
    if args.selftest:
        return _selftest()
    if not args.base:
        print(
            "ERROR: --base <sha> is required (make rust-changed-crate-gate BASE_SHA=<sha>)",
            file=sys.stderr,
        )
        return 2
    try:
        require_hosted_ci()
    except GateError as exc:
        failure = {
            "schema_version": "ai-platform/rust-changed-crate-gate/v2",
            "gate": "rust-changed-crate-gate",
            "tier": "L1",
            "base": args.base,
            "executor_policy": "hosted-ci-only",
            "error": str(exc),
            "result": "error",
        }
        try:
            _write_evidence(args.evidence_out, failure)
        except GateError as evidence_error:
            print(f"GATE ERROR: {evidence_error}", file=sys.stderr)
        print(f"GATE ERROR: {exc}", file=sys.stderr)
        return 2
    if _selftest() != 0:
        failure = {
            "schema_version": "ai-platform/rust-changed-crate-gate/v2",
            "gate": "rust-changed-crate-gate",
            "tier": "L1",
            "base": args.base,
            "control_selftest": "fail",
            "result": "error",
        }
        try:
            _write_evidence(args.evidence_out, failure)
        except GateError as exc:
            print(f"GATE ERROR: {exc}", file=sys.stderr)
        return 2
    try:
        changed = changed_paths(args.base)
    except GateError as exc:
        print(f"GATE ERROR: {exc}", file=sys.stderr)
        return 2

    source = args.source
    if source is None and os.environ.get("AI_PLATFORM_AGENT_RUNTIME_SOURCE"):
        source = Path(os.environ["AI_PLATFORM_AGENT_RUNTIME_SOURCE"])
    fetch_source = args.fetch_public_source
    if fetch_source is None and os.environ.get("AI_PLATFORM_RUST_GATE_FETCH_PUBLIC_SOURCE"):
        fetch_source = Path(os.environ["AI_PLATFORM_RUST_GATE_FETCH_PUBLIC_SOURCE"])
    return execute_gate(
        repo_root=ROOT,
        base=args.base,
        changed=changed,
        lock_path=args.lock,
        evidence_path=args.evidence_out,
        source_root=source,
        fetch_public_source=fetch_source,
        cargo=args.cargo or shutil.which("cargo"),
        rustc=args.rustc or shutil.which("rustc"),
        control_selftest_passed=True,
    )


if __name__ == "__main__":
    raise SystemExit(main())
