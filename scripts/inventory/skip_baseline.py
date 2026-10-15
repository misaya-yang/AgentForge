"""Skip/xfail baseline.

Produces ``skip-baseline.json``: every statically discoverable skip/xfail in
the Python test suite and in the TypeScript spec/test files, with file,
marker kind, and (where statically extractable) the reason.

AC-M16 requires release-required gates to have zero *unexpected* skips. This
baseline names what exists today so later packages can tell a new skip apart
from an inherited one. Live-gated integration tests (skipped unless an env
flag is set) are classified as ``conditional-live`` — legitimate today, but
they must never be counted as passing coverage.
"""

from __future__ import annotations

import ast
import re

from _common import REPO_ROOT, base_envelope, walk_files

_TS_SKIP = re.compile(r"\b(?:test|it|describe|suite)\s*\.\s*(skip|fixme|only)\b")


def _decorator_name(deco) -> str | None:
    """Flatten @pytest.mark.skip[if] / @pytest.mark.xfail attribute chains."""
    parts: list[str] = []
    node = deco.func if isinstance(deco, ast.Call) else deco
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
    parts.reverse()
    return ".".join(parts)


def _reason_from_call(call: ast.Call) -> str | None:
    for kw in call.keywords:
        if kw.arg == "reason" and isinstance(kw.value, ast.Constant):
            return str(kw.value.value)
    if call.args and isinstance(call.args[0], ast.Constant) and isinstance(call.args[0].value, str):
        return call.args[0].value
    return None


def scan_python() -> list[dict]:
    rows: list[dict] = []
    for rel in walk_files((".py",), roots=("tests", "apps", "packages", "sdk")):
        if not (str(rel).endswith(".py")):
            continue
        path = REPO_ROOT / rel
        try:
            tree = ast.parse(path.read_text(encoding="utf-8"))
        except (SyntaxError, UnicodeDecodeError):
            continue
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Module)):
                decorators = getattr(node, "decorator_list", [])
                for deco in decorators:
                    name = _decorator_name(deco)
                    if not name or not name.startswith("pytest.mark."):
                        continue
                    marker = name.rsplit(".", 1)[-1]
                    if marker not in ("skip", "skipif", "xfail"):
                        continue
                    reason = _reason_from_call(deco) if isinstance(deco, ast.Call) else None
                    rows.append(
                        {
                            "file": str(rel),
                            "line": deco.lineno,
                            "kind": f"pytest.mark.{marker}",
                            "target": getattr(node, "name", "<module>"),
                            "reason": reason,
                        }
                    )
            if isinstance(node, ast.Call):
                func = node.func
                if isinstance(func, ast.Attribute) and func.attr == "skip":
                    value = func.value
                    if isinstance(value, ast.Name) and value.id == "pytest":
                        rows.append(
                            {
                                "file": str(rel),
                                "line": node.lineno,
                                "kind": "pytest.skip (runtime)",
                                "target": "<runtime>",
                                "reason": _reason_from_call(node),
                            }
                        )
    rows.sort(key=lambda row: (row["file"], row["line"]))
    return rows


def scan_typescript() -> list[dict]:
    rows: list[dict] = []
    for rel in walk_files((".ts", ".tsx"), roots=("web", "sdk")):
        base = rel.name
        if not (base.endswith((".spec.ts", ".spec.tsx", ".test.ts", ".test.tsx"))):
            continue
        text = (REPO_ROOT / rel).read_text(encoding="utf-8")
        for match in _TS_SKIP.finditer(text):
            line = text.count("\n", 0, match.start()) + 1
            rows.append(
                {
                    "file": str(rel),
                    "line": line,
                    "kind": match.group(1),
                }
            )
    rows.sort(key=lambda row: (row["file"], row["line"]))
    return rows


def _classify(rows: list[dict]) -> dict[str, int]:
    live = 0
    static = 0
    for row in rows:
        reason = (row.get("reason") or "").lower()
        if any(
            token in reason
            for token in ("set run_", "live", "credentials", "password", "env", "docker", "not available locally")
        ):
            live += 1
        else:
            static += 1
    return {"conditional_live_or_env_gated": live, "unconditional_or_static": static}


def build() -> dict:
    python_rows = scan_python()
    ts_rows = scan_typescript()
    python_files = sorted({row["file"] for row in python_rows})
    return {
        **base_envelope("skip-baseline"),
        "policy": (
            "AC-M16: release-required gates must show zero unexpected skips. Skips are legal "
            "only when named and classified; this baseline is the reference set."
        ),
        "python": {
            "marker_count": len(python_rows),
            "file_count": len(python_files),
            "classification": _classify(python_rows),
            "markers": python_rows,
        },
        "typescript": {
            "marker_count": len(ts_rows),
            "file_count": len({row["file"] for row in ts_rows}),
            "note": "scan of *.spec.ts/*.test.ts(x) under web/ and sdk/ for test/describe .skip/.fixme/.only",
            "markers": ts_rows,
        },
        "known_gap_context": (
            "The frontend type-check gate historically checked zero files (kb-rag-ui-t5 loop-state "
            "notes); its green results were no-ops. That is an ARC-00B fix target, recorded here so "
            "the skip baseline and the false-green gate list stay in one place."
        ),
    }


if __name__ == "__main__":
    from _common import write_json

    path = write_json("skip-baseline.json", build())
    print(f"wrote {path}")
