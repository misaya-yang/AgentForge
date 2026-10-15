"""Command-line surface of the single migration authority.

    python -m database.authority migrate       # the only sanctioned writer
    python -m database.authority init-fresh    # empty DB -> frozen baseline
    python -m database.authority status        # read-only
    python -m database.authority verify        # read-only fingerprints
    python -m database.authority startup-check # app boot gate (read-only)
    python -m database.authority fingerprint   # print fingerprints (read-only)
    python -m database.authority provision-roles # admin-only cluster bootstrap
    python -m database.authority prepare-cutover-ownership # admin legacy owner transfer
    python -m database.authority cutover       # existing DB only; never fresh init
    python -m database.authority verify-role-connections # prove separate identities

The DSN follows the fail-closed rule of database/cli.py: DATABASE_URL or
GATEWAY_DATABASE__DSN, no default password, never printed.
"""

from __future__ import annotations

import argparse
import asyncio
import os
import sys
from pathlib import Path

from .bootstrap import provision_roles_admin
from .ledger_acl import owner_scope
from .commands import (
    command_init_fresh,
    command_bootstrap_ledger_acl,
    command_migrate,
    command_prepare_cutover_ownership,
    command_source_kind,
    command_startup_check,
    command_status,
    command_verify,
    default_paths,
    load_baseline,
)
from .constants import DEFAULT_BASELINE_ID
from .credentials import (
    ADMIN,
    APPLICATION_PRINCIPALS,
    MIGRATOR,
    AuthorityCredentialError,
    dsn_for_principal,
    verify_role_connections,
)
from .runner import MigrationAuthority, role_prefix_from_env


def get_dsn() -> str:
    """DSN from environment only; fail closed, never print it."""
    dsn = os.environ.get("DATABASE_URL") or os.environ.get("GATEWAY_DATABASE__DSN")
    if dsn:
        return dsn
    try:
        from src.config.settings import Settings

        settings = Settings()
        if getattr(settings, "database", None) and settings.database.dsn:
            return settings.database.dsn
    except Exception:  # noqa: BLE001 - fall through to the fail-closed message
        pass
    print(
        "DATABASE_URL and GATEWAY_DATABASE__DSN are not set and Settings "
        "provided no DSN; refusing to guess a database connection.",
        file=sys.stderr,
    )
    sys.exit(2)


def get_role_dsn(principal: str) -> str:
    """Resolve one explicit role DSN without legacy/shared fallbacks."""
    try:
        return dsn_for_principal(principal, os.environ)
    except AuthorityCredentialError as exc:
        print(str(exc), file=sys.stderr)
        raise SystemExit(2) from exc


async def _provision_roles_command(role_prefix: str) -> int:
    authority = MigrationAuthority(
        get_role_dsn(ADMIN),
        default_paths(),
        role_prefix=role_prefix,
    )
    conn = await authority.connect()
    try:
        await provision_roles_admin(conn, authority.paths, role_prefix)
    finally:
        await conn.close()
    return 0


async def _verify_role_connections_command(role_prefix: str) -> int:
    import asyncpg

    verified = await verify_role_connections(
        asyncpg,
        os.environ,
        role_prefix=role_prefix,
    )
    print(f"verified {len(verified)} distinct database role connections")
    return 0


async def _fingerprint_command(authority: MigrationAuthority, baseline_id: str) -> int:
    from .fingerprint import compute_fingerprints

    paths = authority.paths
    baseline, _sha = load_baseline(paths, baseline_id)
    conn = await authority.connect(read_only=True)
    try:
        async with owner_scope(conn, authority.role_prefix):
            computed = await compute_fingerprints(
                conn, role_prefix=authority.role_prefix, reference_sets=baseline.reference_data
            )
    finally:
        await conn.close()
    for name in ("structural", "acl", "extensions", "reference_data"):
        print(f"{name}: {computed[name]}")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(prog="database.authority")
    parser.add_argument(
        "command",
        choices=(
            "migrate",
            "init-fresh",
            "status",
            "source-kind",
            "verify",
            "startup-check",
            "fingerprint",
            "provision-roles",
            "bootstrap-ledger-acl",
            "prepare-cutover-ownership",
            "cutover",
            "verify-role-connections",
        ),
    )
    parser.add_argument("--baseline", default=DEFAULT_BASELINE_ID)
    parser.add_argument(
        "--principal",
        choices=APPLICATION_PRINCIPALS,
        default="gateway",
        help="application identity for startup-check",
    )
    parser.add_argument(
        "--expected-database",
        help="exact database name guard for admin ownership cutover",
    )
    parser.add_argument(
        "--no-adoption",
        action="store_true",
        help="migrate only the legacy chain; never write the adoption marker",
    )
    parser.add_argument(
        "--reconciliation-evidence-out",
        type=Path,
        default=None,
        help=(
            "explicit path for numeric-ledger reconciliation JSON; "
            "no evidence file is written by default"
        ),
    )
    args = parser.parse_args(argv)

    role_prefix = role_prefix_from_env()
    if args.command == "bootstrap-ledger-acl":
        import json
        authority = MigrationAuthority(get_role_dsn(ADMIN), default_paths(), role_prefix=role_prefix)
        print(json.dumps(asyncio.run(command_bootstrap_ledger_acl(authority, baseline_id=args.baseline)), sort_keys=True))
        return 0
    if args.command == "provision-roles":
        return asyncio.run(_provision_roles_command(role_prefix))
    if args.command == "verify-role-connections":
        return asyncio.run(_verify_role_connections_command(role_prefix))
    if args.command == "prepare-cutover-ownership":
        if not args.expected_database:
            parser.error("prepare-cutover-ownership requires --expected-database")
        authority = MigrationAuthority(
            get_role_dsn(ADMIN),
            default_paths(),
            role_prefix=role_prefix,
        )
        result = asyncio.run(
            command_prepare_cutover_ownership(
                authority,
                baseline_id=args.baseline,
                expected_database=args.expected_database,
                reconciliation_evidence_out=args.reconciliation_evidence_out,
            )
        )
        return result.exit_code

    principal = args.principal if args.command == "startup-check" else MIGRATOR
    dsn = get_role_dsn(principal)
    paths = default_paths()
    authority = MigrationAuthority(dsn, paths, role_prefix=role_prefix)

    if args.command == "migrate":
        result = asyncio.run(
            command_migrate(
                authority,
                baseline_id=args.baseline,
                allow_adoption=not args.no_adoption,
                reconciliation_evidence_out=args.reconciliation_evidence_out,
            )
        )
        return result.exit_code
    if args.command == "init-fresh":
        return asyncio.run(command_init_fresh(authority, baseline_id=args.baseline))
    if args.command == "cutover":
        result = asyncio.run(
            command_migrate(
                authority,
                baseline_id=args.baseline,
                allow_adoption=True,
                allow_fresh=False,
                reconciliation_evidence_out=args.reconciliation_evidence_out,
            )
        )
        return result.exit_code
    if args.command == "status":
        return asyncio.run(command_status(authority))
    if args.command == "source-kind":
        return asyncio.run(command_source_kind(authority))
    if args.command == "verify":
        return asyncio.run(command_verify(authority, baseline_id=args.baseline))
    if args.command == "startup-check":
        return asyncio.run(command_startup_check(authority, principal=principal))
    return asyncio.run(_fingerprint_command(authority, args.baseline))


if __name__ == "__main__":
    sys.exit(main())
