#!/usr/bin/env python3
"""Read-only retirement shim; the legacy migration is unsafe for BM25 v2.

The former implementation copied points into a replacement collection and
then deleted/aliased collections. That workflow is incompatible with the
versioned Qdrant-native ``bm25_v2`` shadow pipeline and is permanently retired.

Only ``--dry-run`` remains. It reads collection metadata without mutating
Qdrant and prints a retirement plan that directs operators to
``scripts/backfill_bm25_v2.py``.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
from collections.abc import Sequence
from dataclasses import asdict, dataclass
from typing import Any

QDRANT_URL = os.getenv("QDRANT_URL", "http://localhost:6333")
BM25_V2_SUPPORTED = False
REPLACEMENT_SCRIPT = "scripts/backfill_bm25_v2.py"


class LegacySparseMigrationRetired(RuntimeError):
    """Raised before any Qdrant access when execution is requested."""


@dataclass(frozen=True, slots=True)
class ReadOnlyMigrationPlan:
    collection: str
    points_count: int | None
    dense_vector_size: int | None
    distance: str | None
    status: str = "retired"
    replacement: str = REPLACEMENT_SCRIPT


def _retired_message() -> str:
    return (
        "legacy sparse-vector migration is permanently retired; "
        f"use {REPLACEMENT_SCRIPT} for the versioned bm25_v2 shadow backfill"
    )


def _dense_vector_config(collection_info: Any) -> Any:
    vectors = collection_info.config.params.vectors
    if not isinstance(vectors, dict):
        return vectors
    dense = vectors.get("") or vectors.get("default")
    if dense is None and len(vectors) == 1:
        dense = next(iter(vectors.values()))
    return dense


def _enum_value(value: Any) -> str | None:
    if value is None:
        return None
    return str(getattr(value, "value", value))


async def migrate(
    collection: str,
    qdrant_url: str,
    batch_size: int = 100,
    dry_run: bool = False,
) -> ReadOnlyMigrationPlan:
    """Inspect a collection only when explicitly requested as a dry run.

    The legacy signature is preserved so existing operator commands fail
    closed. A non-dry-run request raises before importing or constructing a
    Qdrant client. The retained dry run calls only ``get_collection`` and
    ``close``.
    """

    if not dry_run:
        raise LegacySparseMigrationRetired(_retired_message())
    if not collection.strip():
        raise ValueError("collection must be non-empty")
    if batch_size <= 0:
        raise ValueError("batch_size must be positive")

    from qdrant_client import AsyncQdrantClient

    client = AsyncQdrantClient(url=qdrant_url, timeout=120)
    try:
        collection_info = await client.get_collection(collection.strip())
    finally:
        await client.close()

    dense_config = _dense_vector_config(collection_info)
    vector_size = getattr(dense_config, "size", None)
    points_count = getattr(collection_info, "points_count", None)
    return ReadOnlyMigrationPlan(
        collection=collection.strip(),
        points_count=int(points_count) if points_count is not None else None,
        dense_vector_size=int(vector_size) if vector_size is not None else None,
        distance=_enum_value(getattr(dense_config, "distance", None)),
    )


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Read-only retirement shim for the legacy sparse migration"
    )
    parser.add_argument("--collection", default="kb_default_1024", help="Collection to inspect")
    parser.add_argument("--qdrant-url", default=QDRANT_URL, help="Qdrant URL")
    parser.add_argument(
        "--batch-size",
        type=int,
        default=100,
        help="Retained for command compatibility; no points are read or written",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Read collection metadata only; never create, delete, write, or alias",
    )
    return parser


def main(argv: Sequence[str] | None = None) -> int:
    parser = _parser()
    args = parser.parse_args(argv)
    if not args.dry_run:
        parser.error(_retired_message())

    plan = asyncio.run(
        migrate(
            args.collection,
            args.qdrant_url,
            args.batch_size,
            dry_run=True,
        )
    )
    print(json.dumps(asdict(plan), sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
