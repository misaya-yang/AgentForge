#!/usr/bin/env python3
"""Offline explicit-env exporter. No dotenv discovery, service calls or secret output."""
import argparse
import json
import os
import stat
import sys
from pathlib import Path
from urllib.parse import urlsplit

ROOT = Path(__file__).resolve().parents[2]


def read_object(path):
    limit = 4 * 1024 * 1024
    # Open nonblocking so a FIFO cannot hang before the descriptor type check.
    # Refuse symlinks rather than follow an unexpected credential destination.
    descriptor = os.open(path, os.O_RDONLY | os.O_NONBLOCK | os.O_NOFOLLOW)
    with os.fdopen(descriptor, "rb") as stream:
        if not stat.S_ISREG(os.fstat(stream.fileno()).st_mode):
            raise ValueError("regular input file required")
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError("input too large")
    value = json.loads(data)
    if not isinstance(value, dict):
        raise ValueError("object required")
    return value


def validate_topology(settings, environment, native):
    def require(condition):
        if not condition:
            raise ValueError("candidate topology configuration mismatch")
    require(environment.get("AI_PLATFORM_TOPOLOGY_MODE") == "full")
    require(settings.get("host") == "0.0.0.0" and settings.get("port") == 8080)
    for section, host, port in [("database", "postgres", 5432), ("redis", "redis", 6379)]:
        value = settings.get(section, {})
        address = urlsplit(value.get("dsn" if section == "database" else "url", ""))
        require(value.get("enabled") is True and address.hostname == host and address.port == port)
    require(settings["database"].get("auto_init") is False)
    require(environment.get("KB_SERVICE_URL") == "http://knowledge-service:8092")
    require(environment.get("AI_PLATFORM_AGENT_RUNTIME_URL") == "http://agent-runtime:8094")
    require(environment.get("AI_PLATFORM_CAPABILITY_WORKER_URL") == "http://agent-capability-worker:8095")
    require(native.get("allow_private_knowledge_http") is True)
    require(len(environment.get("AI_PLATFORM_INTERNAL_TOKEN", "")) >= 16)
    require(bool(native.get("kernel_revision")) and "<" not in native["kernel_revision"])
    if "SDK_AGENT_WORKER_URL" in environment:
        require(environment["SDK_AGENT_WORKER_URL"] == "http://127.0.0.1:8096")
        require(bool(environment.get("SDK_AGENT_WORKER_TOKEN")))
    require(native.get("local_channel") == "disabled")
    require(native.get("confluence_registration") == "disabled")


def prepare(environment, native):
    if not all(isinstance(k, str) and isinstance(v, str) for k, v in environment.items()):
        raise ValueError("explicit environment must map strings to strings")
    # Only the operator-provided map participates, including settings validators
    # which access os.environ directly. Disable BaseSettings dotenv discovery.
    previous = dict(os.environ)
    try:
        os.environ.clear()
        os.environ.update(environment)
        sys.path.insert(0, str(ROOT))
        from src.config.settings import Settings
        settings = Settings(_env_file=None).model_dump(mode="json")
    finally:
        os.environ.clear()
        os.environ.update(previous)
    if "<" in json.dumps(native):
        raise ValueError("replace all native option placeholders")
    validate_topology(settings, environment, native)
    return {"settings": settings, "environment": environment, "native": native}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--environment-json", type=Path, required=True)
    parser.add_argument("--native-json", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    try:
        output = args.output.resolve()
        if output.is_relative_to(ROOT):
            raise ValueError("output must be outside source tree")
        document = prepare(read_object(args.environment_json), read_object(args.native_json))
        payload = json.dumps(document).encode()
        if len(payload) > 4 * 1024 * 1024:
            raise ValueError("output too large")
        # Exclusive creation refuses overwrites and symlink destinations.
        descriptor = os.open(output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        with os.fdopen(descriptor, "wb") as stream:
            stream.write(payload)
    except Exception:
        # Pydantic and URL errors may embed credentials; never print exceptions.
        print("Configuration preparation failed; check explicit inputs, topology, dependencies and external output path. No input values are logged.", file=sys.stderr)
        return 1
    print("Private configuration file created. Run the native binary's --check-config-stdin before runtime approval; this export is not readiness evidence.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
