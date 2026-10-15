from __future__ import annotations

import io
import json
import subprocess
import tarfile
from pathlib import Path

import pytest

from scripts.harness import rust_changed_crate_gate as gate
from scripts.harness import rust_gate_identity as identity

ROOT = Path(__file__).resolve().parents[2]
CARGO_VERSION = "cargo 1.95.0 (f2d3ce0bd 2026-03-21)"
RUSTC_VERSION = "rustc 1.95.0 (59807616e 2026-04-14)"


def _git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args],
        cwd=root,
        capture_output=True,
        text=True,
        check=True,
    ).stdout.strip()


def _commit_platform(repo: Path) -> str:
    _git(repo, "init", "-q")
    _git(repo, "config", "user.email", "rust-gate@example.invalid")
    _git(repo, "config", "user.name", "Rust Gate Test")
    _git(repo, "add", ".")
    _git(repo, "commit", "-qm", "platform baseline")
    return _git(repo, "rev-parse", "HEAD")


def _init_source(
    tmp_path: Path,
    *,
    marker: str = "pinned object",
) -> tuple[Path, str]:
    source = tmp_path / "controlled-source"
    crate = source / "codex-rs" / "upstream-base"
    crate.mkdir(parents=True)
    (source / "codex-rs" / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["upstream-base"]\nresolver = "2"\n',
        encoding="utf-8",
    )
    (source / "codex-rs" / "Cargo.lock").write_text(
        "# upstream lock\n", encoding="utf-8"
    )
    (source / "codex-rs" / "upstream-only.txt").write_text(
        f"{marker}\n", encoding="utf-8"
    )
    (crate / "Cargo.toml").write_text(
        '[package]\nname = "upstream-base"\nversion = "0.1.0"\n',
        encoding="utf-8",
    )
    (crate / "src").mkdir()
    (crate / "src/lib.rs").write_text("pub const BASE: bool = true;\n", encoding="utf-8")
    _git(source, "init", "-q")
    _git(source, "config", "user.email", "rust-gate@example.invalid")
    _git(source, "config", "user.name", "Rust Gate Test")
    _git(source, "add", ".")
    _git(source, "commit", "-qm", "pinned upstream")
    return source, _git(source, "rev-parse", "HEAD")


def _init_platform(tmp_path: Path, upstream_sha: str) -> tuple[Path, Path]:
    repo = tmp_path / "platform"
    workspace = repo / gate.OVERLAY_WORKSPACE_REL
    crate = workspace / "crate-a"
    (crate / "src").mkdir(parents=True)
    (workspace / "Cargo.toml").write_text(
        '[workspace]\nmembers = ["crate-a"]\nresolver = "2"\n',
        encoding="utf-8",
    )
    (workspace / "Cargo.lock").write_text("# overlay lock\n", encoding="utf-8")
    (crate / "Cargo.toml").write_text(
        '[package]\nname = "crate-a"\nversion = "0.1.0"\n', encoding="utf-8"
    )
    (crate / "src/lib.rs").write_text("pub const OVERLAY: bool = true;\n", encoding="utf-8")

    overlay = gate.overlay_identity(repo / gate.OVERLAY_ROOT_REL)
    cargo_lock_sha = gate._sha256_file(workspace / "Cargo.lock")
    manifest = {
        "schema_version": "ai-platform/agent-runtime-overlay/v1",
        "source_revision": "2" * 40,
        "upstream_sha": upstream_sha,
        "file_count": overlay["file_count"],
        "sha256": overlay["sha256"],
        "cargo_lock_sha256": cargo_lock_sha,
    }
    manifest_path = repo / gate.OVERLAY_MANIFEST_REL
    manifest_path.write_text(json.dumps(manifest) + "\n", encoding="utf-8")

    lock = {
        "schema_version": gate.LOCK_SCHEMA,
        "source": {
            "upstream_sha": upstream_sha,
            "upstream_url": gate.CANONICAL_UPSTREAM_URL,
        },
        "build": {
            "cargo": CARGO_VERSION,
            "rustc": RUSTC_VERSION,
            "overlay_manifest": gate.OVERLAY_MANIFEST_REL.as_posix(),
            "overlay_sha256": overlay["sha256"],
            "overlay_file_count": overlay["file_count"],
            "overlay_cargo_lock_sha256": cargo_lock_sha,
        },
    }
    lock_path = repo / gate.LOCK_REL
    lock_path.parent.mkdir(parents=True)
    lock_path.write_text(json.dumps(lock) + "\n", encoding="utf-8")
    return repo, lock_path


def _fake_tools(tmp_path: Path, *, rustc_version: str = RUSTC_VERSION) -> tuple[Path, Path]:
    tools = tmp_path / "tools"
    tools.mkdir()
    cargo = tools / "cargo"
    cargo.write_text(
        "#!/usr/bin/env python3\n"
        "import json, os, pathlib, sys\n"
        f"CARGO_VERSION = {CARGO_VERSION!r}\n"
        "if sys.argv[1:] == ['--version']:\n"
        "    print(CARGO_VERSION)\n"
        "    raise SystemExit(0)\n"
        "if not sys.argv[1:] or sys.argv[1] != 'test':\n"
        "    raise SystemExit(91)\n"
        "args = sys.argv[1:]\n"
        "manifest = pathlib.Path(args[args.index('--manifest-path') + 1])\n"
        "workspace = manifest.parent\n"
        "if not (workspace / 'upstream-only.txt').is_file():\n"
        "    raise SystemExit(92)\n"
        "if not (workspace / 'crate-a/src/lib.rs').is_file():\n"
        "    raise SystemExit(93)\n"
        "pathlib.Path(os.environ['FAKE_CARGO_LOG']).write_text(json.dumps({\n"
        "    'args': args,\n"
        "    'cwd': os.getcwd(),\n"
        "    'jobs': os.environ.get('CARGO_BUILD_JOBS'),\n"
        "    'toolchain': os.environ.get('RUSTUP_TOOLCHAIN'),\n"
        "}))\n",
        encoding="utf-8",
    )
    cargo.chmod(0o755)
    rustc = tools / "rustc"
    rustc.write_text(
        "#!/usr/bin/env python3\n"
        "import sys\n"
        f"print({rustc_version!r})\n"
        "raise SystemExit(0 if sys.argv[1:] == ['--version'] else 90)\n",
        encoding="utf-8",
    )
    rustc.chmod(0o755)
    return cargo, rustc


def _fixture(tmp_path: Path) -> tuple[Path, Path, Path, Path, Path]:
    source, upstream_sha = _init_source(tmp_path)
    repo, lock_path = _init_platform(tmp_path, upstream_sha)
    cargo, rustc = _fake_tools(tmp_path)
    return repo, source, lock_path, cargo, rustc


def _v2_manifest(repo: Path, source: Path) -> dict:
    manifest_path = repo / gate.OVERLAY_MANIFEST_REL
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    tree = {}
    for line in _git(source, "ls-tree", "-r", manifest["upstream_sha"], "--", "codex-rs").splitlines():
        metadata, path = line.split("\t", 1)
        tree[path.removeprefix("codex-rs/")] = metadata.split()[2]
    workspace = repo / gate.OVERLAY_WORKSPACE_REL
    manifest["schema_version"] = identity.OVERLAY_SCHEMA_V2
    manifest["upstream_blobs"] = {
        path.relative_to(workspace).as_posix(): tree.get(path.relative_to(workspace).as_posix())
        for path in workspace.rglob("*")
        if path.is_file()
    }
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
    return manifest


def _execute(
    repo: Path,
    source: Path | None,
    lock_path: Path,
    cargo: Path,
    rustc: Path,
    evidence: Path,
) -> int:
    return gate.execute_gate(
        repo_root=repo,
        base="fixture-base",
        changed=[f"{gate.OVERLAY_WORKSPACE_REL.as_posix()}/crate-a/src/lib.rs"],
        lock_path=lock_path,
        evidence_path=evidence,
        source_root=source,
        fetch_public_source=None,
        cargo=str(cargo),
        rustc=str(rustc),
    )


@pytest.mark.parametrize("schema", (identity.OVERLAY_SCHEMA_V1, identity.OVERLAY_SCHEMA_V2))
def test_gate_composes_pinned_source_and_current_overlay(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    schema: str,
) -> None:
    repo, source, lock_path, cargo, rustc = _fixture(tmp_path)
    if schema == identity.OVERLAY_SCHEMA_V2:
        _v2_manifest(repo, source)
    evidence_path = tmp_path / "evidence.json"
    cargo_log = tmp_path / "cargo.json"
    monkeypatch.setenv("FAKE_CARGO_LOG", str(cargo_log))

    assert _execute(repo, source, lock_path, cargo, rustc, evidence_path) == 0

    evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
    invocation = json.loads(cargo_log.read_text(encoding="utf-8"))
    upstream_sha = _git(source, "rev-parse", "HEAD")
    assert evidence["result"] == "pass"
    assert evidence["authority"]["upstream_sha"] == upstream_sha
    assert evidence["source"]["upstream_sha"] == upstream_sha
    assert evidence["source"]["upstream_tree_sha"] == _git(
        source, "rev-parse", f"{upstream_sha}^{{tree}}"
    )
    assert evidence["overlay"]["sha256"] == gate.overlay_identity(
        repo / gate.OVERLAY_ROOT_REL
    )["sha256"]
    assert evidence["toolchain"]["version"] == "1.95.0"
    assert evidence["crates"]["crate-a"]["command"] == [
        "cargo",
        "test",
        "--locked",
        "-p",
        "crate-a",
        "--manifest-path",
        "<composed-source>/codex-rs/Cargo.toml",
    ]
    assert invocation["jobs"] == "1"
    assert invocation["toolchain"] == "1.95.0"
    assert "--locked" in invocation["args"]


@pytest.mark.parametrize(
    ("change", "message"),
    (
        ("wrong-blob", "upstream blob mismatch for Cargo.toml"),
        ("null-existing", "upstream blob mismatch for Cargo.toml"),
        ("blob-added", "upstream blob mismatch for crate-a/src/lib.rs"),
        ("missing-path", "exact payload file set"),
        ("extra-path", "exact payload file set"),
        ("no-map", "requires an upstream_blobs object"),
        ("invalid-blob", "40-character hash"),
        ("unsafe-path", "invalid overlay upstream_blobs path"),
        ("wrong-pin", "upstream_sha does not match"),
        ("unknown-schema", "unsupported overlay manifest schema"),
        ("invalid-schema", "unsupported overlay manifest schema"),
    ),
)
def test_overlay_upstream_base_rejects_stale_or_incomplete_provenance_before_copy(
    tmp_path: Path, change: str, message: str
) -> None:
    repo, source, lock_path, _cargo, _rustc = _fixture(tmp_path)
    manifest = _v2_manifest(repo, source)
    blobs = manifest["upstream_blobs"]
    if change == "wrong-blob":
        blobs["Cargo.toml"] = "8" * 40
    elif change == "null-existing":
        blobs["Cargo.toml"] = None
    elif change == "blob-added":
        blobs["crate-a/src/lib.rs"] = "8" * 40
    elif change == "missing-path":
        del blobs["Cargo.toml"]
    elif change == "extra-path":
        blobs["absent.rs"] = None
    elif change == "no-map":
        del manifest["upstream_blobs"]
    elif change == "invalid-blob":
        blobs["Cargo.toml"] = True
    elif change == "unsafe-path":
        blobs["../escape"] = None
    elif change == "wrong-pin":
        manifest["upstream_sha"] = "8" * 40
    elif change == "unknown-schema":
        manifest["schema_version"] = "ai-platform/agent-runtime-overlay/v3"
    else:
        manifest["schema_version"] = []
    (repo / gate.OVERLAY_MANIFEST_REL).write_text(json.dumps(manifest), encoding="utf-8")
    destination = tmp_path / "composition"
    destination.mkdir()

    with pytest.raises(gate.GateError, match=message):
        gate.compose_workspace(
            repo_root=repo,
            source_root=source,
            authority=gate.load_lock_authority(lock_path),
            destination=destination,
        )

    assert not (destination / "source").exists()


def test_overlay_upstream_base_rejects_payload_symlink(tmp_path: Path) -> None:
    repo, source, lock_path, _cargo, _rustc = _fixture(tmp_path)
    _v2_manifest(repo, source)
    (repo / gate.OVERLAY_WORKSPACE_REL / "escape").symlink_to(source)

    with pytest.raises(gate.GateError, match="regular file or directory"):
        identity.verify_overlay_upstream_base(
            repo_root=repo,
            source=source,
            upstream_sha=gate.load_lock_authority(lock_path).upstream_sha,
        )


@pytest.mark.parametrize("collision", ("directory", "symlink", "parent-symlink"))
def test_overlay_upstream_base_rejects_nonregular_upstream_targets(
    tmp_path: Path, collision: str
) -> None:
    repo, source, _lock_path, _cargo, _rustc = _fixture(tmp_path)
    if collision == "directory":
        target = source / "codex-rs/crate-a/src/lib.rs"
        target.mkdir(parents=True)
        (target / "child.rs").write_text("upstream child", encoding="utf-8")
    elif collision == "symlink":
        target = source / "codex-rs/crate-a/src/lib.rs"
        target.parent.mkdir(parents=True)
        target.symlink_to("../../upstream-base/src/lib.rs")
    else:
        (source / "codex-rs/crate-a").symlink_to("upstream-base")
    _git(source, "add", ".")
    _git(source, "commit", "-qm", "upstream path collision")
    upstream_sha = _git(source, "rev-parse", "HEAD")
    manifest_path = repo / gate.OVERLAY_MANIFEST_REL
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    manifest["upstream_sha"] = upstream_sha
    manifest_path.write_text(json.dumps(manifest), encoding="utf-8")
    _v2_manifest(repo, source)

    with pytest.raises(gate.GateError, match="upstream (regular file|non-directory)"):
        identity.verify_overlay_upstream_base(repo_root=repo, source=source, upstream_sha=upstream_sha)


def test_missing_controlled_source_fails_closed(tmp_path: Path) -> None:
    repo, _source, lock_path, cargo, rustc = _fixture(tmp_path)
    evidence = tmp_path / "missing-source.json"

    assert _execute(repo, None, lock_path, cargo, rustc, evidence) == 2
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert payload["result"] == "error"
    assert "controlled Runtime source is required" in payload["error"]


def test_identity_receipts_do_not_expand_real_changed_crates(tmp_path: Path) -> None:
    repo, _source, lock_path, _cargo, _rustc = _fixture(tmp_path)
    base = _commit_platform(repo)
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    lock["build"]["overlay_sha256"] = "3" * 64
    lock["build"]["overlay_file_count"] += 1
    lock_path.write_text(json.dumps(lock), encoding="utf-8")
    crate_path = f"{gate.OVERLAY_WORKSPACE_REL.as_posix()}/crate-a/src/lib.rs"

    crates, unmapped = gate.plan(
        repo,
        [crate_path, gate.LOCK_REL.as_posix(), gate.OVERLAY_MANIFEST_REL.as_posix()],
        base=base,
    )

    assert unmapped == []
    assert crates == {"crate-a": [crate_path]}


@pytest.mark.parametrize(
    ("section", "field", "value"),
    (
        ("source", "upstream_sha", "4" * 40),
        ("build", "cargo", "cargo 1.95.0 (changed-build 2026-03-21)"),
        ("build", "rustc", "rustc 1.95.0 (changed-build 2026-04-14)"),
        ("build", "overlay_cargo_lock_sha256", "5" * 64),
    ),
)
def test_compiled_identity_change_selects_workspace_alongside_crate_changes(
    tmp_path: Path, section: str, field: str, value: str
) -> None:
    repo, _source, lock_path, _cargo, _rustc = _fixture(tmp_path)
    base = _commit_platform(repo)
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    lock[section][field] = value
    lock_path.write_text(json.dumps(lock), encoding="utf-8")
    changed = [
        f"{gate.OVERLAY_WORKSPACE_REL.as_posix()}/crate-a/src/lib.rs",
        gate.LOCK_REL.as_posix(),
    ]

    crates, unmapped = gate.plan(repo, changed, base=base)

    assert unmapped == []
    assert crates == {"@workspace": sorted(changed)}


@pytest.mark.parametrize("base", (None, "missing-baseline"))
def test_unproven_baseline_cannot_narrow_source_lock_change(
    tmp_path: Path, base: str | None
) -> None:
    repo, _source, _lock_path, _cargo, _rustc = _fixture(tmp_path)
    changed = [
        f"{gate.OVERLAY_WORKSPACE_REL.as_posix()}/crate-a/src/lib.rs",
        gate.LOCK_REL.as_posix(),
    ]

    crates, unmapped = gate.plan(repo, changed, base=base)

    assert unmapped == []
    assert crates == {"@workspace": sorted(changed)}


def test_invalid_current_lock_cannot_claim_receipt_only_change(tmp_path: Path) -> None:
    repo, _source, lock_path, _cargo, _rustc = _fixture(tmp_path)
    base = _commit_platform(repo)
    lock_path.write_text("{}", encoding="utf-8")

    with pytest.raises(gate.GateError, match="unsupported source lock schema"):
        gate.plan(
            repo,
            [f"{gate.OVERLAY_WORKSPACE_REL}/crate-a/src/lib.rs", gate.LOCK_REL.as_posix()],
            base=base,
        )


@pytest.mark.parametrize(
    "crate",
    (
        "ai-platform-agent-runtime",
        "ai-platform-capability-worker",
        "ai-platform-capability-contract",
        "ai-platform-office",
    ),
)
def test_all_platform_crates_are_selected_and_are_workspace_members(crate: str) -> None:
    relative = f"{gate.OVERLAY_WORKSPACE_REL.as_posix()}/{crate}/src/lib.rs"

    selected, unmapped = gate.plan(ROOT, [relative])

    assert selected == {crate: [relative]}
    assert unmapped == []
    workspace = (ROOT / gate.OVERLAY_WORKSPACE_REL / "Cargo.toml").read_text(encoding="utf-8")
    members = workspace.split("members = [", 1)[1].split("]", 1)[0]
    assert f'"{crate}"' in members


def test_sparse_upstream_dependency_change_selects_whole_workspace() -> None:
    relative = f"{gate.OVERLAY_WORKSPACE_REL.as_posix()}/core/src/session/mod.rs"

    selected, unmapped = gate.plan(ROOT, [relative])

    assert selected == {"@workspace": [relative]}
    assert unmapped == []


def test_unmapped_rust_change_fails_before_source_or_cargo(tmp_path: Path) -> None:
    evidence = tmp_path / "unmapped.json"

    result = gate.execute_gate(
        repo_root=tmp_path,
        base="fixture-base",
        changed=["rust/unknown/src/lib.rs"],
        lock_path=tmp_path / "missing-lock.json",
        evidence_path=evidence,
        source_root=None,
        fetch_public_source=None,
        cargo=None,
        rustc=None,
    )

    assert result == 2
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert payload["result"] == "error"
    assert "Rust changes map to no composed Cargo crate" in payload["error"]


def test_gate_control_change_is_pass_only_after_embedded_selftest(tmp_path: Path) -> None:
    evidence = tmp_path / "control-only.json"
    kwargs = {
        "repo_root": tmp_path,
        "base": "fixture-base",
        "changed": ["scripts/harness/rust_changed_crate_gate.py"],
        "lock_path": tmp_path / "missing-lock.json",
        "evidence_path": evidence,
        "source_root": None,
        "fetch_public_source": None,
        "cargo": None,
        "rustc": None,
    }

    assert gate.execute_gate(**kwargs) == 2
    assert gate.execute_gate(**kwargs, control_selftest_passed=True) == 0
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert payload["result"] == "pass"
    assert payload["mode"] == "control-only-selftest"


def test_main_rejects_host_execution_before_selftest_or_cargo(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    evidence = tmp_path / "host-rejected.json"
    for name in ("CI", "GITHUB_ACTIONS", gate.HOSTED_CI_OVERRIDE):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setattr(
        gate,
        "_selftest",
        lambda: pytest.fail("host policy must run before the executable gate selftest"),
    )

    result = gate.main(
        ["--base", "fixture-base", "--evidence-out", str(evidence)]
    )

    assert result == 2
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert payload["executor_policy"] == "hosted-ci-only"
    assert payload["result"] == "error"
    assert "host Rust execution is prohibited" in payload["error"]


@pytest.mark.parametrize(
    "environment",
    (
        {"CI": "true", "GITHUB_ACTIONS": "true"},
        {"CI": "true", gate.HOSTED_CI_OVERRIDE: "1"},
    ),
)
def test_hosted_ci_policy_accepts_github_or_explicit_hosted_runner(
    environment: dict[str, str],
) -> None:
    gate.require_hosted_ci(environment)


@pytest.mark.parametrize(
    "environment",
    (
        {"GITHUB_ACTIONS": "true"},
        {gate.HOSTED_CI_OVERRIDE: "1"},
        {"CI": "true"},
        {"CI": "1", "GITHUB_ACTIONS": "true"},
    ),
)
def test_hosted_ci_policy_rejects_incomplete_or_ambiguous_signals(
    environment: dict[str, str],
) -> None:
    with pytest.raises(gate.GateError, match="host Rust execution is prohibited"):
        gate.require_hosted_ci(environment)


def test_overlay_drift_fails_before_cargo(tmp_path: Path) -> None:
    repo, source, lock_path, cargo, rustc = _fixture(tmp_path)
    evidence = tmp_path / "overlay-drift.json"
    (repo / gate.OVERLAY_WORKSPACE_REL / "crate-a/src/lib.rs").write_text(
        "pub const DRIFT: bool = true;\n", encoding="utf-8"
    )

    assert _execute(repo, source, lock_path, cargo, rustc, evidence) == 2
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert "overlay manifest sha256 does not match" in payload["error"]


def test_exact_toolchain_output_is_lock_authoritative(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    source, upstream_sha = _init_source(tmp_path)
    repo, lock_path = _init_platform(tmp_path, upstream_sha)
    cargo, rustc = _fake_tools(tmp_path, rustc_version="rustc 1.94.0 (wrong 2026-01-01)")
    evidence = tmp_path / "toolchain-drift.json"
    monkeypatch.setenv("FAKE_CARGO_LOG", str(tmp_path / "unused-cargo.json"))

    assert _execute(repo, source, lock_path, cargo, rustc, evidence) == 2
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert "rustc toolchain drift" in payload["error"]


def test_source_must_contain_exact_locked_object(tmp_path: Path) -> None:
    repo, _source, lock_path, cargo, rustc = _fixture(tmp_path)
    unrelated, _unrelated_sha = _init_source(tmp_path / "other", marker="unrelated")
    evidence = tmp_path / "wrong-source.json"

    assert _execute(repo, unrelated, lock_path, cargo, rustc, evidence) == 2
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert "rev-parse" in payload["error"]


def test_dirty_controlled_source_fails_closed(tmp_path: Path) -> None:
    repo, source, lock_path, cargo, rustc = _fixture(tmp_path)
    evidence = tmp_path / "dirty-source.json"
    (source / "untracked.txt").write_text("dirty\n", encoding="utf-8")

    assert _execute(repo, source, lock_path, cargo, rustc, evidence) == 2
    payload = json.loads(evidence.read_text(encoding="utf-8"))
    assert "source must be clean" in payload["error"]


def test_unsafe_upstream_url_is_rejected_from_lock(tmp_path: Path) -> None:
    _repo, _source, lock_path, _cargo, _rustc = _fixture(tmp_path)
    lock = json.loads(lock_path.read_text(encoding="utf-8"))
    lock["source"]["upstream_url"] = "https://token@github.com/openai/codex.git"
    lock_path.write_text(json.dumps(lock) + "\n", encoding="utf-8")

    with pytest.raises(gate.GateError, match="canonical public HTTPS upstream"):
        gate.load_lock_authority(lock_path)


def test_archive_symlink_cannot_escape_composed_source(tmp_path: Path) -> None:
    archive_path = tmp_path / "unsafe.tar"
    with tarfile.open(archive_path, "w") as archive:
        directory = tarfile.TarInfo("codex-rs")
        directory.type = tarfile.DIRTYPE
        archive.addfile(directory)
        link = tarfile.TarInfo("codex-rs/escape")
        link.type = tarfile.SYMTYPE
        link.linkname = "../../outside"
        archive.addfile(link, io.BytesIO())

    with pytest.raises(gate.GateError, match="escapes"):
        gate._extract_upstream_archive(archive_path, tmp_path / "out")


def test_ci_toolchain_pin_is_derived_from_current_lock() -> None:
    authority = gate.load_lock_authority(ROOT / gate.LOCK_REL)
    workflow = (ROOT / ".github/workflows/ci.yml").read_text(encoding="utf-8")

    assert authority.toolchain_version == "1.95.0"
    assert f"uses: dtolnay/rust-toolchain@{authority.toolchain_version}" in workflow
    assert "AI_PLATFORM_RUST_GATE_FETCH_PUBLIC_SOURCE" in workflow
    assert "AI_PLATFORM_RUST_GATE_HOSTED_CI" in workflow
    assert "tests/harness/test_rust_changed_crate_gate.py" in workflow
    assert "dtolnay/rust-toolchain@stable" not in workflow


def test_rustup_proxy_name_is_not_resolved_away(tmp_path: Path) -> None:
    rustup = tmp_path / "rustup"
    rustup.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    rustup.chmod(0o755)
    cargo = tmp_path / "cargo"
    cargo.symlink_to(rustup)

    assert gate._resolve_executable(str(cargo), label="cargo") == str(cargo)
