"""Fail-closed DSN ownership for database admin, migrator and applications."""

from __future__ import annotations

import re
from collections.abc import Mapping
from typing import Any

from .constants import DEFAULT_ROLE_PREFIX

ADMIN = "admin"
MIGRATOR = "migrator"
APPLICATION_PRINCIPALS = (
    "gateway",
    "runtime",
    "capability_worker",
    "knowledge_api",
    "knowledge_worker",
)
ALL_PRINCIPALS = (ADMIN, MIGRATOR, *APPLICATION_PRINCIPALS)

DSN_ENV_BY_PRINCIPAL = {
    ADMIN: "AI_GATEWAY_DATABASE_ADMIN_DSN",
    MIGRATOR: "AI_GATEWAY_DATABASE_MIGRATOR_DSN",
    "gateway": "AI_GATEWAY_DATABASE_GATEWAY_DSN",
    "runtime": "AI_GATEWAY_DATABASE_RUNTIME_DSN",
    "capability_worker": "AI_GATEWAY_DATABASE_CAPABILITY_WORKER_DSN",
    "knowledge_api": "AI_GATEWAY_DATABASE_KNOWLEDGE_API_DSN",
    "knowledge_worker": "AI_GATEWAY_DATABASE_KNOWLEDGE_WORKER_DSN",
}


class AuthorityCredentialError(RuntimeError):
    """A required role-specific connection was absent or impersonated another role."""


def dsn_for_principal(
    principal: str,
    environ: Mapping[str, str],
) -> str:
    env_name = DSN_ENV_BY_PRINCIPAL.get(principal)
    if env_name is None:
        raise AuthorityCredentialError(f"unknown database principal {principal!r}")
    dsn = environ.get(env_name)
    if not dsn:
        raise AuthorityCredentialError(
            f"{env_name} is required; shared or inferred database credentials are forbidden"
        )
    return dsn


def role_dsns(environ: Mapping[str, str]) -> dict[str, str]:
    """Resolve every role DSN and reject credential reuse without revealing values."""
    resolved = {
        principal: dsn_for_principal(principal, environ)
        for principal in ALL_PRINCIPALS
    }
    by_value: dict[str, list[str]] = {}
    for principal, dsn in resolved.items():
        by_value.setdefault(dsn, []).append(principal)
    reused = [sorted(principals) for principals in by_value.values() if len(principals) > 1]
    if reused:
        raise AuthorityCredentialError(
            f"database role DSNs must be distinct; reused principal groups={sorted(reused)}"
        )
    return resolved


def _role_prefix(prefix: str) -> str:
    if re.fullmatch(r"[a-z][a-z0-9_]{0,20}_", prefix) is None:
        raise AuthorityCredentialError(f"unsafe role prefix {prefix!r}")
    return prefix


async def verify_role_connections(
    asyncpg_module: Any,
    environ: Mapping[str, str],
    *,
    role_prefix: str = DEFAULT_ROLE_PREFIX,
    expected_database: str | None = None,
) -> dict[str, str]:
    """Connect through every DSN and prove its effective least-privilege identity."""
    prefix = _role_prefix(role_prefix)
    dsns = role_dsns(environ)
    verified: dict[str, str] = {}
    for principal in ALL_PRINCIPALS:
        conn = await asyncpg_module.connect(
            dsns[principal],
            server_settings={"application_name": f"ai_gateway_role_probe_{principal}"},
        )
        try:
            row = await conn.fetchrow(
                "SELECT current_user, session_user, "
                "current_database() AS database_name, "
                "current_setting('is_superuser', true) = 'on' AS is_superuser"
            )
            current_user = str(row["current_user"])
            session_user = str(row["session_user"])
            if current_user != session_user:
                raise AuthorityCredentialError(
                    f"{principal} DSN starts under an implicit SET ROLE identity"
                )
            if expected_database is not None and str(row["database_name"]) != expected_database:
                raise AuthorityCredentialError(
                    f"{principal} DSN does not target the expected database"
                )
            if principal == ADMIN:
                if not bool(row["is_superuser"]):
                    raise AuthorityCredentialError("admin DSN is not a PostgreSQL admin")
            else:
                expected = f"{prefix}{principal}"
                if current_user != expected:
                    raise AuthorityCredentialError(
                        f"{principal} DSN authenticates as {current_user!r}, expected {expected!r}"
                    )
                attributes = await conn.fetchrow(
                    "SELECT rolsuper, rolcreatedb, rolcreaterole, rolreplication, "
                    "rolbypassrls, rolinherit FROM pg_roles WHERE rolname = current_user"
                )
                forbidden = (
                    "rolsuper",
                    "rolcreatedb",
                    "rolcreaterole",
                    "rolreplication",
                    "rolbypassrls",
                    "rolinherit",
                )
                if attributes is None or any(bool(attributes[field]) for field in forbidden):
                    raise AuthorityCredentialError(
                        f"{principal} role has a forbidden cluster or inheritance attribute"
                    )
            verified[principal] = current_user
        finally:
            await conn.close()
    return verified
