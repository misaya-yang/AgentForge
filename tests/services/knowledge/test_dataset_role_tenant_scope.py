"""Role names are tenant-local; explicit user/public sharing remains valid."""

from types import SimpleNamespace

import pytest
from knowledge_service.auth.user_context import UserContext
from knowledge_service.core.exceptions import PermissionDeniedError
from knowledge_service.services.knowledge.dataset_service import DatasetService


class PermissionStore:
    def __init__(self, permission, *, visibility="private", explicit_user=False):
        self.permission = permission
        self.explicit_user = explicit_user
        self.dataset = {
            "dataset_id": "dataset-a", "tenant_id": "tenant-a",
            "created_by": "owner-a", "visibility": visibility,
        }
        self.lookups = []

    async def get_dataset(self, dataset_id):
        assert dataset_id == "dataset-a"
        return self.dataset.copy()

    async def get_dataset_permission(self, dataset_id, subject_type, subject_id):
        self.lookups.append((dataset_id, subject_type, subject_id))
        if subject_type == "role" and subject_id == "analyst":
            return {"permission": self.permission}
        if self.explicit_user and subject_type == "user" and subject_id == "user-b":
            return {"permission": "viewer"}
        return None


@pytest.mark.parametrize("permission", ["viewer", "editor", "owner"])
async def test_foreign_same_named_role_cannot_read_or_authorize_dataset(permission):
    store = PermissionStore(permission)
    service = DatasetService(SimpleNamespace(), store)
    user = UserContext(user_id="user-b", tenant_id="tenant-b", roles=["analyst"])

    with pytest.raises(PermissionDeniedError):
        await service.require_dataset_access(user, "dataset-a", required="viewer")
    assert await service.authorize_datasets(user, ["dataset-a"]) == []
    assert not any(kind == "role" for _, kind, _ in store.lookups)


@pytest.mark.parametrize("permission", ["viewer", "editor", "owner"])
async def test_same_tenant_role_retains_its_permission(permission):
    store = PermissionStore(permission)
    service = DatasetService(SimpleNamespace(), store)
    user = UserContext(user_id="user-b", tenant_id="tenant-a", roles=["analyst"])
    assert await service.require_dataset_access(user, "dataset-a", required=permission) == store.dataset


@pytest.mark.parametrize("visibility,explicit_user", [("public", False), ("private", True)])
async def test_explicit_cross_tenant_sharing_does_not_inherit_role_owner(visibility, explicit_user):
    store = PermissionStore("owner", visibility=visibility, explicit_user=explicit_user)
    service = DatasetService(SimpleNamespace(), store)
    user = UserContext(user_id="user-b", tenant_id="tenant-b", roles=["analyst"])
    assert await service._effective_dataset_permission(store.dataset, user) == "viewer"
    with pytest.raises(PermissionDeniedError):
        await service.require_dataset_access(user, "dataset-a", required="editor")
