from __future__ import annotations

import datetime as dt
import json
from collections.abc import Callable
from pathlib import Path

import pytest

from scripts.harness.hygiene_check import (
    apply_allowlist,
    compare_skip_baseline,
    run,
    scan_js,
    scan_python,
    scan_skip_markers,
)


def _write(root: Path, relative: str, content: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def test_python_syntax_error_fails_closed(tmp_path: Path) -> None:
    _write(tmp_path, "tests/test_broken.py", "def test_broken(:\n    pass\n")
    _write(
        tmp_path,
        "web/src/clean.test.ts",
        'test("real", () => { expect(value).toBe(1); });\n',
    )

    failures, warnings = scan_python(tmp_path)

    assert warnings == []
    assert failures == [
        {
            "file": "tests/test_broken.py",
            "line": 1,
            "test": "<module>",
            "issue": "python syntax error",
        }
    ]
    assert run(tmp_path, tmp_path / "evidence.json", tmp_path / "missing-allowlist.json") == 1


def test_python_ast_flags_empty_and_self_proving_bodies_only(tmp_path: Path) -> None:
    _write(
        tmp_path,
        "tests/test_shapes.py",
        "import pytest\n\n"
        "def test_empty():\n"
        "    pass\n\n"
        "def test_literal_truth():\n"
        "    assert True\n\n"
        "def test_literal_comparison():\n"
        "    assert 1 < 2 < 3\n\n"
        "def test_identity(value):\n"
        "    assert value is value\n\n"
        "def test_equality(value):\n"
        "    assert value == value\n\n"
        "@pytest.mark.parametrize('value', [1])\n"
        "def test_parametrized_self_proof(value):\n"
        "    assert True\n\n"
        "def test_real(result):\n"
        "    assert result == 2\n\n"
        "def test_helper_only():\n"
        "    helper()\n",
    )

    failures, warnings = scan_python(tmp_path)

    issues = {(item["test"], item["issue"]) for item in failures}
    assert issues == {
        ("test_empty", "empty test body"),
        ("test_literal_truth", "self-proving assertion-only test"),
        ("test_literal_comparison", "self-proving assertion-only test"),
        ("test_identity", "self-proving assertion-only test"),
        ("test_equality", "self-proving assertion-only test"),
        ("test_parametrized_self_proof", "self-proving assertion-only test"),
    }
    assert [item["test"] for item in warnings] == ["test_helper_only"]


def test_typescript_full_file_scan_catches_multiline_and_placeholders(tmp_path: Path) -> None:
    _write(
        tmp_path,
        "web/src/multiline.test.ts",
        "test\n"
        "  .only(\"focused\", () => { expect(1).toBe(1); });\n"
        "describe\n"
        "  .fixme(\"disabled\", () => { expect(1).toBe(1); });\n"
        "test(\n"
        "  \"comment placeholder\",\n"
        "  async ({ page }) => {\n"
        "    // TODO: exercise page\n"
        "  },\n"
        ");\n"
        "it(\"function placeholder\", function () {\n"
        "  /* intentionally empty for now */\n"
        "});\n"
        "test(\"real\", async () => { expect(1).toBe(1); });\n",
    )

    failures = scan_js(tmp_path)

    assert [item["issue"] for item in failures] == [
        ".only( focused test",
        ".fixme( disabled test",
        "empty test body",
        "empty test body",
    ]
    assert [item.get("test") for item in failures[-2:]] == [
        "comment placeholder",
        "function placeholder",
    ]


def test_skip_baseline_allows_removal_but_rejects_new_or_reclassified_skip(
    tmp_path: Path,
) -> None:
    _write(
        tmp_path,
        "tests/test_markers.py",
        "import pytest\n\n"
        "@pytest.mark.skipif(True, reason='known platform condition')\n"
        "def test_known():\n    assert value\n\n"
        "def test_new():\n    pytest.skip('new environment gap')\n",
    )
    _write(
        tmp_path,
        "web/src/marker.spec.ts",
        'test.skip("known", () => { expect(value).toBe(1); });\n',
    )
    baseline = tmp_path / "skip-baseline.json"
    baseline.write_text(
        json.dumps(
            {
                "schema": "ai-gateway/baseline/skip-baseline/v1",
                "python": {
                    "marker_count": 1,
                    "markers": [
                        {
                            "file": "tests/test_markers.py",
                            "line": 3,
                            "kind": "pytest.mark.skipif",
                            "target": "test_known",
                            "reason": "known platform condition",
                        }
                    ],
                },
                "typescript": {
                    "marker_count": 1,
                    "markers": [
                        {"file": "web/src/marker.spec.ts", "line": 1, "kind": "skip"}
                    ],
                },
            }
        ),
        encoding="utf-8",
    )

    result = compare_skip_baseline(scan_skip_markers(tmp_path), baseline)

    assert result["retired"] == 0
    assert result["unexpected"] == [
        {
            "language": "python",
            "file": "tests/test_markers.py",
            "line": 8,
            "kind": "pytest.skip (runtime)",
            "target": "<runtime>",
            "reason": "new environment gap",
        }
    ]


def test_dated_allowlist_match_expiry_and_staleness_are_preserved() -> None:
    failures = [
        {
            "file": "tests/test_backlog.py",
            "line": 1,
            "test": "test_allowed",
            "issue": "empty test body",
        },
        {
            "file": "tests/test_backlog.py",
            "line": 2,
            "test": "test_expired",
            "issue": "empty test body",
        },
    ]
    entries = [
        {
            "file": "tests/test_backlog.py",
            "test": "test_allowed",
            "owner": "ARC-07",
            "reason": "dated backlog",
            "expires": "2026-09-01",
        },
        {
            "file": "tests/test_backlog.py",
            "test": "test_expired",
            "owner": "ARC-07",
            "reason": "dated backlog",
            "expires": "2026-08-01",
        },
        {
            "file": "tests/test_backlog.py",
            "test": "test_stale",
            "owner": "ARC-07",
            "reason": "dated backlog",
            "expires": "2026-09-01",
        },
    ]

    enforced, allowlisted, expired, stale = apply_allowlist(
        failures, entries, dt.date(2026, 8, 30)
    )

    assert enforced == []
    assert [item["test"] for item in allowlisted] == ["test_allowed"]
    assert [item["test"] for item in expired] == ["test_expired"]
    assert [item["test"] for item in stale] == ["test_stale"]


def test_empty_scan_fails_closed(tmp_path: Path) -> None:
    assert (
        run(tmp_path, tmp_path / "evidence.json", tmp_path / "missing-allowlist.json")
        == 2
    )


@pytest.mark.parametrize(
    ("present_file", "missing_scan"),
    [
        (
            "tests/test_real.py",
            "typescript_test_files",
        ),
        (
            "web/src/real.test.ts",
            "python_test_files",
        ),
    ],
)
def test_one_language_scan_falling_to_zero_fails_closed(
    tmp_path: Path,
    present_file: str,
    missing_scan: str,
) -> None:
    content = (
        "def test_real(value):\n    assert value\n"
        if present_file.endswith(".py")
        else 'test("real", () => { expect(value).toBe(1); });\n'
    )
    _write(tmp_path, present_file, content)
    evidence = tmp_path / "evidence.json"
    scan_counts: dict[str, int] = {}
    scan_python(tmp_path, scan_counts=scan_counts)
    scan_js(tmp_path, scan_counts=scan_counts)

    assert run(tmp_path, evidence, tmp_path / "missing-allowlist.json") == 2
    assert scan_counts[missing_scan] == 0


@pytest.mark.parametrize(
    ("relative", "scanner"),
    [
        ("tests/test_unreadable.py", scan_python),
        ("web/src/unreadable.test.ts", scan_js),
    ],
)
def test_source_read_error_fails_closed(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    relative: str,
    scanner: Callable[[Path], object],
) -> None:
    path = tmp_path / relative
    _write(tmp_path, relative, "placeholder\n")
    original = Path.read_text

    def unreadable(self: Path, *args, **kwargs):
        if self == path:
            raise PermissionError("synthetic unreadable source")
        return original(self, *args, **kwargs)

    monkeypatch.setattr(Path, "read_text", unreadable)
    result = scanner(tmp_path)
    failures = result[0] if isinstance(result, tuple) else result

    assert len(failures) == 1
    assert "source unreadable" in failures[0]["issue"]


def test_symlinked_scan_subtree_fails_closed(tmp_path: Path) -> None:
    external = tmp_path / "external"
    external.mkdir()
    _write(external, "test_hidden.py", "def test_hidden():\n    assert True\n")
    (tmp_path / "tests").mkdir()
    (tmp_path / "tests" / "linked").symlink_to(external, target_is_directory=True)

    assert (
        run(tmp_path, tmp_path / "evidence.json", tmp_path / "missing-allowlist.json")
        == 2
    )
