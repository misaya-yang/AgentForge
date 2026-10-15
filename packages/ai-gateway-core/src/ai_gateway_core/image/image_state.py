"""Image multi-turn state persistence — backed by Postgres.

Tables (see ``database/migrations/per_service/assistant/001_image_session_artifacts.sql``):

* ``assistant.image_sessions`` — focused (owner, session) state mirror with
  ``latest_artifact_id`` pointer + ``locked_style``.
* ``assistant.image_turns``    — per-turn audit trail (prompt, parent, output,
  status, error).
* ``assistant.image_idempotency`` — dedup of ``client_request_id`` within an
  ``owner_scope``.

This module is intentionally a thin layer of explicit asyncpg calls — no
ORM. Callers pass an ``asyncpg``-style pool (``database._pool``) and get
back plain dicts. Concurrency control: ``advance_latest_artifact_cas`` is
the ``image_sessions`` compare-and-swap used for the
``expected_parent_artifact_id`` race-detection contract.
"""

from __future__ import annotations

import functools
import hashlib
import json
import logging
import uuid
from datetime import datetime
from typing import Any

logger = logging.getLogger(__name__)


def _db_safe(default):
    """Wrap async DB helpers so a non-asyncpg pool (test mocks, missing DB,
    transient outage) degrades to ``default`` instead of raising.

    ``default`` may be a callable factory for mutable defaults (lists/tuples).
    Real CAS conflicts inside the function still surface as ``False``; only
    accidental TypeError / AttributeError / asyncpg errors get swallowed."""
    def deco(fn):
        @functools.wraps(fn)
        async def wrapper(*args, **kwargs):
            try:
                return await fn(*args, **kwargs)
            except Exception as exc:  # noqa: BLE001
                logger.warning("%s: DB unavailable (%s) — fallback", fn.__name__, exc)
                return default() if callable(default) else default
        return wrapper
    return deco


# ----- Owner scope ---------------------------------------------------------

_OWNER_SCOPE_SEP = "\x1f"  # ASCII unit separator — not a legal identifier char


def compute_owner_scope(
    user_id: str,
    *,
    app_tenant_id: str | None = None,
    app_user_id: str | None = None,
) -> str:
    """Compute the opaque owner-scope identity key.

    Format:
      * Both ``app_tenant_id`` AND ``app_user_id`` present:
          ``f"{user_id}\\x1f{app_tenant_id}\\x1f{app_user_id}"``
        — separator is ASCII unit separator (0x1F), which is not a legal
        character in tenant / user identifiers in any of our backends. Using
        a printable separator like ``|`` would let a caller forge collisions
        by embedding the separator in their identifiers (e.g. send
        ``app_user_id="alice|admin"`` to align with another caller's scope).
      * Else: fallback to ``user_id`` (legacy single-tenant flow).

    Treat the returned string as opaque — never parse, never display.
    """
    if app_tenant_id and app_user_id:
        if _OWNER_SCOPE_SEP in app_tenant_id or _OWNER_SCOPE_SEP in app_user_id:
            # Defense-in-depth: should never happen since our headers are
            # http-cleaned, but if a control char ever slips through reject
            # the request rather than poison the scope namespace.
            raise ValueError(
                "app_tenant_id / app_user_id must not contain control characters"
            )
        return f"{user_id}{_OWNER_SCOPE_SEP}{app_tenant_id}{_OWNER_SCOPE_SEP}{app_user_id}"
    return user_id


# ----- Request hashing -----------------------------------------------------

def compute_request_hash(payload: dict[str, Any]) -> str:
    """Sha256-hex of the canonical JSON encoding of a request payload.

    Used by the idempotency layer: same ``client_request_id`` + same
    ``request_hash`` → idempotent replay returns the original task_id.
    Same client_request_id + *different* hash → 409, the caller is
    re-using the id with new content.

    ``client_request_id`` itself is excluded from the hash so replay of
    the original request is detected.
    """
    cleaned = {k: v for k, v in payload.items() if k != "client_request_id"}
    canonical = json.dumps(cleaned, sort_keys=True, default=str)
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def new_turn_id() -> str:
    return f"itn_{uuid.uuid4().hex[:16]}"


# ----- image_sessions ------------------------------------------------------

@_db_safe(None)
async def get_image_session(pool, session_id: str) -> dict | None:
    """Return the image_sessions row for ``session_id`` (or None)."""
    if pool is None:
        return None
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT * FROM assistant.image_sessions WHERE session_id = $1",
            session_id,
        )
    return dict(row) if row else None


@_db_safe(None)
async def upsert_image_session(
    pool,
    *,
    session_id: str,
    owner_scope: str,
    app_user_id: str | None,
    app_tenant_id: str | None,
    locked_style: str | None = None,
) -> None:
    """Create-or-touch an image_sessions row.

    Does NOT change ``latest_artifact_id`` — that's owned by
    ``advance_latest_artifact_cas``. Style writes are explicit via
    ``set_locked_style``.
    """
    if pool is None:
        return
    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO assistant.image_sessions
                (session_id, owner_scope, app_user_id, app_tenant_id, locked_style)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (session_id) DO UPDATE SET
                updated_at = NOW(),
                locked_style = COALESCE(EXCLUDED.locked_style, assistant.image_sessions.locked_style)
            """,
            session_id,
            owner_scope,
            app_user_id,
            app_tenant_id,
            locked_style,
        )


@_db_safe(None)
async def set_locked_style(pool, session_id: str, style: str | None) -> None:
    """Overwrite ``locked_style``. ``None`` clears the lock."""
    if pool is None:
        return
    async with pool.acquire() as conn:
        await conn.execute(
            "UPDATE assistant.image_sessions SET locked_style = $1, updated_at = NOW() "
            "WHERE session_id = $2",
            style,
            session_id,
        )


@_db_safe(False)
async def advance_latest_artifact_cas(
    pool,
    *,
    session_id: str,
    expected_parent: str | None,
    new_artifact_id: str,
) -> bool:
    """Compare-and-swap update of ``latest_artifact_id``.

    Returns True iff the row's current ``latest_artifact_id`` equals
    ``expected_parent`` (NULL-safe) and the swap succeeded. The caller is
    expected to have *just* generated against the parent; if a competing
    request overwrote ``latest_artifact_id`` in the meantime we leave the
    pointer pointing at the racer's output and return False.

    NOTE: callers must have created the image_sessions row first via
    ``upsert_image_session``. We use SELECT FOR UPDATE inside a tx to
    serialize concurrent writers on the same session_id.
    """
    if pool is None:
        return False
    async with pool.acquire() as conn, conn.transaction():
        row = await conn.fetchrow(
            "SELECT latest_artifact_id FROM assistant.image_sessions "
            "WHERE session_id = $1 FOR UPDATE",
            session_id,
        )
        if row is None:
            # Row missing — caller forgot to upsert. Refuse to silently
            # create here; CAS semantics demand the row exist.
            return False
        current = row["latest_artifact_id"]
        if current != expected_parent:
            return False
        await conn.execute(
            "UPDATE assistant.image_sessions "
            "SET latest_artifact_id = $1, updated_at = NOW() "
            "WHERE session_id = $2",
            new_artifact_id,
            session_id,
        )
        return True


# ----- image_blobs ---------------------------------------------------------


async def create_image_blob(
    pool,
    *,
    blob_id: str,
    owner_scope: str,
    content_sha256: str | None,
    byte_size: int | None,
    mime_type: str,
    storage_key: str,
    source: str,
    status: str,
    artifact_id: str | None = None,
    error: str | None = None,
) -> bool:
    """Create or update an image blob pointer.

    Real database failures intentionally propagate. Test/mock pools degrade to
    ``False`` so route-unit tests can keep patching only the functions they
    exercise.
    """
    if pool is None:
        return False
    try:
        async with pool.acquire() as conn:
            result = await conn.execute(
                """
                INSERT INTO assistant.image_blobs (
                    blob_id, owner_scope, content_sha256, byte_size, mime_type,
                    storage_key, source, status, artifact_id, error
                ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                ON CONFLICT (blob_id) DO UPDATE SET
                    content_sha256 = COALESCE(EXCLUDED.content_sha256, assistant.image_blobs.content_sha256),
                    byte_size = COALESCE(EXCLUDED.byte_size, assistant.image_blobs.byte_size),
                    mime_type = EXCLUDED.mime_type,
                    storage_key = EXCLUDED.storage_key,
                    source = EXCLUDED.source,
                    status = EXCLUDED.status,
                    artifact_id = COALESCE(EXCLUDED.artifact_id, assistant.image_blobs.artifact_id),
                    error = EXCLUDED.error,
                    completed_at = CASE
                        WHEN EXCLUDED.status = 'ready' THEN NOW()
                        ELSE assistant.image_blobs.completed_at
                    END,
                    updated_at = NOW()
                """,
                blob_id, owner_scope, content_sha256, byte_size, mime_type,
                storage_key, source, status, artifact_id, error,
            )
    except (TypeError, AttributeError) as exc:
        logger.debug("create_image_blob: non-real pool (%s) — False", exc)
        return False
    try:
        return result.endswith(" 1")
    except AttributeError:
        return True


async def get_image_blob(
    pool,
    *,
    blob_id: str,
    owner_scope: str | None = None,
) -> dict | None:
    if pool is None:
        return None
    try:
        async with pool.acquire() as conn:
            if owner_scope is None:
                row = await conn.fetchrow(
                    "SELECT * FROM assistant.image_blobs WHERE blob_id = $1",
                    blob_id,
                )
            else:
                row = await conn.fetchrow(
                    "SELECT * FROM assistant.image_blobs "
                    "WHERE blob_id = $1 AND owner_scope = $2",
                    blob_id, owner_scope,
                )
    except (TypeError, AttributeError) as exc:
        logger.debug("get_image_blob: non-real pool (%s) — None", exc)
        return None
    return dict(row) if row else None


async def update_image_blob_status(
    pool,
    *,
    blob_id: str,
    owner_scope: str,
    status: str,
    content_sha256: str | None = None,
    byte_size: int | None = None,
    mime_type: str | None = None,
    artifact_id: str | None = None,
    error: str | None = None,
) -> bool:
    if pool is None:
        return False
    try:
        async with pool.acquire() as conn:
            result = await conn.execute(
                """
                UPDATE assistant.image_blobs SET
                    status = $3::varchar,
                    content_sha256 = COALESCE($4::varchar, content_sha256),
                    byte_size = COALESCE($5, byte_size),
                    mime_type = COALESCE($6::varchar, mime_type),
                    artifact_id = COALESCE($7::varchar, artifact_id),
                    error = $8::text,
                    completed_at = CASE WHEN $3::varchar = 'ready' THEN NOW() ELSE completed_at END,
                    updated_at = NOW()
                WHERE blob_id = $1 AND owner_scope = $2
                """,
                blob_id, owner_scope, status, content_sha256, byte_size,
                mime_type, artifact_id, error,
            )
    except (TypeError, AttributeError) as exc:
        logger.debug("update_image_blob_status: non-real pool (%s) — False", exc)
        return False
    try:
        return result.endswith(" 1")
    except AttributeError:
        return True


# ----- image_turns ---------------------------------------------------------

@_db_safe(None)
async def insert_turn(
    pool,
    *,
    turn_id: str,
    session_id: str,
    owner_scope: str,
    task_id: str | None,
    prompt: str | None,
    model_id: str | None,
    style: str | None,
    add_watermark: bool,
    parent_artifact_id: str | None,
    output_artifact_id: str | None,
    status: str,
    error: str | None,
    error_code: str | None,
    client_request_id: str | None,
    request_hash: str | None,
    completed_at: datetime | None = None,
    thought_signature: str | None = None,
    provider_text: str | None = None,
    output_artifact_ids: list[str] | None = None,
    state: str | None = None,
) -> None:
    if pool is None:
        logger.warning("insert_turn: pool is None, turn %s not persisted", turn_id)
        return
    _state = state or status   # Resolve before SQL to avoid COALESCE type mismatch
    async with pool.acquire() as conn:
        await conn.execute(
            """
            INSERT INTO assistant.image_turns (
                turn_id, session_id, owner_scope, task_id, prompt, model_id,
                style, add_watermark, parent_artifact_id, output_artifact_id,
                status, error, error_code, client_request_id, request_hash,
                completed_at, thought_signature, provider_text,
                output_artifact_ids, state
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                $17, $18, $19::jsonb, $20
            )
            ON CONFLICT (turn_id) DO UPDATE SET
                status = EXCLUDED.status,
                error = EXCLUDED.error,
                error_code = EXCLUDED.error_code,
                output_artifact_id = COALESCE(EXCLUDED.output_artifact_id, assistant.image_turns.output_artifact_id),
                completed_at = COALESCE(EXCLUDED.completed_at, assistant.image_turns.completed_at),
                thought_signature = COALESCE(EXCLUDED.thought_signature, assistant.image_turns.thought_signature),
                provider_text = COALESCE(EXCLUDED.provider_text, assistant.image_turns.provider_text),
                output_artifact_ids = COALESCE(EXCLUDED.output_artifact_ids, assistant.image_turns.output_artifact_ids),
                state = COALESCE(EXCLUDED.state, EXCLUDED.status)
            """,
            turn_id, session_id, owner_scope, task_id, prompt, model_id,
            style, add_watermark, parent_artifact_id, output_artifact_id,
            status, error, error_code, client_request_id, request_hash,
            completed_at, thought_signature, provider_text,
            json.dumps(output_artifact_ids) if output_artifact_ids is not None else None,
            _state,
        )


@_db_safe(None)
async def update_turn_status(
    pool,
    *,
    turn_id: str,
    status: str,
    output_artifact_id: str | None = None,
    error: str | None = None,
    error_code: str | None = None,
) -> None:
    if pool is None:
        return
    async with pool.acquire() as conn:
        await conn.execute(
            """
            UPDATE assistant.image_turns SET
                status = $2::varchar,
                output_artifact_id = COALESCE($3::varchar, output_artifact_id),
                error = $4::text,
                error_code = $5::varchar,
                completed_at = CASE
                    WHEN $2::varchar IN ('completed', 'failed', 'unknown') THEN NOW()
                    ELSE completed_at
                END
            WHERE turn_id = $1
            """,
            turn_id, status, output_artifact_id, error, error_code,
        )


@_db_safe(None)
async def get_turn(pool, turn_id: str) -> dict | None:
    if pool is None:
        return None
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT * FROM assistant.image_turns WHERE turn_id = $1",
            turn_id,
        )
    return dict(row) if row else None


@_db_safe(None)
async def get_turn_by_task(pool, task_id: str) -> dict | None:
    """Look up a turn by its async task_id (poll fallback)."""
    if pool is None:
        return None
    async with pool.acquire() as conn:
        row = await conn.fetchrow(
            "SELECT * FROM assistant.image_turns WHERE task_id = $1 "
            "ORDER BY created_at DESC LIMIT 1",
            task_id,
        )
    return dict(row) if row else None


@_db_safe(lambda: ([], None))
async def list_turns(
    pool,
    *,
    session_id: str,
    owner_scope: str,
    limit: int = 50,
    cursor: str | None = None,
) -> tuple[list[dict], str | None]:
    """Return (turns, next_cursor). Cursor is the ``created_at`` ISO of the
    last row returned — pagination is "older than this timestamp".

    Tied with ``turn_id`` for stable ordering when timestamps collide.
    """
    if pool is None:
        return [], None

    args: list[Any] = [session_id, owner_scope]
    where_extra = ""
    if cursor:
        # cursor format "ISO|turn_id" — keysetted on (created_at DESC, turn_id DESC)
        try:
            parts = cursor.split("|", 1)
            cur_ts = datetime.fromisoformat(parts[0])
            cur_turn = parts[1] if len(parts) > 1 else ""
            args.extend([cur_ts, cur_turn])
            where_extra = "AND (created_at, turn_id) < ($3, $4)"
        except Exception:
            logger.warning("Invalid cursor %r — ignoring", cursor)

    sql = f"""
        SELECT * FROM assistant.image_turns
        WHERE session_id = $1 AND owner_scope = $2
        {where_extra}
        ORDER BY created_at DESC, turn_id DESC
        LIMIT {int(min(max(limit, 1), 200)) + 1}
    """
    async with pool.acquire() as conn:
        rows = await conn.fetch(sql, *args)

    rows_d = [dict(r) for r in rows]
    next_cursor: str | None = None
    requested = int(min(max(limit, 1), 200))
    if len(rows_d) > requested:
        last = rows_d[requested - 1]
        next_cursor = f"{last['created_at'].isoformat()}|{last['turn_id']}"
        rows_d = rows_d[:requested]
    return rows_d, next_cursor


# ----- image_tasks ---------------------------------------------------------


async def create_image_task(
    pool,
    *,
    task_id: str,
    owner_scope: str,
    status: str,
    prompt: str,
    model_id: str,
    request_payload: dict[str, Any],
    progress: int = 0,
    provider: str | None = None,
    turn_id: str | None = None,
    session_id: str | None = None,
    parent_artifact_id: str | None = None,
    output_artifact_id: str | None = None,
    client_request_id: str | None = None,
    request_hash: str | None = None,
) -> bool:
    if pool is None:
        return False
    try:
        async with pool.acquire() as conn:
            result = await conn.execute(
                """
                INSERT INTO assistant.image_tasks (
                    task_id, owner_scope, status, prompt, model_id,
                    request_payload, progress, provider, turn_id, session_id,
                    parent_artifact_id, output_artifact_id, client_request_id,
                    request_hash
                ) VALUES (
                    $1, $2, $3, $4, $5, $6::jsonb, $7, $8, $9, $10, $11, $12, $13, $14
                )
                ON CONFLICT (task_id) DO NOTHING
                """,
                task_id, owner_scope, status, prompt, model_id,
                json.dumps(request_payload, default=str), progress, provider,
                turn_id, session_id, parent_artifact_id, output_artifact_id,
                client_request_id, request_hash,
            )
    except (TypeError, AttributeError) as exc:
        logger.debug("create_image_task: non-real pool (%s) — False", exc)
        return False
    try:
        return result.endswith(" 1")
    except AttributeError:
        return True


async def reserve_scoped_image_task(
    pool,
    *,
    task_id: str,
    tenant_id: str,
    user_id: str,
    owner_scope: str,
    status: str,
    prompt: str,
    model_id: str,
    request_payload: dict[str, Any],
    turn_id: str,
    session_id: str,
    parent_artifact_id: str | None,
    client_request_id: str | None,
    request_hash: str,
    progress: int = 0,
) -> dict[str, str]:
    """Atomically reserve an idempotency key and its durable image task.

    Returning an existing task never creates a second task row.  A failed task
    insert rolls back the idempotency reservation, preventing a permanent
    ``duplicate_request_in_flight`` tombstone after a transient database error.
    """

    if pool is None:
        return {"state": "unavailable", "task_id": task_id}
    if (
        not tenant_id
        or not user_id
        or not owner_scope
        or status not in {"pending", "running"}
        or not request_hash
    ):
        raise ValueError("image task scope is invalid")
    async with pool.acquire() as conn, conn.transaction():
        if client_request_id:
            reservation = await conn.fetchrow(
                """
                INSERT INTO assistant.image_idempotency
                    (owner_scope, client_request_id, request_hash, task_id)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (owner_scope, client_request_id) DO NOTHING
                RETURNING request_hash, task_id
                """,
                owner_scope,
                client_request_id,
                request_hash,
                task_id,
            )
            if reservation is None:
                existing = await conn.fetchrow(
                    """
                    SELECT request_hash, task_id
                      FROM assistant.image_idempotency
                     WHERE owner_scope=$1 AND client_request_id=$2
                    """,
                    owner_scope,
                    client_request_id,
                )
                if existing is None:
                    raise RuntimeError("image idempotency reservation disappeared")
                return {
                    "state": "existing"
                    if str(existing["request_hash"]) == request_hash
                    else "conflict",
                    "task_id": str(existing["task_id"]),
                }

        inserted = await conn.fetchrow(
            """
            INSERT INTO assistant.image_tasks (
                task_id, tenant_id, user_id, runtime_scope_version,
                owner_scope, status, prompt, model_id, request_payload,
                progress, turn_id, session_id, parent_artifact_id,
                client_request_id, request_hash
            ) VALUES (
                $1, $2, $3, 1, $4, $5, $6, $7, $8::jsonb,
                $9, $10, $11, $12, $13, $14
            )
            ON CONFLICT (task_id) DO NOTHING
            RETURNING task_id
            """,
            task_id,
            tenant_id,
            user_id,
            owner_scope,
            status,
            prompt,
            model_id,
            json.dumps(request_payload, default=str),
            progress,
            turn_id,
            session_id,
            parent_artifact_id,
            client_request_id,
            request_hash,
        )
        if inserted is None:
            raise RuntimeError("image task id collision")
    return {"state": "reserved", "task_id": task_id}


async def update_image_task(
    pool,
    *,
    task_id: str,
    status: str | None = None,
    progress: int | None = None,
    provider: str | None = None,
    result: dict[str, Any] | None = None,
    error: str | None = None,
    error_code: str | None = None,
    parent_artifact_id: str | None = None,
    output_artifact_id: str | None = None,
    locked_seconds: int | None = None,
    increment_attempt: bool = False,
) -> bool:
    if pool is None:
        return False
    try:
        async with pool.acquire() as conn:
            result_status = await conn.execute(
                """
                UPDATE assistant.image_tasks SET
                    status = COALESCE($2::varchar, status),
                    progress = COALESCE($3, progress),
                    provider = COALESCE($4::varchar, provider),
                    result = COALESCE($5::jsonb, result),
                    error = $6::text,
                    error_code = $7::varchar,
                    parent_artifact_id = COALESCE($8::varchar, parent_artifact_id),
                    output_artifact_id = COALESCE($9::varchar, output_artifact_id),
                    locked_until = CASE
                        WHEN $10::int IS NULL THEN locked_until
                        ELSE NOW() + ($10::int * INTERVAL '1 second')
                    END,
                    attempt_count = attempt_count + CASE WHEN $11 THEN 1 ELSE 0 END,
                    started_at = CASE
                        WHEN $2 = 'running' AND started_at IS NULL THEN NOW()
                        ELSE started_at
                    END,
                    completed_at = CASE
                        WHEN $2 IN ('completed', 'failed', 'dead_letter', 'unknown') THEN NOW()
                        ELSE completed_at
                    END,
                    updated_at = NOW()
                WHERE task_id = $1
                """,
                task_id, status, progress, provider,
                json.dumps(result, default=str) if result is not None else None,
                error, error_code, parent_artifact_id, output_artifact_id,
                locked_seconds, increment_attempt,
            )
    except (TypeError, AttributeError) as exc:
        logger.debug("update_image_task: non-real pool (%s) — False", exc)
        return False
    try:
        return result_status.endswith(" 1")
    except AttributeError:
        return True


async def get_image_task(pool, task_id: str) -> dict | None:
    if pool is None:
        return None
    try:
        async with pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT * FROM assistant.image_tasks WHERE task_id = $1",
                task_id,
            )
    except (TypeError, AttributeError) as exc:
        logger.debug("get_image_task: non-real pool (%s) — None", exc)
        return None
    return dict(row) if row else None


async def count_active_image_tasks(
    pool,
    *,
    owner_scope: str | None = None,
) -> int | None:
    if pool is None:
        return None
    try:
        async with pool.acquire() as conn:
            if owner_scope is None:
                value = await conn.fetchval(
                    "SELECT COUNT(*) FROM assistant.image_tasks "
                    "WHERE status IN ('pending', 'running')",
                )
            else:
                value = await conn.fetchval(
                    "SELECT COUNT(*) FROM assistant.image_tasks "
                    "WHERE owner_scope = $1 AND status IN ('pending', 'running')",
                    owner_scope,
                )
    except (TypeError, AttributeError) as exc:
        logger.debug("count_active_image_tasks: non-real pool (%s) — None", exc)
        return None
    return int(value or 0)


# ----- Idempotency --------------------------------------------------------

async def lookup_idempotent(
    pool,
    *,
    owner_scope: str,
    client_request_id: str,
) -> dict | None:
    """Return the recorded (request_hash, task_id) for this key, or None.

    NOT wrapped in ``_db_safe`` — idempotency is wallet-critical. If the DB
    is unreachable we MUST surface that (caller should 503 + retry) rather
    than silently fall through to ``run anyway`` which doubles charges. The
    only swallowed case is ``pool is None`` (test mocks / dev without DB).
    """
    if pool is None:
        return None
    try:
        async with pool.acquire() as conn:
            row = await conn.fetchrow(
                "SELECT request_hash, task_id, created_at FROM assistant.image_idempotency "
                "WHERE owner_scope = $1 AND client_request_id = $2",
                owner_scope, client_request_id,
            )
    except (TypeError, AttributeError) as exc:
        # Mock pool in tests — surfaces as TypeError ('await' on non-coroutine)
        # or AttributeError. Production asyncpg failures are PostgresError
        # subclasses or OSError → we let those propagate.
        logger.debug("lookup_idempotent: non-real pool (%s) — None", exc)
        return None
    return dict(row) if row else None


async def record_idempotent(
    pool,
    *,
    owner_scope: str,
    client_request_id: str,
    request_hash: str,
    task_id: str,
) -> bool:
    """Record an idempotent claim. Returns True on insert, False on existing.

    NOT wrapped in ``_db_safe`` — see ``lookup_idempotent`` for the rationale.
    A real Postgres error MUST propagate so the route returns 5xx and the
    caller retries. Silent ``return False`` on transient errors causes the
    route to fall through to ``run anyway`` → double-spend.
    """
    if pool is None:
        return False
    try:
        async with pool.acquire() as conn:
            result = await conn.execute(
                """
                INSERT INTO assistant.image_idempotency
                    (owner_scope, client_request_id, request_hash, task_id)
                VALUES ($1, $2, $3, $4)
                ON CONFLICT (owner_scope, client_request_id) DO NOTHING
                """,
                owner_scope, client_request_id, request_hash, task_id,
            )
    except (TypeError, AttributeError) as exc:
        # Mock pool in tests — surfaces as TypeError on awaiting MagicMock or
        # similar. Production asyncpg.PostgresError / OSError must propagate
        # so the route surfaces 5xx and the caller retries.
        logger.debug("record_idempotent: non-real pool (%s) — False", exc)
        return False
    # asyncpg "INSERT 0 1" / "INSERT 0 0"
    try:
        return result.endswith(" 1")
    except AttributeError:
        return True


__all__ = [
    "advance_latest_artifact_cas",
    "count_active_image_tasks",
    "compute_owner_scope",
    "compute_request_hash",
    "create_image_blob",
    "create_image_task",
    "get_image_blob",
    "get_image_session",
    "get_image_task",
    "get_turn",
    "get_turn_by_task",
    "insert_turn",
    "list_turns",
    "lookup_idempotent",
    "new_turn_id",
    "record_idempotent",
    "reserve_scoped_image_task",
    "set_locked_style",
    "update_image_blob_status",
    "update_image_task",
    "update_turn_status",
    "upsert_image_session",
]
