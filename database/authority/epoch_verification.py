"""Exact post-baseline fingerprints, frozen from an isolated reference build."""
from __future__ import annotations

import json
import re

from .manifest import BaselineManifest, file_sha256, load_epoch_manifest
from .runner import AuthorityError, AuthorityPaths


def expected_epoch_fingerprints(
    paths: AuthorityPaths, baseline: BaselineManifest, applied: dict[int, str],
) -> dict[str, str]:
    if not applied:
        return baseline.fingerprints
    directory = paths.epoch_dir(baseline.baseline_id)
    manifest = load_epoch_manifest(directory / "manifest.yml")
    sequence = max(applied)
    expected_changes = {c.sequence: c.sha256 for c in manifest.changes if c.sequence <= sequence}
    if sorted(applied) != list(range(1, sequence + 1)) or applied != expected_changes:
        raise AuthorityError("epoch verification requires an exact contiguous declared ledger")
    path = directory / f"{sequence:03d}_verification.json"
    if not path.is_file():
        raise AuthorityError("epoch reference fingerprints are unavailable")
    try:
        reference = json.loads(path.read_text())
        fingerprints = reference["fingerprints"]
        valid = (
            reference["baseline_id"] == baseline.baseline_id
            and reference["sequence"] == sequence
            and reference["baseline_manifest_sha256"] == file_sha256(paths.baseline_dir(baseline.baseline_id) / "manifest.json")
            and reference["changes"] == {str(k): v for k, v in expected_changes.items()}
            and set(fingerprints) == set(baseline.fingerprints)
            and all(isinstance(v, str) and re.fullmatch(r"[0-9a-f]{64}", v) for v in fingerprints.values())
        )
    except (KeyError, TypeError, ValueError):
        valid = False
    if not valid:
        raise AuthorityError("epoch reference fingerprints do not bind the current baseline and changes")
    return fingerprints
