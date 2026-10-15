"""
Artifact Share Manager — frozen public and internal agent artifact links.

A share is a frozen snapshot: `payload` (public content) + `answer_keys`
(grading data, never served). kind='quiz' shares grade via QuizGrader and
persist attempts into quiz_attempts for continuity with the legacy share
flow; other kinds only bump attempt_count until a per-kind attempt store
is designed.
"""

from __future__ import annotations

import hashlib
import html
import json
import logging
import re
import secrets
import uuid
from datetime import datetime, timedelta, timezone
from typing import TYPE_CHECKING, Any

from ai_gateway_core.logging import record_internal_exception

if TYPE_CHECKING:
    from ai_gateway_core.persistence import DatabaseStorageLike

logger = logging.getLogger(__name__)

SHARE_CODE_LENGTH = 12
MAX_DISPLAY_NAME_LEN = 100
UNTIMED_ATTEMPT_TOKEN_TTL_MINUTES = 60
ATTEMPT_TOKEN_RETENTION_HOURS = 24
ATTEMPT_TOKEN_CLEANUP_BATCH_SIZE = 100
DISPLAY_NAME_RE = re.compile(r"^[\w\s一-鿿؀-ۿ.\-@]+$", re.UNICODE)


class ArtifactShareError(ValueError):
    """Base class for stable artifact-share failures."""

    code = "artifact_share_error"


class ShareUnavailableError(ArtifactShareError):
    """The public share is missing, inactive, or expired."""

    code = "share_unavailable"


class AttemptLimitReachedError(ArtifactShareError):
    """The share has no remaining attempt slots."""

    code = "attempt_limit_reached"


class AttemptInputError(ArtifactShareError):
    """The attempt input or token is invalid."""

    code = "attempt_input_invalid"


class AttemptConflictError(ArtifactShareError):
    """The attempt conflicts with an already consumed identity or token."""

    code = "attempt_conflict"


_SQLSTATE_ERRORS: dict[str, type[ArtifactShareError]] = {
    "P4040": ShareUnavailableError,
    "P4290": AttemptLimitReachedError,
    "P4000": AttemptInputError,
    "P4090": AttemptConflictError,
}


def _sanitize_display_name(name: str | None) -> str | None:
    """Sanitize display_name: strip, length-limit, reject suspicious chars."""
    if not name:
        return None
    stripped = name.strip()
    if len(stripped) > MAX_DISPLAY_NAME_LEN:
        raise AttemptInputError("Display name is too long")
    if not stripped or not DISPLAY_NAME_RE.match(stripped):
        raise AttemptInputError("Display name contains unsupported characters")
    return html.escape(stripped)


def _token_hash(
    token: str, *, audience: str = "public", user_id: str | None = None,
) -> str:
    if audience == "internal":
        if not user_id:
            raise AttemptInputError("A signed-in account is required for this attempt")
        token = f"internal:{user_id}\0{token}"
    return hashlib.sha256(token.encode("utf-8")).hexdigest()


def _raise_typed_database_error(exc: Exception) -> None:
    error_type = _SQLSTATE_ERRORS.get(str(getattr(exc, "sqlstate", "")))
    if error_type is not None:
        raise error_type(str(exc)) from None
    raise exc


class ArtifactShareManager:
    """Manages kind-generic artifact shares after route-level access checks."""

    def __init__(self, db: DatabaseStorageLike) -> None:
        self.db = db

    async def create_share(
        self,
        *,
        kind: str,
        title: str,
        payload: dict[str, Any],
        answer_keys: dict[str, Any] | list[Any] | None = None,
        tenant_id: str | None = None,
        user_id: str | None = None,
        expires_hours: int | None = None,
        max_attempts: int | None = None,
        require_name: bool = True,
        time_limit_minutes: int | None = None,
        audience: str = "public",
        source_scope: dict[str, Any] | None = None,
    ) -> dict:
        """Create a shareable link with a frozen payload snapshot."""
        if audience not in {"public", "internal"} or (audience == "internal") != (source_scope is not None):
            raise ValueError("Invalid share audience or source scope")
        if "source_scope" in payload:
            raise ValueError("Source scope must not be stored in a share payload")
        share_code = secrets.token_urlsafe(SHARE_CODE_LENGTH)[:SHARE_CODE_LENGTH]
        share_id = str(uuid.uuid4())
        now = datetime.now(timezone.utc)
        expires_at = now + timedelta(hours=expires_hours) if expires_hours else None

        columns = (
            "id, share_code, kind, title, payload, answer_keys, tenant_id, created_by, "
            "is_active, max_attempts, expires_at, require_name, time_limit_minutes, created_at"
        )
        values = "$1, $2, $3, $4, $5, $6, $7, $8, TRUE, $9, $10, $11, $12, $13"
        arguments: tuple[Any, ...] = (
            uuid.UUID(share_id),
            share_code,
            kind,
            title,
            json.dumps(payload),
            json.dumps(answer_keys) if answer_keys is not None else None,
            tenant_id,
            user_id,
            max_attempts,
            expires_at,
            require_name,
            time_limit_minutes,
            now,
        )
        if audience == "internal":
            columns += ", audience, source_scope"
            values += ", $14, $15::jsonb"
            arguments += (audience, json.dumps(source_scope))
        await self.db.execute(
            f"INSERT INTO assistant.artifact_shares ({columns}) VALUES ({values})",
            *arguments,
        )

        logger.info("Created %s share %s by %s", kind, share_code, user_id or "anonymous")
        return {
            "share_id": share_id,
            "share_code": share_code,
            "kind": kind,
            "title": title,
            "expires_at": expires_at.isoformat() if expires_at else None,
            "require_name": require_name,
            "max_attempts": max_attempts,
            "time_limit_minutes": time_limit_minutes,
            "audience": audience,
        }

    async def get_share_by_code(self, share_code: str) -> dict | None:
        """Look up an active share by code with expiry enforced."""
        row = await self.db.fetchrow(
            """
            SELECT id, share_code, kind, title, payload, answer_keys, audience,
                   tenant_id, created_by, is_active, max_attempts, expires_at,
                   require_name, time_limit_minutes, attempt_count, created_at
            FROM assistant.artifact_shares
            WHERE share_code = $1 AND is_active = TRUE
            """,
            share_code,
        )
        if not row:
            return None
        if row["expires_at"] and row["expires_at"] < datetime.now(timezone.utc):
            return None

        payload = row["payload"]
        if isinstance(payload, str):
            payload = json.loads(payload)
        return {
            "share_id": str(row["id"]),
            "share_code": row["share_code"],
            "kind": row["kind"],
            "title": row["title"],
            "payload": payload,
            "answer_keys": row["answer_keys"],
            "tenant_id": row["tenant_id"],
            "created_by": row["created_by"],
            "require_name": row["require_name"],
            "expires_at": row["expires_at"].isoformat() if row["expires_at"] else None,
            "time_limit_minutes": row["time_limit_minutes"],
            "max_attempts": row["max_attempts"],
            "attempt_count": row["attempt_count"],
            "created_at": row["created_at"],
            "audience": row.get("audience") or "public",
        }

    async def get_public_artifact(self, share_code: str, *, audience: str = "public") -> dict | None:
        """Audience-matched snapshot: payload only, never answer keys or sources."""
        share = await self.get_share_by_code(share_code)
        if not share or share["audience"] != audience:
            return None
        if share["max_attempts"] is not None and share["attempt_count"] >= share["max_attempts"]:
            return None
        public_payload = {
            key: value for key, value in share["payload"].items()
            if key not in {"source_scope", "answer_keys", "audience", "tenant_id", "created_by"}
        }
        return {
            **public_payload,
            "share_code": share["share_code"],
            "kind": share["kind"],
            "title": share["title"],
            "require_name": share["require_name"],
            "time_limit_minutes": share["time_limit_minutes"],
            "audience": share["audience"],
        }

    async def start_attempt(
        self, share_code: str, *, audience: str = "public", user_id: str | None = None,
    ) -> dict[str, Any]:
        """Issue a short-lived, opaque token whose clock starts now.

        Tokens are stored only as SHA-256 digests. Starting an attempt does not
        consume a capped attempt slot; the atomic submission transaction owns
        that decision so abandoned starts do not reduce the remaining quota.
        """
        share = await self.get_share_by_code(share_code)
        if not share or share["audience"] != audience:
            raise ShareUnavailableError("Share not found or expired")
        if (
            share["max_attempts"] is not None
            and share["attempt_count"] >= share["max_attempts"]
        ):
            raise AttemptLimitReachedError("Maximum attempts reached")

        started_at = datetime.now(timezone.utc)
        ttl_minutes = int(
            share.get("time_limit_minutes") or UNTIMED_ATTEMPT_TOKEN_TTL_MINUTES
        )
        expires_at = started_at + timedelta(minutes=ttl_minutes)
        share_expires_at = share.get("expires_at")
        if isinstance(share_expires_at, str):
            share_expires_at = datetime.fromisoformat(share_expires_at)
        if share_expires_at is not None:
            if share_expires_at.tzinfo is None:
                share_expires_at = share_expires_at.replace(tzinfo=timezone.utc)
            expires_at = min(expires_at, share_expires_at)

        token = secrets.token_urlsafe(32)
        inserted = await self.db.execute(
            """
            WITH stale_tokens AS (
                SELECT id
                FROM assistant.artifact_share_attempt_tokens
                WHERE expires_at < NOW() - ($6 * INTERVAL '1 hour')
                   OR consumed_at < NOW() - ($6 * INTERVAL '1 hour')
                ORDER BY LEAST(expires_at, COALESCE(consumed_at, expires_at))
                LIMIT $7
            ), cleanup AS (
                DELETE FROM assistant.artifact_share_attempt_tokens
                WHERE id IN (SELECT id FROM stale_tokens)
            )
            INSERT INTO assistant.artifact_share_attempt_tokens
                (id, share_id, token_hash, started_at, expires_at)
            SELECT $1, id, $2, $3, $4
            FROM assistant.artifact_shares
            WHERE share_code = $5
              AND is_active = TRUE
              AND (expires_at IS NULL OR expires_at > NOW())
              AND (max_attempts IS NULL OR attempt_count < max_attempts)
            """,
            uuid.uuid4(),
            _token_hash(token, audience=audience, user_id=user_id),
            started_at,
            expires_at,
            share_code,
            ATTEMPT_TOKEN_RETENTION_HOURS,
            ATTEMPT_TOKEN_CLEANUP_BATCH_SIZE,
        )
        if "INSERT 0 1" not in str(inserted):
            # State may have changed after the read (revocation, expiry, or a
            # concurrent final submission). Re-read only to preserve the
            # public 404-vs-429 contract; submission remains authoritative.
            current = await self.get_share_by_code(share_code)
            if (
                current and current["audience"] == audience
                and current["max_attempts"] is not None
                and current["attempt_count"] >= current["max_attempts"]
            ):
                raise AttemptLimitReachedError("Maximum attempts reached")
            raise ShareUnavailableError("Share not found or expired")
        return {
            "attempt_token": token,
            "started_at": started_at,
            "expires_at": expires_at,
        }

    async def get_attempt_result(
        self, share_code: str, attempt_token: str, *, audience: str = "public",
        user_id: str | None = None,
    ) -> dict[str, Any] | None:
        """Recover only the result bound to this active share and opaque token."""
        share = await self.get_share_by_code(share_code)
        if not share or share["audience"] != audience:
            raise ShareUnavailableError("Share not found or expired")
        if share["kind"] != "quiz" or not attempt_token:
            raise AttemptInputError("Attempt token is required")
        row = await self.db.fetchrow(
            "SELECT a.id, a.result_payload FROM assistant.quiz_attempts AS a "
            "JOIN assistant.artifact_share_attempt_tokens AS token "
            "ON token.token_hash = a.attempt_token_hash "
            "WHERE a.share_id = $1 AND token.token_hash = $2",
            uuid.UUID(str(share["share_id"])),
            _token_hash(attempt_token, audience=audience, user_id=user_id),
        )
        if not row:
            return None
        payload = row["result_payload"]
        if isinstance(payload, str):
            payload = json.loads(payload)
        return {"attempt_id": str(row["id"]), **payload} if isinstance(payload, dict) else None

    async def submit_attempt(
        self,
        share_code: str,
        answers: dict[str, str],
        display_name: str | None = None,
        client_ip: str | None = None,
        attempt_token: str | None = None,
        audience: str = "public",
        user_id: str | None = None,
    ) -> dict:
        """Submit an audience-matched attempt against a shared artifact."""
        display_name = _sanitize_display_name(display_name)
        share = await self.get_share_by_code(share_code)
        if not share or share["audience"] != audience:
            raise ShareUnavailableError("Share not found or expired")
        if share["kind"] == "quiz":
            if not attempt_token or not attempt_token.strip():
                raise AttemptInputError("An attempt token is required")
            prior = await self.db.fetchrow(
                "SELECT a.id, a.answers, a.display_name, a.result_payload "
                "FROM assistant.quiz_attempts AS a "
                "JOIN assistant.artifact_share_attempt_tokens AS token "
                "ON token.token_hash = a.attempt_token_hash "
                "WHERE a.share_id = $1 AND token.token_hash = $2",
                uuid.UUID(str(share["share_id"])),
                _token_hash(attempt_token, audience=audience, user_id=user_id),
            )
            if prior:
                old_answers = prior["answers"]
                if isinstance(old_answers, str):
                    old_answers = json.loads(old_answers)
                if old_answers != answers or prior["display_name"] != display_name:
                    raise AttemptConflictError("Attempt token belongs to another answer")
                saved = prior["result_payload"]
                if isinstance(saved, str):
                    saved = json.loads(saved)
                if not isinstance(saved, dict):
                    raise AttemptConflictError("Attempt result is unavailable")
                return {"attempt_id": str(prior["id"]), **saved, "cached": True}
        if share["max_attempts"] is not None and share["attempt_count"] >= share["max_attempts"]:
            raise AttemptLimitReachedError("Maximum attempts reached")
        if share["require_name"] and not display_name:
            raise AttemptInputError("This share requires a name before submitting")
        if share.get("time_limit_minutes") and not attempt_token:
            raise AttemptInputError("An attempt token is required for timed shares")

        result: dict[str, Any] = {}
        if share["kind"] == "quiz":
            graded = await self._grade_quiz(share, answers)
            result = await self._record_quiz_attempt(
                share=share,
                answers=answers,
                graded=graded,
                display_name=display_name,
                client_ip=client_ip,
                attempt_token=attempt_token,
                audience=audience,
                user_id=user_id,
            )
        else:
            # Generic shares do not yet have a per-kind attempt table. Their
            # untimed compatibility path keeps the existing atomic cap update.
            if attempt_token is not None or share.get("time_limit_minutes"):
                raise AttemptInputError("Attempt tokens are only supported for quiz shares")
            await self._reserve_attempt_slot(share)

        logger.info(
            "Public attempt on %s share %s: %s",
            share["kind"],
            share_code,
            result.get("correct_count", "recorded"),
        )
        return result

    async def _claim_display_name(self, share_id: uuid.UUID, display_name: str) -> None:
        claimed = await self.db.execute(
            """
            INSERT INTO assistant.artifact_share_submitters (share_id, display_name)
            VALUES ($1, $2)
            ON CONFLICT (share_id, display_name) DO NOTHING
            """,
            share_id,
            display_name,
        )
        if "INSERT 0 1" not in claimed:
            raise AttemptConflictError(
                f"You have already submitted as '{html.unescape(display_name)}'."
            )

    async def _release_display_name(
        self,
        share_id: uuid.UUID,
        display_name: str | None,
    ) -> None:
        if display_name is None:
            return
        await self.db.execute(
            "DELETE FROM assistant.artifact_share_submitters "
            "WHERE share_id = $1 AND display_name = $2",
            share_id,
            display_name,
        )

    async def _reserve_attempt_slot(self, share: dict) -> None:
        """Atomically consume one attempt slot, enforcing the cap under concurrency.

        The read-side check in ``get_share_by_code`` alone is a TOCTOU window:
        two concurrent submissions could both pass ``attempt_count < max_attempts``
        and both land. The conditional UPDATE closes it — Postgres applies the
        WHERE against the row version at statement time, so exactly one of the
        two succeeds once the cap is one slot away.
        """
        reserved = await self.db.execute(
            """
            UPDATE assistant.artifact_shares
            SET attempt_count = attempt_count + 1
            WHERE id = $1
              AND is_active = TRUE
              AND (expires_at IS NULL OR expires_at > NOW())
              AND (max_attempts IS NULL OR attempt_count < max_attempts)
            """,
            uuid.UUID(share["share_id"]),
        )
        if "UPDATE 1" not in reserved:
            raise AttemptLimitReachedError("Maximum attempts reached")

    async def _grade_quiz(
        self,
        share: dict,
        answers: dict[str, str],
    ) -> dict:
        from ai_gateway_core.quiz.quiz_grader import QuizGrader

        answer_keys = share["answer_keys"]
        if isinstance(answer_keys, str):
            answer_keys = json.loads(answer_keys)

        payload = share["payload"]
        payload_questions = payload.get("questions") if isinstance(payload, dict) else None
        questions_by_id = {
            str(item.get("id")): item
            for item in (payload_questions or [])
            if isinstance(item, dict)
        }
        merged_keys: list[dict[str, Any]] = []
        for item in answer_keys or []:
            if not isinstance(item, dict):
                continue
            source = questions_by_id.get(str(item.get("id")), {})
            merged_keys.append(
                {
                    **item,
                    "question_type": item.get("question_type")
                    or source.get("question_type")
                    or "mc_single",
                    "question_text": item.get("question_text")
                    or source.get("question_text")
                    or "",
                }
            )
        grader = QuizGrader()
        return grader.grade(merged_keys, answers)

    async def _record_quiz_attempt(
        self,
        *,
        share: dict[str, Any],
        answers: dict[str, str],
        graded: dict[str, Any],
        display_name: str | None,
        client_ip: str | None,
        attempt_token: str | None,
        audience: str = "public",
        user_id: str | None = None,
    ) -> dict[str, Any]:
        """Atomically consume identity/token/slot and persist the graded row."""
        payload = share.get("payload") or {}
        quiz_id = payload.get("quiz_id") if isinstance(payload, dict) else None
        if not quiz_id:
            raise AttemptInputError("Shared quiz is missing its quiz id")

        attempt_id = uuid.uuid4()
        try:
            row = await self.db.fetchrow(
                """
                SELECT attempt_id, result_payload
                FROM assistant.record_artifact_share_quiz_attempt_v2(
                    $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11
                )
                """,
                share["share_code"],
                _token_hash(attempt_token, audience=audience, user_id=user_id)
                if attempt_token is not None else None,
                display_name,
                attempt_id,
                uuid.UUID(str(quiz_id)),
                json.dumps(answers),
                graded["total_score"],
                graded["correct_count"],
                graded["total_count"],
                client_ip,
                json.dumps(graded, default=str),
            )
        except Exception as exc:
            record_internal_exception(
                __name__,
                "artifact_share.quiz_attempt.persistence_failed",
                exc,
            )
            _raise_typed_database_error(exc)
        if not row:
            raise ShareUnavailableError("Share not found or expired")
        saved = row["result_payload"]
        if isinstance(saved, str):
            saved = json.loads(saved)
        if not isinstance(saved, dict):
            raise AttemptConflictError("Attempt result is unavailable")
        return {"attempt_id": str(row["attempt_id"]), **saved}

    async def revoke_share(
        self,
        share_id: str | uuid.UUID,
        user_id: str | None = None,
        tenant_id: str | None = None,
    ) -> bool:
        """Deactivate a share link."""
        share_uuid = share_id if isinstance(share_id, uuid.UUID) else uuid.UUID(share_id)
        if user_id is None:
            result = await self.db.execute(
                "UPDATE assistant.artifact_shares SET is_active = FALSE, "
                "revoked_at = NOW() WHERE id = $1",
                share_uuid,
            )
        else:
            if tenant_id is None:
                raise ValueError("tenant_id is required for creator-scoped revocation")
            result = await self.db.execute(
                "UPDATE assistant.artifact_shares "
                "SET is_active = FALSE, revoked_at = NOW() "
                "WHERE id = $1 AND created_by = $2 AND tenant_id = $3",
                share_uuid,
                user_id,
                tenant_id,
            )
        return "UPDATE 1" in result
