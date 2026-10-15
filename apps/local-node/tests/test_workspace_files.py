from __future__ import annotations

import hashlib
import os

import pytest

from local_node.errors import CapabilityDenied, PathEscapeError
from local_node.files import LocalFileService
from local_node.grants import DirectoryGrantStore
from local_node.ledger import ActionLedger
from local_node.workspace import SecureWorkspace


def _services(tmp_path):
    root = tmp_path / "workspace"
    root.mkdir()
    grants = DirectoryGrantStore()
    grant = grants.issue(
        root,
        frozenset({"list", "read", "search", "watch", "write", "rollback"}),
        tenant_id="tenant-a",
        user_id="user-a",
    )
    ledger = ActionLedger(tmp_path / "state" / "ledger.sqlite")
    files = LocalFileService(grants, tmp_path / "state" / "rollbacks", ledger)
    return root, grants, grant, ledger, files


def test_read_list_and_search_return_exact_bytes_and_hash(tmp_path):
    root, _, grant, _, files = _services(tmp_path)
    (root / "notes.txt").write_text("alpha\nneedle here\n", encoding="utf-8")
    result = files.read_file(grant.grant_id, "notes.txt")
    assert result.content == b"alpha\nneedle here\n"
    assert result.sha256 == hashlib.sha256(result.content).hexdigest()
    assert result.encoding == "utf-8"
    assert files.list_files(grant.grant_id)[0].relative_path == "notes.txt"
    match = files.search(grant.grant_id, "needle")[0]
    assert (match.line, match.column) == (2, 1)
    assert match.file_sha256 == result.sha256


def test_explicit_multi_file_analysis_returns_exact_bytes_hashes_and_citations(tmp_path):
    root, grants, grant, _, files = _services(tmp_path)
    (root / "docs").mkdir()
    fixtures = {
        "docs/alpha.txt": b"heading\nneedle alpha\nend\n",
        "docs/beta.txt": b"needle beta\nsecond needle beta\n",
        "docs/binary.dat": b"\xff\x00needle\xfe",
    }
    for relative_path, content in fixtures.items():
        (root / relative_path).write_bytes(content)

    analyzed = files.analyze_files(
        grant.grant_id,
        tuple(fixtures),
        query="needle",
    )

    assert [item.read.relative_path for item in analyzed] == list(fixtures)
    for item in analyzed:
        expected = fixtures[item.read.relative_path]
        assert item.read.content == expected
        assert item.read.size == len(expected)
        assert item.read.sha256 == hashlib.sha256(expected).hexdigest()
        assert all(match.file_sha256 == item.read.sha256 for match in item.matches)
    assert [
        (match.relative_path, match.line, match.column, match.file_sha256)
        for item in analyzed
        for match in item.matches
    ] == [
        ("docs/alpha.txt", 2, 1, analyzed[0].read.sha256),
        ("docs/beta.txt", 1, 1, analyzed[1].read.sha256),
        ("docs/beta.txt", 2, 8, analyzed[1].read.sha256),
    ]
    assert analyzed[2].read.encoding is None
    assert analyzed[2].matches == ()

    grants.revoke(grant.grant_id)
    with pytest.raises(CapabilityDenied, match="unavailable"):
        files.analyze_files(grant.grant_id, ("docs/alpha.txt",), query="needle")
    with pytest.raises(CapabilityDenied, match="unavailable"):
        files.list_files(grant.grant_id, recursive=True)
    with pytest.raises(CapabilityDenied, match="unavailable"):
        files.read_file(grant.grant_id, "docs/alpha.txt")
    with pytest.raises(CapabilityDenied, match="unavailable"):
        files.search(grant.grant_id, "needle")


@pytest.mark.parametrize(
    "relative_paths",
    [
        ("docs/alpha.txt", "../outside.txt"),
        ("docs/alpha.txt", "/etc/passwd"),
        ("docs/alpha.txt", "docs/alpha.txt"),
    ],
)
def test_multi_file_analysis_rejects_escape_and_duplicate_before_returning_content(
    tmp_path, relative_paths
):
    root, _, grant, _, files = _services(tmp_path)
    (root / "docs").mkdir()
    (root / "docs" / "alpha.txt").write_text("safe fixture", encoding="utf-8")

    with pytest.raises(CapabilityDenied):
        files.analyze_files(grant.grant_id, relative_paths, query="safe")


@pytest.mark.parametrize(
    "unsafe",
    [
        "../outside",
        "/etc/passwd",
        "C:\\Users\\user\\secret",
        "\\\\server\\share",
        ".env",
        ".ssh/id_rsa",
        ".npmrc",
        ".netrc",
        ".git-credentials",
        "application_default_credentials.json",
        "wallet/seed-phrase.txt",
    ],
)
def test_containment_and_sensitive_defaults_fail_closed(tmp_path, unsafe):
    _, _, grant, _, _ = _services(tmp_path)
    with pytest.raises((PathEscapeError, FileNotFoundError)):
        SecureWorkspace(grant).open_read(unsafe)


def test_symlink_and_hardlink_are_not_file_capabilities(tmp_path):
    root, _, grant, _, _ = _services(tmp_path)
    outside = tmp_path / "outside.txt"
    outside.write_text("secret", encoding="utf-8")
    (root / "link.txt").symlink_to(outside)
    os.link(outside, root / "hard.txt")
    workspace = SecureWorkspace(grant)
    with pytest.raises(PathEscapeError):
        workspace.open_read("link.txt")
    with pytest.raises(PathEscapeError):
        workspace.open_read("hard.txt")


def test_grant_is_revocable_and_tenant_bound(tmp_path):
    _, grants, grant, _, _ = _services(tmp_path)
    with pytest.raises(CapabilityDenied):
        grants.get(grant.grant_id, "read", tenant_id="tenant-b")
    grants.revoke(grant.grant_id)
    with pytest.raises(CapabilityDenied):
        grants.get(grant.grant_id, "read")


def test_rollback_is_recovery_under_write_and_cannot_be_granted_standalone(tmp_path):
    root = tmp_path / "workspace"
    root.mkdir()
    grants = DirectoryGrantStore()

    with pytest.raises(CapabilityDenied, match="without file write"):
        grants.issue(root, frozenset({"rollback"}))

    grant = grants.issue(root, frozenset({"write"}))
    assert grant.capabilities == frozenset({"write", "rollback"})
