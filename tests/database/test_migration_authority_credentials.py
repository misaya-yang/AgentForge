from __future__ import annotations

from typing import Any

import pytest

from database.authority.credentials import (
    ALL_PRINCIPALS,
    DSN_ENV_BY_PRINCIPAL,
    AuthorityCredentialError,
    dsn_for_principal,
    role_dsns,
    verify_role_connections,
)


def _environment() -> dict[str, str]:
    return {
        env_name: f"postgresql://{principal}.example.invalid/gateway"
        for principal, env_name in DSN_ENV_BY_PRINCIPAL.items()
    }


def test_role_dsn_resolution_has_no_shared_fallback() -> None:
    environment = _environment()
    assert dsn_for_principal("migrator", environment).startswith(
        "postgresql://migrator."
    )
    environment.pop(DSN_ENV_BY_PRINCIPAL["migrator"])
    environment["DATABASE_URL"] = "postgresql://shared.example.invalid/gateway"

    with pytest.raises(AuthorityCredentialError, match="shared or inferred"):
        dsn_for_principal("migrator", environment)


def test_role_dsns_reject_credential_reuse_without_disclosing_values() -> None:
    environment = _environment()
    environment[DSN_ENV_BY_PRINCIPAL["runtime"]] = environment[
        DSN_ENV_BY_PRINCIPAL["gateway"]
    ]

    with pytest.raises(AuthorityCredentialError, match="gateway.*runtime") as exc_info:
        role_dsns(environment)

    assert "postgresql://" not in str(exc_info.value)


class FakeConnection:
    def __init__(self, principal: str) -> None:
        self.principal = principal
        self.closed = False

    async def fetchrow(self, query: str) -> dict[str, Any]:
        if "current_setting('is_superuser'" in query:
            role = "cluster_admin" if self.principal == "admin" else f"ai_gateway_{self.principal}"
            return {
                "current_user": role,
                "session_user": role,
                "database_name": "arc03_verify",
                "is_superuser": self.principal == "admin",
            }
        return {
            "rolsuper": False,
            "rolcreatedb": False,
            "rolcreaterole": False,
            "rolreplication": False,
            "rolbypassrls": False,
            "rolinherit": False,
        }

    async def close(self) -> None:
        self.closed = True


class FakeAsyncpg:
    def __init__(self) -> None:
        self.connections: list[FakeConnection] = []

    async def connect(self, dsn: str, **_kwargs: Any) -> FakeConnection:
        principal = dsn.removeprefix("postgresql://").split(".", 1)[0]
        conn = FakeConnection(principal)
        self.connections.append(conn)
        return conn


async def test_role_connection_probe_verifies_all_distinct_effective_users() -> None:
    driver = FakeAsyncpg()

    verified = await verify_role_connections(
        driver,
        _environment(),
        expected_database="arc03_verify",
    )

    assert tuple(verified) == ALL_PRINCIPALS
    assert verified["runtime"] == "ai_gateway_runtime"
    assert all(conn.closed for conn in driver.connections)


async def test_role_connection_probe_rejects_wrong_database() -> None:
    driver = FakeAsyncpg()

    with pytest.raises(AuthorityCredentialError, match="expected database"):
        await verify_role_connections(
            driver,
            _environment(),
            expected_database="arc03_other",
        )
