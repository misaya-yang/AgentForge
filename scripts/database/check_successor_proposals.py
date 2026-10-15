#!/usr/bin/env python3
"""Validate inert successor sources; optionally emit a NEW external preview only.

No DB clients, credentials, services, SQL execution or source-tree application.
The resulting authority view lacks measured verification files and is not ready
for migration or application activation. Exact preimages are mandatory.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import shutil
import sys
from pathlib import Path

PLAN = Path("docs/design/canonical-schema-successors.json")
BASELINE = "2026_08_post_kb_v1"
EXPECTED_SUCCESSORS = {
    18: ("application_schema_admission", "docs/design/application-schema-admission.sql.proposal"),
    19: ("knowledge_progress_trigger_authority", "docs/design/knowledge-progress-trigger-authority.sql.proposal"),
    20: ("gateway_task_deliveries", "docs/design/generic-task-durable-schema.sql.proposal"),
}


class ProposalError(RuntimeError):
    pass


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def source_path(root: Path, relative: str) -> Path:
    path = Path(relative)
    if path.is_absolute() or ".." in path.parts:
        raise ProposalError("unsafe source path")
    target = (root / path).resolve()
    if not target.is_relative_to(root.resolve()) or not target.is_file():
        raise ProposalError("missing or escaped source path")
    return target


def validate(root: Path) -> tuple[dict, dict]:
    root = root.resolve()
    sys.path[:0] = [str(root), str(root / "packages/ai-gateway-contracts/src")]
    from database.authority.manifest import load_epoch_manifest
    from database.authority.runner import MigrationAuthority

    plan = json.loads(source_path(root, str(PLAN)).read_text())
    if (plan.get("schema_version") != "ai-platform/canonical-successor-source-plan/v1"
            or plan.get("status") != "inactive_source_candidate"
            or plan.get("database_actions_authorized") is not False
            or plan.get("fingerprint_status") != "not_measured"
            or plan.get("no_verification_files") is not True):
        raise ProposalError("proposal must remain inactive and unverified")
    if (plan.get("baseline_id") != BASELINE or plan.get("predecessor_epoch") != 17
            or plan.get("candidate_epoch") != 20 or len(plan.get("successors", [])) != 3):
        raise ProposalError("unknown successor baseline or sequence allocation")
    # Validate every output name and input source before any preview write.
    for number, entry in enumerate(plan["successors"], 18):
        name, proposal = EXPECTED_SUCCESSORS[number]
        change = entry["change"]
        if (change.get("name") != name or change.get("file") != f"{number:03d}_{name}.sql"
                or entry.get("proposal") != proposal):
            raise ProposalError("unsafe or unreviewed successor destination")
    for relative, expected in plan["frozen_preimages"].items():
        if digest(source_path(root, relative)) != expected:
            raise ProposalError(f"exact preimage changed: {relative}")
    directory = root / "database/migrations" / plan["baseline_id"]
    current = load_epoch_manifest(directory / "manifest.yml")
    if (current.epoch != plan["predecessor_epoch"]
            or digest(directory / "manifest.yml") != plan["predecessor_manifest_sha256"]):
        raise ProposalError("canonical predecessor changed; re-review required")
    if any((directory / f"{number:03d}_verification.json").exists()
           for number in range(current.epoch + 1, plan["candidate_epoch"] + 1)):
        raise ProposalError("unexpected successor reference in inactive source")
    changes = []
    for number, entry in enumerate(plan["successors"], current.epoch + 1):
        change = entry["change"]
        proposal = source_path(root, entry["proposal"])
        if (change["sequence"] != number or digest(proposal) != change["sha256"]
                or change["owner"] != "owner"
                or change["transaction_mode"] != "transactional"
                or change["rollback_class"] != "forward-fix-only"):
            raise ProposalError("successor order, authority or source hash changed")
        if (directory / change["file"]).exists():
            raise ProposalError("inactive successor already entered migration discovery")
        MigrationAuthority._reject_embedded_transactions(proposal.read_text(), change["name"])
        changes.append(change)
    if not changes or changes[-1]["sequence"] != plan["candidate_epoch"]:
        raise ProposalError("incomplete successor chain")
    manifest = json.loads((directory / "manifest.yml").read_text())
    manifest["epoch"] = plan["candidate_epoch"]
    manifest["changes"] += changes
    return plan, manifest


def materialize(root: Path, output: Path) -> dict:
    """Write an exclusive external authority preview, never mutate an input tree."""
    root = root.resolve()
    output = output.absolute()
    resolved = output.resolve()
    if output != resolved or resolved.is_relative_to(root) or root.is_relative_to(resolved):
        raise ProposalError("preview must be a nonsymlink external descendant of another directory")
    if output.exists():
        raise ProposalError("preview output must not already exist")
    plan, manifest = validate(root)
    # Exclusive mkdir closes the ordinary exists/create race; no overwrite mode.
    baseline = plan["baseline_id"]
    for relative in (f"database/baselines/{baseline}", f"database/migrations/{baseline}"):
        source = root / relative
        if source.is_symlink() or any(p.is_symlink() for p in source.rglob("*")):
            raise ProposalError("authority preview refuses source symlinks")
    output.mkdir(parents=False, exist_ok=False)
    for relative in (f"database/baselines/{baseline}", f"database/migrations/{baseline}"):
        source = root / relative
        if any(p.is_symlink() for p in source.rglob("*")):
            raise ProposalError("authority preview refuses source symlinks")
        shutil.copytree(source, output / relative)
    directory = output / "database/migrations" / baseline
    for entry in plan["successors"]:
        destination = directory / entry["change"]["file"]
        if destination.parent != directory or destination.exists():
            raise ProposalError("preview destination collision")
        with destination.open("xb") as stream:
            stream.write(source_path(root, entry["proposal"]).read_bytes())
    (directory / "manifest.yml").write_text(json.dumps(manifest, indent=2) + "\n")
    from database.authority.manifest import load_epoch_manifest
    validated = load_epoch_manifest(directory / "manifest.yml")
    receipt = {"status": "inactive_external_preview_only", "epoch": validated.epoch,
               "manifest_sha256": digest(directory / "manifest.yml"),
               "changes": {str(c.sequence): c.sha256 for c in validated.changes},
               "missing_measurements": [18, 19, 20], "database_actions": False}
    (output / "PREVIEW_NOT_ACTIVATED.json").write_text(json.dumps(receipt, indent=2) + "\n")
    return receipt


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=Path(__file__).resolve().parents[2])
    parser.add_argument("--preview-output", type=Path)
    args = parser.parse_args()
    if args.preview_output:
        result = materialize(args.root, args.preview_output)
    else:
        plan, manifest = validate(args.root)
        result = {"status": "source_invariants_passed", "canonical_epoch": plan["predecessor_epoch"],
                  "candidate_epoch": manifest["epoch"], "database_actions": False}
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (ProposalError, KeyError, ValueError) as exc:
        raise SystemExit(str(exc)) from None
