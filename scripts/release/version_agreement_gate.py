#!/usr/bin/env python3
"""Verify live HTTP and container identities against one release manifest."""

from __future__ import annotations

import hashlib
import json
import os
import re
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

from scripts.release.compatibility_manifest import (
    APP_CONTAINER_SERVICES,
    CONTAINER_IDENTITY_SERVICES,
    HTTP_VERSION_SERVICES,
    IMAGE_DIGEST,
    RELEASE_ID,
    RUST_CONTAINER_SERVICES,
    SCHEMA,
    SERVICES,
    ManifestError,
    _load,
)

HEX_40 = re.compile(r"^[0-9a-f]{40}$")
HEX_64 = re.compile(r"^[0-9a-f]{64}$")
CONTAINER_REF = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$")


class VersionAgreementError(RuntimeError):
    pass


def _fetch(url: str) -> dict[str, Any]:
    parsed = urllib.parse.urlsplit(url)
    if (
        parsed.scheme not in {"http", "https"}
        or not parsed.hostname
        or parsed.username
        or parsed.password
        or parsed.path != "/version"
        or parsed.query
        or parsed.fragment
    ):
        raise VersionAgreementError(
            "version endpoint must be an exact http(s) /version URL without "
            "userinfo, query, or fragment"
        )
    try:
        with urllib.request.urlopen(url, timeout=10) as response:  # noqa: S310
            if response.status != 200:
                raise VersionAgreementError(f"version endpoint returned HTTP {response.status}")
            payload = json.load(response)
    except (OSError, urllib.error.URLError, json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise VersionAgreementError("version endpoint unavailable") from exc
    if not isinstance(payload, dict):
        raise VersionAgreementError(f"version endpoint did not return an object: {url}")
    return payload


def _inspect_container(target: str) -> dict[str, Any]:
    if CONTAINER_REF.fullmatch(target) is None:
        raise VersionAgreementError("container target is invalid")
    try:
        result = subprocess.run(
            ["docker", "container", "inspect", target],
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as exc:
        raise VersionAgreementError("Docker container inspection is unavailable") from exc
    if result.returncode != 0:
        raise VersionAgreementError(f"release container is unavailable: {target}")
    try:
        payload = json.loads(result.stdout)
    except (json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise VersionAgreementError("Docker container inspection returned invalid JSON") from exc
    if not isinstance(payload, list) or len(payload) != 1 or not isinstance(payload[0], dict):
        raise VersionAgreementError("Docker container inspection returned an invalid shape")
    return payload[0]


def _targets(value: Any, service: str) -> list[str]:
    raw = [value] if isinstance(value, str) else value
    if (
        not isinstance(raw, list)
        or not raw
        or not all(
            isinstance(target, str) and CONTAINER_REF.fullmatch(target) is not None
            for target in raw
        )
        or len(set(raw)) != len(raw)
    ):
        raise VersionAgreementError(f"container target list is invalid: {service}")
    return raw


def _service_identity(
    manifest: dict[str, Any], service: str, expected_mode: str
) -> tuple[dict[str, Any], dict[str, Any]]:
    source = manifest.get("source") if isinstance(manifest.get("source"), dict) else {}
    services = manifest.get("services") if isinstance(manifest.get("services"), dict) else {}
    record = services.get(service) if isinstance(services.get(service), dict) else {}
    identity = record.get("identity") if isinstance(record.get("identity"), dict) else {}
    expected = {
        "mode": expected_mode,
        "service_id": service,
        "release_id": manifest.get("release_id"),
        "git_sha": source.get("git_sha"),
        "image_digest": record.get("image_digest"),
    }
    if IMAGE_DIGEST.fullmatch(str(record.get("image_digest"))) is None or any(
        identity.get(key) != value for key, value in expected.items()
    ):
        raise VersionAgreementError(f"manifest service identity is invalid: {service}")
    return record, identity


def _expected_container_labels(
    manifest: dict[str, Any], service: str, identity: dict[str, Any]
) -> dict[str, str]:
    source = manifest.get("source") if isinstance(manifest.get("source"), dict) else {}
    if service in APP_CONTAINER_SERVICES:
        image_version = identity.get("image_version")
        git_sha = source.get("git_sha")
        if (
            not isinstance(image_version, str)
            or not image_version
            or HEX_40.fullmatch(str(git_sha)) is None
        ):
            raise VersionAgreementError(f"manifest OCI label identity is invalid: {service}")
        return {
            "org.opencontainers.image.revision": git_sha,
            "org.opencontainers.image.version": image_version,
        }

    if service not in RUST_CONTAINER_SERVICES:
        raise VersionAgreementError(f"unsupported container identity service: {service}")
    overlay = (
        manifest.get("runtime_overlay") if isinstance(manifest.get("runtime_overlay"), dict) else {}
    )
    upstream = overlay.get("upstream_sha")
    overlay_sha = overlay.get("overlay_sha256")
    if HEX_40.fullmatch(str(upstream)) is None or HEX_64.fullmatch(str(overlay_sha)) is None:
        raise VersionAgreementError("Runtime source-lock identity is invalid")
    revision = f"{upstream}+{str(overlay_sha)[:12]}"
    if service == "agent-runtime":
        schema = overlay.get("runtime_schema_digest")
        if HEX_64.fullmatch(str(schema)) is None:
            raise VersionAgreementError("Runtime schema identity is invalid")
        return {
            "org.opencontainers.image.revision": revision,
            "com.misaya.ai-platform.agent-runtime.schema-sha256": schema,
            "com.misaya.ai-platform.agent-runtime.artifact": "agent_runtime",
            "com.misaya.ai-platform.agent-runtime.binary": "ai-platform-agent-runtime",
        }
    schema = overlay.get("capability_worker_schema_digest")
    if HEX_64.fullmatch(str(schema)) is None:
        raise VersionAgreementError("Capability Worker schema identity is invalid")
    return {
        "org.opencontainers.image.revision": revision,
        "com.misaya.ai-platform.capability-worker.schema-sha256": schema,
        "com.misaya.ai-platform.capability-worker.artifact": "capability_worker",
        "com.misaya.ai-platform.capability-worker.binary": "ai-platform-capability-worker",
    }


def _verify_container(
    manifest: dict[str, Any],
    service: str,
    target: str,
    expected_digest: str,
    identity: dict[str, Any],
) -> None:
    inspected = _inspect_container(target)
    if inspected.get("Image") != expected_digest:
        raise VersionAgreementError(f"running container image identity mismatch: {service}")
    state = inspected.get("State") if isinstance(inspected.get("State"), dict) else {}
    if service == "migrator":
        running_or_succeeded = state.get("Running") is True or (
            state.get("Status") == "exited" and state.get("ExitCode") == 0
        )
        if not running_or_succeeded:
            raise VersionAgreementError("migration release container did not succeed")
    elif state.get("Running") is not True or state.get("Status") != "running":
        raise VersionAgreementError(f"release container is not running: {service}")
    config = inspected.get("Config") if isinstance(inspected.get("Config"), dict) else {}
    labels = config.get("Labels") if isinstance(config.get("Labels"), dict) else {}
    expected_labels = _expected_container_labels(manifest, service, identity)
    if any(labels.get(key) != value for key, value in expected_labels.items()):
        raise VersionAgreementError(f"running container OCI identity mismatch: {service}")


def verify(
    manifest: dict[str, Any],
    endpoints: dict[str, Any],
    containers: dict[str, Any],
    *,
    manifest_sha256: str,
) -> dict[str, Any]:
    if (
        manifest.get("schema_version") != SCHEMA
        or manifest.get("status") not in {"release_candidate", "released"}
        or RELEASE_ID.fullmatch(str(manifest.get("release_id"))) is None
        or HEX_64.fullmatch(manifest_sha256) is None
    ):
        raise VersionAgreementError("compatibility manifest is not a runnable release")
    services = manifest.get("services")
    if not isinstance(services, dict) or set(services) != SERVICES:
        raise VersionAgreementError("compatibility manifest service set drift")
    if set(endpoints) != HTTP_VERSION_SERVICES:
        raise VersionAgreementError("HTTP version endpoint set differs from compatibility policy")
    if set(containers) != CONTAINER_IDENTITY_SERVICES:
        raise VersionAgreementError("container identity set differs from compatibility policy")

    observations: dict[str, dict[str, Any]] = {}
    for service in sorted(HTTP_VERSION_SERVICES):
        url = endpoints[service]
        if not isinstance(url, str):
            raise VersionAgreementError(f"version endpoint URL is invalid: {service}")
        record, identity = _service_identity(manifest, service, "http_version")
        payload = _fetch(url)
        expected = {
            "schema_version": "ai-platform/service-version/v1",
            "service_id": service,
            "release_id": identity["release_id"],
            "git_sha": identity["git_sha"],
            "image_digest": record["image_digest"],
            "manifest_sha256": manifest_sha256,
        }
        if any(payload.get(key) != value for key, value in expected.items()):
            raise VersionAgreementError(f"live /version identity mismatch: {service}")
        observations[service] = {"mode": "http_version", "replicas": 1}

    all_targets: set[str] = set()
    for service in sorted(CONTAINER_IDENTITY_SERVICES):
        record, identity = _service_identity(manifest, service, "container_image")
        service_targets = _targets(containers[service], service)
        if all_targets.intersection(service_targets):
            raise VersionAgreementError("one running container cannot identify two services")
        all_targets.update(service_targets)
        for target in service_targets:
            _verify_container(
                manifest,
                service,
                target,
                record["image_digest"],
                identity,
            )
        observations[service] = {
            "mode": "container_image",
            "replicas": len(service_targets),
        }

    return {
        "result": "pass",
        "services": sorted(observations),
        "manifest_sha256": manifest_sha256,
        "observations": observations,
    }


def main() -> int:
    manifest_path = os.environ.get("AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH", "")
    raw_endpoints = os.environ.get("AI_PLATFORM_VERSION_ENDPOINTS", "")
    raw_containers = os.environ.get("AI_PLATFORM_VERSION_CONTAINERS", "")
    try:
        if not manifest_path or not raw_endpoints or not raw_containers:
            raise VersionAgreementError(
                "compatibility manifest, HTTP endpoints, and container identities are required"
            )
        endpoints = json.loads(raw_endpoints)
        containers = json.loads(raw_containers)
        if not isinstance(endpoints, dict) or not isinstance(containers, dict):
            raise VersionAgreementError("version observations must be JSON objects")
        path = Path(manifest_path)
        manifest_sha256 = hashlib.sha256(path.read_bytes()).hexdigest()
        result = verify(
            _load(path, "compatibility manifest"),
            endpoints,
            containers,
            manifest_sha256=manifest_sha256,
        )
        print(json.dumps(result, indent=2, sort_keys=True))
        return 0
    except (
        VersionAgreementError,
        ManifestError,
        json.JSONDecodeError,
        OSError,
        UnicodeDecodeError,
    ) as exc:
        print(f"VERSION AGREEMENT ERROR: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
