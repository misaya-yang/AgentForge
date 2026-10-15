#!/usr/bin/env python3
"""Minimal test-hygiene gate (L0).

Fail-closed checks:
  Python (test_*.py under tests/, sdk/, apps/, packages/):
    - empty test bodies (only pass/Ellipsis/docstring)
    - self-proving assertion-only tests
    - syntax errors (collection would fail, so the hygiene gate must fail too)
    - @pytest.mark.skip without a reason= (skipif is fine: the condition is the reason)
    - pytest.skip() calls with no message
  TypeScript (web/src, web/e2e, sdk *.test.* / *.spec.*):
    - .only(  — including multiline forms; a focused test makes CI run a subset silently
    - .fixme( — including multiline forms; a disabled test masquerades as coverage
    - test("...") callbacks with an empty or comment-only placeholder body

Informational (warning only, listed in the evidence so the bar can tighten):
  - Python test functions without an assertion token. Many legitimate tests
    assert through helpers today, so this is reported, not failed.

Pre-existing backlog may be suppressed through
``scripts/harness/hygiene_allowlist.json``; every entry needs an owner, a
reason and an expiry date and must name the exact test. Expired or stale
(no longer matching) entries fail the gate, so exceptions cannot outlive
their cleanup.

Usage:
  python3 scripts/harness/hygiene_check.py            # real gate
  python3 scripts/harness/hygiene_check.py --selftest # synthetic violations

Exit codes: 0 clean, 1 violations, 2 gate error.
Evidence: tmp/gate-evidence/hygiene-check.json
"""

from __future__ import annotations

import argparse
import ast
import datetime as dt
import json
import re
import stat
import sys
import tempfile
from collections import Counter
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_ALLOWLIST = Path(__file__).resolve().parent / "hygiene_allowlist.json"
DEFAULT_EVIDENCE = ROOT / "tmp" / "gate-evidence" / "hygiene-check.json"
DEFAULT_SKIP_BASELINE = (
    ROOT / "docs" / "architecture" / "baselines" / "2026-08-post-rag" / "skip-baseline.json"
)

PY_SCAN_ROOTS = ("tests", "sdk", "apps", "packages")
JS_SCAN_ROOTS = ("web", "sdk")
PRUNED_DIRS = {
    "node_modules", "dist", "build", "__pycache__", ".venv", "venv",
    ".pytest_cache", ".ruff_cache", ".mypy_cache", ".playwright",
    "test-results", "playwright-report", "tmp", "target",
}
ASSERTION_TOKENS = ("assert", "raises(", "warns(", "fail(", "expect(")


class HygieneScanError(RuntimeError):
    """Raised when the gate cannot completely inspect its declared roots."""


def _iter_files(roots: tuple[str, ...], base: Path, suffixes: tuple[str, ...]):
    for root in roots:
        start = base / root
        try:
            start_mode = start.lstat().st_mode
        except FileNotFoundError:
            continue
        except OSError as exc:
            raise HygieneScanError(f"cannot inspect scan root {start}: {exc}") from exc
        if stat.S_ISLNK(start_mode) or not stat.S_ISDIR(start_mode):
            raise HygieneScanError(f"scan root is not a real directory: {start}")
        stack = [start]
        while stack:
            current = stack.pop()
            try:
                entries = sorted(current.iterdir())
            except OSError as exc:
                raise HygieneScanError(f"cannot list scan directory {current}: {exc}") from exc
            for entry in entries:
                try:
                    mode = entry.lstat().st_mode
                    if stat.S_ISLNK(mode):
                        raise HygieneScanError(
                            f"symlink inside scan roots is not inspectable: {entry}"
                        )
                    if stat.S_ISDIR(mode):
                        if entry.name not in PRUNED_DIRS:
                            stack.append(entry)
                    elif stat.S_ISREG(mode) and entry.suffix in suffixes:
                        yield entry
                except OSError as exc:
                    raise HygieneScanError(f"cannot inspect scan path {entry}: {exc}") from exc


def _parse_python_test_file(path: Path) -> tuple[ast.Module | None, str, dict | None]:
    try:
        source = path.read_text(encoding="utf-8")
    except (UnicodeDecodeError, OSError) as exc:
        return None, "", {
            "line": 1,
            "test": "<module>",
            "issue": f"python source unreadable: {type(exc).__name__}",
        }
    try:
        return ast.parse(source, filename=str(path)), source, None
    except SyntaxError as exc:
        return None, source, {
            "line": int(exc.lineno or 1),
            "test": "<module>",
            "issue": "python syntax error",
        }


def _empty_body(node: ast.AST) -> bool:
    body = [
        stmt
        for stmt in node.body
        if not (isinstance(stmt, ast.Expr) and isinstance(getattr(stmt, "value", None), ast.Constant))
    ]
    if not body:
        return True
    return all(
        isinstance(stmt, ast.Pass) or (isinstance(stmt, ast.Expr) and stmt.value is Ellipsis)
        for stmt in body
    )


def _self_proving_assert(node: ast.Assert) -> bool:
    expression = node.test
    if isinstance(expression, ast.Constant):
        return bool(expression.value)
    if (
        isinstance(expression, ast.UnaryOp)
        and isinstance(expression.op, ast.Not)
        and isinstance(expression.operand, ast.Constant)
    ):
        return not bool(expression.operand.value)
    if not isinstance(expression, ast.Compare):
        return False
    operands = [expression.left, *expression.comparators]
    if all(
        isinstance(operator, (ast.Eq, ast.Is))
        and ast.dump(operands[index]) == ast.dump(operands[index + 1])
        and (
            isinstance(operands[index], ast.Name)
            or (
                isinstance(operator, ast.Is)
                and isinstance(operands[index], ast.Constant)
                and operands[index].value in (None, True, False)
            )
        )
        for index, operator in enumerate(expression.ops)
    ):
        return True
    try:
        values = [ast.literal_eval(operand) for operand in operands]
    except (ValueError, TypeError):
        return False
    for index, operator in enumerate(expression.ops):
        left = values[index]
        right = values[index + 1]
        try:
            result = (
                left == right
                if isinstance(operator, ast.Eq)
                else left != right
                if isinstance(operator, ast.NotEq)
                else left < right
                if isinstance(operator, ast.Lt)
                else left <= right
                if isinstance(operator, ast.LtE)
                else left > right
                if isinstance(operator, ast.Gt)
                else left >= right
                if isinstance(operator, ast.GtE)
                else left in right
                if isinstance(operator, ast.In)
                else left not in right
                if isinstance(operator, ast.NotIn)
                else False
            )
        except (TypeError, ValueError):
            return False
        if not result:
            return False
    return True


def _self_proving_body(node: ast.AST) -> bool:
    body = [
        stmt
        for stmt in node.body
        if not (isinstance(stmt, ast.Expr) and isinstance(getattr(stmt, "value", None), ast.Constant))
    ]
    return bool(body) and all(
        isinstance(stmt, ast.Assert) and _self_proving_assert(stmt) for stmt in body
    )


def _skip_decorator_without_reason(node) -> bool:
    for decorator in getattr(node, "decorator_list", []):
        target = decorator.func if isinstance(decorator, ast.Call) else decorator
        if isinstance(target, ast.Attribute) and target.attr == "skip":
            value = target.value
            if isinstance(value, ast.Attribute) and value.attr == "mark":
                if not isinstance(decorator, ast.Call):
                    return True  # bare @pytest.mark.skip
                if not any(keyword.arg == "reason" for keyword in decorator.keywords):
                    return True
    return False


def scan_python(
    base: Path,
    *,
    scan_counts: dict[str, int] | None = None,
) -> tuple[list[dict], list[dict]]:
    failures: list[dict] = []
    warnings: list[dict] = []
    files_scanned = 0
    for path in _iter_files(PY_SCAN_ROOTS, base, (".py",)):
        if not path.name.startswith("test_"):
            continue
        files_scanned += 1
        rel = str(path.relative_to(base))
        tree, source, parse_failure = _parse_python_test_file(path)
        if parse_failure is not None:
            failures.append({"file": rel, **parse_failure})
            continue
        assert tree is not None
        for node in ast.walk(tree):
            if not (
                isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                and node.name.startswith("test")
            ):
                continue
            if _empty_body(node):
                failures.append({"file": rel, "line": node.lineno, "test": node.name, "issue": "empty test body"})
                continue
            if _skip_decorator_without_reason(node):
                failures.append({"file": rel, "line": node.lineno, "test": node.name, "issue": "@pytest.mark.skip without reason"})
                continue
            if _self_proving_body(node):
                failures.append(
                    {
                        "file": rel,
                        "line": node.lineno,
                        "test": node.name,
                        "issue": "self-proving assertion-only test",
                    }
                )
                continue
            segment = ast.get_source_segment(source, node) or ""
            if re.search(r"pytest\.skip\(\s*\)", segment):
                failures.append({"file": rel, "line": node.lineno, "test": node.name, "issue": "pytest.skip() without a message"})
            if not any(token in segment for token in ASSERTION_TOKENS):
                warnings.append({"file": rel, "line": node.lineno, "test": node.name, "issue": "no assertion token found"})
    if scan_counts is not None:
        scan_counts["python_test_files"] = files_scanned
    return failures, warnings


JS_ONLY = re.compile(r"\b(?:test|it|describe)\s*\.\s*only\s*\(")
JS_FIXME = re.compile(r"\b(?:test|it|describe)\s*\.\s*fixme\s*\(")
JS_EMPTY_BODY = re.compile(
    r"\b(?:test|it)\s*\(\s*(?P<title>[\"'`][^\"'`]*[\"'`])\s*,\s*"
    r"(?:(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*=>|"
    r"(?:async\s+)?function(?:\s+[A-Za-z_$][\w$]*)?\s*\([^)]*\))\s*\{"
    r"(?P<body>(?:\s|;|//[^\n]*(?:\n|$)|/\*.*?\*/)*)"
    r"\}\s*(?:,\s*(?:[0-9_]+\s*)?)?\)",
    re.DOTALL,
)
JS_SKIP_MARKER = re.compile(r"\b(?:test|it|describe|suite)\s*\.\s*(skip|fixme|only)\b")


def _decorator_name(decorator: ast.expr) -> str | None:
    parts: list[str] = []
    node = decorator.func if isinstance(decorator, ast.Call) else decorator
    while isinstance(node, ast.Attribute):
        parts.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        parts.append(node.id)
    return ".".join(reversed(parts)) if parts else None


def _skip_reason(call: ast.Call) -> str | None:
    for keyword in call.keywords:
        if keyword.arg == "reason" and isinstance(keyword.value, ast.Constant):
            return str(keyword.value.value)
    if call.args and isinstance(call.args[0], ast.Constant) and isinstance(
        call.args[0].value, str
    ):
        return call.args[0].value
    return None


def scan_skip_markers(base: Path) -> dict[str, list[dict]]:
    """Inventory current skip/xfail markers without trusting test collection."""

    python_rows: list[dict] = []
    for path in _iter_files(PY_SCAN_ROOTS, base, (".py",)):
        if not path.name.startswith("test_"):
            continue
        rel = path.relative_to(base).as_posix()
        tree, _source, parse_failure = _parse_python_test_file(path)
        if parse_failure is not None or tree is None:
            continue
        for node in ast.walk(tree):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Module)):
                for decorator in getattr(node, "decorator_list", []):
                    name = _decorator_name(decorator)
                    marker = name.rsplit(".", 1)[-1] if name else ""
                    if not name or not name.startswith("pytest.mark.") or marker not in {
                        "skip",
                        "skipif",
                        "xfail",
                    }:
                        continue
                    python_rows.append(
                        {
                            "file": rel,
                            "line": decorator.lineno,
                            "kind": f"pytest.mark.{marker}",
                            "target": getattr(node, "name", "<module>"),
                            "reason": _skip_reason(decorator)
                            if isinstance(decorator, ast.Call)
                            else None,
                        }
                    )
            if (
                isinstance(node, ast.Call)
                and isinstance(node.func, ast.Attribute)
                and node.func.attr == "skip"
                and isinstance(node.func.value, ast.Name)
                and node.func.value.id == "pytest"
            ):
                python_rows.append(
                    {
                        "file": rel,
                        "line": node.lineno,
                        "kind": "pytest.skip (runtime)",
                        "target": "<runtime>",
                        "reason": _skip_reason(node),
                    }
                )

    typescript_rows: list[dict] = []
    for path in _iter_files(JS_SCAN_ROOTS, base, (".ts", ".tsx")):
        if not re.search(r"\.(test|spec)\.tsx?$", path.name):
            continue
        rel = path.relative_to(base).as_posix()
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        for match in JS_SKIP_MARKER.finditer(text):
            typescript_rows.append(
                {
                    "file": rel,
                    "line": text.count("\n", 0, match.start()) + 1,
                    "kind": match.group(1),
                }
            )
    python_rows.sort(key=lambda row: (row["file"], row["line"]))
    typescript_rows.sort(key=lambda row: (row["file"], row["line"]))
    return {"python": python_rows, "typescript": typescript_rows}


def compare_skip_baseline(
    current: dict[str, list[dict]], baseline_path: Path
) -> dict[str, object]:
    """Allow skip removal, but reject every new/reclassified skip or xfail."""

    try:
        baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError, UnicodeDecodeError) as exc:
        raise HygieneScanError(f"skip baseline unreadable: {baseline_path}: {exc}") from exc
    if baseline.get("schema") != "ai-gateway/baseline/skip-baseline/v1":
        raise HygieneScanError("unsupported skip baseline schema")

    unexpected: list[dict] = []
    retired = 0
    counts: dict[str, dict[str, int]] = {}
    for language in ("python", "typescript"):
        section = baseline.get(language)
        rows = section.get("markers") if isinstance(section, dict) else None
        if not isinstance(rows, list) or section.get("marker_count") != len(rows):
            raise HygieneScanError(f"malformed {language} skip baseline")

        remaining = Counter(_skip_marker_key(row, language) for row in rows)
        for row in current[language]:
            marker = _skip_marker_key(row, language)
            if remaining[marker]:
                remaining[marker] -= 1
            else:
                unexpected.append({"language": language, **row})
        retired += sum(remaining.values())
        counts[language] = {"baseline": len(rows), "current": len(current[language])}
    return {"counts": counts, "unexpected": unexpected, "retired": retired}


def _skip_marker_key(row: dict, language: str) -> tuple:
    if language == "python":
        return (
            row.get("file"),
            row.get("kind"),
            row.get("target"),
            row.get("reason"),
        )
    return (row.get("file"), row.get("kind"))


def scan_js(
    base: Path,
    *,
    scan_counts: dict[str, int] | None = None,
) -> list[dict]:
    failures: list[dict] = []
    files_scanned = 0
    for path in _iter_files(JS_SCAN_ROOTS, base, (".ts", ".tsx")):
        if not re.search(r"\.(test|spec)\.tsx?$", path.name):
            continue
        files_scanned += 1
        rel = str(path.relative_to(base))
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError) as exc:
            failures.append(
                {
                    "file": rel,
                    "line": 1,
                    "test": "<module>",
                    "issue": f"typescript source unreadable: {type(exc).__name__}",
                }
            )
            continue
        for pattern, issue in (
            (JS_ONLY, ".only( focused test"),
            (JS_FIXME, ".fixme( disabled test"),
        ):
            for match in pattern.finditer(text):
                failures.append(
                    {
                        "file": rel,
                        "line": text.count("\n", 0, match.start()) + 1,
                        "issue": issue,
                    }
                )
        for match in JS_EMPTY_BODY.finditer(text):
            failures.append(
                {
                    "file": rel,
                    "line": text.count("\n", 0, match.start()) + 1,
                    "test": match.group("title")[1:-1],
                    "issue": "empty test body",
                }
            )
    if scan_counts is not None:
        scan_counts["typescript_test_files"] = files_scanned
    return failures


def load_allowlist(path: Path) -> list[dict]:
    data = json.loads(path.read_text(encoding="utf-8"))
    entries = data.get("entries") or []
    for entry in entries:
        for key in ("file", "test", "owner", "reason", "expires"):
            if key not in entry:
                raise ValueError(f"allowlist entry missing '{key}': {entry!r}")
        dt.date.fromisoformat(entry["expires"])  # validates format
    return entries


def apply_allowlist(
    failures: list[dict], entries: list[dict], today: dt.date
) -> tuple[list[dict], list[dict], list[dict], list[dict]]:
    """Split failures into enforced vs allowlisted; find stale/expired entries."""
    enforced: list[dict] = []
    allowlisted: list[dict] = []
    expired: list[dict] = []
    matched_entry_ids: set[int] = set()
    for failure in failures:
        matched = None
        for index, entry in enumerate(entries):
            if entry["file"] == failure["file"] and entry["test"] == failure.get("test"):
                matched = (index, entry)
                break
        if matched is None:
            enforced.append(failure)
            continue
        index, entry = matched
        matched_entry_ids.add(index)
        record = {**failure, "allowlist_owner": entry["owner"], "expires": entry["expires"]}
        if dt.date.fromisoformat(entry["expires"]) < today:
            expired.append(record)
        else:
            allowlisted.append(record)
    stale = [
        {"file": entry["file"], "test": entry["test"], "owner": entry["owner"]}
        for index, entry in enumerate(entries)
        if index not in matched_entry_ids
    ]
    return enforced, allowlisted, expired, stale


def run(base: Path, evidence_path: Path, allowlist_path: Path) -> int:
    try:
        entries = load_allowlist(allowlist_path) if allowlist_path.exists() else []
    except (OSError, ValueError, json.JSONDecodeError) as exc:
        print(f"GATE ERROR: bad allowlist {allowlist_path}: {exc}", file=sys.stderr)
        return 2

    scan_counts: dict[str, int] = {}
    try:
        py_failures, py_warnings = scan_python(base, scan_counts=scan_counts)
        js_failures = scan_js(base, scan_counts=scan_counts)
        skip_result = (
            compare_skip_baseline(scan_skip_markers(base), DEFAULT_SKIP_BASELINE)
            if base == ROOT
            else {"counts": {}, "unexpected": [], "retired": 0}
        )
    except HygieneScanError as exc:
        result = {
            "gate": "hygiene-check",
            "tier": "L0",
            "scanned": scan_counts,
            "scan_error": str(exc),
            "result": "error",
        }
        if base == ROOT:
            evidence_path.parent.mkdir(parents=True, exist_ok=True)
            evidence_path.write_text(
                json.dumps(result, ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )
        print(f"GATE ERROR: hygiene scan incomplete: {exc}", file=sys.stderr)
        return 2
    empty_scans = sorted(
        key
        for key in ("python_test_files", "typescript_test_files")
        if scan_counts.get(key, 0) == 0
    )
    if empty_scans:
        detail = f"no files discovered for: {', '.join(empty_scans)}"
        result = {
            "gate": "hygiene-check",
            "tier": "L0",
            "scanned": scan_counts,
            "scan_error": detail,
            "result": "error",
        }
        if base == ROOT:
            evidence_path.parent.mkdir(parents=True, exist_ok=True)
            evidence_path.write_text(
                json.dumps(result, ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )
        print(
            f"GATE ERROR: hygiene scan incomplete: {detail}",
            file=sys.stderr,
        )
        return 2
    enforced, allowlisted, expired, stale = apply_allowlist(
        py_failures + js_failures, entries, dt.date.today()
    )
    result = {
        "gate": "hygiene-check",
        "tier": "L0",
        "scanned": scan_counts,
        "failures": enforced,
        "allowlisted": allowlisted,
        "expired_entries": expired,
        "stale_entries": stale,
        "no_assertion_warnings": py_warnings,
        "skip_baseline": skip_result,
        "result": "pass"
        if not (enforced or expired or stale or skip_result["unexpected"])
        else "fail",
    }
    if base == ROOT:
        evidence_path.parent.mkdir(parents=True, exist_ok=True)
        evidence_path.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")

    print(
        f"hygiene failures: {len(enforced)}; allowlisted: {len(allowlisted)}; "
        f"expired entries: {len(expired)}; stale entries: {len(stale)}; "
        f"unexpected skips: {len(skip_result['unexpected'])}; "
        f"no-assertion warnings: {len(py_warnings)}"
    )
    for item in enforced:
        test = item.get("test")
        where = f"{item['file']}:{item['line']}" + (f" {test}" if test else "")
        print(f"  FAIL {where}: {item['issue']}")
    for item in allowlisted:
        print(
            f"  allowlisted {item['file']} {item.get('test')} "
            f"(owner={item['allowlist_owner']}, expires={item['expires']})"
        )
    for item in expired:
        print(
            f"  EXPIRED allowlist entry still needed: {item['file']} {item.get('test')} "
            f"(owner={item['allowlist_owner']}, expired {item['expires']})"
        )
    for entry in stale:
        print(f"  STALE allowlist entry matches nothing: {entry['file']} {entry['test']}")
    for item in skip_result["unexpected"]:
        print(
            f"  NEW SKIP {item['file']}:{item['line']} {item['kind']}: "
            f"{item.get('reason') or '(no static reason)'}"
        )
    for item in py_warnings[:20]:
        print(f"  WARN {item['file']}:{item['line']} {item['test']}: {item['issue']}")
    if len(py_warnings) > 20:
        print(f"  ... and {len(py_warnings) - 20} more no-assertion warnings (see evidence)")
    if enforced or expired or stale or skip_result["unexpected"]:
        return 1
    print(f"OK: test hygiene intact (evidence: {evidence_path.relative_to(base) if base == ROOT else 'selftest'})")
    return 0


def _selftest() -> int:
    layout = {
        "tests/test_bad_empty.py": "def test_nothing():\n    pass\n",
        "tests/test_bad_docstring_only.py": "def test_nothing():\n    \"\"\"only docs.\"\"\"\n",
        "tests/test_bad_syntax.py": "def test_broken(:\n    pass\n",
        "tests/test_bad_self_proving.py": "def test_trivial():\n    assert 1 == 1\n",
        "tests/test_bad_skip_no_reason.py": (
            "import pytest\n\n@pytest.mark.skip\ndef test_skipped():\n    assert True\n"
        ),
        "tests/test_bad_skip_no_message.py": (
            "import pytest\n\ndef test_skipped():\n    pytest.skip()\n"
        ),
        "tests/test_good.py": (
            "import pytest\n\n"
            "@pytest.mark.skip(reason='needs live stack')\n"
            "def test_live(value):\n    assert value == 2\n\n"
            "@pytest.mark.skipif(True, reason='conditional')\n"
            "def test_cond(value):\n    assert value == 2\n\n"
            "def test_real(value):\n    assert value == 2\n\n"
            "def helper():\n    pass\n"
        ),
        "web/src/example.test.ts": (
            'test("focused", async () => { expect(1).toBe(1); });\n'
            'test\n  .only("left behind", () => { expect(1).toBe(1); });\n'
            'test\n  .fixme("disabled", () => { expect(1).toBe(1); });\n'
            'test(\n  "empty body",\n  async () => {\n    // TODO: implement\n  },\n);\n'
        ),
        "web/src/example.good.spec.ts": 'test("ok", () => { expect(1).toBe(1); });\n',
    }
    failures_seen: list[str] = []
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        for rel, content in layout.items():
            path = base / rel
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8")
        py_failures, py_warnings = scan_python(base)
        js_failures = scan_js(base)
        all_failures = py_failures + js_failures
        failures_seen = [f"{f['file']}:{f['issue']}" for f in all_failures]

    expected = [
        "tests/test_bad_empty.py:empty test body",
        "tests/test_bad_docstring_only.py:empty test body",
        "tests/test_bad_syntax.py:python syntax error",
        "tests/test_bad_self_proving.py:self-proving assertion-only test",
        "tests/test_bad_skip_no_reason.py:@pytest.mark.skip without reason",
        "tests/test_bad_skip_no_message.py:pytest.skip() without a message",
        "web/src/example.test.ts:.only( focused test",
        "web/src/example.test.ts:.fixme( disabled test",
        "web/src/example.test.ts:empty test body",
    ]
    with tempfile.TemporaryDirectory() as tmp:
        baseline = Path(tmp) / "skip-baseline.json"
        baseline.write_text(
            json.dumps(
                {
                    "schema": "ai-gateway/baseline/skip-baseline/v1",
                    "python": {"marker_count": 0, "markers": []},
                    "typescript": {"marker_count": 0, "markers": []},
                }
            ),
            encoding="utf-8",
        )
        skip_probe = compare_skip_baseline(
            {
                "python": [
                    {
                        "file": "tests/test_new.py",
                        "line": 1,
                        "kind": "pytest.mark.skip",
                        "target": "test_new",
                        "reason": "new skip",
                    }
                ],
                "typescript": [],
            },
            baseline,
        )
        if len(skip_probe["unexpected"]) != 1:
            print("SELFTEST FAILED: new skip marker was not rejected", file=sys.stderr)
            return 1
    missing = [e for e in expected if e not in failures_seen]
    extra = [f for f in failures_seen if f not in expected]
    if missing or extra:
        print(f"SELFTEST FAILED\n  missing: {missing}\n  unexpected: {extra}", file=sys.stderr)
        return 1
    clean = [f["file"] for f in all_failures if f["file"].startswith(("tests/test_good", "web/src/example.good"))]
    if clean:
        print(f"SELFTEST FAILED: clean files flagged: {clean}", file=sys.stderr)
        return 1

    # Allowlist behaviour: match + expiry + staleness.
    entries = [
        {"file": "tests/test_bad_empty.py", "test": "test_nothing",
         "owner": "selftest", "reason": "synthetic", "expires": "2999-01-01"},
        {"file": "tests/test_bad_skip_no_reason.py", "test": "test_skipped",
         "owner": "selftest", "reason": "expired entry must fail", "expires": "2000-01-01"},
        {"file": "tests/does_not_exist.py", "test": "test_ghost",
         "owner": "selftest", "reason": "stale entry must fail", "expires": "2999-01-01"},
    ]
    enforced, allowlisted, expired, stale = apply_allowlist(all_failures, entries, dt.date.today())
    if [f["file"] for f in allowlisted] != ["tests/test_bad_empty.py"]:
        print(f"SELFTEST FAILED: allowlist match wrong: {allowlisted}", file=sys.stderr)
        return 1
    if [f["file"] for f in expired] != ["tests/test_bad_skip_no_reason.py"]:
        print(f"SELFTEST FAILED: expired entry not flagged: {expired}", file=sys.stderr)
        return 1
    if [e["file"] for e in stale] != ["tests/does_not_exist.py"]:
        print(f"SELFTEST FAILED: stale entry not flagged: {stale}", file=sys.stderr)
        return 1
    if any(f["file"] == "tests/test_bad_empty.py" for f in enforced):
        print("SELFTEST FAILED: allowlisted finding still enforced", file=sys.stderr)
        return 1
    print(f"SELFTEST OK: {len(expected)} synthetic hygiene violations detected, clean files untouched, "
          "allowlist match/expiry/staleness enforced")
    return 0


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, default=ROOT)
    parser.add_argument("--allowlist", type=Path, default=DEFAULT_ALLOWLIST)
    parser.add_argument("--evidence-out", type=Path, default=DEFAULT_EVIDENCE)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)
    if args.selftest:
        return _selftest()
    if not args.root.is_dir():
        print(f"GATE ERROR: root not found: {args.root}", file=sys.stderr)
        return 2
    return run(args.root, args.evidence_out, args.allowlist)


if __name__ == "__main__":
    raise SystemExit(main())
