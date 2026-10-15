"""Persistence surface for PRD T0-#2 (version-pinned KB golden QA store).

Owns the tables created by ``database/migrations/104_kb_eval_golden.sql``:

* ``kb_eval_golden`` — the (version, case_id) projection of the manifest-pinned
  golden JSONL.  A row's case shape is exactly what ``validate_rag_cases``
  accepts (``track``/``query``/``relevance``/``reference_answer``/``metadata``),
  with two columns promoted out of metadata because they gate behaviour:
  ``split`` (``frozen`` regression set vs ``growth`` set) and ``provenance``.
  ``case_to_row``/``row_to_case`` are the single mapping used by both import
  and read-back, so a version round-tripped through Postgres is byte-equal to
  its JSONL case (modulo the promoted fields, which ride back in metadata).
* ``kb_eval_golden_release`` — the pointer pinning which version the current
  gates/baselines cite (default key ``current``).

The store takes an asyncpg pool at construction (scripts and callers build
one from the ``.env`` PostgreSQL config); it deliberately does not touch
``database.py``.  Content hashing is not done here: the review gate is the
git manifest (make kb-golden-gate), this table only pins versions server-side.
"""

from __future__ import annotations

import json
import logging
from typing import Any

logger = logging.getLogger("knowledge_service.persistence.kb_eval_golden_store")

GOLDEN_TRACKS = frozenset({"retrieval_only", "answer_aware"})
GOLDEN_SPLITS = frozenset({"frozen", "growth"})
DEFAULT_RELEASE_KEY = "current"


class GoldenStoreError(RuntimeError):
    """Base error for the T0 golden store (state violations, unknown versions)."""


def _json_dumps(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _loads_json(value: Any, fallback: Any) -> Any:
    if value is None:
        return fallback
    if isinstance(value, str):
        return json.loads(value)
    return value


def _clean_version(version: str) -> str:
    text = str(version or "").strip()
    if not text:
        raise ValueError("version must be a non-empty string")
    return text


def case_to_row(
    case: dict[str, Any],
    *,
    version: str,
    default_split: str = "growth",
    provenance: str | None = None,
) -> dict[str, Any]:
    """Map one validate_rag_cases-shaped case dict to a kb_eval_golden row.

    Raises ValueError on any shape violation the migration would only catch
    after a partially-applied import — the importer is fail-closed per case
    before touching the database.  ``metadata.split`` (when valid) overrides
    ``default_split``; ``provenance`` argument overrides ``metadata.provenance``.
    """
    if not isinstance(case, dict):
        raise ValueError("golden case must be a dict")
    case_id = str(case.get("case_id") or "").strip()
    if not case_id:
        raise ValueError("golden case is missing case_id")
    track = str(case.get("track") or "")
    if track not in GOLDEN_TRACKS:
        raise ValueError(f"case {case_id}: track {track!r} is not in {sorted(GOLDEN_TRACKS)}")
    query = str(case.get("query") or "").strip()
    if not query:
        raise ValueError(f"case {case_id}: query must be non-empty")
    relevance = case.get("relevance")
    if not isinstance(relevance, dict) or not relevance:
        raise ValueError(f"case {case_id}: relevance must be a non-empty object")
    bad_grades = [
        k
        for k, v in relevance.items()
        if isinstance(v, bool) or not isinstance(v, int | float)
    ]
    if bad_grades:
        raise ValueError(f"case {case_id}: relevance grades must be numeric (bad: {bad_grades})")
    metadata = case.get("metadata") or {}
    if not isinstance(metadata, dict):
        raise ValueError(f"case {case_id}: metadata must be an object when present")

    split = str(metadata.get("split") or default_split)
    if split not in GOLDEN_SPLITS:
        raise ValueError(f"case {case_id}: split {split!r} is not in {sorted(GOLDEN_SPLITS)}")
    row_provenance = provenance if provenance is not None else str(metadata.get("provenance") or "")

    return {
        "case_id": case_id,
        "version": _clean_version(version),
        "track": track,
        "query": query,
        "relevance": dict(relevance),
        "reference_answer": case.get("reference_answer"),
        "split": split,
        "metadata": dict(metadata),
        "provenance": row_provenance[:100],
    }


def row_to_case(record: Any) -> dict[str, Any]:
    """Map a kb_eval_golden row back to the canonical RAG case dict shape.

    ``split`` and ``provenance`` ride back inside metadata (the JSONL shape
    has no top-level home for them — validate_rag_cases rejects unknown
    top-level keys), so import→list is case-equal for any metadata-consistent
    input.
    """
    metadata = dict(_loads_json(record["metadata"], {}))
    metadata["split"] = record["split"]
    if record.get("provenance"):
        metadata.setdefault("provenance", record["provenance"])
    case: dict[str, Any] = {
        "case_id": record["case_id"],
        "track": record["track"],
        "query": record["query"],
        "relevance": _loads_json(record["relevance"], {}),
        "metadata": metadata,
    }
    if record.get("reference_answer"):
        case["reference_answer"] = record["reference_answer"]
    return case


class KbEvalGoldenStore:
    """Asyncpg-backed store over kb_eval_golden + kb_eval_golden_release."""

    def __init__(self, pool: Any) -> None:
        if pool is None:
            raise ValueError("KbEvalGoldenStore requires an asyncpg pool")
        self._pool = pool

    async def import_cases(
        self,
        cases: list[dict[str, Any]],
        *,
        version: str,
        default_split: str = "growth",
        provenance: str | None = None,
    ) -> dict[str, int]:
        """Upsert mapped cases under ``version``; returns {"imported", "frozen", "growth"}.

        Re-importing the same (version, case_id) refreshes the row in place
        (created_at preserved, updated_at bumped), so the import is idempotent.
        """
        if not cases:
            raise ValueError("import_cases requires at least one case")
        rows = [
            case_to_row(case, version=version, default_split=default_split, provenance=provenance)
            for case in cases
        ]
        seen: set[tuple[str, str]] = set()
        for row in rows:
            key = (row["version"], row["case_id"])
            if key in seen:
                raise ValueError(f"duplicate case_id in one import: {row['case_id']}")
            seen.add(key)

        async with self._pool.acquire() as conn, conn.transaction():
            for row in rows:
                await conn.execute(
                    """
                        INSERT INTO kb_eval_golden
                            (case_id, version, track, query, relevance,
                             reference_answer, split, metadata, provenance)
                        VALUES
                            ($1, $2, $3, $4, $5::jsonb, $6, $7, $8::jsonb, $9)
                        ON CONFLICT (version, case_id) DO UPDATE SET
                            track = EXCLUDED.track,
                            query = EXCLUDED.query,
                            relevance = EXCLUDED.relevance,
                            reference_answer = EXCLUDED.reference_answer,
                            split = EXCLUDED.split,
                            metadata = EXCLUDED.metadata,
                            provenance = EXCLUDED.provenance,
                            updated_at = NOW()
                        """,
                    row["case_id"],
                    row["version"],
                    row["track"],
                    row["query"],
                    _json_dumps(row["relevance"]),
                    row["reference_answer"],
                    row["split"],
                    _json_dumps(row["metadata"]),
                    row["provenance"],
                )
        counts = {"imported": len(rows)}
        for split in GOLDEN_SPLITS:
            counts[split] = sum(1 for row in rows if row["split"] == split)
        logger.info(
            "imported %d golden cases under version %s (frozen=%d growth=%d)",
            counts["imported"],
            rows[0]["version"],
            counts["frozen"],
            counts["growth"],
        )
        return counts

    async def list_cases(
        self,
        version: str | None = None,
        *,
        split: str | None = None,
        track: str | None = None,
        release_key: str = DEFAULT_RELEASE_KEY,
    ) -> list[dict[str, Any]]:
        """Return case dicts for ``version`` (default: the release pointer)."""
        if split is not None and split not in GOLDEN_SPLITS:
            raise ValueError(f"split {split!r} is not in {sorted(GOLDEN_SPLITS)}")
        if track is not None and track not in GOLDEN_TRACKS:
            raise ValueError(f"track {track!r} is not in {sorted(GOLDEN_TRACKS)}")
        resolved = _clean_version(version) if version is not None else await self.get_release(release_key)
        if resolved is None:
            raise GoldenStoreError(
                f"no golden version pinned under release key {release_key!r}; pass version explicitly"
            )
        clauses = ["version = $1"]
        args: list[Any] = [resolved]
        if split is not None:
            args.append(split)
            clauses.append(f"split = ${len(args)}")
        if track is not None:
            args.append(track)
            clauses.append(f"track = ${len(args)}")
        async with self._pool.acquire() as conn:
            records = await conn.fetch(
                f"""
                SELECT case_id, track, query, relevance, reference_answer,
                       split, metadata, provenance
                FROM kb_eval_golden
                WHERE {" AND ".join(clauses)}
                ORDER BY case_id
                """,
                *args,
            )
        # asyncpg Record -> dict so row_to_case can use .get() on the
        # nullable columns regardless of driver version.
        return [row_to_case(dict(record)) for record in records]

    async def count_cases(self, version: str, *, split: str | None = None) -> int:
        args: list[Any] = [_clean_version(version)]
        clause = ""
        if split is not None:
            if split not in GOLDEN_SPLITS:
                raise ValueError(f"split {split!r} is not in {sorted(GOLDEN_SPLITS)}")
            args.append(split)
            clause = f" AND split = ${len(args)}"
        async with self._pool.acquire() as conn:
            return int(
                await conn.fetchval(
                    f"SELECT count(*) FROM kb_eval_golden WHERE version = $1{clause}", *args
                )
            )

    async def set_split(
        self, version: str, case_ids: list[str], split: str
    ) -> int:
        """Promote/demote rows between frozen and growth; every id must exist.

        Promotion to ``frozen`` is the review gate PRD T0-#2 asks for: it is
        an explicit, caller-authorized write, never a side effect of import.
        """
        if split not in GOLDEN_SPLITS:
            raise ValueError(f"split {split!r} is not in {sorted(GOLDEN_SPLITS)}")
        ids = [str(case_id).strip() for case_id in case_ids if str(case_id).strip()]
        if not ids:
            raise ValueError("set_split requires at least one case_id")
        resolved = _clean_version(version)
        async with self._pool.acquire() as conn, conn.transaction():
            present = {
                record["case_id"]
                for record in await conn.fetch(
                    "SELECT case_id FROM kb_eval_golden WHERE version = $1 AND case_id = ANY($2::text[])",
                    resolved,
                    ids,
                )
            }
            missing = sorted(set(ids) - present)
            if missing:
                raise GoldenStoreError(
                    f"set_split: case_ids not in version {resolved!r}: {missing}"
                )
            await conn.execute(
                """
                UPDATE kb_eval_golden
                SET split = $3, updated_at = NOW()
                WHERE version = $1 AND case_id = ANY($2::text[])
                """,
                resolved,
                ids,
                split,
            )
        return len(set(ids))

    async def pin_release(
        self, version: str, *, release_key: str = DEFAULT_RELEASE_KEY, note: str = ""
    ) -> None:
        """Point ``release_key`` at ``version``; the version must have rows."""
        resolved = _clean_version(version)
        if not release_key.strip():
            raise ValueError("release_key must be non-empty")
        if await self.count_cases(resolved) == 0:
            raise GoldenStoreError(f"cannot pin empty golden version {resolved!r}")
        async with self._pool.acquire() as conn:
            await conn.execute(
                """
                INSERT INTO kb_eval_golden_release (release_key, version, note, set_at)
                VALUES ($1, $2, $3, NOW())
                ON CONFLICT (release_key) DO UPDATE SET
                    version = EXCLUDED.version,
                    note = EXCLUDED.note,
                    set_at = NOW()
                """,
                release_key.strip(),
                resolved,
                note,
            )
        logger.info("pinned golden release %s -> %s", release_key, resolved)

    async def get_release(self, release_key: str = DEFAULT_RELEASE_KEY) -> str | None:
        async with self._pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT version FROM kb_eval_golden_release WHERE release_key = $1",
                release_key,
            )
        return row["version"] if row else None
