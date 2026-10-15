#!/usr/bin/env python3
"""Create native desktop candidates from a clean, already-built source revision.

--matrix reads target metadata only. --build-env emits non-secret build identity
and native compilation options for the selected target. Packaging verifies the
supplied and extracted executable offline. It does not build, download, sign,
notarize, publish, open a GUI, install anything or read credentials.
"""
from __future__ import annotations

import argparse
import io
import json
from pathlib import Path
import plistlib
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import zipfile

from package_platformctl import archive, binary_machine, digest

ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = Path("rust/platform-native")
TARGETS = Path("deploy/release/desktop-targets.json")
DESKTOP = WORKSPACE / "crates/platform-desktop"
CLIENT = WORKSPACE / "crates/platform-client"
DISTRIBUTION = Path("docs/architecture/native-desktop-distribution.md")
MAX_BINARY_BYTES = 256 * 1024 * 1024


def target_manifest() -> dict:
    value = json.loads((ROOT / TARGETS).read_text(encoding="utf-8"))
    if value.get("schema_version") != "ai-platform-desktop/build-targets/v1":
        raise ValueError("Unsupported desktop target manifest")
    expected = {"linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64", "win32-x64"}
    targets = value["targets"]
    if len(targets) != len(expected) or {item["name"] for item in targets} != expected:
        raise ValueError("Desktop target manifest must contain five distinct native targets")
    for target in targets:
        windows = target["os"] == "windows"
        if target["binary"] != ("ai-platform-desktop.exe" if windows else "ai-platform-desktop"):
            raise ValueError("Unexpected desktop binary name")
        if target["archive"] != ("zip" if windows else "tar.gz"):
            raise ValueError("Unexpected desktop archive format")
    return value


def git(*args: str) -> str:
    return subprocess.check_output(["git", "-C", str(ROOT), *args], text=True).strip()


def source_materials() -> dict[str, bytes]:
    paths = {
        Path("LICENSE"), Path("RELEASE.md"), WORKSPACE / "Cargo.toml",
        WORKSPACE / "Cargo.lock", WORKSPACE / "README.md", TARGETS, DISTRIBUTION,
        Path("docs/architecture/native-client-capability-matrix.md"),
        Path("scripts/harness/package_desktop.py"),
        Path("scripts/harness/package_platformctl.py"),
        Path(".github/workflows/native-desktop.yml"),
    }
    paths.update(path.relative_to(ROOT) for path in (ROOT / WORKSPACE / "crates").glob("*/Cargo.toml"))
    for crate in (CLIENT, DESKTOP):
        paths.update(path.relative_to(ROOT) for path in (ROOT / crate / "src").rglob("*.rs"))
        build = ROOT / crate / "build.rs"
        if build.exists():
            paths.add(build.relative_to(ROOT))
    source = {}
    for path in sorted(paths):
        absolute = ROOT / path
        if absolute.is_symlink() or not absolute.is_file() or ROOT not in absolute.resolve().parents:
            raise ValueError(f"Source material must be a repository-local regular file: {path}")
        source[path.as_posix()] = absolute.read_bytes()
    tracked = set(git("ls-files", "--", *source).splitlines())
    if tracked != set(source):
        raise ValueError("Every package source material must be tracked and committed")
    if git("status", "--porcelain", "--untracked-files=all", "--", *source, str(CLIENT), str(DESKTOP)):
        raise ValueError("Desktop package inputs must be committed and clean")
    return source


def source_identity(source: dict[str, bytes]) -> tuple[str, str, str, str]:
    sha = git("rev-parse", "HEAD")
    if not re.fullmatch(r"[0-9a-f]{40,64}", sha):
        raise ValueError("Invalid source commit identity")
    workspace = tomllib.loads(source[(WORKSPACE / "Cargo.toml").as_posix()].decode())
    version = workspace["workspace"]["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?", version):
        raise ValueError("Invalid native workspace version")
    material_hashes = {path: digest(value) for path, value in sorted(source.items())}
    fingerprint = digest(json.dumps(material_hashes, sort_keys=True, separators=(",", ":")).encode())
    return sha, fingerprint, version, workspace["workspace"]["package"]["license"]


def build_environment(target: dict, sha: str, fingerprint: str) -> dict[str, str]:
    values = {
        "AI_PLATFORM_DESKTOP_SOURCE_COMMIT": sha,
        "AI_PLATFORM_DESKTOP_SOURCE_DIGEST": fingerprint,
        "AI_PLATFORM_DESKTOP_RUST_TARGET": target["rust_target"],
    }
    if target["os"] == "macos":
        values["MACOSX_DEPLOYMENT_TARGET"] = target["macos_minimum"]
    if target["os"] == "windows":
        # Avoid an undeclared redistributable installer in the portable candidate.
        values["CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS"] = "-C target-feature=+crt-static"
    return values


def smoke(binary: Path, expected: dict) -> None:
    # This entrypoint exits before GUI/runtime initialization. A successful result
    # proves native process loading and identity, not window rendering or login.
    result = subprocess.run([str(binary), "--package-info"], capture_output=True, timeout=20, check=False)
    if result.returncode != 0 or len(result.stdout) > 8192:
        raise ValueError("Desktop offline identity smoke failed")
    try:
        actual = json.loads(result.stdout)
    except (ValueError, UnicodeError) as error:
        raise ValueError("Desktop returned invalid offline identity") from error
    if actual != expected:
        raise ValueError("Desktop binary identity does not match current version, source and target")
    help_result = subprocess.run([str(binary), "--help"], capture_output=True, timeout=20, check=False)
    if help_result.returncode != 0 or b"Usage: ai-platform-desktop" not in help_result.stdout:
        raise ValueError("Desktop offline help smoke failed")


def verify_archive(payload: bytes, name: str, files: dict[str, bytes], binary_name: str,
                   kind: str, expected: dict, temporary_parent: Path) -> None:
    # Extract only the exact generated member set, with no extractall or links.
    # Keep the real .app tree so the packaged executable is exercised in place.
    with tempfile.TemporaryDirectory(prefix="desktop-extraction-", dir=temporary_parent) as directory:
        root = Path(directory)
        if kind == "zip":
            with zipfile.ZipFile(io.BytesIO(payload)) as packed:
                names = packed.namelist()
                extracted = [(item.filename, packed.read(item), item.external_attr >> 16)
                             for item in packed.infolist()]
        else:
            with tarfile.open(fileobj=io.BytesIO(payload), mode="r:gz") as packed:
                members = packed.getmembers()
                if any(not member.isfile() for member in members):
                    raise ValueError("Desktop archive contains a non-regular member")
                names = [member.name for member in members]
                extracted = [(member.name, packed.extractfile(member).read(), member.mode)
                             for member in members]
        expected_names = {f"{name}/{path}" for path in files}
        if len(names) != len(expected_names) or set(names) != expected_names:
            raise ValueError("Desktop archive member set mismatch")
        for member_name, data, mode in extracted:
            relative = member_name[len(name) + 1:]
            if data != files[relative]:
                raise ValueError("Desktop archive content mismatch")
            desired_mode = 0o755 if relative == binary_name else 0o644
            if mode & 0o777 != desired_mode:
                raise ValueError("Desktop archive permission mismatch")
            path = root / member_name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
            path.chmod(desired_mode)
        smoke(root / name / binary_name, expected)


def package(args: argparse.Namespace, manifest: dict, target: dict) -> None:
    if not args.binary or not args.output:
        raise ValueError("--binary and --output are required")
    output = Path(args.output).resolve()
    if output == ROOT or ROOT in output.parents:
        raise ValueError("Package output must be outside the repository")
    supplied = Path(args.binary)
    if supplied.is_symlink():
        raise ValueError("Desktop package binary must not be a symlink")
    binary = supplied.resolve(strict=True)
    if not binary.is_file() or binary.name != target["binary"]:
        raise ValueError("Desktop binary filename does not match target")
    if binary.stat().st_size > MAX_BINARY_BYTES:
        raise ValueError("Desktop binary exceeds packaging size bound")
    content = binary.read_bytes()
    binary_machine(content, target)
    source = source_materials()
    sha, fingerprint, version, license_expression = source_identity(source)
    expected = {
        "schema_version": "ai-platform-desktop/build-info/v1",
        "package": "ai-platform-desktop", "version": version,
        "os": target["os"], "arch": target["arch"], "source_commit": sha,
        "source_digest": fingerprint, "rust_target": target["rust_target"],
    }
    smoke(binary, expected)
    name = f"ai-platform-desktop-{version}-{target['name']}"
    binary_name = target["binary"]
    if target["os"] == "macos":
        binary_name = f"AI Platform.app/Contents/MacOS/{binary_name}"
    receipt = {
        "schema_version": "ai-platform-desktop/native-artifact/v1",
        "version": version, "source_commit": sha, "source_digest": fingerprint,
        "target": target["name"], "rust_target": target["rust_target"],
        "binary": binary_name, "binary_sha256": digest(content),
        "declared_build_baseline": target["declared_build_baseline"],
        "native_identity_smoke": "passed", "extracted_identity_smoke": "passed",
        "native_help_smoke": "passed", "extracted_help_smoke": "passed",
        "gui_acceptance": "not_run", "authenticated_acceptance": "not_run",
        "publisher_signature": "not_provided", "platform_adhoc_signature": "not_assessed",
        "notarization": "not_performed", "public_release_eligible": False,
        "crate_license_expression": license_expression,
        "repository_license_material": "LICENSE.repository",
        "licensing_note": "Repository LICENSE is MIT; native workspace declares Apache-2.0. Preserved without relicensing; reconcile before public release.",
        "materials": {path: digest(value) for path, value in sorted(source.items())},
    }
    receipt_bytes = (json.dumps(receipt, indent=2) + "\n").encode()
    readme = f"""AI Platform Desktop {version} ({target['name']})
Source commit: {sha}
Declared build baseline: {target['declared_build_baseline']}

Internal candidate. No publisher signature or notarization is supplied.
A checksum detects corruption; it does not authenticate the publisher.
Verify the companion .sha256 before extracting into a trusted private directory.
Keep all extracted files together. See native-desktop-distribution.md for launch,
platform prerequisites, removal, limitations and public release gates.

macOS: open AI Platform.app after approved platform security review.
Linux: execute ./ai-platform-desktop in an X11 or XWayland graphical session.
Windows: run ai-platform-desktop.exe from the extracted directory.
Security warnings must not be bypassed by this package or its setup instructions.

Offline --help and --package-info do not contact the Gateway or open a window.
The archive passed these offline process checks; GUI and authenticated acceptance
have NOT been run by this packaging command. This is not an installer or updater.
No service, startup entry, browser extension or stored credential is added.

LICENSE.repository preserves the MIT repository license verbatim. The native
workspace declares Apache-2.0. Owner reconciliation and complete dependency
license/notice review are required before external release.
""".encode()
    files = {
        binary_name: content, "artifact.json": receipt_bytes, "README.txt": readme,
        "LICENSE.repository": source["LICENSE"],
        "native-desktop-distribution.md": source[DISTRIBUTION.as_posix()],
        "native-client-capability-matrix.md": source["docs/architecture/native-client-capability-matrix.md"],
    }
    if target["os"] == "macos":
        # Prerelease suffixes stay in artifact.json; Apple's bundle version fields
        # use the unchanged numeric major.minor.patch component.
        bundle_version = version.split("-", 1)[0].split("+", 1)[0]
        files["AI Platform.app/Contents/Info.plist"] = plistlib.dumps({
            "CFBundleExecutable": target["binary"],
            "CFBundleIdentifier": manifest["bundle_identifier"],
            "CFBundleName": "AI Platform", "CFBundleDisplayName": "AI Platform",
            "CFBundlePackageType": "APPL", "CFBundleInfoDictionaryVersion": "6.0",
            "CFBundleShortVersionString": bundle_version, "CFBundleVersion": bundle_version,
            "LSMinimumSystemVersion": target["macos_minimum"],
            "NSHighResolutionCapable": True,
        }, sort_keys=True)
        files["AI Platform.app/Contents/Resources/LICENSE.repository"] = source["LICENSE"]
        files["AI Platform.app/Contents/Resources/artifact.json"] = receipt_bytes
    elif target["os"] == "linux":
        files["ai-platform-desktop.desktop"] = b"""[Desktop Entry]
Type=Application
Name=AI Platform
Comment=Native AI Platform Gateway client
Exec=ai-platform-desktop
TryExec=ai-platform-desktop
Terminal=false
Categories=Office;Network;
StartupNotify=false
"""
    payload = archive(name, files, binary_name, target["archive"])
    filename = f"{name}.{target['archive']}"
    artifacts = {
        filename: payload,
        f"{filename}.sha256": f"{digest(payload)}  {filename}\n".encode(),
        f"{name}.artifact.json": receipt_bytes,
    }
    output.mkdir(parents=True, exist_ok=True)
    if any((output / path).exists() for path in artifacts):
        raise ValueError("Output already exists; use a fresh output directory")
    verify_archive(payload, name, files, binary_name, target["archive"], expected, output)
    # Recheck after native process execution, before making a successful receipt
    # visible. This also rejects source edits during the packaging window.
    if source_materials() != source or git("rev-parse", "HEAD") != sha:
        raise ValueError("Source inputs changed during packaging")
    if binary.read_bytes() != content:
        raise ValueError("Desktop input binary changed during packaging")
    for path, data in artifacts.items():
        with (output / path).open("xb") as handle:
            handle.write(data)
    print(json.dumps({"archive": str(output / filename), "source_commit": sha,
                      "target": target["name"], "gui_acceptance": "not_run",
                      "publication": "not_performed"}))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_mutually_exclusive_group()
    modes.add_argument("--matrix", action="store_true")
    modes.add_argument("--build-env", action="store_true")
    parser.add_argument("--target")
    parser.add_argument("--binary")
    parser.add_argument("--output")
    args = parser.parse_args()
    try:
        manifest = target_manifest()
        if args.matrix:
            if args.target or args.binary or args.output:
                raise ValueError("--matrix cannot be combined with other options")
            print(json.dumps({"include": manifest["targets"]}, separators=(",", ":")))
            return 0
        target = next((item for item in manifest["targets"] if item["name"] == args.target), None)
        if target is None:
            raise ValueError("Select a desktop --target from --matrix")
        if args.build_env:
            if args.binary or args.output:
                raise ValueError("--build-env cannot be combined with --binary or --output")
            sha, fingerprint, _, _ = source_identity(source_materials())
            for key, value in build_environment(target, sha, fingerprint).items():
                print(f"{key}={value}")
        else:
            package(args, manifest, target)
        return 0
    except (ValueError, OSError, KeyError, TypeError, subprocess.SubprocessError) as error:
        # Never include captured native stdout/stderr or environment values.
        print(f"Desktop packaging failed: {type(error).__name__}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
