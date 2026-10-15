"""Credential-storage seam for the Local Node device identity.

No production storage adapter is selected implicitly.  Native Keychain or an
equivalent OS-backed secret service must be injected by the packaged companion.
"""

from __future__ import annotations

import hashlib
import os
from enum import StrEnum
from pathlib import Path
from typing import Protocol, runtime_checkable

from .errors import PairingError


class CredentialStorageStatus(StrEnum):
    READY = "ready"
    UNAVAILABLE = "unavailable"
    TEST_ONLY_INSECURE = "test_only_insecure"


@runtime_checkable
class SecureCredentialStore(Protocol):
    """Independently configured storage for long-lived credential bytes."""

    @property
    def status(self) -> CredentialStorageStatus: ...

    def load(self, reference: str) -> bytes | None: ...

    def store(self, reference: str, secret: bytes) -> None: ...


class UnavailableSecureCredentialStore:
    """Truthful default until a native secure-store adapter is installed."""

    status = CredentialStorageStatus.UNAVAILABLE

    def load(self, reference: str) -> bytes | None:
        del reference
        raise PairingError("native secure credential storage is unavailable")

    def store(self, reference: str, secret: bytes) -> None:
        del reference, secret
        raise PairingError("native secure credential storage is unavailable")


class TestOnlyInsecureFileCredentialStore:
    """Explicit plaintext fixture for tests; never suitable for production.

    The alarming name and non-ready status are intentional.  Production code
    must not select this adapter as a fallback when native secure storage is
    missing.
    """

    status = CredentialStorageStatus.TEST_ONLY_INSECURE
    __test__ = False

    def __init__(self, directory: Path) -> None:
        self.directory = directory
        directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        os.chmod(directory, 0o700)

    def _path(self, reference: str) -> Path:
        if not reference or len(reference) > 512:
            raise PairingError("credential reference is invalid")
        name = hashlib.sha256(reference.encode("utf-8")).hexdigest() + ".test-secret"
        return self.directory / name

    def load(self, reference: str) -> bytes | None:
        path = self._path(reference)
        if not path.exists():
            return None
        if path.is_symlink():
            raise PairingError("test credential fixture cannot be a symlink")
        info = path.stat(follow_symlinks=False)
        if info.st_nlink != 1:
            raise PairingError("test credential fixture cannot be hard-linked")
        os.chmod(path, 0o600)
        return path.read_bytes()

    def store(self, reference: str, secret: bytes) -> None:
        path = self._path(reference)
        fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            view = memoryview(secret)
            while view:
                written = os.write(fd, view)
                view = view[written:]
            os.fsync(fd)
        finally:
            os.close(fd)
