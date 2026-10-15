"""One content identity for release validation and frozen Eval execution."""

from __future__ import annotations

import json
from collections.abc import Mapping, Sequence
from typing import Any


def _mapping(value: Any) -> dict[str, Any]:
    if isinstance(value, Mapping):
        return dict(value)
    if isinstance(value, str):
        decoded = json.loads(value)
        return dict(decoded) if isinstance(decoded, dict) else {}
    return {}


def build_eval_dataset_manifest(
    dataset: Mapping[str, Any], examples: Sequence[Mapping[str, Any]],
) -> dict[str, Any]:
    cases = []
    for row in examples:
        metadata = _mapping(row.get("metadata"))
        cases.append({
            "case_id": str(metadata.get("case_id") or row["example_id"]),
            "example_id": str(row["example_id"]),
            "split": str(row.get("split") or "regression"),
            "input": _mapping(row.get("input")),
            "expected_output": _mapping(row.get("expected_output")),
            "expected_trajectory": metadata.get("expected_trajectory") or {},
            "assertions": metadata.get("assertions") or [],
            "metadata": {key: value for key, value in metadata.items()
                         if key not in {"expected_trajectory", "assertions"}},
            "source_trace_id": str(row["source_trace_id"]) if row.get("source_trace_id") else None,
            "source_span_id": str(row["source_span_id"]) if row.get("source_span_id") else None,
        })
    cases.sort(key=lambda item: (item["case_id"], item["example_id"]))
    return {
        "dataset": {
            "dataset_id": str(dataset["dataset_id"]),
            "tenant_id": str(dataset["tenant_id"]),
            "name": str(dataset.get("name") or ""),
            "description": str(dataset.get("description") or ""),
            "version": str(dataset.get("version") or "v1"),
            "schema": _mapping(dataset.get("schema")),
            "metadata": _mapping(dataset.get("metadata")),
        },
        "examples": cases,
    }
