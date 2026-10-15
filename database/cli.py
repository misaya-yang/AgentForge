#!/usr/bin/env python
"""Compatibility CLI for the single PostgreSQL migration authority.

New automation should call ``python -m database.authority`` directly. This
module keeps the historical ``ai-gateway-db`` and ``database/cli.py`` command
names, but owns no connection, SQL execution, transaction, or legacy-ledger
write path. Every supported operation delegates to ``database.authority``.
"""

from __future__ import annotations

import hashlib
import re
import sys
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from database.authority import cli as authority_cli  # noqa: E402
from database.authority.commands import (  # noqa: E402
    command_init_fresh,
    command_migrate,
    command_startup_check,
    command_status,
    default_paths,
)
from database.authority.constants import (  # noqa: E402
    MIGRATION_ADVISORY_LOCK_NAMESPACE as MIGRATION_ADVISORY_LOCK_NAMESPACE,
)
from database.authority.discovery import HISTORICAL_FILENAME_DUPLICATES  # noqa: E402
from database.authority.runner import (  # noqa: E402
    MigrationAuthority,
    role_prefix_from_env,
)

DATABASE_DIR = Path(__file__).parent
MIGRATIONS_DIR = DATABASE_DIR / "migrations"
MIGRATION_PATTERN = re.compile(r"^(\d{3})_(.+)\.sql$")
ROLLBACK_SUFFIX = "_rollback.sql"


class MigrationChainError(RuntimeError):
    """A retired partial-chain request or ambiguous legacy chain was refused."""


def get_dsn() -> str:
    """Use the authority's explicit migrator credential only."""
    return authority_cli.get_role_dsn("migrator")


def mask_dsn(dsn: str) -> str:
    """Mask password-bearing userinfo for legacy display callers."""
    scheme_sep = dsn.find("://")
    if scheme_sep == -1:
        return dsn
    userinfo_start = scheme_sep + 3
    at = dsn.rfind("@")
    if at <= userinfo_start:
        return dsn
    colon = dsn.find(":", userinfo_start)
    if colon == -1 or colon > at:
        return dsn
    return f"{dsn[: colon + 1]}******{dsn[at:]}"


def compute_checksum(content: str) -> str:
    """Historical read-only helper retained for downstream inventory tests."""
    return hashlib.sha256(content.encode("utf-8")).hexdigest()[:16]


def discover_migrations(
    migrations_dir: Path = MIGRATIONS_DIR,
) -> list[tuple[str, str, Path]]:
    """Read legacy forward filenames without granting an execution surface."""
    if not migrations_dir.exists():
        return []
    discovered: list[tuple[str, str, Path]] = []
    for file_path in sorted(migrations_dir.glob("*.sql")):
        if file_path.name.endswith(ROLLBACK_SUFFIX):
            continue
        match = MIGRATION_PATTERN.fullmatch(file_path.name)
        if match:
            discovered.append(
                (
                    match.group(1),
                    match.group(2).replace("_", " ").title(),
                    file_path,
                )
            )
    return discovered


def validate_migration_chain(
    migrations: list[tuple[str, str, Path]],
    *,
    allow_historical_filename_duplicates: bool,
) -> None:
    """Read-only compatibility validation; execution remains authority-only."""
    files_by_version: dict[str, set[str]] = {}
    for version, _description, file_path in migrations:
        files_by_version.setdefault(version, set()).add(file_path.name)
    for version, filenames in sorted(files_by_version.items()):
        if len(filenames) == 1:
            continue
        if allow_historical_filename_duplicates and frozenset(
            filenames
        ) == HISTORICAL_FILENAME_DUPLICATES.get(version):
            continue
        raise MigrationChainError(
            f"duplicate migration version {version}: {', '.join(sorted(filenames))}; "
            "the single authority must reconcile the chain before execution"
        )


def _authority() -> MigrationAuthority:
    return MigrationAuthority(
        get_dsn(),
        default_paths(),
        role_prefix=role_prefix_from_env(),
    )


async def cmd_init() -> int:
    """Compatibility alias for the authority's empty-database initializer."""
    return await command_init_fresh(_authority())


async def cmd_migrate(target_version: str | None = None) -> int:
    """Delegate the complete immutable chain; selective versions are retired."""
    if target_version is not None:
        raise MigrationChainError(
            "selective migration versions are retired; run the complete "
            "`python -m database.authority migrate` plan"
        )
    result = await command_migrate(_authority())
    return result.exit_code


async def cmd_status() -> int:
    """Delegate to the authority's read-only status surface."""
    return await command_status(_authority())


async def cmd_check() -> int:
    """Compatibility alias for the authority's read-only startup gate."""
    return await command_startup_check(_authority())


async def cmd_reset() -> int:
    """The destructive legacy reset path is intentionally unavailable."""
    raise MigrationChainError(
        "database reset is not a migration operation and is retired; restore an "
        "approved backup into an isolated replacement database instead"
    )


def main(argv: list[str] | None = None) -> int:
    """Translate legacy command names, then use the public authority CLI."""
    args = list(sys.argv[1:] if argv is None else argv)
    if not args or args[0] in {"-h", "--help", "help"}:
        return authority_cli.main(["--help"])

    command, *rest = args
    translated = {
        "init": "init-fresh",
        "migrate": "migrate",
        "status": "status",
        "check": "startup-check",
    }.get(command)
    if command == "reset":
        print(
            "database reset is retired; use an approved isolated restore workflow",
            file=sys.stderr,
        )
        return 2
    if translated is None:
        print(f"unknown database command: {command}", file=sys.stderr)
        return 2
    if command == "migrate" and rest and re.fullmatch(r"\d{1,3}", rest[0]):
        print(
            "selective migration versions are retired; run the complete authority plan",
            file=sys.stderr,
        )
        return 2
    return authority_cli.main([translated, *rest])


if __name__ == "__main__":
    raise SystemExit(main())
