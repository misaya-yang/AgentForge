"""Tenant-scoped native Thread/Turn/Item persistence for the V2 API.

The store deliberately keeps the old ``sessions.history`` column as an input
and compatibility projection only.  New writes are append-only runtime Items;
legacy import is delegated to one PostgreSQL function so the session lock,
in-flight guard, item append, and ready marker share one transaction.
"""

from __future__ import annotations

import json
import logging
import re
import uuid
from dataclasses import dataclass
from hashlib import sha256
from typing import Any, Protocol

logger = logging.getLogger(__name__)

_READONLY_CONTEXT_DUMP = re.compile(
    r"\[AI_PLATFORM_READONLY_CONTEXT_V1\].*?\[/AI_PLATFORM_READONLY_CONTEXT_V1\]",
    re.DOTALL,
)


_QUIZ_ID_PATTERN = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")


def _visible_message_text(text: str) -> str:
    """Drop overlay catalog dumps that were mistakenly stored as user/assistant text."""

    return _READONLY_CONTEXT_DUMP.sub("", text).strip()


def _safe_history_runtime_event(event: Any) -> Any:
    """Expose terminal facts without forwarding Runtime error text to history clients."""

    if not isinstance(event, dict) or event.get("event_type") not in {
        "run_finished", "run_error", "cancelled", "side_effect_unknown"
    }:
        return event
    data = event.get("data") if isinstance(event.get("data"), dict) else {}
    safe_data: dict[str, Any] = {}
    status = data.get("status")
    if isinstance(status, str) and status in {"succeeded", "failed", "cancelled", "blocked"}:
        safe_data["status"] = status
    if (
        status == "cancelled"
        and data.get("error_code") == "AI_PLATFORM_AGENT_RUNTIME_APPROVAL_ORPHANED"
    ):
        safe_data["terminal_reason"] = "runtime_restart_interrupted"
    envelope = data.get("terminal_envelope")
    if isinstance(envelope, dict) and envelope.get("exit_reason") == "side_effect_unknown":
        safe_data["terminal_envelope"] = {"exit_reason": "side_effect_unknown"}
    return {
        "event_type": event["event_type"],
        "data": safe_data,
        "timestamp": event.get("timestamp"),
    }


def _project_knowledge_contexts(events: list[Any], allowed_datasets: dict[str, str]) -> list[dict[str, Any]]:
    """Rebuild bounded source cards only for datasets still visible to this reader."""
    grouped: dict[str, list[dict[str, Any]]] = {}
    for event in events:
        if not isinstance(event, dict) or event.get("event_type") != "context_retrieved":
            continue
        data = event.get("data")
        if not isinstance(data, dict) or not isinstance(data.get("chunks"), list):
            continue
        for raw in data["chunks"][:50]:
            if not isinstance(raw, dict):
                continue
            dataset_id = raw.get("dataset_id")
            if not isinstance(dataset_id, str) or dataset_id not in allowed_datasets:
                continue
            content = raw.get("content")
            if not isinstance(content, str):
                continue
            score = raw.get("score")
            raw_metadata = raw.get("metadata") if isinstance(raw.get("metadata"), dict) else {}
            source_version = raw.get("source_version")
            if source_version is None:
                source_version = raw_metadata.get("source_version")
            source_hash = raw.get("source_hash")
            if source_hash is None:
                source_hash = raw_metadata.get("source_hash")
            citation_metadata = {
                "document_id": raw.get("document_id") if isinstance(raw.get("document_id"), str) else raw_metadata.get("document_id"),
                "segment_id": raw.get("segment_id") if isinstance(raw.get("segment_id"), str) else raw_metadata.get("segment_id"),
            }
            if source_version is not None or source_hash is not None:
                citation_metadata["source_version"] = source_version
                citation_metadata["source_hash"] = source_hash
            grouped.setdefault(dataset_id, []).append({
                "content": content[:2000],
                "score": float(score) if isinstance(score, (int, float)) else 0.0,
                "metadata": citation_metadata,
            })
    return [
        {"dataset_id": dataset_id, "dataset_name": allowed_datasets[dataset_id],
         "chunks": chunks[:20], "query": "", "took_ms": 0}
        for dataset_id, chunks in grouped.items()
    ]


class ThreadStoreDatabase(Protocol):
    async def fetchrow(self, query: str, *args: Any): ...

    async def fetch(self, query: str, *args: Any): ...


class ThreadStoreError(RuntimeError):
    def __init__(self, code: str, *, status_code: int = 409) -> None:
        self.code = code
        self.status_code = status_code
        super().__init__(code)


@dataclass(frozen=True, slots=True)
class RuntimeThread:
    runtime_thread_id: str
    tenant_id: str
    user_id: str
    session_id: str
    kernel_owner: str
    source_kind: str
    import_status: str
    last_sequence: int


def _as_thread(row: Any) -> RuntimeThread:
    return RuntimeThread(
        runtime_thread_id=str(row["runtime_thread_id"]),
        tenant_id=str(row["tenant_id"]),
        user_id=str(row["user_id"]),
        session_id=str(row["session_id"]),
        kernel_owner=str(row["kernel_owner"]),
        source_kind=str(row["source_kind"]),
        import_status=str(row["import_status"]),
        last_sequence=int(row.get("last_sequence", 0) or 0),
    )


class AgentThreadStore:
    def __init__(self, database: ThreadStoreDatabase) -> None:
        self.database = database

    async def get(
        self, *, tenant_id: str, user_id: str, runtime_thread_id: str
    ) -> RuntimeThread | None:
        row = await self.database.fetchrow(
            """
            SELECT runtime_thread_id, tenant_id, user_id, session_id,
                   kernel_owner, source_kind, import_status, last_sequence
              FROM assistant_runtime_threads
             WHERE runtime_thread_id = $1 AND tenant_id = $2 AND user_id = $3
               AND deleted_at IS NULL
            """,
            uuid.UUID(str(runtime_thread_id)),
            tenant_id,
            user_id,
        )
        return _as_thread(row) if row else None

    async def get_for_session(
        self, *, tenant_id: str, user_id: str, session_id: str
    ) -> RuntimeThread | None:
        row = await self.database.fetchrow(
            """
            SELECT runtime_thread_id, tenant_id, user_id, session_id,
                   kernel_owner, source_kind, import_status, last_sequence
              FROM assistant_runtime_threads
             WHERE tenant_id = $1 AND user_id = $2 AND session_id = $3
               AND deleted_at IS NULL
            """,
            tenant_id,
            user_id,
            session_id,
        )
        return _as_thread(row) if row else None

    async def ensure_native(
        self, *, tenant_id: str, user_id: str, session_id: str, runtime_thread_id: str | None = None
    ) -> RuntimeThread:
        thread_id = uuid.UUID(str(runtime_thread_id or uuid.uuid4()))
        await self.database.fetchrow(
            "SELECT ensure_assistant_runtime_thread($1, $2, $3, $4, 'native')",
            thread_id,
            tenant_id,
            user_id,
            session_id,
        )
        thread = await self.get_for_session(
            tenant_id=tenant_id, user_id=user_id, session_id=session_id
        )
        if thread is None:
            raise ThreadStoreError(
                "AI_PLATFORM_AGENT_RUNTIME_THREAD_CREATE_FAILED", status_code=503
            )
        return thread

    async def import_legacy(
        self, *, tenant_id: str, user_id: str, session_id: str, runtime_thread_id: str | None = None
    ) -> RuntimeThread:
        thread_id = uuid.UUID(str(runtime_thread_id or uuid.uuid4()))
        try:
            await self.database.fetchrow(
                """
                SELECT * FROM import_assistant_legacy_session($1, $2, $3, $4)
                """,
                thread_id,
                tenant_id,
                user_id,
                session_id,
            )
        except Exception as exc:  # database exposes stable SQLSTATE message
            message = str(exc)
            if "IMPORT_IN_FLIGHT" in message:
                raise ThreadStoreError(
                    "AI_PLATFORM_AGENT_RUNTIME_IMPORT_IN_FLIGHT", status_code=409
                ) from exc
            if "SESSION_NOT_FOUND" in message or "SCOPE_MISMATCH" in message:
                raise ThreadStoreError(
                    "AI_PLATFORM_AGENT_RUNTIME_SESSION_NOT_FOUND", status_code=404
                ) from exc
            raise ThreadStoreError(
                "AI_PLATFORM_AGENT_RUNTIME_IMPORT_FAILED", status_code=503
            ) from exc
        thread = await self.get_for_session(
            tenant_id=tenant_id, user_id=user_id, session_id=session_id
        )
        if thread is None or thread.import_status != "ready":
            raise ThreadStoreError("AI_PLATFORM_AGENT_RUNTIME_IMPORT_FAILED", status_code=503)
        return thread

    async def events(
        self,
        *,
        tenant_id: str,
        user_id: str,
        runtime_thread_id: str,
        after_sequence: int,
        limit: int,
    ) -> list[dict[str, Any]]:
        rows = await self.database.fetch(
            """
            SELECT sequence, event_id, event_key, turn_id, item_id,
                   event_type, item_type, status, payload, created_at
              FROM assistant_runtime_items
             WHERE runtime_thread_id = $1 AND tenant_id = $2 AND user_id = $3
               AND sequence > $4
             ORDER BY sequence ASC LIMIT $5
            """,
            uuid.UUID(str(runtime_thread_id)),
            tenant_id,
            user_id,
            max(0, int(after_sequence)),
            max(1, min(int(limit), 1000)),
        )
        return [dict(row) for row in rows]

    async def history_messages(
        self,
        *,
        tenant_id: str,
        user_id: str,
        runtime_thread_id: str,
        limit: int,
        allowed_datasets: dict[str, str] | None = None,
        restricted_run_ids: set[str] | None = None,
    ) -> tuple[list[dict[str, Any]], int]:
        rows = await self.database.fetch(
            """
            SELECT role, content, sequence, run_id, created_at, thinking_content,
                   tool_calls, tool_results, runtime_events,
                   COUNT(*) OVER() AS total
              FROM (
                    SELECT 'user'::text AS role,
                           COALESCE(
                               NULLIF(snapshot.snapshot #>> '{input,message}', ''),
                               run.request_preview
                           ) AS content,
                           NULL::bigint AS sequence,
                           run.run_id::text AS run_id,
                           run.started_at AS created_at,
                           NULL::text AS thinking_content,
                           NULL::jsonb AS tool_calls,
                           NULL::jsonb AS tool_results,
                           NULL::jsonb AS runtime_events
                      FROM assistant_runs AS run
                      JOIN assistant_runtime_snapshots AS snapshot
                        ON snapshot.run_id = run.run_id
                       AND snapshot.runtime_thread_id = run.harness_thread_id
                       AND snapshot.tenant_id = run.tenant_id
                       AND snapshot.user_id = run.user_id
                       AND snapshot.session_id = run.session_id
                     WHERE run.harness_thread_id = $1
                       AND run.tenant_id = $2 AND run.user_id = $3
                       AND run.engine = 'agent_runtime'
                    UNION ALL
                    SELECT 'assistant'::text AS role,
                           item.payload #>> '{payload,message}' AS content,
                           item.sequence,
                           active_turn.turn_id AS run_id,
                           item.created_at,
                           (
                               SELECT string_agg(
                                   reasoning.payload #>> '{data,content}',
                                   '' ORDER BY reasoning.sequence
                               )
                                 FROM assistant_runtime_items AS reasoning
                                WHERE reasoning.runtime_thread_id = item.runtime_thread_id
                                  AND reasoning.tenant_id = item.tenant_id
                                  AND reasoning.user_id = item.user_id
                                  AND reasoning.turn_id = active_turn.turn_id
                                  AND reasoning.event_type = 'compat/v1/thinking_delta'
                           ) AS thinking_content
                           ,(
                               SELECT jsonb_agg(
                                   jsonb_build_object(
                                       'id', tool.payload #>> '{data,tool_call_id}',
                                       'name', tool.payload #>> '{data,tool_name}',
                                       'arguments', COALESCE(
                                           tool.payload #> '{data,arguments}',
                                           '{}'::jsonb
                                       ),
                                       'status', tool.payload #>> '{data,status}'
                                   ) ORDER BY tool.sequence
                               )
                                 FROM assistant_runtime_items AS tool
                                WHERE tool.runtime_thread_id = item.runtime_thread_id
                                  AND tool.tenant_id = item.tenant_id
                                  AND tool.user_id = item.user_id
                                  AND tool.turn_id = active_turn.turn_id
                                  AND tool.event_type = 'compat/v1/tool_call_start'
                           ) AS tool_calls,
                           (
                               SELECT jsonb_agg(
                                   jsonb_build_object(
                                       'tool_call_id', result.payload #>> '{data,tool_call_id}',
                                       'name', result.payload #>> '{data,tool_name}',
                                       'result', result.payload #> '{data,result}',
                                       'error', CASE
                                           WHEN COALESCE(
                                               (result.payload #>> '{data,success}')::boolean,
                                               FALSE
                                           ) THEN NULL
                                           ELSE COALESCE(
                                               result.payload #>> '{data,error}',
                                               result.payload #>> '{data,status}'
                                           )
                                       END,
                                       'duration_ms', result.payload #> '{data,duration_ms}'
                                   ) ORDER BY result.sequence
                               )
                                 FROM assistant_runtime_items AS result
                                WHERE result.runtime_thread_id = item.runtime_thread_id
                                  AND result.tenant_id = item.tenant_id
                                  AND result.user_id = item.user_id
                                  AND result.turn_id = active_turn.turn_id
                                  AND result.event_type = 'compat/v1/tool_call_result'
                           ) AS tool_results
                           ,(
                               SELECT jsonb_agg(
                                   jsonb_build_object(
                                       'event_type', CASE WHEN runtime.event_type = 'agent-runtime/tool-lifecycle'
                                                          THEN 'side_effect_unknown'
                                                          ELSE replace(runtime.event_type, 'compat/v1/', '') END,
                                       'data', runtime.payload #> '{data}',
                                       'timestamp', EXTRACT(EPOCH FROM runtime.created_at)
                                   ) ORDER BY runtime.sequence
                               )
                                 FROM assistant_runtime_items AS runtime
                                WHERE runtime.runtime_thread_id = item.runtime_thread_id
                                  AND runtime.tenant_id = item.tenant_id
                                  AND runtime.user_id = item.user_id
                                  AND runtime.turn_id = active_turn.turn_id
                                  AND (runtime.event_type IN (
                                      'compat/v1/subagent_started',
                                      'compat/v1/subagent_step',
                                      'compat/v1/subagent_finished',
                                      'compat/v1/plan_update',
                                      'compat/v1/context_compaction',
                                      'compat/v1/context_retrieved',
                                      'compat/v1/memory_loaded',
                                      'compat/v1/side_effect_unknown',
                                      'compat/v1/run_finished',
                                      'compat/v1/run_error',
                                      'compat/v1/cancelled'
                                  ) OR (runtime.event_type = 'agent-runtime/tool-lifecycle'
                                        AND runtime.item_type = 'tool_result'
                                        AND runtime.status = 'side_effect_unknown'))
                           ) AS runtime_events
                      FROM assistant_runtime_items AS item
                      JOIN LATERAL (
                           SELECT started.turn_id
                             FROM assistant_runtime_items AS started
                            WHERE started.runtime_thread_id = item.runtime_thread_id
                              AND started.tenant_id = item.tenant_id
                              AND started.user_id = item.user_id
                              AND started.event_type = 'compat/v1/run_started'
                              AND started.sequence <= item.sequence
                            ORDER BY started.sequence DESC LIMIT 1
                      ) AS active_turn ON TRUE
                     WHERE item.runtime_thread_id = $1
                       AND item.tenant_id = $2 AND item.user_id = $3
                       AND item.event_type = 'rollout/item'
                       AND item.item_type = 'event_msg'
                       AND item.payload #>> '{payload,type}' = 'agent_message'
                    UNION ALL
                    SELECT 'assistant'::text AS role,
                           string_agg(
                               CASE
                                   WHEN delta.event_type = 'compat/v1/text_delta'
                                   THEN COALESCE(delta.payload #>> '{data,content}', '')
                                   ELSE ''
                               END,
                               '' ORDER BY delta.sequence
                           ) AS content,
                           MAX(delta.sequence) AS sequence,
                           delta.turn_id AS run_id,
                           COALESCE((
                               SELECT stopped.finished_at FROM assistant_runs AS stopped
                                WHERE stopped.run_id::text = delta.turn_id
                                  AND stopped.harness_thread_id = delta.runtime_thread_id
                                  AND stopped.tenant_id = delta.tenant_id AND stopped.user_id = delta.user_id
                                  AND stopped.status = 'cancelled'
                           ), MAX(delta.created_at)) AS created_at,
                           (
                               SELECT string_agg(
                                   reasoning.payload #>> '{data,content}',
                                   '' ORDER BY reasoning.sequence
                               )
                                 FROM assistant_runtime_items AS reasoning
                                WHERE reasoning.runtime_thread_id = delta.runtime_thread_id
                                  AND reasoning.tenant_id = delta.tenant_id
                                  AND reasoning.user_id = delta.user_id
                                  AND reasoning.turn_id = delta.turn_id
                                  AND reasoning.event_type = 'compat/v1/thinking_delta'
                           ) AS thinking_content,
                           NULL::jsonb AS tool_calls,
                           NULL::jsonb AS tool_results,
                           (
                               SELECT jsonb_agg(
                                   jsonb_build_object(
                                       'event_type', CASE WHEN runtime.event_type = 'agent-runtime/tool-lifecycle'
                                                          THEN 'side_effect_unknown'
                                                          ELSE replace(runtime.event_type, 'compat/v1/', '') END,
                                       'data', runtime.payload #> '{data}',
                                       'timestamp', EXTRACT(EPOCH FROM runtime.created_at)
                                   ) ORDER BY runtime.sequence
                               )
                                 FROM assistant_runtime_items AS runtime
                                WHERE runtime.runtime_thread_id = delta.runtime_thread_id
                                  AND runtime.tenant_id = delta.tenant_id
                                  AND runtime.user_id = delta.user_id
                                  AND runtime.turn_id = delta.turn_id
                                  AND (runtime.event_type IN (
                                      'compat/v1/subagent_started',
                                      'compat/v1/subagent_step',
                                      'compat/v1/subagent_finished',
                                      'compat/v1/plan_update',
                                      'compat/v1/context_compaction',
                                      'compat/v1/context_retrieved',
                                      'compat/v1/memory_loaded',
                                      'compat/v1/side_effect_unknown',
                                      'compat/v1/run_finished',
                                      'compat/v1/run_error',
                                      'compat/v1/cancelled'
                                  ) OR (runtime.event_type = 'agent-runtime/tool-lifecycle'
                                        AND runtime.item_type = 'tool_result'
                                        AND runtime.status = 'side_effect_unknown'))
                           ) AS runtime_events
                      FROM assistant_runtime_items AS delta
                      LEFT JOIN LATERAL (
                           SELECT start.sequence
                             FROM assistant_runtime_items AS start
                            WHERE start.runtime_thread_id = delta.runtime_thread_id
                              AND start.tenant_id = delta.tenant_id
                              AND start.user_id = delta.user_id
                              AND start.turn_id = delta.turn_id
                              AND start.event_type = 'compat/v1/run_started'
                            ORDER BY start.sequence ASC LIMIT 1
                      ) AS started ON TRUE
                     WHERE delta.runtime_thread_id = $1
                       AND delta.tenant_id = $2 AND delta.user_id = $3
                       AND delta.event_type IN (
                           'compat/v1/text_delta',
                           'compat/v1/side_effect_unknown',
                           'compat/v1/run_finished',
                           'compat/v1/run_error',
                           'compat/v1/cancelled'
                       )
                       AND (started.sequence IS NOT NULL OR (
                           delta.event_type = 'compat/v1/cancelled' AND EXISTS (
                               SELECT 1 FROM assistant_runs AS stopped
                                JOIN assistant_runtime_snapshots AS snapshot ON snapshot.run_id = stopped.run_id
                                 AND snapshot.runtime_thread_id = stopped.harness_thread_id
                                 AND snapshot.tenant_id = stopped.tenant_id AND snapshot.user_id = stopped.user_id
                                 AND snapshot.session_id = stopped.session_id
                                WHERE stopped.run_id::text = delta.turn_id
                                  AND stopped.harness_thread_id = delta.runtime_thread_id
                                  AND stopped.tenant_id = delta.tenant_id AND stopped.user_id = delta.user_id
                                  AND stopped.status = 'cancelled' AND stopped.engine = 'agent_runtime'
                           )
                       ))
                       AND NOT EXISTS (
                           SELECT 1
                             FROM assistant_runtime_items AS completed
                            WHERE completed.runtime_thread_id = delta.runtime_thread_id
                              AND completed.tenant_id = delta.tenant_id
                              AND completed.user_id = delta.user_id
                              AND completed.event_type = 'rollout/item'
                              AND completed.item_type = 'event_msg'
                              AND completed.payload #>> '{payload,type}' = 'agent_message'
                              AND completed.sequence > started.sequence
                              AND NOT EXISTS (
                                  SELECT 1
                                    FROM assistant_runtime_items AS next_started
                                   WHERE next_started.runtime_thread_id = delta.runtime_thread_id
                                     AND next_started.tenant_id = delta.tenant_id
                                     AND next_started.user_id = delta.user_id
                                     AND next_started.event_type = 'compat/v1/run_started'
                                     AND next_started.turn_id IS DISTINCT FROM delta.turn_id
                                     AND next_started.sequence > started.sequence
                                     AND next_started.sequence <= completed.sequence
                              )
                       )
                     GROUP BY delta.runtime_thread_id, delta.tenant_id,
                              delta.user_id, delta.turn_id, started.sequence
                   ) AS message
             WHERE content IS NOT NULL
               AND (
                   content <> ''
                   OR EXISTS (
                       SELECT 1
                         FROM jsonb_array_elements(
                             COALESCE(runtime_events, '[]'::jsonb)
                         ) AS terminal_event
                        WHERE terminal_event ->> 'event_type' IN (
                            'side_effect_unknown', 'run_finished', 'run_error', 'cancelled'
                        )
                   )
               )
             ORDER BY created_at DESC, sequence DESC NULLS LAST LIMIT $4
            """,
            uuid.UUID(str(runtime_thread_id)),
            tenant_id,
            user_id,
            max(1, min(int(limit), 500)),
        )
        messages: list[dict[str, Any]] = []
        for row in reversed(rows):
            role = row.get("role")
            text = row.get("content")
            if role is None or not isinstance(text, str):
                continue
            text = _visible_message_text(text)
            runtime_events_value = row.get("runtime_events")
            if isinstance(runtime_events_value, str):
                try:
                    runtime_events_value = json.loads(runtime_events_value)
                except json.JSONDecodeError:
                    runtime_events_value = None
            source_access_revoked = (
                role == "assistant"
                and (str(row.get("run_id") or "") in (restricted_run_ids or set()) or (
                    isinstance(runtime_events_value, list)
                    and any(
                    isinstance(event, dict)
                    and event.get("event_type") == "context_retrieved"
                    and isinstance(event.get("data"), dict)
                    and isinstance(event["data"].get("chunks"), list)
                    and any(
                        isinstance(chunk, dict)
                        and isinstance(chunk.get("dataset_id"), str)
                        and chunk["dataset_id"] not in (allowed_datasets or {})
                        for chunk in event["data"]["chunks"]
                    )
                    for event in runtime_events_value
                    )
                ))
            )
            if source_access_revoked:
                text = ""
            has_terminal_runtime_event = isinstance(runtime_events_value, list) and any(
                isinstance(event, dict)
                and (
                    event.get("event_type") in {"run_error", "cancelled"}
                    or event.get("event_type") in {"side_effect_unknown", "run_finished"}
                    or (
                        isinstance(event.get("data"), dict)
                        and event["data"].get("status") == "cancelled"
                    )
                )
                for event in runtime_events_value
            )
            if not text and not (role == "assistant" and (has_terminal_runtime_event or source_access_revoked)):
                continue
            created_at = row.get("created_at")
            metadata = {"runtime_run_id": row.get("run_id")}
            if source_access_revoked:
                metadata["source_access_revoked"] = True
            if row.get("sequence") is not None:
                metadata["runtime_sequence"] = int(row["sequence"])
            thinking_content = row.get("thinking_content")
            if role == "assistant" and not source_access_revoked and isinstance(thinking_content, str) and thinking_content:
                metadata["thinking_content"] = thinking_content
            for key in ("tool_calls", "tool_results", "runtime_events"):
                value = row.get(key)
                if isinstance(value, str):
                    value = json.loads(value)
                if role == "assistant" and isinstance(value, list) and value and (not source_access_revoked or key == "runtime_events"):
                    if key == "runtime_events":
                        contexts = _project_knowledge_contexts(value, allowed_datasets or {}) if not source_access_revoked else []
                        if contexts:
                            metadata["contexts"] = contexts
                        value = [
                            _safe_history_runtime_event(event)
                            for event in value
                            if isinstance(event, dict)
                            and event.get("event_type") != "context_retrieved"
                            and (
                                not source_access_revoked
                                or event.get("event_type") in {"run_finished", "run_error", "cancelled", "side_effect_unknown"}
                            )
                        ]
                    if value:
                        metadata[key] = value
            runtime_events = metadata.get("runtime_events")
            if role == "assistant" and isinstance(runtime_events, list):
                steps: list[dict[str, Any]] = []
                summary_status = "succeeded"
                outcome_uncertain = False
                cancelled = False
                terminal_reason: str | None = None
                for runtime_event in runtime_events:
                    if not isinstance(runtime_event, dict):
                        continue
                    event_type = runtime_event.get("event_type")
                    data = runtime_event.get("data")
                    data = data if isinstance(data, dict) else {}
                    if event_type == "plan_update" and isinstance(data.get("plan"), list):
                        for index, item in enumerate(data["plan"]):
                            if (
                                not isinstance(item, dict)
                                or not str(item.get("step") or "").strip()
                            ):
                                continue
                            status = str(item.get("status") or "pending")
                            normalized_status = "pending"
                            if status in {"inProgress", "in_progress"}:
                                normalized_status = "running"
                            elif status == "completed":
                                normalized_status = "completed"
                            steps.append(
                                {
                                    "id": str(item.get("id") or f"plan-{index}"),
                                    "title": str(item["step"])[:500],
                                    "status": normalized_status,
                                }
                            )
                    elif event_type == "context_compaction":
                        steps.append(
                            {
                                "id": "context-compaction",
                                "title": "Context compacted",
                                "status": "completed",
                            }
                        )
                    elif event_type == "memory_loaded":
                        steps.append(
                            {
                                "id": "memory-loaded",
                                "title": "Memory loaded",
                                "status": "completed",
                            }
                        )
                    elif event_type == "side_effect_unknown":
                        summary_status = "failed"
                        outcome_uncertain = True
                    elif event_type in {"run_error", "cancelled"}:
                        summary_status = "failed"
                        if event_type == "cancelled" or data.get("status") == "cancelled":
                            summary_status = "cancelled"
                            cancelled = True
                            if data.get("terminal_reason") == "runtime_restart_interrupted":
                                terminal_reason = "runtime_restart_interrupted"
                        envelope = data.get("terminal_envelope")
                        if isinstance(envelope, dict) and envelope.get("exit_reason") == "side_effect_unknown":
                            outcome_uncertain = True
                if cancelled:
                    summary_status = "cancelled"
                if steps or summary_status != "succeeded" or not text:
                    metadata["process_summary"] = {
                        "collapsed": True,
                        "run_id": row.get("run_id"),
                        "status": summary_status,
                        "outcome_uncertain": outcome_uncertain,
                        "terminal_reason": terminal_reason,
                        "steps": steps,
                        "tools": [],
                    }
            messages.append(
                {
                    "role": role,
                    "content": text,
                    "timestamp": created_at.isoformat() if created_at else None,
                    "metadata": metadata,
                }
            )
        await self._attach_user_uploads(messages, tenant_id=tenant_id, user_id=user_id, runtime_thread_id=runtime_thread_id)
        await self._attach_turn_parameters(messages, tenant_id=tenant_id, user_id=user_id, runtime_thread_id=runtime_thread_id)
        await self._attach_quiz_ids(messages, tenant_id=tenant_id, user_id=user_id)
        total = int(rows[0].get("total") or 0) if rows else 0
        return messages, total

    async def _attach_turn_parameters(
        self, messages: list[dict[str, Any]], *, tenant_id: str, user_id: str,
        runtime_thread_id: str,
    ) -> None:
        """Show only allowlisted, immutable effective settings for the owner's turns."""
        pending = {
            str(message["metadata"]["runtime_run_id"]): message
            for message in messages
            if message.get("role") == "assistant"
            and isinstance(message.get("metadata"), dict)
            and not message["metadata"].get("source_access_revoked")
            and message["metadata"].get("runtime_run_id")
        }
        if not pending:
            return
        rows = await self.database.fetch(
            """
            SELECT s.run_id::text AS run_id,
                   s.snapshot #> '{reasoning}' AS reasoning,
                   s.snapshot #>> '{model,id}' AS model_id,
                   s.snapshot #> '{parameters,temperature}' AS temperature
              FROM assistant_runtime_snapshots AS s
              JOIN assistant_runs AS r
                ON r.run_id = s.run_id AND r.harness_thread_id = s.runtime_thread_id
               AND r.tenant_id = s.tenant_id AND r.user_id = s.user_id AND r.session_id = s.session_id
             WHERE s.runtime_thread_id = $1 AND s.tenant_id = $2 AND s.user_id = $3
               AND s.run_id::text = ANY($4::text[])
            """,
            uuid.UUID(str(runtime_thread_id)), tenant_id, user_id, list(pending),
        )
        for row in rows:
            message = pending.get(str(row.get("run_id") or ""))
            if message is None:
                continue
            events = message["metadata"].get("runtime_events") or []
            terminal = any(
                isinstance(event, dict) and event.get("event_type") in {"run_finished", "run_error", "cancelled", "side_effect_unknown"}
                for event in events
            )
            summary = message["metadata"].setdefault("process_summary", {
                "collapsed": terminal, "run_id": row["run_id"],
                "status": "succeeded" if terminal else "running",
                "steps": [], "tools": [],
            })
            reasoning = row.get("reasoning")
            if isinstance(reasoning, str):
                reasoning = json.loads(reasoning)
            if isinstance(reasoning, dict):
                summary["reasoning"] = {
                    "requested_option": reasoning.get("requested_option"),
                    "effective_option": reasoning.get("effective_option"),
                    "canonical_effort": reasoning.get("canonical_effort"),
                    "adapter_id": reasoning.get("adapter_id"),
                    "fallback_reason": reasoning.get("fallback_reason"),
                }
            if isinstance(row.get("model_id"), str):
                summary["model_id"] = row["model_id"]
            temperature = row.get("temperature")
            if isinstance(temperature, str):
                try:
                    temperature = float(temperature)
                except ValueError:
                    temperature = None
            if isinstance(temperature, (int, float)) and not isinstance(temperature, bool) and 0 <= temperature <= 2:
                summary["temperature"] = float(temperature)

    async def _attach_user_uploads(
        self, messages: list[dict[str, Any]], *, tenant_id: str, user_id: str,
        runtime_thread_id: str,
    ) -> None:
        """Recover selected uploads from the immutable snapshot, without execution."""
        pending: dict[str, list[dict[str, Any]]] = {}
        for message in messages:
            if message.get("role") == "user" and message.get("metadata", {}).get("runtime_run_id"):
                pending.setdefault(str(message["metadata"]["runtime_run_id"]), []).append(message)
        if not pending:
            return
        rows = await self.database.fetch(
            """
            SELECT s.run_id::text AS run_id, a.artifact_id, a.filename, a.mime_type,
                   a.metadata ->> 'original_filename' AS original_filename
              FROM assistant_runtime_snapshots AS s
              JOIN assistant_runs AS r
                ON r.run_id = s.run_id AND r.harness_thread_id = s.runtime_thread_id
               AND r.tenant_id = s.tenant_id AND r.user_id = s.user_id AND r.session_id = s.session_id
             CROSS JOIN LATERAL jsonb_array_elements(
                 CASE WHEN jsonb_typeof(s.snapshot #> '{readonly_capabilities,platform_config,attachments}') = 'array'
                      THEN s.snapshot #> '{readonly_capabilities,platform_config,attachments}'
                      ELSE '[]'::jsonb END
             ) WITH ORDINALITY AS selected(value, ordinality)
              JOIN assistant.artifacts AS a
                ON a.artifact_id = selected.value ->> 'ref'
               AND a.tenant_id = s.tenant_id AND a.user_id = s.user_id AND a.session_id = s.session_id
             WHERE s.runtime_thread_id = $1 AND s.tenant_id = $2 AND s.user_id = $3
               AND s.run_id::text = ANY($4::text[])
               AND selected.value ->> 'descriptor' = 'read_attachment'
               AND selected.value ->> 'version' = 'v1'
               AND a.source = 'user' AND a.size_bytes > 0
             ORDER BY s.run_id, selected.ordinality
            """,
            uuid.UUID(str(runtime_thread_id)), tenant_id, user_id, list(pending),
        )
        seen: set[tuple[str, str]] = set()
        for row in rows:
            run_id, artifact_id = str(row.get("run_id") or ""), str(row.get("artifact_id") or "")
            if run_id not in pending or not re.fullmatch(r"art_[0-9a-f]{16}", artifact_id) or (run_id, artifact_id) in seen:
                continue
            seen.add((run_id, artifact_id))
            attachment = {
                "type": "image" if str(row.get("mime_type") or "").startswith("image/") else "file",
                "filename": row.get("original_filename") or row.get("filename"),
                "url": f"/api/v1/assistant/artifacts/{artifact_id}/download",
            }
            for message in pending[run_id]:
                message["metadata"].setdefault("attachments", []).append(attachment)

    async def _attach_quiz_ids(
        self,
        messages: list[dict[str, Any]],
        *,
        tenant_id: str,
        user_id: str,
    ) -> None:
        """Key assistant messages that generated a quiz by its quiz id.

        The Rust worker persists the quiz and reports it on the capability
        ledger's ``result_summary``; its compat ``tool_call_result`` event
        carries only the arguments. Everything that renders a quiz — the chat
        card restore and conversation-share freezing — keys off
        ``metadata.quiz_id``, so recover it from the ledger.
        """

        pending = {
            str(message["metadata"]["runtime_run_id"]): message
            for message in messages
            if message.get("role") == "assistant"
            and isinstance(message.get("metadata"), dict)
            and not message["metadata"].get("source_access_revoked")
            and not message["metadata"].get("quiz_id")
            and message["metadata"].get("runtime_run_id")
        }
        if not pending:
            return
        try:
            rows = await self.database.fetch(
                """
                SELECT run_id::text AS run_id,
                       result_summary ->> 'quiz_id' AS quiz_id
                  FROM assistant_capability_executions
                 WHERE tenant_id = $1 AND user_id = $2
                   AND capability_id = 'generate_quiz'
                   AND status = 'succeeded'
                   AND run_id::text = ANY($3::text[])
                 ORDER BY created_at
                """,
                tenant_id,
                user_id,
                list(pending),
            )
        except Exception:  # pragma: no cover - ledger is advisory for rendering
            logger.warning("Failed to resolve quiz ids for assistant history", exc_info=True)
            return
        for row in rows or []:
            quiz_id = str(row.get("quiz_id") or "").strip()
            message = pending.get(str(row.get("run_id") or ""))
            if message is not None and _QUIZ_ID_PATTERN.match(quiz_id.lower()):
                message["metadata"]["quiz_id"] = quiz_id

    async def turn_metadata(
        self,
        *,
        tenant_id: str,
        user_id: str,
        session_id: str,
        runtime_thread_id: str,
        turn_id: str,
    ) -> dict[str, Any] | None:
        row = await self.database.fetchrow(
            """
            SELECT snapshot, capability_revision,
                   snapshot->>'kernel_revision' AS kernel_revision
              FROM assistant_runtime_snapshots
             WHERE run_id = $1 AND runtime_thread_id = $2
               AND tenant_id = $3 AND user_id = $4 AND session_id = $5
            """,
            uuid.UUID(str(turn_id)),
            uuid.UUID(str(runtime_thread_id)),
            tenant_id,
            user_id,
            session_id,
        )
        if not row:
            return None
        snapshot = row.get("snapshot") or {}
        if isinstance(snapshot, str):
            snapshot = json.loads(snapshot)
        reasoning = snapshot.get("reasoning") if isinstance(snapshot, dict) else {}
        if not isinstance(reasoning, dict):
            reasoning = {}
        model = snapshot.get("model") if isinstance(snapshot, dict) else {}
        parameters = snapshot.get("parameters") if isinstance(snapshot, dict) else {}
        temperature = parameters.get("temperature") if isinstance(parameters, dict) else None
        return {
            "requested_reasoning_option": reasoning.get("requested_option"),
            "effective_reasoning_option": reasoning.get("effective_option"),
            "reasoning_adapter_id": reasoning.get("adapter_id"),
            "capability_revision": int(row.get("capability_revision") or 1),
            "reasoning_fallback_reason": reasoning.get("fallback_reason"),
            "kernel": "agent",
            "kernel_revision": row.get("kernel_revision"),
            "model_id": model.get("id") if isinstance(model, dict) else None,
            "temperature": float(temperature) if isinstance(temperature, (int, float)) and not isinstance(temperature, bool) and 0 <= temperature <= 2 else None,
        }

    async def append_event(
        self,
        *,
        thread: RuntimeThread,
        event_key: str,
        event_type: str,
        payload: dict[str, Any],
        turn_id: str | None = None,
        item_id: str | None = None,
        status: str | None = None,
    ) -> int:
        payload_json = json.dumps(
            payload, ensure_ascii=False, sort_keys=True, separators=(",", ":")
        )
        event_id = uuid.uuid5(
            uuid.NAMESPACE_URL, f"ai-platform:{thread.runtime_thread_id}:{event_key}"
        )
        row = await self.database.fetchrow(
            """
            SELECT append_assistant_runtime_item(
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13::jsonb, $14
            ) AS sequence
            """,
            uuid.UUID(thread.runtime_thread_id),
            uuid.UUID(thread.runtime_thread_id),
            thread.tenant_id,
            thread.user_id,
            thread.session_id,
            event_id,
            event_key,
            turn_id,
            item_id,
            event_type,
            "item" if item_id else None,
            status,
            payload_json,
            sha256(payload_json.encode()).hexdigest(),
        )
        return int(row["sequence"])


__all__ = ["AgentThreadStore", "RuntimeThread", "ThreadStoreError"]
