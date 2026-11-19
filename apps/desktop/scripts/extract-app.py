"""Bounded macOS updater extraction, before independent codesign/notary checks.

Only a new private directory and one .app root are allowed. Symlinks/hardlinks
are refused by this candidate format contract, so archive members cannot redirect
extraction. No shell, subprocess, service, download or signature bypass exists.
"""
import json
import os
from pathlib import Path, PurePosixPath
import sys
import tarfile


def extract_app(archive: Path, destination: Path) -> Path:
    with tarfile.open(archive, "r:gz") as source:
        members = []
        roots, names, total = set(), set(), 0
        for member in source:
            if len(members) >= 50000:
                raise ValueError("archive count refused")
            value = PurePosixPath(member.name)
            if value.is_absolute() or ".." in value.parts or "\\" in member.name or "\x00" in member.name or not value.parts:
                raise ValueError("archive path refused")
            if not (member.isfile() or member.isdir()) or member.issym() or member.islnk():
                raise ValueError("archive links and special entries refused")
            if value in names:
                raise ValueError("duplicate archive path refused")
            names.add(value)
            roots.add(value.parts[0])
            total += member.size
            if member.size < 0 or member.size > 512 * 1024 * 1024 or total > 2 * 1024 * 1024 * 1024:
                raise ValueError("archive expansion budget refused")
            members.append(member)
        if len(roots) != 1 or not next(iter(roots)).endswith(".app"):
            raise ValueError("one application root required")
        destination.mkdir(mode=0o700)  # refuse reuse or ambient extraction
        # Never call tarfile.extract: only prevalidated regular files and new
        # directories are materialized. No archived owner, device, link or
        # permission metadata is restored. Supports the system Python runtime.
        for member in members:
            target = destination.joinpath(*PurePosixPath(member.name).parts)
            target.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
            if member.isdir():
                target.mkdir(mode=0o700, exist_ok=True)
                continue
            stream = source.extractfile(member)
            if stream is None:
                raise ValueError("archive file unavailable")
            flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
            descriptor = os.open(target, flags, 0o600)
            written = 0
            with os.fdopen(descriptor, "wb") as output, stream:
                while True:
                    chunk = stream.read(1024 * 1024)
                    if not chunk:
                        break
                    written += len(chunk)
                    if written > member.size:
                        raise ValueError("archive file size changed")
                    output.write(chunk)
                if written != member.size:
                    raise ValueError("archive truncated")
            target.chmod(member.mode & 0o755)
    app = destination / next(iter(roots))
    if not (app / "Contents" / "Info.plist").is_file():
        raise ValueError("application layout refused")
    return app


if __name__ == "__main__":
    try:
        if len(sys.argv) != 3:
            raise ValueError("archive and new private output required")
        print(json.dumps({"app": str(extract_app(Path(sys.argv[1]), Path(sys.argv[2])))}))
    except Exception:
        print("Application extraction refused; no publisher acceptance is implied.", file=sys.stderr)
        sys.exit(1)
