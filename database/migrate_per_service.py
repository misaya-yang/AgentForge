#!/usr/bin/env python
"""Retired per-service writer, retained as a thin authority wrapper.

The per-service SQL files remain immutable adoption input, but this module no
longer creates ``schema_migrations_meta`` or executes a partial service chain.
Only the complete ``database.authority`` plan may write schema or ledgers.
"""

from __future__ import annotations

import sys
from collections.abc import Iterable
from pathlib import Path

PROJECT_ROOT = Path(__file__).resolve().parent.parent
if str(PROJECT_ROOT) not in sys.path:
    sys.path.insert(0, str(PROJECT_ROOT))

from database.authority.cli import get_dsn  # noqa: E402
from database.authority.commands import command_migrate, default_paths  # noqa: E402
from database.authority.runner import (  # noqa: E402
    MigrationAuthority,
    role_prefix_from_env,
)

ROOT = Path(__file__).resolve().parent
PER_SVC = ROOT / "migrations" / "per_service"
SERVICE_ORDER = ("_global", "gateway", "assistant", "knowledge")


def _dsn() -> str:
    return get_dsn()


def _files_for(service: str) -> list[Path]:
    """Read-only compatibility inventory for frozen per-service files."""
    directory = PER_SVC / service
    if not directory.is_dir():
        return []
    return sorted(path for path in directory.glob("*.sql") if path.is_file())


async def main(argv: Iterable[str] | None = None) -> int:
    args = list(argv or [])
    if args:
        print(
            "partial --service migrations are retired; run the complete "
            "`python -m database.authority migrate` plan",
            file=sys.stderr,
        )
        return 2
    authority = MigrationAuthority(
        _dsn(),
        default_paths(),
        role_prefix=role_prefix_from_env(),
    )
    result = await command_migrate(authority)
    return result.exit_code


if __name__ == "__main__":
    import asyncio

    raise SystemExit(asyncio.run(main(sys.argv[1:])))
