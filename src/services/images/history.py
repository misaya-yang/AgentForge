"""Read-only recovery of persisted image outcomes; never dispatch a provider."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from typing import Any
from urllib.parse import quote

from fastapi import Request

from ...core.auth.user_resolver import UserContext
from ..assistant_entry.source_access import (
    ConversationSources,
    source_documents_at_creation,
    source_ids_at_creation,
    source_versions_at_creation,
    visible_document_keys,
    visible_source_version_keys,
)


def _value(value: Any) -> Any:
    return json.loads(value) if isinstance(value, str) else value


def _date(value: Any) -> datetime | None:
    if isinstance(value, str):
        try:
            value = datetime.fromisoformat(value.replace("Z", "+00:00"))
        except ValueError:
            return None
    if isinstance(value, datetime):
        return value if value.tzinfo else value.replace(tzinfo=timezone.utc)
    return None


def _matches_image(
    message: dict[str, Any], identity: dict, turn_id: str, task_id: str, family_ids: set[str]
) -> bool:
    metadata = {**identity, **(message.get("metadata") or {})}
    return message.get("role") == "assistant" and (
        metadata.get("image_turn_id") == turn_id
        or metadata.get("image_task_id") == task_id
        or (metadata.get("process_summary") or {}).get("diagnostic_id") == task_id
        or bool(set(metadata.get("artifact_ids") or []) & family_ids)
    )


def merge_image_history(
    history: list[dict[str, Any]],
    turns: list[dict[str, Any]],
    artifacts: list[dict[str, Any]],
    sources: ConversationSources | None,
    visible: dict[str, str],
    identity_metadata: list[dict | None] | None = None,
    visible_documents: set[tuple[str, str]] | None = None,
    visible_versions: set[tuple[str, str, int, str]] | None = None,
) -> list[dict[str, Any]]:
    """Replace a matching legacy result, or insert one stable outcome by time."""
    merged = list(history)
    identities = (
        {
            id(message): (identity_metadata[index] or {})
            for index, message in enumerate(history[: len(identity_metadata or [])])
        }
        if identity_metadata
        else {}
    )
    for turn in turns:
        task_id, turn_id = str(turn["task_id"]), str(turn["turn_id"])
        family = [a for a in artifacts if str(a.get("turn_id")) == turn_id]
        family_ids = {str(a["artifact_id"]) for a in family}
        result = _value(turn.get("result")) or {}
        family_ids.update(
            str(i["artifact_id"])
            for i in result.get("images", [])
            if isinstance(i, dict) and i.get("artifact_id")
        )
        match = next(
            (
                i
                for i, message in enumerate(merged)
                if _matches_image(
                    message,
                    identities.get(id(message), {}),
                    turn_id,
                    task_id,
                    family_ids,
                )
            ),
            None,
        )
        public_artifacts = [a for a in family if a.get("variant") == "display"] or [
            a for a in family if a.get("variant") == "raw"
        ]
        task_status = turn.get("task_status")
        turn_status = turn.get("status")
        status = (
            task_status
            if task_status in {"completed", "failed", "cancelled", "unknown"}
            else turn_status
        )
        if public_artifacts and turn_status == "completed":
            status = "completed"
        if status == "completed" and not public_artifacts:
            status = "unknown"
        running = status in {"pending", "queued", "running"}
        restricted = bool(
            sources
            and (
                not source_ids_at_creation(sources, turn.get("created_at")) <= visible.keys()
                or not source_documents_at_creation(sources, turn.get("created_at")) <= (visible_documents or set())
                or not source_versions_at_creation(sources, turn.get("created_at")) <= (visible_versions or set())
            )
        )
        process_status = (
            "running"
            if running
            else "succeeded"
            if status == "completed"
            else "cancelled"
            if status == "cancelled"
            else "failed"
        )
        metadata: dict[str, Any] = {
            "source_kind": "image_generation",
            "image_turn_id": turn_id,
            "image_task_id": task_id,
            "image_generating": running,
            "image_generation_prompt": turn.get("prompt") if running and not restricted else None,
            "process_summary": {
                "status": process_status,
                "outcome_uncertain": status == "unknown",
                "diagnostic_id": task_id,
                "collapsed": True,
                "steps": [],
                "tools": [],
            },
        }
        old = merged[match] if match is not None else None
        content = old.get("content", "") if old else ""
        if restricted:
            content = ""
            metadata["source_access_revoked"] = True
        elif public_artifacts:
            metadata["artifact_ids"] = [str(a["artifact_id"]) for a in public_artifacts]
            if not content:
                content = "\n\n".join(
                    f"![Generated image](/api/v1/assistant/artifacts/{quote(str(a['artifact_id']), safe='')}/download)"
                    for a in public_artifacts
                )
            if status == "completed" and result.get("effective_model_id"):
                metadata["model_id"] = result["effective_model_id"]
        record = {
            "role": "assistant",
            "content": content,
            "metadata": {**((old or {}).get("metadata") or {}), **metadata},
            "timestamp": (old or {}).get("timestamp")
            or (
                _date(turn.get("created_at")).isoformat() if _date(turn.get("created_at")) else None
            ),
        }
        if restricted:
            # Preserve public identity/status only; legacy fields may contain source content.
            record["metadata"] = metadata
        if match is not None:
            merged[match] = record
        else:
            created = _date(record["timestamp"])
            index = next(
                (
                    i
                    for i, m in enumerate(merged)
                    if created and _date(m.get("timestamp")) and _date(m["timestamp"]) > created
                ),
                len(merged),
            )
            merged.insert(index, record)
    return merged


async def image_history(
    request: Request,
    user: UserContext,
    session_id: str,
    history: list[dict[str, Any]],
    sources: ConversationSources | None,
    visible: dict[str, str],
    *,
    limit: int,
    identity_metadata: list[dict | None] | None = None,
) -> list[dict[str, Any]]:
    # Only image-bearing histories need a second ledger read. This is a hint,
    # not authority: both queries below enforce the original owner.
    if not any(
        (m.get("metadata") or {}).get("source_kind") == "image_generation"
        or (m.get("role") == "user" and str(m.get("content") or "").startswith("🎨"))
        for m in history
    ):
        return history
    db = getattr(request.app.state, "database", None)
    if db is None:
        return history
    turns = await db.fetch(
        """SELECT t.turn_id, t.task_id, t.prompt, t.status, t.created_at,
                  task.status AS task_status, task.result
             FROM assistant.image_turns t JOIN assistant.image_tasks task ON task.task_id=t.task_id
            WHERE t.session_id=$1 AND task.tenant_id=$2 AND task.user_id=$3
            ORDER BY t.created_at DESC, t.turn_id DESC LIMIT $4""",
        session_id,
        user.tenant_id,
        user.user_id,
        limit,
    )
    if not turns:
        return history
    artifacts = await db.fetch(
        """SELECT artifact_id, turn_id, variant FROM assistant.artifacts
            WHERE session_id=$1 AND tenant_id=$2 AND user_id=$3
              AND turn_id = ANY($4::text[]) AND size_bytes > 0 AND variant IN ('raw','display')""",
        session_id,
        user.tenant_id,
        user.user_id,
        [str(t["turn_id"]) for t in turns],
    )
    return merge_image_history(
        history,
        [dict(t) for t in reversed(turns)],
        [dict(a) for a in artifacts],
        sources,
        visible,
        identity_metadata,
        await visible_document_keys(request, user, sources.document_ids) if sources else set(),
        await visible_source_version_keys(request, user, sources.source_versions) if sources else set(),
    )
