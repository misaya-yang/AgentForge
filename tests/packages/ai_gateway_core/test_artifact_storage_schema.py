"""Architecture contract for the Assistant-owned artifact metadata table."""

from __future__ import annotations

import inspect
import re
from pathlib import Path

from ai_gateway_core.storage.artifact_storage import ArtifactStorageService


def test_artifact_sql_targets_the_authoritative_assistant_schema() -> None:
    """Gateway and capability worker must share authoritative receipts."""

    source = inspect.getsource(ArtifactStorageService)
    unqualified_artifact_sql = re.compile(
        r"\b(?:FROM|INTO|UPDATE|DELETE\s+FROM)\s+artifacts\b",
        re.IGNORECASE,
    )

    assert unqualified_artifact_sql.search(source) is None
    assert source.count("assistant.artifacts") == 10


def test_runtime_python_has_no_unqualified_artifact_table_sql() -> None:
    root = Path(__file__).resolve().parents[3]
    runtime_roots = (
        root / "src",
        root / "packages" / "ai-gateway-core" / "src",
    )
    unqualified_artifact_sql = re.compile(
        r"\b(?:FROM|INTO|UPDATE|DELETE\s+FROM)\s+artifacts\b",
        re.IGNORECASE,
    )
    offenders: list[str] = []
    for runtime_root in runtime_roots:
        for path in runtime_root.rglob("*.py"):
            if unqualified_artifact_sql.search(path.read_text(encoding="utf-8")):
                offenders.append(str(path.relative_to(root)))

    assert offenders == []
