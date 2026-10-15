"""Descriptor-based containment for granted local directories."""

from __future__ import annotations

import os
import stat
import unicodedata
from pathlib import Path, PurePath, PureWindowsPath
from typing import BinaryIO

from .errors import PathEscapeError
from .grants import DirectoryGrant


_O_DIRECTORY = getattr(os, "O_DIRECTORY", 0)
_O_CLOEXEC = getattr(os, "O_CLOEXEC", 0)
_O_NOFOLLOW = getattr(os, "O_NOFOLLOW", 0)


class SecureWorkspace:
    """Resolve through open directory descriptors, never through ambient cwd.

    ``open_parent`` returns an owned descriptor; callers must close it.
    """

    def __init__(self, grant: DirectoryGrant) -> None:
        self.grant = grant

    _SENSITIVE_COMPONENTS = frozenset(
        {
            ".ssh",
            ".gnupg",
            ".aws",
            ".azure",
            ".kube",
            ".docker",
            "wallet",
            "wallets",
            "keychains",
        }
    )
    _SENSITIVE_NAMES = frozenset(
        {
            "id_rsa",
            "id_dsa",
            "id_ecdsa",
            "id_ed25519",
            "credentials",
            "credentials.json",
            "application_default_credentials.json",
            ".npmrc",
            ".pypirc",
            ".netrc",
            ".git-credentials",
            "pip.conf",
            "nuget.config",
            "login data",
            "cookies",
            "key4.db",
            "logins.json",
            "wallet.dat",
            "seed",
            "seed.txt",
            "seed-phrase.txt",
            "seed_phrase.txt",
            "mnemonic",
            "mnemonic.txt",
        }
    )

    @classmethod
    def assert_safe_relative(cls, relative: str | os.PathLike[str]) -> tuple[str, ...]:
        """Reject default-sensitive files even when their parent was granted.

        Reading a project directory is not authority to retrieve credentials.
        A future secret broker must use a separate capability and must never
        return secret bytes to this file API.
        """
        parts = cls._parts(relative, allow_root=True)
        lowered = tuple(part.casefold() for part in parts)
        for component in lowered:
            if component in cls._SENSITIVE_COMPONENTS:
                raise PathEscapeError("sensitive credential directory is not file-readable")
            if component in cls._SENSITIVE_NAMES:
                raise PathEscapeError("sensitive credential file is not file-readable")
            if component == ".env" or component.startswith(".env."):
                raise PathEscapeError("environment secret files are not file-readable")
            if component.endswith((".pem", ".p12", ".pfx", ".key")):
                raise PathEscapeError("private key material is not file-readable")
            if "seed-phrase" in component or "seed_phrase" in component or "mnemonic" in component:
                raise PathEscapeError("wallet recovery material is not file-readable")
        return parts

    @staticmethod
    def _parts(relative: str | os.PathLike[str], *, allow_root: bool = False) -> tuple[str, ...]:
        raw = os.fspath(relative)
        if "\x00" in raw:
            raise PathEscapeError("NUL is not allowed in a path")
        if unicodedata.normalize("NFC", raw) != raw:
            raise PathEscapeError("path must use canonical NFC form")
        if "\\" in raw:
            raise PathEscapeError("cross-platform path separators are not allowed")
        path = PurePath(raw)
        windows_path = PureWindowsPath(raw)
        if path.is_absolute() or windows_path.is_absolute() or windows_path.drive:
            raise PathEscapeError("absolute paths are not allowed")
        parts = path.parts
        if (not parts or parts == (".",)) and allow_root:
            return ()
        if not parts or any(part in {"", ".", ".."} for part in parts):
            raise PathEscapeError("path contains an unsafe component")
        return tuple(parts)

    def _root_fd(self) -> int:
        fd = os.open(self.grant.root, os.O_RDONLY | _O_DIRECTORY | _O_CLOEXEC | _O_NOFOLLOW)
        current = os.fstat(fd)
        if (current.st_dev, current.st_ino) != (self.grant.root_device, self.grant.root_inode):
            os.close(fd)
            raise PathEscapeError("grant root changed")
        return fd

    def open_parent(self, relative: str | os.PathLike[str]) -> tuple[int, str]:
        parts = self.assert_safe_relative(relative)
        if not parts:
            raise PathEscapeError("a file path is required")
        fd = self._root_fd()
        try:
            for component in parts[:-1]:
                next_fd = os.open(
                    component,
                    os.O_RDONLY | _O_DIRECTORY | _O_CLOEXEC | _O_NOFOLLOW,
                    dir_fd=fd,
                )
                os.close(fd)
                fd = next_fd
            return fd, parts[-1]
        except (OSError, ValueError) as exc:
            os.close(fd)
            raise PathEscapeError("path escapes or traverses a non-directory") from exc

    def open_dir(self, relative: str | os.PathLike[str] = ".") -> int:
        parts = self.assert_safe_relative(relative)
        fd = self._root_fd()
        try:
            for component in parts:
                next_fd = os.open(
                    component,
                    os.O_RDONLY | _O_DIRECTORY | _O_CLOEXEC | _O_NOFOLLOW,
                    dir_fd=fd,
                )
                os.close(fd)
                fd = next_fd
            return fd
        except OSError as exc:
            os.close(fd)
            raise PathEscapeError("directory is outside the grant or contains a symlink") from exc

    def open_read(self, relative: str | os.PathLike[str]) -> BinaryIO:
        parent_fd, name = self.open_parent(relative)
        try:
            fd = os.open(name, os.O_RDONLY | _O_CLOEXEC | _O_NOFOLLOW, dir_fd=parent_fd)
        except FileNotFoundError:
            raise
        except OSError as exc:
            raise PathEscapeError("file is unavailable or is a symlink") from exc
        finally:
            os.close(parent_fd)
        info = os.fstat(fd)
        if not stat.S_ISREG(info.st_mode) or info.st_nlink != 1:
            os.close(fd)
            raise PathEscapeError("only single-link regular files may be read")
        return os.fdopen(fd, "rb", closefd=True)

    def resolve(self, relative: str | os.PathLike[str], *, allow_missing: bool = False) -> Path:
        """Return a checked display/path value; security-sensitive I/O uses descriptors."""
        parts = self.assert_safe_relative(relative)
        if not parts:
            return self.grant.root
        parent_fd, name = self.open_parent(relative)
        try:
            try:
                info = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
            except FileNotFoundError:
                if allow_missing:
                    return self.grant.root.joinpath(*parts)
                raise
            if stat.S_ISLNK(info.st_mode):
                raise PathEscapeError("symlinks are outside the file capability")
            if stat.S_ISREG(info.st_mode) and info.st_nlink != 1:
                raise PathEscapeError("hard-linked files are outside the file capability")
            return self.grant.root.joinpath(*parts)
        finally:
            os.close(parent_fd)
