"""Real-filesystem containment attacks for OS-A03/A04/A07/A16/A19."""

from __future__ import annotations

import os
import unicodedata
from pathlib import Path

import pytest
from local_node.errors import CapabilityDenied, PathEscapeError
from local_node.grants import DirectoryGrantStore
from local_node.workspace import SecureWorkspace


@pytest.fixture
def granted_workspace(tmp_path: Path):
    root = tmp_path / "granted"
    root.mkdir()
    store = DirectoryGrantStore()
    grant = store.issue(
        root,
        frozenset({"list", "read", "search", "watch", "write", "rollback"}),
        tenant_id="tenant-a",
        user_id="user-a",
    )
    return store, grant, SecureWorkspace(grant), root


def test_lexical_escape_corpus_is_rejected(
    granted_workspace,
    path_attack_fixture: dict[str, list[str]],
) -> None:
    _store, _grant, workspace, _root = granted_workspace

    for attack in path_attack_fixture["lexical_escapes"]:
        with pytest.raises(PathEscapeError, match="path|absolute|NUL"):
            workspace.resolve(attack, allow_missing=True)


def test_decomposed_unicode_path_is_rejected(granted_workspace) -> None:
    _store, _grant, workspace, _root = granted_workspace
    decomposed = unicodedata.normalize("NFD", "café.txt")
    assert decomposed != unicodedata.normalize("NFC", decomposed)

    with pytest.raises(PathEscapeError):
        workspace.resolve(decomposed, allow_missing=True)


def test_symlink_file_and_directory_escape_are_rejected(
    granted_workspace,
    tmp_path: Path,
) -> None:
    _store, _grant, workspace, root = granted_workspace
    outside = tmp_path / "outside"
    outside.mkdir()
    (outside / "secret.txt").write_text("outside-secret", encoding="utf-8")
    (root / "file-link").symlink_to(outside / "secret.txt")
    (root / "dir-link").symlink_to(outside, target_is_directory=True)

    for relative in ("file-link", "dir-link/secret.txt"):
        with pytest.raises(PathEscapeError), workspace.open_read(relative):
            pass


def test_hardlink_to_outside_file_is_rejected(granted_workspace, tmp_path: Path) -> None:
    _store, _grant, workspace, root = granted_workspace
    outside = tmp_path / "outside-secret.txt"
    outside.write_text("outside-secret", encoding="utf-8")
    os.link(outside, root / "hardlink-secret.txt")

    with (
        pytest.raises(PathEscapeError, match="single-link"),
        workspace.open_read("hardlink-secret.txt"),
    ):
        pass


def test_toctou_swap_from_safe_file_to_symlink_fails_closed(
    granted_workspace,
    tmp_path: Path,
) -> None:
    _store, _grant, workspace, root = granted_workspace
    target = root / "proposal.txt"
    target.write_text("approved-safe-content", encoding="utf-8")
    assert workspace.resolve("proposal.txt") == target

    target.unlink()
    outside = tmp_path / "outside-after-approval.txt"
    outside.write_text("attacker-content", encoding="utf-8")
    target.symlink_to(outside)

    with pytest.raises(PathEscapeError), workspace.open_read("proposal.txt"):
        pass


def test_sensitive_paths_are_denied_even_inside_granted_root(
    granted_workspace,
    path_attack_fixture: dict[str, list[str]],
) -> None:
    _store, _grant, workspace, root = granted_workspace

    for relative in path_attack_fixture["sensitive_paths"]:
        candidate = root / relative
        candidate.parent.mkdir(parents=True, exist_ok=True)
        candidate.write_text("SECRET_CANARY_LOCAL_OS", encoding="utf-8")
        with (
            pytest.raises((PathEscapeError, CapabilityDenied)),
            workspace.open_read(relative),
        ):
            pass


def test_grant_is_tenant_user_scoped_and_revocation_is_immediate(granted_workspace) -> None:
    store, grant, _workspace, _root = granted_workspace

    assert store.get(
        grant.grant_id,
        "read",
        tenant_id="tenant-a",
        user_id="user-a",
    ) == grant
    with pytest.raises(CapabilityDenied):
        store.get(grant.grant_id, "read", tenant_id="tenant-b", user_id="user-a")
    with pytest.raises(CapabilityDenied):
        store.get(grant.grant_id, "read", tenant_id="tenant-a", user_id="user-b")

    store.revoke(grant.grant_id)
    with pytest.raises(CapabilityDenied):
        store.get(grant.grant_id, "read", tenant_id="tenant-a", user_id="user-a")


def test_read_grant_cannot_expand_into_write_or_network_upload(tmp_path: Path) -> None:
    root = tmp_path / "read-only"
    root.mkdir()
    store = DirectoryGrantStore()
    grant = store.issue(root, frozenset({"read"}))

    for forbidden in ("write", "network.upload", "credential.use"):
        with pytest.raises(CapabilityDenied):
            store.get(grant.grant_id, forbidden)


def test_grant_root_replacement_invalidates_original_authority(tmp_path: Path) -> None:
    root = tmp_path / "workspace"
    root.mkdir()
    store = DirectoryGrantStore()
    grant = store.issue(root, frozenset({"read"}))

    moved = tmp_path / "workspace-old"
    root.rename(moved)
    root.mkdir()

    with pytest.raises(CapabilityDenied, match="identity changed"):
        store.get(grant.grant_id, "read")
