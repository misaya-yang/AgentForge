#!/usr/bin/env python3
"""Print a source-build plan only. Never invoke Docker or read credentials."""
import argparse
import json
from pathlib import Path


def plan(root, sdk=False):
    required = ["harness.yml", "rust/platform-native/Cargo.lock",
                "deploy/platform-native/Dockerfile", "docker-compose.native-candidate.yml"]
    if not all((root / name).is_file() for name in required):
        raise ValueError("native candidate source markers missing")
    compose = ["docker", "compose", "--env-file", "<operator-env-file>"]
    for name in ["docker-compose.yml", "docker-compose.full.yml", "docker-compose.build.yml",
                 "docker-compose.native-candidate.yml"]:
        compose += ["-f", name]
    if sdk:
        for name in ["docker-compose.sdk-agent-worker.yml", "docker-compose.sdk-agent-worker.build.yml"]:
            compose += ["-f", name]
        compose += ["--profile", "sdk-agents"]
    services = ["migrate", "gateway", "frontend", "knowledge-service", "knowledge-worker"]
    if sdk:
        services.append("sdk-agent-worker")
    return {"mode": "plan-only", "cwd": str(root), "executed": False,
            "prerequisites": ["Read runtime-and-secrets and verify Compose ownership before any Docker action",
                              "Set unique candidate image tags and external reviewed GatewayDocument",
                              "Controlled Runtime upstream must remain a clean verified Git checkout",
                              "Review epoch 17 and scoped role evidence before any startup"],
            "commands": [["bash", "scripts/harness/build_agent_runtime_image.sh"],
                         ["bash", "scripts/harness/build_agent_capability_worker_image.sh"],
                         ["env", "COMPOSE_PARALLEL_LIMIT=1"] + compose + ["build"] + services],
            "excluded": ["startup", "migration execution", "provider calls", "publication", "live acceptance"]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", action="store_true")
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    print(json.dumps(plan(root, args.sdk), indent=2))


if __name__ == "__main__":
    main()
