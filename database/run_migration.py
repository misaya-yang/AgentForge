"""Retired single-file migration entrypoint.

Only the canonical full-chain runners may mutate schema:

    make migrate
    python -m database.authority migrate

Executing one SQL file bypasses ordered discovery, the canonical ledger,
schema ownership, and restore-required checks, so this module always fails
closed before reading a DSN or opening a database connection.
"""

from __future__ import annotations

import sys

RETIREMENT_MESSAGE = (
    "database/run_migration.py is retired and cannot execute SQL; "
    "use `make migrate` or `python -m database.authority migrate`"
)


class RetiredMigrationRunnerError(RuntimeError):
    """A caller attempted to bypass the canonical migration chain."""


def get_dsn() -> str:
    """Fail before inspecting configuration or credentials."""
    raise RetiredMigrationRunnerError(RETIREMENT_MESSAGE)


async def run_migration(file_path: str, dsn: str) -> None:
    """Refuse the legacy programmatic single-file execution surface."""
    raise RetiredMigrationRunnerError(RETIREMENT_MESSAGE)


def main() -> int:
    print(RETIREMENT_MESSAGE, file=sys.stderr)
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
