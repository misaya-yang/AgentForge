from __future__ import annotations

from ai_gateway_core.exceptions import PermissionDeniedError


class RBAC:
    def __init__(self, role_permissions: dict[str, list[str]]):
        self.role_permissions = role_permissions

    def _matches_permission(self, permissions: set[str], permission: str) -> bool:
        if "admin:*" in permissions:
            return True
        if permission in permissions:
            return True
        parts = permission.split(":")
        for i in range(len(parts) - 1, 0, -1):
            wildcard = ":".join(parts[:i]) + ":*"
            if wildcard in permissions:
                return True
        return False

    def permissions_for_roles(self, roles: list[str]) -> set[str]:
        permissions: set[str] = set()
        for role in roles:
            permissions.update(self.role_permissions.get(role, []))
        return permissions

    def has_permission(self, roles: list[str], permission: str) -> bool:
        # If permissions are already provided in the roles list, honor them directly.
        if self._matches_permission(set(roles), permission):
            return True
        # Fall back to role -> permission mapping.
        permissions = self.permissions_for_roles(roles)
        return self._matches_permission(permissions, permission)

    def require(self, roles: list[str], permission: str) -> None:
        if not self.has_permission(roles, permission):
            raise PermissionDeniedError(f"Missing permission: {permission}")
