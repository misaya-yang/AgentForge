"""Own one isolated development Compose project; never reuse existing app data."""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
import secrets
import subprocess
import sys
import uuid
from pathlib import Path

from dotenv import dotenv_values

ROOT = Path(__file__).resolve().parents[2]
CONFIG = ROOT / ".env.dev"
PROJECT = "agentforge-dev-" + hashlib.sha256(str(ROOT).encode()).hexdigest()[:10]
SYSTEM_ENV = {key: value for key, value in os.environ.items() if key in {
    "PATH", "HOME", "TMPDIR", "LANG", "SSL_CERT_FILE", "DOCKER_HOST", "DOCKER_CONTEXT",
}}
SYSTEM_ENV["COMPOSE_PARALLEL_LIMIT"] = "1"
COMPOSE = ["docker", "compose", "--project-directory", str(ROOT), "-p", PROJECT,
           "--env-file", str(CONFIG), "-f", str(ROOT / "deploy/local/compose.yml")]


def prepare() -> dict[str, str]:
    if not CONFIG.exists():
        subprocess.run(["bash", "scripts/new/init-env.sh", "--env", str(CONFIG)],
                       cwd=ROOT, env=SYSTEM_ENV, check=True)
        values = dict(dotenv_values(CONFIG))
        values.update({
            "POSTGRES_DB": "agentforge_dev", "POSTGRES_PORT": "15432",
            "REDIS_PORT": "16379", "QDRANT_HTTP_PORT": "16333",
            "QDRANT_GRPC_PORT": "16334", "LOCAL_S3_PORT": "19000",
            "LOCAL_S3_CONSOLE_PORT": "19001", "LOCAL_S3_ACCESS_KEY": "agentforge-local",
            "LOCAL_S3_SECRET_KEY": secrets.token_hex(32),
        })
        values["GATEWAY_DATABASE__DSN"] = (
            f"postgresql://{values['POSTGRES_USER']}:{values['POSTGRES_PASSWORD']}"
            f"@127.0.0.1:15432/{values['POSTGRES_DB']}"
        )
        values["GATEWAY_REDIS__URL"] = f"redis://:{values['REDIS_PASSWORD']}@127.0.0.1:16379/0"
        for principal in ("MIGRATOR", "GATEWAY", "RUNTIME", "CAPABILITY_WORKER", "KNOWLEDGE_API", "KNOWLEDGE_WORKER"):
            values[f"LOCAL_DB_{principal}_PASSWORD"] = secrets.token_hex(32)
        CONFIG.write_text("".join(f"{key}={value or ''}\n" for key, value in values.items()))
        CONFIG.chmod(0o600)
    values = {key: value for key, value in dotenv_values(CONFIG).items() if value is not None}
    # Add new local connection inputs without rotating any existing secret.
    additions = {
        "AI_GATEWAY_DATABASE_ADMIN_DSN": values.get("AI_GATEWAY_DATABASE_ADMIN_DSN", values["GATEWAY_DATABASE__DSN"]),
        "GATEWAY_STORAGE__S3__ENDPOINT_URL": f"http://127.0.0.1:{values['LOCAL_S3_PORT']}",
        "GATEWAY_STORAGE__S3__ACCESS_KEY": values["LOCAL_S3_ACCESS_KEY"],
        "GATEWAY_STORAGE__S3__SECRET_KEY": values["LOCAL_S3_SECRET_KEY"],
        "GATEWAY_STORAGE__S3__BUCKET": "agentforge-dev-fixtures",
        "GATEWAY_STORAGE__S3__REGION": "us-east-1",
    }
    missing = {key: value for key, value in additions.items() if not values.get(key)}
    if missing:
        with CONFIG.open("a") as destination:
            destination.write("".join(f"{key}={value}\n" for key, value in missing.items()))
        values.update(missing)
    return values


def assert_owner() -> None:
    result = subprocess.run(["docker", "ps", "-aq", "--filter", f"label=com.docker.compose.project={PROJECT}"],
                            env=SYSTEM_ENV, check=True, text=True, capture_output=True)
    for container in result.stdout.split():
        data = json.loads(subprocess.check_output(["docker", "inspect", container], env=SYSTEM_ENV))[0]
        owner = data["Config"]["Labels"].get("com.docker.compose.project.working_dir")
        if owner != str(ROOT):
            raise RuntimeError("development container belongs to another checkout")


def role_environment(values: dict[str, str]) -> dict[str, str]:
    env = dict(SYSTEM_ENV)
    env["AI_GATEWAY_DATABASE_ADMIN_DSN"] = values["AI_GATEWAY_DATABASE_ADMIN_DSN"]
    for principal in ("MIGRATOR", "GATEWAY", "RUNTIME", "CAPABILITY_WORKER", "KNOWLEDGE_API", "KNOWLEDGE_WORKER"):
        env[f"AI_GATEWAY_DATABASE_{principal}_DSN"] = (
            f"postgresql://ai_gateway_{principal.lower()}:{values[f'LOCAL_DB_{principal}_PASSWORD']}"
            f"@127.0.0.1:{values['POSTGRES_PORT']}/{values['POSTGRES_DB']}"
        )
    env["GATEWAY_DATABASE__DSN"] = env["AI_GATEWAY_DATABASE_GATEWAY_DSN"]
    env["KNOWLEDGE_DATABASE__DSN"] = env["AI_GATEWAY_DATABASE_KNOWLEDGE_API_DSN"]
    return env


async def initialize(values: dict[str, str]) -> None:
    import asyncpg

    env = role_environment(values)
    subprocess.run([sys.executable, "-m", "database.authority", "provision-roles"], cwd=ROOT, env=env, check=True)
    connection = await asyncpg.connect(env["AI_GATEWAY_DATABASE_ADMIN_DSN"])
    try:
        for principal in ("MIGRATOR", "GATEWAY", "RUNTIME", "CAPABILITY_WORKER", "KNOWLEDGE_API", "KNOWLEDGE_WORKER"):
            # Fixed identifiers and freshly generated hex secrets; no user SQL input.
            await connection.execute(f"ALTER ROLE ai_gateway_{principal.lower()} PASSWORD '{values[f'LOCAL_DB_{principal}_PASSWORD']}'")
    finally:
        await connection.close()
    for command in ("verify-role-connections", "migrate", "verify"):
        subprocess.run([sys.executable, "-m", "database.authority", command], cwd=ROOT, env=env, check=True)


async def check(values: dict[str, str]) -> None:
    import asyncpg
    import boto3
    import httpx
    import redis

    connection = await asyncpg.connect(values["GATEWAY_DATABASE__DSN"])
    try:
        assert await connection.fetchval("SELECT 1") == 1
    finally:
        await connection.close()
    print("PASS PostgreSQL authenticated SELECT 1")
    cache = redis.Redis(host="127.0.0.1", port=int(values["REDIS_PORT"]), password=values["REDIS_PASSWORD"])
    key = "agentforge-dev-probe:" + uuid.uuid4().hex
    try:
        cache.set(key, "isolated-fixture", ex=60)
        assert cache.get(key) == b"isolated-fixture"
    finally:
        cache.delete(key)
        cache.close()
    print("PASS Redis authenticated set/get/delete")
    with httpx.Client(trust_env=False, timeout=15) as client:
        client.get(f"http://127.0.0.1:{values['QDRANT_HTTP_PORT']}/readyz").raise_for_status()
    print("PASS Qdrant readiness")
    store = boto3.client("s3", endpoint_url=f"http://127.0.0.1:{values['LOCAL_S3_PORT']}",
                         aws_access_key_id=values["LOCAL_S3_ACCESS_KEY"],
                         aws_secret_access_key=values["LOCAL_S3_SECRET_KEY"], region_name="us-east-1")
    bucket = "agentforge-dev-fixtures"
    if bucket not in {item["Name"] for item in store.list_buckets()["Buckets"]}:
        store.create_bucket(Bucket=bucket)
    try:
        store.put_object(Bucket=bucket, Key=key, Body=b"isolated-fixture")
        assert store.get_object(Bucket=bucket, Key=key)["Body"].read() == b"isolated-fixture"
    finally:
        store.delete_object(Bucket=bucket, Key=key)
    print("PASS local S3 authenticated put/get/delete")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("prepare", "up", "check", "init", "status", "stop"))
    action = parser.parse_args().action
    values = prepare()
    if action == "prepare":
        print(f"Prepared ignored mode-0600 configuration; project={PROJECT}")
        return
    assert_owner()
    if action == "up":
        for service in ("postgres", "redis", "qdrant", "object-store"):
            subprocess.run([*COMPOSE, "up", "-d", "--wait", "--wait-timeout", "120", service], cwd=ROOT, env=SYSTEM_ENV, check=True)
        asyncio.run(check(values))
    elif action == "init":
        asyncio.run(initialize(values))
    elif action == "check":
        asyncio.run(check(values))
    elif action == "status":
        subprocess.run([*COMPOSE, "ps"], cwd=ROOT, env=SYSTEM_ENV, check=True)
    else:
        subprocess.run([*COMPOSE, "stop"], cwd=ROOT, env=SYSTEM_ENV, check=True)


if __name__ == "__main__":
    try:
        main()
    except Exception as exc:
        # DSNs/passwords may occur in library exceptions. Preserve the safe error type only.
        print(f"Local services action failed: {type(exc).__name__}", file=sys.stderr)
        raise SystemExit(1) from None
