"""Contract freeze snapshot.

Produces ``contract-freeze.json``: SHA-256 digests of every public-contract
artifact that can be hashed offline at the pinned Git revision, plus named
pointers (source file + exact command) for the live-stack proofs that ARC-00B
and later packages must produce.

Frozen offline here:

* ``sdk/openapi.json`` — the published Gateway OpenAPI snapshot;
* ``sdk/fixtures/sse_inner_envelopes.json`` — the shared SSE envelope fixture
  behind ``make sdk-sse-contract``;
* Capability V2 authorities in the Rust overlay (contract crate source,
  platform catalog, worker fixtures) plus the Rust overlay manifest;
* the PostgreSQL function list, derived deterministically from
  ``database/schema.sql`` + ``database/migrations/**``;
* the Compose service resolution, parsed from the committed Compose files;
* the image revision defaults and the Rust overlay identity;
* the frozen PPR-00 TTFT baseline receipts.

Anything that needs a running stack (live OpenAPI diff, live SSE against the
gateway, registry image digests, ``/version`` agreement) is listed under
``live_stack_required`` with the command that produces it; nothing in this
file claims those checks passed.
"""

from __future__ import annotations

import json

from _common import REPO_ROOT, base_envelope, canonical_sha256, sha256_file
from service_topology import parse_compose_services

CAPABILITY_FIXTURES = (
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/src/platform_catalog_v1.json",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/tests/fixtures/mcp_read_request_v1.json",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/tests/fixtures/attachment_read_request_v1.json",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-worker/tests/fixtures/attachment_zip_bomb_manifest.json",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-office/fixtures/en_semantic.json",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-office/fixtures/zh_semantic.json",
    "rust/agent-runtime-overlay/kernel-rs/ai-platform-office/fixtures/visual_input.json",
)

PPR00_RECEIPTS = tuple(
    f"reports/performance/assistant-ttft-baseline-2026-08-28-run{n}.json" for n in (1, 2, 3, 4)
)


def _file_entry(rel: str) -> dict:
    path = REPO_ROOT / rel
    if not path.is_file():
        return {"path": rel, "exists": False}
    return {
        "path": rel,
        "exists": True,
        "bytes": path.stat().st_size,
        "sha256": sha256_file(path),
    }


def _db_function_digest() -> dict:
    from data_access import parse_schema_objects

    objects, _ = parse_schema_objects()
    functions = sorted(
        name for name, record in objects.items() if "FUNCTION" in record["kinds"]
    )
    return {
        "function_count": len(functions),
        "functions": functions,
        "canonical_sha256": canonical_sha256({"functions": functions}),
        "derived_from": ["database/schema.sql", "database/migrations/**"],
    }


def _compose_resolution() -> dict:
    files = (
        "docker-compose.yml",
        "docker-compose.dev.yml",
        "docker-compose.build.yml",
        "docker-compose.kbms.yml",
        "docker-compose.capability.yml",
    )
    resolution = {}
    for rel in files:
        services = parse_compose_services(REPO_ROOT / rel)
        resolution[rel] = {
            name: {
                "image": info.get("image"),
                "container_name": info.get("container_name"),
                "build": info.get("build"),
            }
            for name, info in sorted(services.items())
        }
    return {
        "services": resolution,
        "canonical_sha256": canonical_sha256(resolution),
        "note": (
            "Parsed structurally from the committed Compose files (offline). "
            "`docker compose config` additionally resolves ${VAR} interpolation against .env; "
            "run `make validate-config` for the rendered form."
        ),
    }


def build() -> dict:
    overlay = REPO_ROOT / "rust" / "agent-runtime-overlay" / "manifest.json"
    rust_manifest = json.loads(overlay.read_text(encoding="utf-8")) if overlay.is_file() else {}

    openapi = _file_entry("sdk/openapi.json")
    openapi_path = REPO_ROOT / "sdk" / "openapi.json"
    if openapi_path.is_file():
        spec = json.loads(openapi_path.read_text(encoding="utf-8"))
        openapi["path_count"] = len(spec.get("paths", {}))

    frozen = {
        "openapi_snapshot": openapi,
        "sse_fixture": _file_entry("sdk/fixtures/sse_inner_envelopes.json"),
        "capability_v2": {
            "authority": _file_entry("rust/agent-runtime-overlay/kernel-rs/ai-platform-capability-contract/src/lib.rs"),
            "fixtures": [_file_entry(rel) for rel in CAPABILITY_FIXTURES],
            "note": (
                "The Rust contract crate (CapabilityDescriptorV2) is the single authority; "
                "Python CapabilityDescriptor in src/services/agent_runtime/readonly_capabilities.py "
                "is the read-only projection used by the Runtime adapter."
            ),
        },
        "rust_overlay_manifest": {
            "path": "rust/agent-runtime-overlay/manifest.json",
            "contents": rust_manifest,
            "sha256": sha256_file(overlay) if overlay.is_file() else None,
        },
        "database_functions": _db_function_digest(),
        "compose_resolution": _compose_resolution(),
        "image_revisions": {
            "compose_defaults": "see compose_resolution.services['docker-compose.yml'][*].image",
            "runtime_worker_local_tags": (
                "docker-compose.yml pins local-<upstream_sha12>-<overlay_sha12> tags by default; "
                "versioned multi-arch images are an ARC-00C/ARC-08 deliverable."
            ),
            "rust_overlay_identity": rust_manifest,
        },
        "ppr00_performance_baseline": {
            "receipts": [_file_entry(rel) for rel in PPR00_RECEIPTS],
            "status": (
                "PPR-00 evidence frozen as-is (run4 is the certified recordable run per "
                "deploy/runbooks/platform-plane-restructure/loop-state.json); content is never rewritten."
            ),
        },
    }

    live_stack_required = [
        {
            "contract": "Live Gateway OpenAPI superset diff",
            "command": "uv run --all-packages --extra test pytest -q --no-cov tests/integration/test_gateway_openapi_contract.py",
            "note": "requires a running gateway (GATEWAY_BASE_URL)",
        },
        {
            "contract": "SDK SSE envelope live agreement",
            "command": "SDK_SSE_CONTRACT_REQUIRE_ALL=1 make sdk-sse-contract",
            "note": "fixture contract itself is offline; the full matrix needs the stack",
        },
        {
            "contract": "Runtime/Worker image digests from a builder",
            "command": "make agent-runtime-contract / make agent-capability-worker-build-local",
            "note": "ARC-00C decides the builder mode; digests cannot be computed offline",
        },
        {
            "contract": "Capability V2 Worker revalidation in a live Turn",
            "command": "make agent-runtime-write-gate",
            "note": "live write-path proof for the frozen contract crate",
        },
        {
            "contract": "Compose rendered resolution against .env",
            "command": "make validate-config",
            "note": "resolves ${VAR} interpolation that the offline parse keeps symbolic",
        },
    ]

    return {
        **base_envelope("contract-freeze"),
        "purpose": (
            "ARC-00A freeze of every offline-computable public contract digest. Later packages "
            "compare against these digests; a mismatch means deliberate contract change and needs "
            "the contract-delta manifest described in the PRD (AC-M04)."
        ),
        "frozen": frozen,
        "live_stack_required": live_stack_required,
    }


if __name__ == "__main__":
    from _common import write_json

    path = write_json("contract-freeze.json", build())
    print(f"wrote {path}")
