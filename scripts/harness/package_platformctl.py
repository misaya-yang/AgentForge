#!/usr/bin/env python3
"""Package an already-built platformctl with identity, checksums and source materials.

This tool never invokes Cargo, downloads dependencies, signs or publishes artifacts.
Use --matrix to emit the CI build matrix without reading any binary.
"""
from __future__ import annotations

import argparse
import gzip
import hashlib
import io
import json
from pathlib import Path
import re
import struct
import subprocess
import sys
import tarfile
import zipfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
WORKSPACE = Path("rust/platform-native")
CLIENT = WORKSPACE / "crates/platform-client"
TARGETS = Path("deploy/release/platformctl-targets.json")


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def targets() -> list[dict]:
    value = json.loads((ROOT / TARGETS).read_text(encoding="utf-8"))
    if value.get("schema_version") != "platformctl/build-targets/v1":
        raise ValueError("Unsupported target manifest")
    return value["targets"]


def git(*args: str) -> str:
    return subprocess.check_output(["git", "-C", str(ROOT), *args], text=True).strip()


def binary_machine(data: bytes, target: dict) -> None:
    """Reject a mislabeled archive before it can become a download candidate."""
    name = target["name"]
    if name.startswith("linux-"):
        if len(data) < 64 or data[:6] != b"\x7fELF\x02\x01":
            raise ValueError("Expected a 64-bit little-endian ELF binary")
        machine = struct.unpack_from("<H", data, 18)[0]
    elif name.startswith("darwin-"):
        if len(data) < 32 or data[:4] != b"\xcf\xfa\xed\xfe":
            raise ValueError("Expected a single-architecture 64-bit Mach-O binary")
        machine = struct.unpack_from("<I", data, 4)[0]
    else:
        if len(data) < 64 or data[:2] != b"MZ":
            raise ValueError("Expected a Windows PE binary")
        offset = struct.unpack_from("<I", data, 60)[0]
        if offset > len(data) - 24 or data[offset:offset + 4] != b"PE\0\0":
            raise ValueError("Invalid PE header")
        machine = struct.unpack_from("<H", data, offset + 4)[0]
    if machine != target["machine"]:
        raise ValueError("Binary architecture does not match requested target")


def source_materials() -> dict[str, bytes]:
    paths = [Path("LICENSE"), WORKSPACE / "Cargo.toml", WORKSPACE / "Cargo.lock",
             CLIENT / "Cargo.toml", TARGETS, WORKSPACE / "README.md",
             Path("docs/architecture/native-client-capability-matrix.md"),
             Path("scripts/harness/package_platformctl.py"), Path(".github/workflows/publish-sdk.yml")]
    paths.extend(sorted(path.relative_to(ROOT) for path in (ROOT / CLIENT / "src").rglob("*.rs")))
    return {path.as_posix(): (ROOT / path).read_bytes() for path in paths}


def archive(name: str, files: dict[str, bytes], binary_name: str, kind: str) -> bytes:
    result = io.BytesIO()
    if kind == "zip":
        with zipfile.ZipFile(result, "w", compression=zipfile.ZIP_DEFLATED) as output:
            for filename, content in sorted(files.items()):
                info = zipfile.ZipInfo(f"{name}/{filename}", date_time=(1980, 1, 1, 0, 0, 0))
                info.create_system = 3
                info.external_attr = (0o100755 if filename == binary_name else 0o100644) << 16
                info.compress_type = zipfile.ZIP_DEFLATED
                output.writestr(info, content)
    else:
        with gzip.GzipFile(fileobj=result, mode="wb", mtime=0, filename="") as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as output:
                for filename, content in sorted(files.items()):
                    info = tarfile.TarInfo(f"{name}/{filename}")
                    info.size = len(content)
                    info.mode = 0o755 if filename == binary_name else 0o644
                    info.mtime = 0
                    output.addfile(info, io.BytesIO(content))
    return result.getvalue()


def package(args: argparse.Namespace) -> None:
    target = next((item for item in targets() if item["name"] == args.target), None)
    if target is None:
        raise ValueError("Unknown target; use --matrix")
    if not args.binary or not args.output:
        raise ValueError("--binary and --output are required")
    output = Path(args.output).resolve()
    if output == ROOT or ROOT in output.parents:
        raise ValueError("Package output must be outside the repository")
    binary = Path(args.binary).resolve(strict=True)
    if binary.is_dir() or binary.name != target["binary"]:
        raise ValueError("Binary filename does not match target")
    if binary.stat().st_size > 256 * 1024 * 1024:
        raise ValueError("Binary exceeds packaging size bound")
    content = binary.read_bytes()
    binary_machine(content, target)
    source = source_materials()
    status = git("status", "--porcelain", "--", *source.keys())
    if status:
        raise ValueError("Package inputs must be committed and clean")
    sha = git("rev-parse", "HEAD")
    if not re.fullmatch(r"[0-9a-f]{40,64}", sha):
        raise ValueError("Invalid source commit identity")
    workspace = tomllib.loads(source[(WORKSPACE / "Cargo.toml").as_posix()].decode())
    version = workspace["workspace"]["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?", version):
        raise ValueError("Invalid package version")
    smoke = subprocess.run([str(binary), "--help"], capture_output=True, timeout=15, check=False)
    if smoke.returncode != 0 or b"platformctl --origin" not in smoke.stdout:
        raise ValueError("Native --help smoke failed on this runner")
    name = f"platformctl-{version}-{target['name']}"
    manifest = {
        "schema_version": "platformctl/native-artifact/v1",
        "version": version, "source_commit": sha,
        "target": target["name"], "rust_target": target["rust_target"],
        "binary": target["binary"], "binary_sha256": digest(content),
        "native_help_smoke": "passed", "live_api_acceptance": "not_run",
        "signature": "unsigned", "notarization": "not_performed",
        "crate_license_expression": workspace["workspace"]["package"]["license"],
        "repository_license_material": "LICENSE.repository",
        "licensing_note": "Repository LICENSE is MIT; native workspace declares Apache-2.0. Preserved without relicensing; reconcile before public release.",
        "materials": {path: digest(value) for path, value in sorted(source.items())},
    }
    receipt = (json.dumps(manifest, indent=2) + "\n").encode()
    readme = f"""platformctl {version} ({target['name']})
Source commit: {sha}

Extract this directory and run {target['binary']} --help.
Move only the executable into a trusted directory on PATH if desired.
No installer, updater, service, background agent or persistent credentials are added.
Use --origin and --token-stdin for authenticated commands; never put tokens in arguments.

Verify the companion .sha256 before extracting. artifact.json records the binary
and source hashes. A checksum is not a publisher signature: this candidate is
unsigned and has not been notarized. No live Gateway acceptance is implied.

LICENSE.repository preserves the repository MIT license unchanged. The native
workspace metadata declares Apache-2.0; this mismatch requires owner reconciliation
before public release and is not resolved by this archive.

Read native-client-capability-matrix.md for supported surfaces and limitations.
""".encode()
    files = {target["binary"]: content, "artifact.json": receipt,
             "README.txt": readme, "LICENSE.repository": source["LICENSE"],
             "native-client-capability-matrix.md": source["docs/architecture/native-client-capability-matrix.md"]}
    payload = archive(name, files, target["binary"], target["archive"])
    filename = f"{name}.{target['archive']}"
    artifacts = {filename: payload, f"{filename}.sha256": f"{digest(payload)}  {filename}\n".encode(), f"{name}.artifact.json": receipt}
    output.mkdir(parents=True, exist_ok=True)
    if any((output / filename).exists() for filename in artifacts):
        raise ValueError("Output already exists; use an empty output directory")
    for filename, payload in artifacts.items():
        with (output / filename).open("xb") as handle:
            handle.write(payload)
    print(json.dumps({"archive": str(output / name) + "." + target["archive"], "source_commit": sha, "target": target["name"], "publication": "not_performed"}))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--matrix", action="store_true")
    parser.add_argument("--target")
    parser.add_argument("--binary")
    parser.add_argument("--output")
    args = parser.parse_args()
    try:
        if args.matrix:
            if args.target or args.binary or args.output:
                raise ValueError("--matrix cannot be combined with packaging options")
            print(json.dumps({"include": targets()}, separators=(",", ":")))
        else:
            package(args)
        return 0
    except (ValueError, OSError, subprocess.SubprocessError, KeyError) as error:
        # Never expose binary stdout/stderr or environment values in diagnostics.
        print(f"Packaging failed: {type(error).__name__}: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
