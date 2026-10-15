from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path

import pytest
from ai_gateway_core.release_manifest import (
    ReleaseManifestUnavailable,
    service_version,
)
from fastapi import FastAPI
from fastapi.testclient import TestClient
from knowledge_service.api.version import version_router

from scripts.release import compatibility_manifest, version_agreement_gate
from src.api.version import router as gateway_version_router

RELEASE_ID = "platform-release-12345678"
GIT_SHA = "1" * 40
IMAGE_DIGEST = f"sha256:{'2' * 64}"
MANIFEST_SHA = "3" * 64
UPSTREAM_SHA = "4" * 40
OVERLAY_SHA = "5" * 64
RUNTIME_SCHEMA = "6" * 64
WORKER_SCHEMA = "7" * 64


def _identity(service: str, mode: str, image: str = IMAGE_DIGEST) -> dict:
    identity = {
        "mode": mode,
        "service_id": service,
        "release_id": RELEASE_ID,
        "git_sha": GIT_SHA,
        "image_digest": image,
    }
    if service in compatibility_manifest.APP_CONTAINER_SERVICES:
        identity["image_version"] = "2.0.0"
    return identity


def _manifest(path: Path, *, status: str = "release_candidate") -> dict:
    services = {
        service: {
            "image_digest": IMAGE_DIGEST,
            "identity": _identity(service, "http_version"),
        }
        for service in compatibility_manifest.HTTP_VERSION_SERVICES
    }
    payload = {
        "schema_version": compatibility_manifest.SCHEMA,
        "status": status,
        "release_id": RELEASE_ID,
        "source": {"git_sha": GIT_SHA, "git_tree_sha": "8" * 40},
        "services": services,
    }
    path.write_text(json.dumps(payload, sort_keys=True), encoding="utf-8")
    return payload


def _set_runtime_identity(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("AI_PLATFORM_RELEASE_ID", RELEASE_ID)
    monkeypatch.setenv("AI_PLATFORM_GIT_SHA", GIT_SHA)
    monkeypatch.setenv("AI_PLATFORM_IMAGE_DIGEST", IMAGE_DIGEST)


def test_services_project_the_same_manifest_identity(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    path = tmp_path / "compatibility.json"
    payload = _manifest(path)
    monkeypatch.setenv("AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH", str(path))
    _set_runtime_identity(monkeypatch)
    expected_hash = hashlib.sha256(path.read_bytes()).hexdigest()
    gateway_app = FastAPI()
    gateway_app.include_router(gateway_version_router)
    knowledge_app = FastAPI()
    knowledge_app.include_router(version_router("knowledge-service"))

    gateway = TestClient(gateway_app).get("/version")
    knowledge = TestClient(knowledge_app).get("/version")

    assert gateway.status_code == knowledge.status_code == 200
    assert gateway.json()["release_id"] == knowledge.json()["release_id"] == payload["release_id"]
    assert gateway.json()["manifest_sha256"] == knowledge.json()["manifest_sha256"] == expected_hash
    assert {gateway.json()["service_id"], knowledge.json()["service_id"]} == {
        "gateway",
        "knowledge-service",
    }


def test_draft_or_runtime_identity_drift_returns_unavailable(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    path = tmp_path / "compatibility.json"
    _manifest(path, status="draft")
    monkeypatch.setenv("AI_PLATFORM_COMPATIBILITY_MANIFEST_PATH", str(path))
    _set_runtime_identity(monkeypatch)
    app = FastAPI()
    app.include_router(gateway_version_router)

    response = TestClient(app).get("/version")

    assert response.status_code == 503
    assert response.json()["code"] == "RELEASE_MANIFEST_UNAVAILABLE"
    _manifest(path)
    monkeypatch.setenv("AI_PLATFORM_GIT_SHA", "9" * 40)
    with pytest.raises(ReleaseManifestUnavailable, match="runtime Git SHA"):
        service_version("gateway", path)


def test_http_version_fails_closed_without_runtime_identity_or_for_worker_mode(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    path = tmp_path / "compatibility.json"
    payload = _manifest(path)

    with pytest.raises(ReleaseManifestUnavailable, match="runtime release identity"):
        service_version("gateway", path)

    _set_runtime_identity(monkeypatch)
    payload["services"]["gateway"]["identity"]["mode"] = "container_image"
    path.write_text(json.dumps(payload, sort_keys=True), encoding="utf-8")
    with pytest.raises(ReleaseManifestUnavailable, match="does not expose HTTP"):
        service_version("gateway", path)


def _full_manifest() -> dict:
    services = {}
    for service in compatibility_manifest.SERVICES:
        mode = (
            "http_version"
            if service in compatibility_manifest.HTTP_VERSION_SERVICES
            else "container_image"
        )
        services[service] = {
            "image_digest": IMAGE_DIGEST,
            "identity": _identity(service, mode),
        }
    return {
        "schema_version": compatibility_manifest.SCHEMA,
        "status": "release_candidate",
        "release_id": RELEASE_ID,
        "source": {"git_sha": GIT_SHA},
        "services": services,
        "runtime_overlay": {
            "upstream_sha": UPSTREAM_SHA,
            "overlay_sha256": OVERLAY_SHA,
            "runtime_schema_digest": RUNTIME_SCHEMA,
            "capability_worker_schema_digest": WORKER_SCHEMA,
        },
    }


def _container_observations(manifest: dict) -> tuple[dict[str, list[str]], dict[str, dict]]:
    targets = {
        service: [f"{service}-container"]
        for service in compatibility_manifest.CONTAINER_IDENTITY_SERVICES
    }
    inspected: dict[str, dict] = {}
    revision = f"{UPSTREAM_SHA}+{OVERLAY_SHA[:12]}"
    for service, service_targets in targets.items():
        labels: dict[str, str]
        if service in compatibility_manifest.APP_CONTAINER_SERVICES:
            labels = {
                "org.opencontainers.image.revision": GIT_SHA,
                "org.opencontainers.image.version": manifest["services"][service]["identity"][
                    "image_version"
                ],
            }
        elif service == "agent-runtime":
            labels = {
                "org.opencontainers.image.revision": revision,
                "com.misaya.ai-platform.agent-runtime.schema-sha256": RUNTIME_SCHEMA,
                "com.misaya.ai-platform.agent-runtime.artifact": "agent_runtime",
                "com.misaya.ai-platform.agent-runtime.binary": "ai-platform-agent-runtime",
            }
        else:
            labels = {
                "org.opencontainers.image.revision": revision,
                "com.misaya.ai-platform.capability-worker.schema-sha256": WORKER_SCHEMA,
                "com.misaya.ai-platform.capability-worker.artifact": "capability_worker",
                "com.misaya.ai-platform.capability-worker.binary": "ai-platform-capability-worker",
            }
        state = (
            {"Running": False, "Status": "exited", "ExitCode": 0}
            if service == "migrator"
            else {"Running": True, "Status": "running", "ExitCode": 0}
        )
        inspected[service_targets[0]] = {
            "Image": manifest["services"][service]["image_digest"],
            "State": state,
            "Config": {"Labels": labels},
        }
    return targets, inspected


def _http_endpoints() -> dict[str, str]:
    return {
        service: f"http://{service}.test/version"
        for service in compatibility_manifest.HTTP_VERSION_SERVICES
    }


def _install_observation_fakes(
    monkeypatch: pytest.MonkeyPatch,
    manifest: dict,
    endpoints: dict[str, str],
    inspected: dict[str, dict],
    *,
    manifest_sha: str = MANIFEST_SHA,
) -> None:
    def fake_fetch(url: str) -> dict:
        service = next(name for name, endpoint in endpoints.items() if endpoint == url)
        identity = manifest["services"][service]["identity"]
        return {
            "schema_version": "ai-platform/service-version/v1",
            "service_id": service,
            "release_id": identity["release_id"],
            "git_sha": identity["git_sha"],
            "image_digest": identity["image_digest"],
            "manifest_sha256": manifest_sha,
        }

    monkeypatch.setattr(version_agreement_gate, "_fetch", fake_fetch)
    monkeypatch.setattr(
        version_agreement_gate,
        "_inspect_container",
        lambda target: copy.deepcopy(inspected[target]),
    )


def test_live_version_agreement_uses_http_and_container_evidence(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manifest = _full_manifest()
    endpoints = _http_endpoints()
    containers, inspected = _container_observations(manifest)
    _install_observation_fakes(monkeypatch, manifest, endpoints, inspected)

    result = version_agreement_gate.verify(
        manifest,
        endpoints,
        containers,
        manifest_sha256=MANIFEST_SHA,
    )

    assert result["result"] == "pass"
    assert set(result["services"]) == compatibility_manifest.SERVICES
    assert result["observations"]["gateway"] == {"mode": "http_version", "replicas": 1}
    assert result["observations"]["migrator"] == {
        "mode": "container_image",
        "replicas": 1,
    }


def test_version_agreement_rejects_fictitious_endpoint_or_missing_container(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    manifest = _full_manifest()
    endpoints = _http_endpoints()
    containers, inspected = _container_observations(manifest)
    _install_observation_fakes(monkeypatch, manifest, endpoints, inspected)

    endpoints["frontend"] = "http://frontend.test/version"
    with pytest.raises(version_agreement_gate.VersionAgreementError, match="endpoint set"):
        version_agreement_gate.verify(manifest, endpoints, containers, manifest_sha256=MANIFEST_SHA)
    endpoints.pop("frontend")
    containers.pop("migrator")
    with pytest.raises(
        version_agreement_gate.VersionAgreementError, match="container identity set"
    ):
        version_agreement_gate.verify(manifest, endpoints, containers, manifest_sha256=MANIFEST_SHA)


@pytest.mark.parametrize(
    ("service", "mutation", "error"),
    [
        (
            "agent-runtime",
            lambda value: value.update(Image=f"sha256:{'9' * 64}"),
            "image identity mismatch",
        ),
        (
            "frontend",
            lambda value: value["Config"]["Labels"].update(
                {"org.opencontainers.image.version": "wrong"}
            ),
            "OCI identity mismatch",
        ),
        (
            "migrator",
            lambda value: value["State"].update(ExitCode=1),
            "did not succeed",
        ),
        (
            "knowledge-worker",
            lambda value: value["State"].update(Running=False, Status="exited"),
            "not running",
        ),
    ],
)
def test_version_agreement_rejects_container_identity_drift(
    monkeypatch: pytest.MonkeyPatch,
    service: str,
    mutation,
    error: str,
) -> None:
    manifest = _full_manifest()
    endpoints = _http_endpoints()
    containers, inspected = _container_observations(manifest)
    mutation(inspected[containers[service][0]])
    _install_observation_fakes(monkeypatch, manifest, endpoints, inspected)

    with pytest.raises(version_agreement_gate.VersionAgreementError, match=error):
        version_agreement_gate.verify(manifest, endpoints, containers, manifest_sha256=MANIFEST_SHA)


def test_version_agreement_rejects_wrong_manifest_bytes_and_unsafe_endpoint(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    real_fetch = version_agreement_gate._fetch
    manifest = _full_manifest()
    endpoints = _http_endpoints()
    containers, inspected = _container_observations(manifest)
    _install_observation_fakes(
        monkeypatch,
        manifest,
        endpoints,
        inspected,
        manifest_sha="8" * 64,
    )

    with pytest.raises(version_agreement_gate.VersionAgreementError, match="/version identity"):
        version_agreement_gate.verify(manifest, endpoints, containers, manifest_sha256=MANIFEST_SHA)
    with pytest.raises(version_agreement_gate.VersionAgreementError, match="exact http"):
        real_fetch("http://gateway.test/version?claimed=true")
