"""Explicit, revocable directory grants.

A grant is a local authority object, not a path supplied by the model.
"""

from __future__ import annotations

import os
import secrets
import stat
import time
import re
from dataclasses import dataclass
from pathlib import Path

from .errors import CapabilityDenied, PathEscapeError


FILE_CAPABILITIES = frozenset({"list", "read", "search", "watch", "write", "rollback"})


@dataclass(frozen=True, slots=True)
class DirectoryGrant:
    grant_id: str
    root: Path
    capabilities: frozenset[str]
    tenant_id: str
    user_id: str
    issued_at: float
    expires_at: float | None
    root_device: int
    root_inode: int


class DirectoryGrantStore:
    def __init__(self) -> None:
        self._grants: dict[str, DirectoryGrant] = {}
        self._revoked: set[str] = set()

    def issue(
        self,
        root: Path,
        capabilities: frozenset[str],
        ttl_seconds: int | None = None,
        *,
        tenant_id: str = "local",
        user_id: str = "local",
        grant_id: str | None = None,
    ) -> DirectoryGrant:
        supplied = Path(root).expanduser()
        if not supplied.is_absolute():
            raise PathEscapeError("grant root must be absolute")
        try:
            supplied_stat = supplied.lstat()
        except FileNotFoundError as exc:
            raise PathEscapeError("grant root does not exist") from exc
        if stat.S_ISLNK(supplied_stat.st_mode):
            raise PathEscapeError("grant root cannot be a symlink")
        if not stat.S_ISDIR(supplied_stat.st_mode):
            raise PathEscapeError("grant root must be a directory")
        unknown = set(capabilities) - FILE_CAPABILITIES
        if unknown or not capabilities:
            raise CapabilityDenied(f"unsupported or empty grant capabilities: {sorted(unknown)}")
        if "rollback" in capabilities and "write" not in capabilities:
            raise CapabilityDenied("rollback cannot be granted without file write authority")
        # Rollback is a recovery operation beneath the platform's file.write
        # capability, not a separately grantable platform authority.
        normalized_capabilities = set(capabilities)
        if "write" in normalized_capabilities:
            normalized_capabilities.add("rollback")
        if ttl_seconds is not None and ttl_seconds <= 0:
            raise CapabilityDenied("grant lifetime must be positive")
        root_path = supplied.resolve(strict=True)
        now = time.time()
        selected_grant_id = grant_id or "grant_" + secrets.token_urlsafe(18)
        if (
            re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._:-]{0,199}", selected_grant_id) is None
            or selected_grant_id in self._grants
        ):
            raise CapabilityDenied("directory grant id is invalid or already in use")
        grant = DirectoryGrant(
            grant_id=selected_grant_id,
            root=root_path,
            capabilities=frozenset(normalized_capabilities),
            tenant_id=tenant_id,
            user_id=user_id,
            issued_at=now,
            expires_at=None if ttl_seconds is None else now + ttl_seconds,
            root_device=supplied_stat.st_dev,
            root_inode=supplied_stat.st_ino,
        )
        self._grants[grant.grant_id] = grant
        return grant

    def get(
        self,
        grant_id: str,
        capability: str,
        *,
        tenant_id: str | None = None,
        user_id: str | None = None,
    ) -> DirectoryGrant:
        grant = self._grants.get(grant_id)
        if grant is None or grant_id in self._revoked:
            raise CapabilityDenied("directory grant unavailable")
        if grant.expires_at is not None and time.time() >= grant.expires_at:
            raise CapabilityDenied("directory grant expired")
        if capability not in grant.capabilities:
            raise CapabilityDenied(f"directory grant does not allow {capability}")
        if tenant_id is not None and tenant_id != grant.tenant_id:
            raise CapabilityDenied("directory grant belongs to another tenant")
        if user_id is not None and user_id != grant.user_id:
            raise CapabilityDenied("directory grant belongs to another user")
        try:
            current = os.stat(grant.root, follow_symlinks=False)
        except OSError as exc:
            raise CapabilityDenied("directory grant root is unavailable") from exc
        if (current.st_dev, current.st_ino) != (grant.root_device, grant.root_inode):
            raise CapabilityDenied("directory grant root identity changed")
        return grant

    def revoke(self, grant_id: str) -> None:
        if grant_id in self._grants:
            self._revoked.add(grant_id)

    def active(self) -> tuple[DirectoryGrant, ...]:
        result: list[DirectoryGrant] = []
        for grant_id, grant in self._grants.items():
            try:
                self.get(grant_id, next(iter(grant.capabilities)))
            except CapabilityDenied:
                continue
            result.append(grant)
        return tuple(result)
