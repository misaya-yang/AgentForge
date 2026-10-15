"""Build actual post-baseline fingerprints using a dedicated synthetic database.

Run with uv and the local .env. No running application database is changed.
The dedicated database is retained, and only non-secret evidence is written.
"""
from __future__ import annotations

import argparse
import asyncio
import json
import uuid
from dataclasses import replace
from pathlib import Path

import asyncpg
from dotenv import dotenv_values

from database.authority.bootstrap import (
    fresh_install,
    provision_extensions_admin,
    provision_roles_admin,
)
from database.authority.commands import default_paths, load_baseline
from database.authority.fingerprint import compute_fingerprints
from database.authority.manifest import load_epoch_manifest
from database.authority.runner import MigrationAuthority


async def main(*, replace_current_reference: bool = False) -> None:
    env = dotenv_values(".env")
    paths, prefix = default_paths(), "p1ref_"
    kwargs = {"host": "127.0.0.1", "port": int(env.get("POSTGRES_PORT") or 5432),
              "user": env["POSTGRES_USER"], "password": env["POSTGRES_PASSWORD"]}
    name = "runtime_reference_" + uuid.uuid4().hex[:10]
    admin = await asyncpg.connect(**kwargs, database=env.get("POSTGRES_DB") or "gateway")
    await admin.execute(f'CREATE DATABASE "{name}"')
    await admin.close()
    conn = await asyncpg.connect(**kwargs, database=name)
    try:
        await provision_roles_admin(conn, paths, prefix)
        await provision_extensions_admin(conn, paths)
        baseline, sha = load_baseline(paths, "2026_08_post_kb_v1")
        await fresh_install(conn, paths, baseline, sha, role_prefix=prefix)
        assert await compute_fingerprints(conn, role_prefix=prefix, reference_sets=baseline.reference_data) == baseline.fingerprints
        directory = paths.epoch_dir(baseline.baseline_id)
        declared = load_epoch_manifest(directory / "manifest.yml")
        authority = MigrationAuthority("unused-reference-only", paths, role_prefix=prefix)
        for sequence in (3, 4):
            manifest = replace(declared, epoch=sequence, changes=tuple(change for change in declared.changes if change.sequence <= sequence))
            await authority.apply_epoch(conn, manifest, directory)
            actual = await compute_fingerprints(conn, role_prefix=prefix, reference_sets=baseline.reference_data)
            record = {
                "baseline_id": baseline.baseline_id, "sequence": sequence, "baseline_manifest_sha256": sha,
                "changes": {str(change.sequence): change.sha256 for change in manifest.changes},
                "fingerprints": actual,
                "provenance": "Isolated fresh frozen baseline (all four hashes matched), then authority epochs through this sequence; p1ref_ service roles; PostgreSQL 16; synthetic empty reference database, no application data copied.",
            }
            output = directory / f"{sequence:03d}_verification.json"
            if output.exists() and json.loads(output.read_text())["fingerprints"] != actual and not (sequence == 4 and replace_current_reference):
                raise RuntimeError("An existing reference differs; review before replacing it")
            output.write_text(json.dumps(record, indent=2) + "\n")
        evidence = Path("tmp/runtime-durable-recovery")
        evidence.mkdir(parents=True, exist_ok=True)
        (evidence / "reference-database.json").write_text(json.dumps({"database": name, "role_prefix": prefix, "sequences": [3, 4], "frozen_baseline": "four hashes matched"}) + "\n")
        print("PASS: isolated baseline and epoch 3/4 reference fingerprints recorded")
    finally:
        await conn.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--replace-current-reference", action="store_true", help="Replace only the in-development epoch 4 reference after reviewed SQL changes; epoch 3 remains immutable.")
    asyncio.run(main(replace_current_reference=parser.parse_args().replace_current_reference))
