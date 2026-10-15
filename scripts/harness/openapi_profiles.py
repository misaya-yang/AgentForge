"""Explicit ownership of the mixed published contract; never infer exclusions from drift."""
from __future__ import annotations

import copy
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OWNERSHIP = ROOT / "sdk/openapi-profiles.json"
PROFILES = ("python-hybrid", "native", "published")
METHODS = {"get", "post", "put", "patch", "delete", "head", "options"}


def load_ownership(published: dict, path: Path = OWNERSHIP, root: Path = ROOT) -> dict:
    ownership = json.loads(path.read_text(encoding="utf-8"))
    if set(ownership) != {"version", "native_only", "native_security_schemes", "python_paths"} or ownership["version"] != 1:
        raise ValueError("unknown OpenAPI ownership format")
    seen = set()
    for entry in ownership["native_only"]:
        if set(entry) not in ({"path", "operations", "source", "source_evidence"},
                              {"path", "operations", "source", "source_evidence", "shared_source_evidence"}):
            raise ValueError("unknown ownership entry fields")
        route = entry["path"]
        if route in seen:
            raise ValueError(f"overlapping ownership: {route}")
        seen.add(route)
        methods = published.get("paths", {}).get(route, {})
        actual = {m: op.get("operationId") for m, op in methods.items() if m in METHODS}
        if not actual or actual != entry["operations"]:
            raise ValueError(f"stale native operation ownership: {route}")
        source = (root / entry["source"]).resolve()
        if not source.is_relative_to((root / "rust/platform-native").resolve()):
            raise ValueError(f"invalid native source: {route}")
        text = source.read_text(encoding="utf-8")
        if not entry["source_evidence"] or any(not proof or proof not in text for proof in entry["source_evidence"]):
            raise ValueError(f"stale native source evidence: {route}")
        shared = entry.get("shared_source_evidence", {})
        if "native_contract" in text and not shared:
            raise ValueError(f"missing shared native source evidence: {route}")
        if not isinstance(shared, dict):
            raise ValueError(f"invalid shared native source evidence: {route}")
        for relative, proofs in shared.items():
            shared_source = (root / relative).resolve()
            if not shared_source.is_relative_to((root / "rust/platform-native").resolve()):
                raise ValueError(f"invalid shared native source: {route}")
            if not isinstance(proofs, list) or not proofs or any(not isinstance(proof, str) or not proof for proof in proofs):
                raise ValueError(f"invalid shared native proof: {route}")
            shared_text = shared_source.read_text(encoding="utf-8")
            if any(proof not in shared_text for proof in proofs):
                raise ValueError(f"stale shared native source evidence: {route}")
    python_paths = ownership["python_paths"]
    if len(python_paths) != len(set(python_paths)) or set(python_paths) & seen:
        raise ValueError("overlapping Python/native path ownership")
    if set(python_paths) | seen != set(published.get("paths", {})):
        raise ValueError("unknown or stale published path ownership")
    schemes = ownership["native_security_schemes"]
    if len(set(schemes)) != len(schemes) or not set(schemes) <= set(published.get("components", {}).get("securitySchemes", {})):
        raise ValueError("stale or overlapping native security ownership")
    for route, methods in published["paths"].items():
        for method, op in methods.items():
            if method not in METHODS:
                continue
            refs = {name for req in op.get("security", published.get("security", [])) for name in req}
            if route not in seen and refs & set(schemes):
                raise ValueError(f"native security overlaps Python ownership: {route}")
    return ownership


def project(published: dict, profile: str, ownership: dict) -> dict:
    if profile not in PROFILES:
        raise ValueError(f"unknown OpenAPI profile: {profile}")
    result = copy.deepcopy(published)
    if profile in {"published", "native"}:
        return result
    result["paths"] = {p: v for p, v in result["paths"].items() if p in ownership["python_paths"]}
    schemes = result.get("components", {}).get("securitySchemes", {})
    result.setdefault("components", {})["securitySchemes"] = {
        name: value for name, value in schemes.items()
        if (name in ownership["native_security_schemes"]) == (profile == "native")
    }
    return result


def check_python_ownership(current: dict, ownership: dict) -> None:
    overlap = set(current.get("paths", {})) & {e["path"] for e in ownership["native_only"]}
    schemes = set(current.get("components", {}).get("securitySchemes", {})) & set(ownership["native_security_schemes"])
    if overlap or schemes:
        raise ValueError(f"Python export overlaps native ownership: {sorted(overlap | schemes)}")
