from __future__ import annotations

import threading
from dataclasses import replace

import pytest

from local_node.errors import CapabilityDenied
from local_node.file_dispatch import ReadOnlyFileActionHandlers
from local_node.files import LocalFileService
from local_node.grants import DirectoryGrantStore
from local_node.ledger import ActionLedger


def _setup(tmp_path, platform_signature_verifier, trusted_local_approval_verifier):
    root = tmp_path / "authorized"
    root.mkdir()
    (root / "alpha.txt").write_text("first\nneedle alpha\n", encoding="utf-8")
    (root / "beta.txt").write_text("needle beta\nlast\n", encoding="utf-8")
    grants = DirectoryGrantStore()
    grant = grants.issue(
        root,
        frozenset({"list", "read", "search", "watch"}),
        tenant_id="tenant-a",
        user_id="user-a",
        grant_id="grant-platform-a",
    )
    ledger = ActionLedger(
        tmp_path / "state" / "ledger.sqlite",
        platform_signature_verifier=platform_signature_verifier,
        trusted_local_approval_verifier=trusted_local_approval_verifier,
    )
    files = LocalFileService(grants, tmp_path / "state" / "rollbacks", ledger)
    return root, grant, ReadOnlyFileActionHandlers(files)


def _action(action_factory, operation: str, arguments: dict, *, action_id: str):
    capability = "file.read" if operation == "file.hash" else operation
    action = action_factory(
        capability,
        arguments,
        "target-a",
        action_id=action_id,
        idempotency_key="idem-" + action_id,
        capability_lease_id=arguments["grant_id"],
        resource_refs=(arguments["grant_id"], arguments["path"]),
        tool_name="local_" + operation.replace(".", "_"),
        operation=operation,
    )
    return replace(action, approval=None)


def test_list_search_read_and_hash_are_exact_and_grant_bound(
    tmp_path,
    action_factory,
    platform_signature_verifier,
    trusted_local_approval_verifier,
):
    _, grant, handlers = _setup(
        tmp_path,
        platform_signature_verifier,
        trusted_local_approval_verifier,
    )
    base = {"grant_id": grant.grant_id, "path": "."}
    listed = handlers.list_files(
        _action(action_factory, "file.list", base, action_id="action-list"),
        base,
    ).result
    assert [entry["relative_path"] for entry in listed["entries"]] == [
        "alpha.txt",
        "beta.txt",
    ]

    search_args = {**base, "query": "needle", "limit": 10}
    searched = handlers.search_files(
        _action(action_factory, "file.search", search_args, action_id="action-search"),
        search_args,
    ).result
    citations = [
        (match["relative_path"], match["line"], match["column"])
        for match in searched["matches"]
    ]
    assert citations == [
        ("alpha.txt", 2, 1),
        ("beta.txt", 1, 1),
    ]
    assert all(len(match["file_sha256"]) == 64 for match in searched["matches"])

    read_args = {"grant_id": grant.grant_id, "path": "alpha.txt"}
    read = handlers.read_file(
        _action(action_factory, "file.read", read_args, action_id="action-read"),
        read_args,
    ).result
    hashed = handlers.read_file(
        _action(action_factory, "file.hash", read_args, action_id="action-hash"),
        read_args,
    ).result
    assert read["content"] == "first\nneedle alpha\n"
    assert hashed["sha256"] == read["sha256"]
    assert "content" not in hashed

    with pytest.raises(CapabilityDenied):
        handlers.read_file(
            _action(action_factory, "file.read", read_args, action_id="action-wrong-grant"),
            {**read_args, "grant_id": "grant-other"},
        )


def test_watch_reports_tmp_metadata_and_rejects_unknown_cursor(
    tmp_path,
    action_factory,
    platform_signature_verifier,
    trusted_local_approval_verifier,
):
    root, grant, handlers = _setup(
        tmp_path,
        platform_signature_verifier,
        trusted_local_approval_verifier,
    )
    arguments = {
        "grant_id": grant.grant_id,
        "path": ".",
        "timeout_ms": 500,
    }
    action = _action(action_factory, "file.watch", arguments, action_id="action-watch")
    timer = threading.Timer(
        0.05,
        lambda: (root / "alpha.txt").write_text("changed\n", encoding="utf-8"),
    )
    timer.start()
    try:
        result = handlers.watch_files(action, arguments).result
    finally:
        timer.join()
    assert result["kind"] == "file_watch"
    assert result["events"][0]["kind"] == "modify"
    assert result["events"][0]["relative_path"] == "alpha.txt"
    assert "content" not in result["events"][0]

    stale = {**arguments, "after_revision": "99", "timeout_ms": 1}
    with pytest.raises(CapabilityDenied):
        handlers.watch_files(
            _action(action_factory, "file.watch", stale, action_id="action-stale-watch"),
            stale,
        )


def test_explicit_grant_id_rejects_collision_and_unsafe_value(tmp_path):
    root = tmp_path / "authorized"
    root.mkdir()
    grants = DirectoryGrantStore()
    grants.issue(root, frozenset({"read"}), grant_id="grant-platform-a")
    with pytest.raises(CapabilityDenied):
        grants.issue(root, frozenset({"read"}), grant_id="grant-platform-a")
    with pytest.raises(CapabilityDenied):
        grants.issue(root, frozenset({"read"}), grant_id="../../unsafe")
