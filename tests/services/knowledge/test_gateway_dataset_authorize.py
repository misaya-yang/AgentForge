"""Tests for the gateway dataset-authorize contract (PRD T8.2).

The gateway resolves agent KB bindings through KS instead of reading KB
tables directly. Contract pinned here:

* DatasetService.authorize_datasets applies exactly the same effective
  permission ladder as every other KB surface (same-tenant admin role -> owner;
  created_by -> owner; dataset_permissions user/role rows; visibility
  public/tenant fallbacks), returning an order-preserving, de-duplicated
  subset and silently dropping unknown/denied ids (fail-closed);
* the internal route requires signature-bound identity headers (401 without
  them — never an empty 200), builds the UserContext from the trusted
  headers only, and treats the body's is_tenant_admin as advisory (it can
  never widen access);
* the endpoint stays closed without a verified gateway signature.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest
from fastapi import HTTPException
from knowledge_service.api.routes.eval import require_verified_gateway
from knowledge_service.api.routes.knowledge import (
    DatasetAuthorizeRequest,
    authorize_gateway_datasets,
)
from knowledge_service.auth.user_context import UserContext, get_user_context
from knowledge_service.core.exceptions import PermissionDeniedError
from knowledge_service.services.knowledge.dataset_service import DatasetService


class AuthorizeDatabase:
    def __init__(self, datasets: dict[str, dict[str, Any]]) -> None:
        self.datasets = datasets
        self.permissions: dict[tuple[str, str, str], str] = {}

    async def get_dataset(self, dataset_id: str) -> dict[str, Any] | None:
        row = self.datasets.get(dataset_id)
        return dict(row) if row else None

    async def get_dataset_permission(
        self, dataset_id: str, subject_type: str, subject_id: str
    ) -> dict[str, Any] | None:
        permission = self.permissions.get((dataset_id, subject_type, subject_id))
        return {"permission": permission} if permission else None


def _service(database: AuthorizeDatabase) -> DatasetService:
    return DatasetService(SimpleNamespace(), database)  # type: ignore[arg-type]


def _user(
    *,
    user_id: str = "user-a",
    tenant_id: str = "tenant-a",
    user_tier: str = "normal",
    roles: list[str] | None = None,
) -> UserContext:
    return UserContext(
        user_id=user_id,
        tenant_id=tenant_id,
        user_tier=user_tier,
        roles=roles or ["user"],
    )


def _datasets() -> dict[str, dict[str, Any]]:
    return {
        "ds-owned": {
            "dataset_id": "ds-owned",
            "tenant_id": "tenant-a",
            "visibility": "private",
            "created_by": "user-a",
        },
        "ds-private-other": {
            "dataset_id": "ds-private-other",
            "tenant_id": "tenant-a",
            "visibility": "private",
            "created_by": "user-b",
        },
        "ds-tenant": {
            "dataset_id": "ds-tenant",
            "tenant_id": "tenant-a",
            "visibility": "tenant",
            "created_by": "user-b",
        },
        "ds-public": {
            "dataset_id": "ds-public",
            "tenant_id": "tenant-z",
            "visibility": "public",
            "created_by": "user-b",
        },
        "ds-granted": {
            "dataset_id": "ds-granted",
            "tenant_id": "tenant-a",
            "visibility": "private",
            "created_by": "user-b",
        },
    }


@pytest.mark.asyncio
async def test_authorize_datasets_applies_the_kb_permission_ladder() -> None:
    database = AuthorizeDatabase(_datasets())
    database.permissions[("ds-granted", "user", "user-a")] = "viewer"
    service = _service(database)

    allowed = await service.authorize_datasets(
        _user(),
        [
            "ds-owned",
            "ds-private-other",
            "ds-tenant",
            "ds-public",
            "ds-granted",
            "ds-gone",
        ],
    )

    # owner-by-creator, tenant visibility, public visibility and the grant
    # all pass; the other user's private dataset and the unknown id drop out.
    assert allowed == ["ds-owned", "ds-tenant", "ds-public", "ds-granted"]


@pytest.mark.asyncio
async def test_authorize_datasets_admin_role_sees_same_tenant_private_datasets() -> None:
    database = AuthorizeDatabase(_datasets())
    service = _service(database)

    allowed = await service.authorize_datasets(
        _user(roles=["admin"]),
        ["ds-private-other", "ds-owned"],
    )

    assert allowed == ["ds-private-other", "ds-owned"]


@pytest.mark.asyncio
@pytest.mark.parametrize(
    ("user_tier", "roles"),
    [("admin", ["user"]), ("normal", ["admin"])],
)
async def test_dataset_service_rejects_cross_tenant_admin_owner_access(
    user_tier: str, roles: list[str]
) -> None:
    datasets = _datasets()
    datasets["ds-private-foreign"] = {
        "dataset_id": "ds-private-foreign",
        "tenant_id": "tenant-b",
        "visibility": "private",
        "created_by": "user-b",
    }
    service = _service(AuthorizeDatabase(datasets))
    admin = _user(user_tier=user_tier, roles=roles)

    same_tenant = await service.require_dataset_access(
        admin, "ds-private-other", required="owner"
    )
    assert same_tenant["tenant_id"] == "tenant-a"

    with pytest.raises(PermissionDeniedError):
        await service.require_dataset_access(
            admin, "ds-private-foreign", required="owner"
        )


@pytest.mark.asyncio
async def test_authorize_datasets_tenant_visibility_needs_same_tenant() -> None:
    database = AuthorizeDatabase(_datasets())
    service = _service(database)

    allowed = await service.authorize_datasets(
        _user(tenant_id="tenant-other"), ["ds-tenant", "ds-public"]
    )

    assert allowed == ["ds-public"]


@pytest.mark.asyncio
async def test_anonymous_tenant_identity_can_only_read_public_dataset() -> None:
    database = AuthorizeDatabase(_datasets())
    database.permissions[("ds-granted", "user", "agent-embed:opaque")] = "viewer"
    service = _service(database)
    anonymous = UserContext(
        user_id="agent-embed:opaque",
        tenant_id="tenant-a",
        user_tier="anonymous",
        user_type="anonymous",
        roles=["guest", "admin"],
    )

    assert not anonymous.is_authenticated
    assert await service.authorize_datasets(
        anonymous, ["ds-owned", "ds-tenant", "ds-granted", "ds-public"]
    ) == ["ds-public"]


@pytest.mark.asyncio
async def test_authorize_datasets_dedupes_and_keeps_order() -> None:
    database = AuthorizeDatabase(_datasets())
    service = _service(database)

    allowed = await service.authorize_datasets(
        _user(), ["ds-owned", "ds-owned", "", "ds-public", "ds-owned"]
    )

    assert allowed == ["ds-owned", "ds-public"]


class FakeRequest:
    def __init__(self, headers: dict[str, str]) -> None:
        self.headers = headers


def _signed_headers(**overrides: str) -> dict[str, str]:
    headers = {
        "X-User-Id": "user-a",
        "X-Tenant-Id": "tenant-a",
        "X-User-Tier": "normal",
        "X-User-Type": "user",
        "X-User-Roles": "user",
    }
    headers.update(overrides)
    return headers


@pytest.mark.asyncio
async def test_authorize_route_requires_signature_bound_identity() -> None:
    database = AuthorizeDatabase(_datasets())
    svc = SimpleNamespace(dataset_service=_service(database))
    body = DatasetAuthorizeRequest(dataset_ids=["ds-owned"])

    with pytest.raises(HTTPException) as excinfo:
        await authorize_gateway_datasets(
            FakeRequest({"X-User-Id": "user-a"}), body=body, svc=svc
        )

    assert excinfo.value.status_code == 401
    assert excinfo.value.detail["code"] == "AUTH_DENIED"

    with pytest.raises(HTTPException) as missing_type:
        await authorize_gateway_datasets(
            FakeRequest(_signed_headers(**{"X-User-Type": ""})), body=body, svc=svc
        )
    assert missing_type.value.status_code == 401


@pytest.mark.asyncio
async def test_authorize_route_and_context_keep_signed_anonymous_identity() -> None:
    database = AuthorizeDatabase(_datasets())
    svc = SimpleNamespace(dataset_service=_service(database))
    headers = _signed_headers(
        **{
            "X-User-Id": "agent-embed:opaque",
            "X-User-Tier": "anonymous",
            "X-User-Type": "anonymous",
            "X-User-Roles": "guest",
        }
    )
    request = FakeRequest(headers)
    user = await get_user_context(request)
    assert not user.is_authenticated
    response = await authorize_gateway_datasets(
        request,
        body=DatasetAuthorizeRequest(dataset_ids=["ds-tenant", "ds-public"]),
        svc=svc,
    )
    assert response.allowed_dataset_ids == ["ds-public"]


@pytest.mark.asyncio
async def test_authorize_route_returns_only_allowed_subset() -> None:
    database = AuthorizeDatabase(_datasets())
    svc = SimpleNamespace(dataset_service=_service(database))
    body = DatasetAuthorizeRequest(
        dataset_ids=["ds-owned", "ds-private-other", "ds-public"]
    )

    response = await authorize_gateway_datasets(
        FakeRequest(_signed_headers()), body=body, svc=svc
    )

    assert response.allowed_dataset_ids == ["ds-owned", "ds-public"]


@pytest.mark.asyncio
async def test_authorize_route_ignores_advisory_admin_flag() -> None:
    """is_tenant_admin comes from the request body and is never trusted:
    admin scope is granted only by the signature-bound headers."""

    database = AuthorizeDatabase(_datasets())
    svc = SimpleNamespace(dataset_service=_service(database))
    body = DatasetAuthorizeRequest(
        dataset_ids=["ds-private-other"], is_tenant_admin=True
    )

    response = await authorize_gateway_datasets(
        FakeRequest(_signed_headers()), body=body, svc=svc
    )

    assert response.allowed_dataset_ids == []

    # The same flag backed by a signed admin role does resolve.
    admin_body = DatasetAuthorizeRequest(
        dataset_ids=["ds-private-other"], is_tenant_admin=True
    )
    admin_response = await authorize_gateway_datasets(
        FakeRequest(_signed_headers(**{"X-User-Roles": "user,admin"})),
        body=admin_body,
        svc=svc,
    )
    assert admin_response.allowed_dataset_ids == ["ds-private-other"]


def test_authorize_route_stays_closed_without_verified_gateway() -> None:
    unverified = SimpleNamespace(
        state=SimpleNamespace(gateway_secret_verified=False)
    )
    with pytest.raises(HTTPException) as excinfo:
        require_verified_gateway(unverified)
    assert excinfo.value.status_code == 401

    verified = SimpleNamespace(state=SimpleNamespace(gateway_secret_verified=True))
    require_verified_gateway(verified)  # must not raise
