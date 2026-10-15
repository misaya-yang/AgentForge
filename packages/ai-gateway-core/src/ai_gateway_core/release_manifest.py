"""Safe service projection of the unified platform compatibility manifest."""

from __future__ import annotations

import hashlib
import json
import os
import re
from pathlib import Path
from typing import Any

SCHEMA = "ai-platform/compatibility-manifest/v1"
IMAGE_DIGEST = re.compile(r"^sha256:[0-9a-f]{64}$")
HEX_40 = re.compile(r"^[0-9a-f]{40}$")
DEFAULT_PATH = Path("deploy/release/compatibility-manifest.json")


class ReleaseManifestUnavailable(RuntimeError):
    pass


def _path(explicit: Path | None = None) -> Path:
    if explicit is not None:
        return explicit
    raw = os.environ.get("AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH")
    return Path(raw) if raw else DEFAULT_PATH


def _sha(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def service_version(service_id: str, path: Path | None = None) -> dict[str, Any]:
    manifest_path = _path(path)
    try:
        raw = manifest_path.read_bytes()
        manifest = json.loads(raw)
    except (OSError, json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise ReleaseManifestUnavailable("compatibility manifest is unavailable") from exc
    if not isinstance(manifest, dict) or manifest.get("schema_version") != SCHEMA:
        raise ReleaseManifestUnavailable("compatibility manifest schema is unsupported")
    if manifest.get("status") not in {"release_candidate", "released"}:
        raise ReleaseManifestUnavailable("compatibility manifest is not a runnable release")
    release_id = manifest.get("release_id")
    source = manifest.get("source")
    services = manifest.get("services")
    if (
        not isinstance(release_id, str)
        or not isinstance(source, dict)
        or HEX_40.fullmatch(str(source.get("git_sha"))) is None
        or not isinstance(services, dict)
        or not isinstance(services.get(service_id), dict)
    ):
        raise ReleaseManifestUnavailable("compatibility manifest identity is incomplete")
    record = services[service_id]
    digest = record.get("image_digest")
    expected = record.get("identity")
    if IMAGE_DIGEST.fullmatch(str(digest)) is None or not isinstance(expected, dict):
        raise ReleaseManifestUnavailable("service release identity is incomplete")
    if expected.get("mode") != "http_version":
        raise ReleaseManifestUnavailable("service does not expose HTTP version identity")
    payload = {
        "schema_version": "ai-platform/service-version/v1",
        "service_id": service_id,
        "release_id": release_id,
        "git_sha": source["git_sha"],
        "image_digest": digest,
        "manifest_sha256": _sha(raw),
    }
    for key in ("service_id", "release_id", "git_sha", "image_digest"):
        if expected.get(key) != payload[key]:
            raise ReleaseManifestUnavailable("service version projection disagrees with manifest")
    runtime_release = os.environ.get("AI_PLATFORM_RELEASE_ID")
    runtime_git = os.environ.get("AI_PLATFORM_GIT_SHA")
    runtime_image = os.environ.get("AI_PLATFORM_IMAGE_DIGEST")
    if not runtime_release or not runtime_git or not runtime_image:
        raise ReleaseManifestUnavailable("runtime release identity is incomplete")
    if runtime_release != payload["release_id"]:
        raise ReleaseManifestUnavailable("runtime release ID disagrees with compatibility manifest")
    if runtime_git != payload["git_sha"]:
        raise ReleaseManifestUnavailable("runtime Git SHA disagrees with compatibility manifest")
    if runtime_image != payload["image_digest"]:
        raise ReleaseManifestUnavailable(
            "runtime image digest disagrees with compatibility manifest"
        )
    return payload


__all__ = ["ReleaseManifestUnavailable", "service_version"]
