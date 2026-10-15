from __future__ import annotations

import asyncio
import contextlib
import copy
import hashlib
import json
import logging
import math
import re
import uuid
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from fastapi import (
    APIRouter,
    Body,
    Depends,
    File,
    Form,
    HTTPException,
    Query,
    Request,
    Response,
    UploadFile,
)
from fastapi.responses import StreamingResponse
from pydantic import BaseModel, Field, model_validator

from ...config.settings import Settings
from ...core.auth.user_resolver import UserContext
from ...core.exceptions import PermissionDeniedError, ValidationFailedError
from ...persistence.database import (
    DOCUMENT_LIFECYCLE_REINDEX_KEY,
    IndexLeaseUnavailableError,
    dataset_index_deletion_fence,
)
from ...persistence.document_batches import DocumentBatchStore
from ...persistence.document_metadata import MetadataRegistryRevisionConflict
from ...services.knowledge.chunking import validate_persisted_chunking_config
from ...services.knowledge.common import SPECIALIZED_REBUILD_UNAVAILABLE, maybe_await
from ...services.knowledge.document_detector import DocumentTypeDetector
from ...services.knowledge.document_metadata import DocumentMetadataManager
from ...services.knowledge.document_service import (
    _dataset_content_generation,
    _require_dataset_index_readable,
    _require_dataset_index_writable,
    _require_unchanged_dataset_content,
    _with_display_status,
)
from ...services.knowledge.knowledge_service import KnowledgeService
from ...services.knowledge.query_observability import QueryObservationConflictError
from ...services.knowledge.source_identity import (
    _published_source_identity_map,
    _source_identity,
    _source_metadata,
)
from ...services.knowledge.special_publication import SOURCE_MANIFEST_KEY
from ...services.knowledge.upload_budget import require_parser_budget
from ...services.knowledge.worker import KnowledgeIngestTask, KnowledgeWorker
from ..deps import get_knowledge_service, get_knowledge_worker, get_settings, get_user_context
from ..schemas.knowledge import (
    BatchDeleteSchema,
    BatchReindexSchema,
    BatchRetrieveRequestSchema,
    ChunkPreviewRequestSchema,
    DatasetConfigUpdateSchema,
    DatasetCreateSchema,
    DatasetDeleteSchema,
    DatasetPermissionGrantSchema,
    DatasetUpdateSchema,
    DocumentArchiveSchema,
    DocumentBatchCreateSchema,
    DocumentCreateTextSchema,
    DocumentCreateUrlSchema,
    DocumentEnableDisableSchema,
    DocumentMetadataBatchUpdateSchema,
    DocumentMetadataRegistryUpdateSchema,
    DocumentUpdateSchema,
    QABatchTestSchema,
    QAQuerySchema,
    QueryFeedbackListSchema,
    QueryFeedbackSchema,
    QueryFeedbackUpsertSchema,
    QueryHistoryListSchema,
    RetrievalEvalRequestSchema,
    RetrieveRequestSchema,
    SegmentBatchEnableDisableSchema,
    SegmentCreateSchema,
    SegmentEnableDisableSchema,
    SegmentUpdateSchema,
)
from .eval import require_verified_gateway

logger = logging.getLogger(__name__)

router = APIRouter()
RETRIEVAL_EVAL_MAX_CASES = 20
RETRIEVAL_EVAL_TIMEOUT_SECONDS = 30.0
_REPLAY_SNAPSHOT_ACTIONS = frozenset({"reprocess", "recover", "retry"})
_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL = (
    "Replay snapshot persistence is unavailable; document was not queued"
)
_PDF_MAX_PAGES_HARD_LIMIT = 2_000
_PDF_SPLIT_MAX_OUTPUT_BYTES_HARD_LIMIT = 96 * 1024 * 1024
DOCUMENT_PROGRESS_STREAM_POLL_SECONDS = 1.0
DOCUMENT_PROGRESS_STREAM_HEARTBEAT_SECONDS = 15.0
DOCUMENT_PROGRESS_STREAM_BATCH_LIMIT = 200

_UNRELEASED_MULTIMODAL_TYPES = frozenset({"image", "page_image", "mixed", "multimodal", "vision"})
_UNRELEASED_IMAGE_METADATA_KEYS = frozenset(
    {
        "associatedimages", "imagebytes", "imagecount", "imagepresignedurl", "images",
        "imagesegmentid", "imageurl", "rawimageurl", "storageurl", "vlmdescription",
    }
)


def _document_batch_store(svc: KnowledgeService) -> DocumentBatchStore:
    return DocumentBatchStore(getattr(svc.db, "_pool", None))


def _pipeline_execution_receipt(
    payload: dict[str, Any],
    *,
    dataset_id: str,
    document_id: str,
    execution_id: str | None,
) -> dict[str, Any]:
    """Preserve the legacy body and add a durable execution receipt."""

    normalized = str(execution_id or "").strip() or None
    return {
        **payload,
        "execution_id": normalized,
        "job_url": (
            f"/api/v1/knowledge/{dataset_id}/documents/{document_id}/"
            f"executions/{normalized}"
            if normalized
            else None
        ),
    }


def _document_metadata_manager(svc: KnowledgeService) -> DocumentMetadataManager:
    manager = getattr(svc, "_document_metadata_manager", None)
    if manager is None:
        manager = DocumentMetadataManager(svc)
        svc._document_metadata_manager = manager
    return manager


def _is_unreleased_multimodal_result(item: Any) -> bool:
    metadata = getattr(item, "metadata", None)
    metadata = metadata if isinstance(metadata, Mapping) else {}
    modality_values = (
        getattr(item, "content_type", None),
        getattr(item, "media_type", None),
        getattr(item, "modality", None),
        metadata.get("content_type"),
        metadata.get("media_type"),
        metadata.get("modality"),
    )
    for raw_value in modality_values:
        normalized = str(raw_value or "").strip().lower()
        if normalized in _UNRELEASED_MULTIMODAL_TYPES or normalized.startswith("image/"):
            return True
    return False


def _strip_unreleased_image_metadata(value: Any) -> Any:
    if isinstance(value, Mapping):
        projected: dict[str, Any] = {}
        for raw_key, nested in value.items():
            normalized_key = "".join(
                character for character in str(raw_key).lower() if character.isalnum()
            )
            if normalized_key in _UNRELEASED_IMAGE_METADATA_KEYS:
                continue
            projected[str(raw_key)] = _strip_unreleased_image_metadata(nested)
        return projected
    if isinstance(value, list):
        return [_strip_unreleased_image_metadata(item) for item in value]
    if isinstance(value, tuple):
        return tuple(_strip_unreleased_image_metadata(item) for item in value)
    return copy.deepcopy(value)


def _build_server_qa_llm_config(selector: Any, settings: Settings) -> Any:
    """Build QA generation config from the server allowlist and credentials.

    The request may tune bounded generation preferences, but it never selects
    an endpoint or supplies/borrows credentials.  This keeps all three QA
    routes on the same fail-closed provider contract as the RAGAS evaluator.
    """

    from ai_gateway_core.config import resolve_dashscope

    from ...services.knowledge.qa_service import LLMConfig, LLMProvider

    config = settings.ragas_eval
    provider_name = (
        str(selector.provider if selector and selector.provider else config.provider)
        .strip()
        .lower()
    )
    model = str(selector.model if selector and selector.model else config.model).strip()
    if provider_name not in config.allowed_providers:
        raise ValidationFailedError(f"QA provider is not allowlisted: {provider_name}")
    if model not in config.allowed_models:
        raise ValidationFailedError(f"QA model is not allowlisted: {model}")
    if provider_name != LLMProvider.DASHSCOPE.value:
        raise ValidationFailedError("Only the server-owned DashScope QA provider is enabled")

    api_key, resolved_base_url = resolve_dashscope("chat")
    if not api_key:
        raise ValidationFailedError("DashScope QA is not configured")
    base_url = str(config.base_url or "").strip()
    if not base_url:
        base_url = f"{resolved_base_url.rstrip('/')}/v1"

    return LLMConfig(
        provider=LLMProvider.DASHSCOPE,
        model=model,
        api_key=api_key,
        base_url=base_url,
        temperature=(
            float(selector.temperature) if selector and selector.temperature is not None else 0.1
        ),
        max_tokens=(
            int(selector.max_tokens) if selector and selector.max_tokens is not None else 2048
        ),
        timeout_seconds=config.timeout_seconds,
        system_prompt=(
            str(selector.system_prompt)
            if selector and selector.system_prompt is not None
            else LLMConfig.system_prompt
        ),
    )


def _require_authenticated_user(user: UserContext) -> None:
    """Reject synthetic/guest identities before any paid or expensive work."""

    if (
        not bool(getattr(user, "is_authenticated", False))
        or bool(getattr(user, "is_anonymous", False))
        or str(getattr(user, "user_id", "") or "").strip().lower() == "anonymous"
        or str(getattr(user, "user_type", "") or "").strip().lower() in {"anonymous", "guest"}
        or "guest" in {str(role).strip().lower() for role in (user.roles or [])}
    ):
        raise PermissionDeniedError("Authenticated user required")


def _index_lease_conflict(exc: IndexLeaseUnavailableError) -> HTTPException:
    """Map transient index publication contention to a retryable response."""

    return HTTPException(
        status_code=409,
        detail=str(exc),
        headers={"Retry-After": "1"},
    )


async def _require_authenticated_dataset_editor(
    svc: KnowledgeService,
    user: UserContext,
    dataset_id: str,
) -> dict[str, Any]:
    _require_authenticated_user(user)
    return await svc.require_dataset_access(user, dataset_id, required="editor")


def _require_safe_chunk_preview_config(config: Any) -> None:
    """Disable attacker-controlled regex execution in preview-only routes."""

    if config is None:
        return
    if hasattr(config, "model_dump"):
        config = config.model_dump(exclude_none=True)
    if not isinstance(config, Mapping):
        raise ValidationFailedError("chunk preview config must be an object")
    mode = str(config.get("mode") or "automatic").strip().lower()
    if (
        mode == "regex"
        or bool(config.get("regex_pattern"))
        or bool(config.get("regex"))
        or bool(config.get("heading_patterns"))
        or bool(config.get("page_marker"))
    ):
        raise ValidationFailedError("custom regex chunk preview is disabled")


async def _enqueue_document_or_conflict(
    worker: KnowledgeWorker,
    dataset_id: str,
    document_id: str,
) -> None:
    """Publish only a generation durably claimed by the database."""

    if not await _try_enqueue_document(worker, dataset_id, document_id):
        raise HTTPException(
            status_code=409,
            detail="Document could not enter a new ingestion generation",
        )


async def _try_enqueue_document(
    worker: KnowledgeWorker,
    dataset_id: str,
    document_id: str,
    *,
    action: str | None = None,
    recover_stage: str | None = None,
    execution_id: str | None = None,
) -> bool:
    try:
        return (
            await worker.enqueue(
                dataset_id,
                document_id,
                action=action,
                recover_stage=recover_stage,
                execution_id=execution_id,
            )
            is True
        )
    except IndexLeaseUnavailableError:
        return False


async def _record_ingest_execution(
    svc: KnowledgeService,
    *,
    dataset_id: str,
    document_id: str,
    action: str,
    trigger_source: str = "api",
    connection: Any | None = None,
) -> str | None:
    """Capture the replay snapshot at submission time (PRD T1 / addendum §1).

    The execution row is created BEFORE the claim so the worker replays exactly
    the configuration the operator saw, never a config mutated mid-flight.
    reprocess/recover/retry are fail-closed: both immutable snapshots and the
    document pin must be durable before the queue claim is attempted.
    """

    snapshot_required = action in _REPLAY_SNAPSHOT_ACTIONS
    record = getattr(svc.db, "record_pipeline_execution", None)
    if not callable(record):
        if snapshot_required:
            raise HTTPException(
                status_code=503,
                detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
            )
        return None
    try:
        connection_kw = {"connection": connection} if connection is not None else {}
        dataset = await svc.db.get_dataset(dataset_id, **connection_kw)
        document = await svc.db.get_document(document_id, **connection_kw)
    except Exception as exc:
        if snapshot_required:
            logger.exception(
                "Failed to read replay snapshot inputs for document %s",
                document_id,
            )
            raise HTTPException(
                status_code=503,
                detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
            ) from exc
        dataset, document = None, None

    if snapshot_required and not isinstance(dataset, Mapping):
        raise HTTPException(
            status_code=503,
            detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
        )
    if snapshot_required and not isinstance(document, Mapping):
        raise HTTPException(
            status_code=503,
            detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
        )

    raw_index_config = (
        dataset.get("index_config") if isinstance(dataset, Mapping) else None
    )
    if raw_index_config is None:
        raw_index_config = {}
    if not isinstance(raw_index_config, Mapping):
        if snapshot_required:
            raise HTTPException(
                status_code=503,
                detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
            )
        raw_index_config = {}
    index_config = copy.deepcopy(dict(raw_index_config))
    raw_chunking = index_config.get("chunking", {})
    if raw_chunking is None:
        raw_chunking = {}
    if not isinstance(raw_chunking, Mapping):
        if snapshot_required:
            raise HTTPException(
                status_code=503,
                detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
            )
        raw_chunking = {}
    chunking = copy.deepcopy(dict(raw_chunking))
    if snapshot_required:
        validate_persisted_chunking_config(chunking)

    doc_metadata = (
        document.get("metadata") if isinstance(document, Mapping) else None
    )
    processing_mode = str(
        doc_metadata.get("processing_mode", "text_only")
        if isinstance(doc_metadata, Mapping)
        else "text_only"
    ).strip().lower() or "text_only"
    input_snapshot: dict[str, Any] = {
        "index_config": index_config,
        # Kept as a top-level field for compatibility with pre-upgrade rows.
        "chunking": chunking,
        "processing_mode": processing_mode,
    }

    # PRD T1 item 7: route-submitted verbs record their immutable rule at the
    # same instant as the execution input. reembed is exempt because vector
    # repair never executes the chunking dialect.
    process_rule_id: str | None = None
    pin_rule = None
    if action != "reembed":
        record_rule = getattr(svc.db, "record_process_rule", None)
        pin_rule = getattr(svc.db, "pin_document_process_rule", None)
        if snapshot_required and (
            not callable(record_rule) or not callable(pin_rule)
        ):
            raise HTTPException(
                status_code=503,
                detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
            )
        if callable(record_rule):
            mode = str(chunking.get("mode") or "automatic").strip().lower()
            rule_snapshot: dict[str, Any] = {
                "index_config": copy.deepcopy(index_config),
                "chunking": copy.deepcopy(chunking),
                "processing_mode": processing_mode,
            }
            try:
                recorded_rule_id = await record_rule(
                    dataset_id,
                    mode=mode or "automatic",
                    rules=rule_snapshot,
                    **connection_kw,
                )
                process_rule_id = str(recorded_rule_id or "").strip() or None
                if snapshot_required and process_rule_id is None:
                    raise RuntimeError("process-rule snapshot returned no id")
            except Exception as exc:
                logger.exception(
                    "Failed to record process-rule snapshot for document %s",
                    document_id,
                )
                if snapshot_required:
                    raise HTTPException(
                        status_code=503,
                        detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
                    ) from exc
                process_rule_id = None

    try:
        recorded_execution_id = await record(
            document_id,
            dataset_id,
            action=action,
            trigger_source=trigger_source,
            process_rule_id=process_rule_id,
            input_snapshot=input_snapshot,
            **connection_kw,
        )
        execution_id = str(recorded_execution_id or "").strip() or None
        if snapshot_required and execution_id is None:
            raise RuntimeError("pipeline execution ledger returned no id")
    except Exception as exc:
        logger.exception(
            "Failed to record pipeline execution for document %s", document_id
        )
        if snapshot_required:
            raise HTTPException(
                status_code=503,
                detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
            ) from exc
        return None

    if process_rule_id and callable(pin_rule) and connection is None:
        try:
            pinned = await pin_rule(
                document_id, process_rule_id, idle_only=snapshot_required,
            )
            if not pinned:
                raise RuntimeError("document process-rule pin was not persisted")
        except Exception as exc:
            logger.exception(
                "Failed to pin process-rule snapshot for document %s",
                document_id,
            )
            await _fail_ingest_execution(
                svc,
                execution_id,
                "process-rule document pin failed before queue claim",
            )
            if snapshot_required:
                raise HTTPException(
                    status_code=503,
                    detail=_REPLAY_SNAPSHOT_UNAVAILABLE_DETAIL,
                ) from exc

    return execution_id


async def _submit_route_execution(
    svc: KnowledgeService,
    worker: KnowledgeWorker,
    dataset_id: str,
    document_id: str,
    *,
    action: str,
    recover_stage: str | None = None,
) -> str | None:
    """Commit the API execution, rule pin, and queue owner together.

    The shared document lease excludes dataset archive while the execution
    row is being created. A crash before commit leaves no orphan running row;
    after commit the durable waiting row survives an API process crash.
    """
    lease = getattr(svc.db, "document_index_update_lease", None)
    claim = getattr(svc.db, "claim_document_for_enqueue", None)
    if not callable(lease) or not callable(claim):
        # Lightweight route doubles use the original public worker contract.
        execution_id = await _record_ingest_execution(
            svc, dataset_id=dataset_id, document_id=document_id, action=action,
        )
        queued = await _try_enqueue_document(
            worker, dataset_id, document_id, action=action,
            recover_stage=recover_stage, execution_id=execution_id,
        )
        if not queued:
            await _fail_ingest_execution(
                svc, execution_id, f"{action} claim rejected or ineligible document",
            )
            raise HTTPException(409, "Document is already queued/processing or ineligible")
        return execution_id

    try:
        async with lease(dataset_id, document_id) as conn, conn.transaction():
            dataset = await svc.db.get_dataset(dataset_id, connection=conn)
            if not dataset:
                raise HTTPException(409, "Dataset is no longer available for indexing")
            from ...services.knowledge.worker import _require_enqueue_bm25_v2_available

            _require_enqueue_bm25_v2_available(svc, dataset.get("index_config") or {})
            execution_id = await _record_ingest_execution(
                svc, dataset_id=dataset_id, document_id=document_id,
                action=action, connection=conn,
            )
            if not execution_id or not await claim(
                dataset_id, document_id, action=action,
                recover_stage=recover_stage, execution_id=execution_id,
                pin_execution_rule=action in _REPLAY_SNAPSHOT_ACTIONS,
                connection=conn,
            ):
                raise HTTPException(409, "Document is already queued/processing or ineligible")
    except IndexLeaseUnavailableError as exc:
        raise _index_lease_conflict(exc) from exc
    await worker.enqueue_claimed(dataset_id, document_id)
    return execution_id


async def _fail_ingest_execution(
    svc: KnowledgeService, execution_id: str | None, error: str
) -> None:
    if not execution_id:
        return
    complete = getattr(svc.db, "complete_pipeline_execution", None)
    if not callable(complete):
        return
    with contextlib.suppress(Exception):
        await complete(execution_id, status="error", error=error)


def _stage_timestamp(value: Any) -> float | None:
    if value is None or not hasattr(value, "timestamp"):
        return None
    try:
        return float(value.timestamp())
    except Exception:
        return None


def _latest_stage_reached(document: Mapping[str, Any]) -> str | None:
    """Derive the recover branch from the furthest stage timestamp."""

    best_stage: str | None = None
    best_value: float | None = None
    for stage in ("indexing", "splitting", "parsing"):
        value = _stage_timestamp(document.get(f"{stage}_started_at"))
        if value is None:
            continue
        if best_value is None or value > best_value:
            best_stage, best_value = stage, value
    return best_stage


def _parse_document_progress_cursor(dataset_id: str, raw_value: str | None) -> int:
    """Parse the dataset-bound ``task:sequence`` SSE cursor."""

    raw = str(raw_value or "").strip()
    if not raw:
        return 0
    task, separator, sequence_text = raw.rpartition(":")
    if not separator or task != dataset_id:
        raise HTTPException(status_code=400, detail="Invalid document progress event cursor")
    try:
        sequence = int(sequence_text)
    except (TypeError, ValueError) as exc:
        raise HTTPException(status_code=400, detail="Invalid document progress event cursor") from exc
    if sequence < 0:
        raise HTTPException(status_code=400, detail="Invalid document progress event cursor")
    return sequence


def _format_document_progress_event(dataset_id: str, event: Mapping[str, Any]) -> str:
    """Format one durable event without exposing database internals."""

    try:
        sequence = int(event.get("event_sequence") or 0)
    except (TypeError, ValueError) as exc:
        raise ValueError("document progress event has an invalid sequence") from exc
    if sequence <= 0:
        raise ValueError("document progress event has an invalid sequence")
    event_type = str(event.get("event_type") or "progress").strip().lower()
    if event_type not in {"progress", "terminal", "deleted"}:
        event_type = "progress"
    payload = event.get("payload")
    if isinstance(payload, str):
        try:
            payload = json.loads(payload)
        except (TypeError, json.JSONDecodeError):
            payload = {}
    if not isinstance(payload, Mapping):
        payload = {}
    public_payload = dict(payload)
    # The durable trigger records the worker's raw exception for operators.
    # Never replay that internal detail over the user-facing SSE stream.
    if "error" in public_payload:
        public_payload["error"] = (
            "Document processing failed. Check the document status and ID."
            if public_payload["error"] else None
        )
    serialized = json.dumps(public_payload, ensure_ascii=False, separators=(",", ":"))
    return (
        f"id: {dataset_id}:{sequence}\n"
        f"event: {event_type}\n"
        f"data: {serialized}\n\n"
    )


def _format_document_progress_reset(dataset_id: str, sequence: int) -> str:
    """Tell a stale client to refresh its authoritative document snapshot."""

    return (
        f"id: {dataset_id}:{sequence}\n"
        "event: reset\n"
        'data: {"reason":"cursor_expired"}\n\n'
    )


async def _get_route_document_or_404(
    svc: KnowledgeService, dataset_id: str, document_id: str
) -> dict[str, Any]:
    document = await svc.db.get_document(document_id)
    if not document or str(document.get("dataset_id") or "") != dataset_id:
        raise HTTPException(status_code=404, detail="Document not found")
    return document


async def _require_active_document(
    svc: KnowledgeService,
    *,
    dataset: Mapping[str, Any],
    dataset_id: str,
    document_id: str,
) -> dict[str, Any]:
    """Resolve one document through the serving active-state authority."""

    tenant_id = str(dataset.get("tenant_id") or "").strip()
    normalized_dataset = str(dataset_id or "").strip()
    normalized_document = str(document_id or "").strip()
    if not tenant_id or not normalized_dataset or not normalized_document:
        raise ValidationFailedError("Document not found")

    # Fetch the bytes before the final active-state read. A disable/archive or
    # lifecycle marker committed during that fetch must retire this response.
    document = await svc.db.get_document(normalized_document)
    if not document or str(document.get("dataset_id") or "") != normalized_dataset:
        raise ValidationFailedError("Document not found")

    filter_active = getattr(svc.db, "filter_active_document_ids", None)
    if not callable(filter_active):
        raise ValidationFailedError("Document active-state authority is unavailable")
    active_ids = await filter_active(
        normalized_dataset,
        tenant_id,
        [normalized_document],
    )
    if set(active_ids or ()) != {normalized_document}:
        raise ValidationFailedError("Document not found")

    return document


async def _filter_active_route_segments(
    svc: KnowledgeService,
    *,
    dataset: Mapping[str, Any],
    dataset_id: str,
    segments: Sequence[Mapping[str, Any]],
) -> list[Mapping[str, Any]]:
    """Fail closed and retain only exact-scope active segment rows."""

    tenant_id = str(dataset.get("tenant_id") or "").strip()
    normalized_dataset = str(dataset_id or "").strip()
    candidate_ids = {
        str(segment.get("segment_id") or "").strip()
        for segment in segments
        if str(segment.get("segment_id") or "").strip()
    }
    if not candidate_ids:
        return []
    if not tenant_id or not normalized_dataset:
        raise ValidationFailedError("Segment active-state authority is unavailable")

    filter_active = getattr(svc.db, "filter_active_segment_ids", None)
    if not callable(filter_active):
        raise ValidationFailedError("Segment active-state authority is unavailable")
    active_ids = set(
        await filter_active(
            normalized_dataset,
            tenant_id,
            sorted(candidate_ids),
        )
        or ()
    )
    if not active_ids.issubset(candidate_ids):
        raise ValidationFailedError("Segment active-state authority returned unexpected IDs")
    return [
        segment
        for segment in segments
        if str(segment.get("segment_id") or "").strip() in active_ids
    ]


def _merge_config_patch(existing: Mapping[str, Any], patch: Mapping[str, Any]) -> dict[str, Any]:
    """Recursively merge explicitly supplied config fields into stored config."""

    merged = dict(existing)
    for key, value in patch.items():
        current = merged.get(key)
        if isinstance(current, Mapping) and isinstance(value, Mapping):
            merged[key] = _merge_config_patch(current, value)
        else:
            merged[key] = value
    return merged


async def require_admin_user(
    user: UserContext = Depends(get_user_context),
) -> UserContext:
    try:
        _require_authenticated_user(user)
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail="Admin role required") from exc
    roles = {str(role).strip().lower() for role in (user.roles or [])}
    tier = (
        str(getattr(user, "tier", getattr(user, "user_tier", "normal")) or "normal").strip().lower()
    )
    if tier != "admin" and "admin" not in roles:
        raise HTTPException(status_code=403, detail="Admin role required")
    return user


@router.get("/knowledge/datasets")
async def list_datasets(
    response: Response,
    limit: int = Query(default=200, ge=1, le=200),
    cursor: str | None = Query(default=None, max_length=1024),
    archived: bool = Query(default=False),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        page = (
            await svc.dataset_service.list_datasets_page(
                user, limit=limit, cursor=cursor, archived_only=True,
            )
            if archived is True else
            await svc.list_datasets_page(user, limit=limit, cursor=cursor)
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    if page["next_cursor"]:
        response.headers["X-Next-Cursor"] = page["next_cursor"]
    return page["items"]


@router.post("/knowledge/datasets")
async def create_dataset(
    request: Request,
    payload: DatasetCreateSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    # Auth is handled by Gateway proxy — KB Service trusts X-User-Id/X-Tenant-Id headers
    try:
        return await svc.create_dataset(user, payload.model_dump())
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.get("/knowledge/datasets/{dataset_id}")
async def get_dataset(
    dataset_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        result = svc.sanitize_dataset_for_response(dataset)
        result["my_permission"] = await svc._effective_dataset_permission(dataset, user)
        return result
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc))


@router.put("/knowledge/datasets/{dataset_id}")
async def update_dataset(
    dataset_id: str,
    patch: DatasetUpdateSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await svc.update_dataset(user, dataset_id, patch.model_dump(exclude_none=True))
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


class DatasetArchiveRequest(BaseModel):
    archived: bool
    reason: str | None = Field(default=None, max_length=500)


@router.patch("/knowledge/datasets/{dataset_id}/archive")
async def set_dataset_archived(
    dataset_id: str,
    payload: DatasetArchiveRequest = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await svc.dataset_service.set_dataset_archived(
            user, dataset_id, archived=payload.archived, reason=payload.reason,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except IndexLeaseUnavailableError as exc:
        raise HTTPException(
            status_code=409, detail=str(exc), headers={"Retry-After": "1"},
        ) from exc
    except ValidationFailedError as exc:
        status = 404 if str(exc) == "dataset not found" else 409
        raise HTTPException(status_code=status, detail=str(exc)) from exc


@router.delete("/knowledge/datasets/{dataset_id}")
async def delete_dataset(
    dataset_id: str,
    request: Request,
    payload: DatasetDeleteSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    from ..deletion_confirmation import require_deletion_confirmation

    require_deletion_confirmation(request, user, dataset_id, payload.confirmation)
    try:
        ok = await svc.delete_dataset(
            user,
            dataset_id,
            deletion_confirmed=True,
            reason=payload.reason,
        )
        return {"status": "success" if ok else "not_found", "dataset_id": dataset_id}
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except IndexLeaseUnavailableError as exc:
        # Deletion takes the same index lifecycle lease as ingestion/reindex.
        # Losing that race is transient contention, not a server fault: report
        # it as a retryable conflict like every other lease-guarded route.
        raise HTTPException(
            status_code=409,
            detail=str(exc),
            headers={"Retry-After": "1"},
        ) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.get("/knowledge/datasets/{dataset_id}/permissions")
async def list_dataset_permissions(
    dataset_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await svc.list_dataset_permissions(user, dataset_id)
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))


@router.post("/knowledge/datasets/{dataset_id}/permissions")
async def grant_dataset_permission(
    dataset_id: str,
    payload: DatasetPermissionGrantSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        await svc.grant_dataset_permission(
            user,
            dataset_id,
            subject_type=payload.subject_type,
            subject_id=payload.subject_id,
            permission=payload.permission,
        )
        return {"status": "success"}
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.delete("/knowledge/datasets/{dataset_id}/permissions")
async def revoke_dataset_permission(
    dataset_id: str,
    subject_type: str = Query(...),
    subject_id: str = Query(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        ok = await svc.revoke_dataset_permission(user, dataset_id, subject_type, subject_id)
        return {"status": "success" if ok else "not_found"}
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))


@router.get("/knowledge/{dataset_id}/documents")
async def list_documents(
    dataset_id: str,
    response: Response,
    # D4 (frontend handoff): pagination. The body stays a bare array for
    # backwards compatibility; the page total is exposed via X-Total-Count.
    limit: int = Query(default=200, ge=1, le=200),
    offset: int = Query(default=0, ge=0),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        page = await svc.list_documents_page(
            user,
            dataset_id,
            limit=limit,
            offset=offset,
        )
        response.headers["X-Total-Count"] = str(page["total"])
        return page["items"]
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        if str(exc) == "dataset content generation changed during read; retry the request":
            raise HTTPException(
                status_code=409,
                detail=str(exc),
                headers={"Retry-After": "1"},
            ) from exc
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@router.post("/knowledge/{dataset_id}/documents/text")
async def create_document_text(
    dataset_id: str,
    payload: DocumentCreateTextSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
):
    try:
        doc = await svc.create_document_from_text(
            user,
            dataset_id,
            title=payload.title,
            content=payload.content,
            metadata=payload.metadata,
        )
        await _enqueue_document_or_conflict(worker, dataset_id, doc["document_id"])
        return doc
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


class _PDFSplitError(ValueError):
    """The uploaded PDF cannot be split safely."""


class _PDFSplitLimitExceeded(_PDFSplitError):
    """A PDF page or expanded-output safety limit was exceeded."""


@dataclass(frozen=True)
class _PDFSplitPart:
    first_page: int
    last_page: int
    path: str


def _split_pdf_parts_to_temp_sync(
    source_path: str | Path,
    pages_per_part: int,
    *,
    max_pages: int,
    max_output_bytes: int,
    temp_dir: str,
) -> tuple[int, list[_PDFSplitPart]]:
    """Split a path-backed PDF into bounded temporary files.

    The source document and one output document are open at a time; PDF bytes
    are never accumulated in memory. The caller must consume and remove the
    returned paths. Any failure before return removes every path created here.
    Run this CPU-bound function through ``asyncio.to_thread``.
    """
    import tempfile

    try:
        import pymupdf as fitz
    except ImportError:
        import fitz  # type: ignore

    if pages_per_part < 1:
        raise _PDFSplitError("PDF pages per part must be positive")
    if max_pages < 1 or max_output_bytes < 1:
        raise _PDFSplitError("PDF safety limits must be positive")

    created_paths: list[Path] = []
    source_doc = None
    try:
        source_doc = fitz.open(str(source_path))
        total_pages = len(source_doc)
        if total_pages < 1:
            raise _PDFSplitError("PDF contains no pages")
        if total_pages > max_pages:
            raise _PDFSplitLimitExceeded(
                f"PDF has {total_pages} pages; maximum allowed is {max_pages}"
            )

        parts: list[_PDFSplitPart] = []
        total_output_bytes = 0
        for start in range(0, total_pages, pages_per_part):
            end = min(start + pages_per_part, total_pages)
            part_doc = fitz.open()
            try:
                part_doc.insert_pdf(source_doc, from_page=start, to_page=end - 1)
                with tempfile.NamedTemporaryFile(
                    delete=False,
                    suffix=".pdf",
                    prefix="kb_pdf_part_",
                    dir=temp_dir,
                ) as handle:
                    part_path = Path(handle.name)
                created_paths.append(part_path)
                part_doc.save(str(part_path))
            finally:
                with contextlib.suppress(Exception):
                    part_doc.close()

            part_size = part_path.stat().st_size
            total_output_bytes += part_size
            if total_output_bytes > max_output_bytes:
                raise _PDFSplitLimitExceeded(
                    "PDF split output exceeds the cumulative safety limit of "
                    f"{max_output_bytes} bytes"
                )
            parts.append(
                _PDFSplitPart(
                    first_page=start + 1,
                    last_page=end,
                    path=str(part_path),
                )
            )
        return total_pages, parts
    except _PDFSplitError:
        for path in created_paths:
            with contextlib.suppress(OSError):
                path.unlink(missing_ok=True)
        raise
    except Exception as exc:
        for path in created_paths:
            with contextlib.suppress(OSError):
                path.unlink(missing_ok=True)
        raise _PDFSplitError("PDF could not be split safely") from exc
    finally:
        if source_doc is not None:
            with contextlib.suppress(Exception):
                source_doc.close()


@router.post("/knowledge/{dataset_id}/documents/upload")
async def upload_document(
    dataset_id: str,
    file: UploadFile = File(...),
    processing_mode: str = Form("auto"),  # auto | text_only | scanned | multimodal
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
    settings: Settings = Depends(get_settings),
):
    """
    Upload a document to the knowledge base.

    Streams the upload to a temporary file to avoid loading large files
    entirely into memory. The direct-service default is 16 MiB and configured
    limits are clamped to 48 MiB.

    Args:
        dataset_id: Target dataset ID
        file: Document file (PDF, DOCX, TXT, etc.)
        processing_mode: Processing mode - one of:
            - auto: Automatic detection (default) - intelligently detects document type
            - text_only: Traditional OCR + text embedding
            - scanned: Page-as-Image vision embedding (for scanned PDFs)
            - multimodal: Combined text + image embedding
    """
    import tempfile

    ALLOWED_EXTENSIONS = {".pdf", ".docx", ".txt", ".md", ".html"}
    KB_MAX_FILE_SIZE_MB = max(1, min(settings.kb_max_file_size_mb, 48))
    KB_MAX_FILE_SIZE = KB_MAX_FILE_SIZE_MB * 1024 * 1024
    PDF_MAX_PAGES = max(
        1,
        min(
            int(getattr(settings, "kb_pdf_max_pages", _PDF_MAX_PAGES_HARD_LIMIT)),
            _PDF_MAX_PAGES_HARD_LIMIT,
        ),
    )
    PDF_SPLIT_MAX_OUTPUT_BYTES = max(
        1,
        min(
            int(
                getattr(
                    settings,
                    "kb_pdf_split_max_output_bytes",
                    _PDF_SPLIT_MAX_OUTPUT_BYTES_HARD_LIMIT,
                )
            ),
            _PDF_SPLIT_MAX_OUTPUT_BYTES_HARD_LIMIT,
        ),
    )
    CHUNK_SIZE = 64 * 1024  # 64KB

    temp_workspace = None
    document_creation_started = False
    try:
        dataset = await _require_authenticated_dataset_editor(svc, user, dataset_id)
        _require_dataset_index_writable(dataset)

        normalized_processing_mode = str(processing_mode or "auto").strip().lower()
        if normalized_processing_mode == "auto":
            normalized_processing_mode = "text_only"
        if normalized_processing_mode != "text_only":
            raise ValidationFailedError(
                "scanned and multimodal uploads are disabled until the unified "
                "multimodal index profile is released"
            )
        processing_mode = normalized_processing_mode

        filename = file.filename or "upload"
        ext = Path(filename).suffix.lower()

        if ext not in ALLOWED_EXTENSIONS:
            raise ValidationFailedError(
                f"Unsupported file type: {ext}. Allowed: {', '.join(sorted(ALLOWED_EXTENSIONS))}"
            )

        # One private workspace owns the source and every derived part. Its
        # final cleanup is the last-resort fence for cancellation or failures.
        temp_workspace = tempfile.TemporaryDirectory(prefix="kb_upload_")
        temp_path = Path(temp_workspace.name) / f"source{ext}"

        size_bytes = 0
        with temp_path.open("wb") as out:
            while True:
                chunk = await file.read(CHUNK_SIZE)
                if not chunk:
                    break
                size_bytes += len(chunk)
                if size_bytes > KB_MAX_FILE_SIZE:
                    raise HTTPException(
                        status_code=413,
                        detail=(
                            f"File too large: {size_bytes / 1024 / 1024:.1f}MB exceeds "
                            f"limit of {KB_MAX_FILE_SIZE_MB}MB"
                        ),
                    )
                out.write(chunk)

        logger.info(
            "Upload started: file=%s, size=%d, dataset=%s, mode=%s",
            filename,
            size_bytes,
            dataset_id,
            processing_mode,
        )

        PDF_SPLIT_THRESHOLD = (
            max(0, min(int(settings.kb_pdf_split_max_size_mb), 48)) * 1024 * 1024
        )
        PDF_SPLIT_PAGES = max(
            1,
            min(int(settings.kb_pdf_split_pages_per_part), PDF_MAX_PAGES),
        )

        # Probe every PDF from its path before any document row or object is
        # published. This bounds compressed high-page-count files as well as
        # PDFs large enough to enter the split path.
        if ext == ".pdf":
            detection = await DocumentTypeDetector().detect(
                temp_path,
                filename=filename,
                mime_type=file.content_type or "application/pdf",
            )
            if detection.page_count is None:
                raise ValidationFailedError("Invalid or unreadable PDF")
            if detection.page_count > PDF_MAX_PAGES:
                raise HTTPException(
                    status_code=413,
                    detail=(
                        f"PDF has {detection.page_count} pages; maximum allowed is "
                        f"{PDF_MAX_PAGES}"
                    ),
                )

        if ext == ".pdf" and size_bytes > PDF_SPLIT_THRESHOLD:
            try:
                total_pages, parts = await asyncio.to_thread(
                    _split_pdf_parts_to_temp_sync,
                    temp_path,
                    PDF_SPLIT_PAGES,
                    max_pages=PDF_MAX_PAGES,
                    max_output_bytes=PDF_SPLIT_MAX_OUTPUT_BYTES,
                    temp_dir=temp_workspace.name,
                )
            except _PDFSplitLimitExceeded as exc:
                raise HTTPException(status_code=413, detail=str(exc)) from exc
            except _PDFSplitError as exc:
                logger.warning("Rejected unsafe PDF split for %s: %s", filename, exc)
                raise ValidationFailedError("Invalid or unreadable PDF") from exc

            logger.info(
                "Auto-splitting large PDF: %s (%d pages, %.1fMB) into %d parts",
                filename,
                total_pages,
                size_bytes / 1024 / 1024,
                len(parts),
            )

            results = []
            for i, part in enumerate(parts):
                part_name = (
                    f"{Path(filename).stem}_Part_{i + 1}_"
                    f"p{part.first_page}-{part.last_page}.pdf"
                )
                part_path = Path(part.path)
                try:
                    require_parser_budget(part_name, ".pdf", part_path.stat().st_size)
                    # The service contract still accepts bytes, so load only
                    # the current bounded part and release it before enqueueing.
                    part_bytes = await asyncio.to_thread(part_path.read_bytes)
                    try:
                        document_creation_started = True
                        doc = await svc.create_document_from_upload(
                            user,
                            dataset_id,
                            filename=part_name,
                            content_bytes=part_bytes,
                            mime_type="application/pdf",
                            processing_mode=processing_mode,
                        )
                    finally:
                        del part_bytes
                finally:
                    with contextlib.suppress(OSError):
                        part_path.unlink(missing_ok=True)
                await _enqueue_document_or_conflict(
                    worker,
                    dataset_id,
                    doc["document_id"],
                )
                results.append(doc)
                logger.info(
                    "Part %d/%d created: %s, pages %d-%d, doc=%s",
                    i + 1,
                    len(parts),
                    part_name,
                    part.first_page,
                    part.last_page,
                    doc["document_id"],
                )

            return {
                "status": "split_and_queued",
                "original_filename": filename,
                "total_pages": total_pages,
                "parts": len(results),
                "documents": results,
            }

        # --- Standard single-document upload ---
        require_parser_budget(filename, ext, size_bytes)
        # The storage contract accepts bytes. This read remains bounded by the
        # 48 MiB compressed-upload fence and runs off the event loop.
        content = await asyncio.to_thread(temp_path.read_bytes)
        document_creation_started = True
        doc = await svc.create_document_from_upload(
            user,
            dataset_id,
            filename=filename,
            content_bytes=content,
            mime_type=file.content_type,
            processing_mode=processing_mode,
        )
        logger.info("Document created: id=%s, enqueueing for ingestion...", doc["document_id"])
        await _enqueue_document_or_conflict(worker, dataset_id, doc["document_id"])
        logger.info(
            "Document enqueued: id=%s, worker queue size ~%d",
            doc["document_id"],
            worker.queue.qsize(),
        )
        return doc
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        if document_creation_started:
            raise HTTPException(
                status_code=409,
                detail="Upload outcome is uncertain; check the document list before retrying",
            ) from exc
        raise HTTPException(status_code=400, detail=str(exc))
    finally:
        if temp_workspace is not None:
            with contextlib.suppress(Exception):
                temp_workspace.cleanup()


@router.post("/knowledge/{dataset_id}/documents/batch-upload")
async def batch_upload_documents(
    dataset_id: str,
    files: list[UploadFile] = File(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
    settings: Settings = Depends(get_settings),
):
    """
    批量上传文档到知识库（支持并行处理）

    支持格式: PDF, DOCX, TXT, MD, HTML
    最大文件数: 63
    最大单文件大小: 由系统配置决定

    使用流式写入临时文件，避免大文件占用过多内存。

    返回:
        {
            "batch_id": "uuid",
            "total": 5,
            "accepted": 5,
            "rejected": 0,
            "documents": [...],
            "errors": []
        }
    """
    import os
    import tempfile
    import uuid as uuid_lib

    import aiofiles

    try:
        dataset = await _require_authenticated_dataset_editor(svc, user, dataset_id)
        _require_dataset_index_writable(dataset)

        MAX_FILES = 50
        CHUNK_SIZE = 64 * 1024  # 64KB chunks for streaming
        ALLOWED_EXTENSIONS = {".pdf", ".docx", ".txt", ".md", ".html"}
        max_file_size_mb = max(
            1,
            min(settings.kb_max_file_size_mb, 48),
        )
        max_file_size = max_file_size_mb * 1024 * 1024
        max_batch_size_mb = max(
            1,
            min(
                settings.kb_max_batch_size_mb,
                48,
            ),
        )
        max_batch_size = max_batch_size_mb * 1024 * 1024

        if len(files) > MAX_FILES:
            raise ValidationFailedError(f"Maximum {MAX_FILES} files allowed per batch")

        if not files:
            raise ValidationFailedError("No files provided")

        batch_id = str(uuid_lib.uuid4())
        documents = []
        errors = []
        accepted_bytes = 0

        for file_index, file in enumerate(files):
            filename = file.filename or "unknown"
            ext = Path(filename).suffix.lower()
            document_creation_started = False

            # Validate extension
            if ext not in ALLOWED_EXTENSIONS:
                errors.append(
                    {
                        "file_index": file_index,
                        "filename": filename,
                        "retry_safe": True,
                        "error": (
                            f"Unsupported file type: {ext}. "
                            f"Allowed: {', '.join(ALLOWED_EXTENSIONS)}"
                        ),
                    }
                )
                continue

            temp_path = None
            try:
                # Stream file to temp location to avoid memory exhaustion
                # Use tempfile with delete=False so we control cleanup
                with tempfile.NamedTemporaryFile(
                    delete=False, suffix=ext, prefix=f"batch_{batch_id[:8]}_"
                ) as tmp:
                    temp_path = tmp.name

                # Stream write using aiofiles
                file_size = 0
                async with aiofiles.open(temp_path, "wb") as out_file:
                    while True:
                        chunk = await file.read(CHUNK_SIZE)
                        if not chunk:
                            break
                        file_size += len(chunk)
                        if file_size > max_file_size:
                            raise ValidationFailedError(
                                f"File too large: {file_size / 1024 / 1024:.1f}MB "
                                f"exceeds limit of {max_file_size_mb}MB"
                            )
                        if accepted_bytes + file_size > max_batch_size:
                            raise ValidationFailedError(
                                f"Batch upload exceeds aggregate limit of {max_batch_size_mb}MB"
                            )
                        await out_file.write(chunk)

                if file_size <= 0:
                    raise ValidationFailedError("Empty files are not accepted")
                require_parser_budget(filename, ext, file_size)
                accepted_bytes += file_size

                # Read back for processing (file is now on disk, not all in memory at once)
                async with aiofiles.open(temp_path, "rb") as in_file:
                    content = await in_file.read()

                # Create document record
                document_creation_started = True
                doc = await svc.create_document_from_upload(
                    user,
                    dataset_id,
                    filename=filename,
                    content_bytes=content,
                    mime_type=file.content_type,
                    processing_mode="text_only",
                )

                # Add batch metadata
                doc["batch_id"] = batch_id
                documents.append((file_index, doc))

            except ValidationFailedError as exc:
                retry_safe = not document_creation_started
                errors.append(
                    {
                        "file_index": file_index,
                        "filename": filename,
                        "error": (
                            str(exc)
                            if retry_safe
                            else "Upload state is uncertain; check the document list before retrying"
                        ),
                        "retry_safe": retry_safe,
                    }
                )
            except Exception as exc:
                logger.error(
                    "Batch %s file %d failed unexpectedly (%s)",
                    batch_id,
                    file_index,
                    type(exc).__name__,
                )
                errors.append(
                    {
                        "file_index": file_index,
                        "filename": filename,
                        "error": "Upload state is uncertain; check the document list before retrying",
                        "retry_safe": False,
                    }
                )
            finally:
                # Clean up temp file
                if temp_path and os.path.exists(temp_path):
                    with contextlib.suppress(Exception):
                        os.unlink(temp_path)

        # Enqueue all documents for parallel processing
        # Worker will process them based on document_worker_concurrency setting
        queued_documents = []
        for file_index, doc in documents:
            if await _try_enqueue_document(worker, dataset_id, doc["document_id"]):
                queued_documents.append(doc)
            else:
                errors.append(
                    {
                        "file_index": file_index,
                        "document_id": doc["document_id"],
                        "filename": doc.get("title"),
                        "error": "Document was not accepted by the durable ingestion queue",
                        "retry_safe": False,
                    }
                )

        return {
            "batch_id": batch_id,
            "total": len(files),
            "accepted": len(queued_documents),
            "rejected": len(errors),
            "documents": queued_documents,
            "errors": errors,
        }

    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.post("/knowledge/{dataset_id}/documents/url")
async def create_document_url(
    dataset_id: str,
    payload: DocumentCreateUrlSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
):
    try:
        doc = await svc.create_document_from_url(
            user,
            dataset_id,
            url=payload.url,
            title=payload.title,
            metadata=payload.metadata,
        )
        await _enqueue_document_or_conflict(worker, dataset_id, doc["document_id"])
        return doc
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.post("/knowledge/{dataset_id}/documents/images")
async def upload_images(
    dataset_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Keep the incomplete multimodal write path unavailable."""
    try:
        await svc.require_dataset_access(user, dataset_id, required="editor")
        raise HTTPException(
            status_code=503,
            detail="Multimodal image ingestion is not enabled for this release.",
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))


@router.get("/knowledge/images/{segment_id}")
async def get_image_segment(
    segment_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """
    Serve image content for a segment.

    Security:
    - Requires dataset viewer permission
    - For local file:// URLs: validates HMAC signature and expiry
    - Path traversal protection via base_path check
    """
    try:
        get_active_segment_by_tenant = getattr(
            svc.db,
            "get_active_segment_by_tenant",
            None,
        )
        if not callable(get_active_segment_by_tenant):
            raise ValidationFailedError("Image active-state authority is unavailable")
        segment = await get_active_segment_by_tenant(segment_id, user.tenant_id)
        if not segment:
            raise HTTPException(status_code=404, detail="Image not found")

        dataset_id = str(segment.get("dataset_id") or "")
        if not dataset_id:
            raise HTTPException(status_code=404, detail="Image not found")

        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        get_segment_scoped = getattr(svc.db, "get_segment_scoped", None)
        if not callable(get_segment_scoped):
            raise ValidationFailedError("Image active-state authority is unavailable")
        segment = await get_segment_scoped(
            segment_id,
            dataset_id,
            str(dataset.get("tenant_id") or ""),
        )
        if not segment or segment.get("content_type") != "image":
            raise HTTPException(status_code=404, detail="Image not found")
        raise HTTPException(
            status_code=503,
            detail="Multimodal image serving is not enabled for this release.",
        )

    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc))


@router.get("/knowledge/{dataset_id}/documents/progress")
async def stream_document_progress(
    dataset_id: str,
    request: Request,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Stream durable document progress with ``Last-Event-ID`` replay.

    The authorization and event-store checks happen before returning the
    streaming response so callers receive a real 403/404/503 status rather
    than an error after SSE headers have already been sent.  The generator
    only reads the append-only ledger; client disconnects release the request
    without holding a database connection or performing a write.
    """

    raw_cursor = request.headers.get("last-event-id")
    cursor = _parse_document_progress_cursor(
        dataset_id,
        raw_cursor,
    )
    reset_sequence: int | None = None
    try:
        await svc.require_dataset_access(user, dataset_id, required="viewer")
        list_events = getattr(getattr(svc, "db", None), "list_document_progress_events", None)
        event_bounds = getattr(
            getattr(svc, "db", None),
            "get_document_progress_event_bounds",
            None,
        )
        if not callable(list_events) or not callable(event_bounds):
            raise RuntimeError("document progress event store is unavailable")
        # A fresh page already owns an authoritative document-list snapshot, so
        # it starts at the current watermark instead of replaying the ledger's
        # complete history. Last-Event-ID reconnects retain lossless replay.
        oldest_sequence, newest_sequence = await event_bounds(dataset_id)
        newest_sequence = newest_sequence or 0
        if not str(raw_cursor or "").strip():
            cursor = newest_sequence
        elif cursor > newest_sequence or (
            oldest_sequence is not None and cursor + 1 < oldest_sequence
        ):
            # Migration 112 bounds retention. A cursor outside that window
            # receives one reset event so the client refreshes the current
            # snapshot and resumes from a valid durable watermark.
            cursor = newest_sequence
            reset_sequence = newest_sequence
        # This read verifies the event table before StreamingResponse commits
        # headers and closes the race between the watermark and generator.
        await list_events(dataset_id, after_sequence=cursor, limit=1)
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc)) from exc
    except HTTPException:
        raise
    except Exception as exc:
        logger.warning(
            "Document progress stream preflight failed",
            extra={"dataset_id": dataset_id},
            exc_info=True,
        )
        raise HTTPException(
            status_code=503,
            detail="Document progress stream is unavailable",
        ) from exc

    async def event_generator():
        sequence = cursor
        last_heartbeat = asyncio.get_running_loop().time()
        yield ": connected\nretry: 2000\n\n"
        if reset_sequence is not None:
            yield _format_document_progress_reset(dataset_id, reset_sequence)
        try:
            while not await request.is_disconnected():
                events = await list_events(
                    dataset_id,
                    after_sequence=sequence,
                    limit=DOCUMENT_PROGRESS_STREAM_BATCH_LIMIT,
                )
                emitted = False
                for event in events:
                    try:
                        event_sequence = int(event.get("event_sequence") or 0)
                    except (TypeError, ValueError):
                        logger.warning(
                            "Ignoring malformed document progress event",
                            extra={"dataset_id": dataset_id},
                        )
                        continue
                    if event_sequence <= sequence:
                        continue
                    sequence = event_sequence
                    yield _format_document_progress_event(dataset_id, event)
                    emitted = True
                if emitted:
                    continue

                now = asyncio.get_running_loop().time()
                if now - last_heartbeat >= DOCUMENT_PROGRESS_STREAM_HEARTBEAT_SECONDS:
                    yield ": keep-alive\n\n"
                    last_heartbeat = now
                await asyncio.sleep(DOCUMENT_PROGRESS_STREAM_POLL_SECONDS)
        except asyncio.CancelledError:
            # Starlette cancels the generator when the client disconnects.
            # No connection is held across iterations, so cancellation is the
            # complete cleanup path.
            raise
        except Exception:
            # Headers are already committed; let the client reconnect using
            # the last emitted ID.  Never turn a transient read failure into a
            # synthetic terminal event.
            logger.warning(
                "Document progress stream ended after a read failure",
                extra={"dataset_id": dataset_id},
                exc_info=True,
            )

    return StreamingResponse(
        event_generator(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )


@router.get("/knowledge/{dataset_id}/documents/{document_id}")
async def get_document(
    dataset_id: str,
    document_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await svc.get_document(user, dataset_id, document_id)
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc))


@router.get("/knowledge/{dataset_id}/documents/{document_id}/source")
async def get_active_document_source(
    dataset_id: str,
    document_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Current serving source for citations, including editor identities."""
    try:
        await svc.get_document(user, dataset_id, document_id)
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        document = await _require_active_document(
            svc, dataset=dataset, dataset_id=dataset_id, document_id=document_id,
        )
        await _require_unchanged_dataset_content(svc, user, dataset_id, generation)
        return _with_display_status(document)
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc))


@router.post("/knowledge/{dataset_id}/documents/{document_id}/reindex")
async def reindex_document(
    dataset_id: str,
    document_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
):
    """PRD T1 item 3: reindex is the reembed verb — in-place vector repair.

    The document is never re-parsed or re-split; persisted chunks keep their
    segment and point identity, so the serving generation never goes dark.
    """

    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        document = await _get_route_document_or_404(svc, dataset_id, document_id)
        if str(document.get("status") or "") == "waiting":
            # Contract B2: an already-queued generation belongs to the queue.
            # Rejecting here also prevents the identical-verb re-claim from
            # re-pinning a fresh ledger row over the queued one's.
            raise HTTPException(
                status_code=409,
                detail="Document is already queued; the durable queue owns this generation",
            )
        execution_id = await _submit_route_execution(
            svc, worker, dataset_id, document_id, action="reembed",
        )
        logger.info("Reindex (reembed) queued for document %s (dataset=%s)", document_id, dataset_id)
        return _pipeline_execution_receipt(
            {"status": "queuing", "document_id": document_id},
            dataset_id=dataset_id,
            document_id=document_id,
            execution_id=execution_id,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


async def _require_special_replay_admission(
    worker: KnowledgeWorker,
    dataset_id: str,
    document_id: str,
    document: dict[str, Any],
    dataset: dict[str, Any],
    *,
    action: str,
) -> None:
    try:
        await worker.require_safe_special_replay_admission(
            KnowledgeIngestTask(dataset_id=dataset_id, document_id=document_id),
            document, dataset, action=action,
        )
    except ValidationFailedError as exc:
        if str(exc) == SPECIALIZED_REBUILD_UNAVAILABLE:
            raise HTTPException(
                status_code=409,
                detail={
                    "code": SPECIALIZED_REBUILD_UNAVAILABLE,
                    "message": "Specialized document replay requires administrator review",
                },
            ) from None
        raise


@router.post("/knowledge/{dataset_id}/documents/{document_id}/reprocess")
async def reprocess_document(
    dataset_id: str,
    document_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
):
    """PRD T1 item 3: full pipeline rerun with the submission-time snapshot.

    Replays the chunking configuration captured when THIS request was
    submitted, so a concurrent dataset-config change can never split the
    generation mid-flight (addendum §1 anti-drift contract).
    """

    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        document = await _get_route_document_or_404(svc, dataset_id, document_id)
        if str(document.get("status") or "") in {
            "waiting", "parsing", "splitting", "indexing",
        }:
            raise HTTPException(
                status_code=409,
                detail="Document is already queued or processing; the current execution owns this generation",
            )
        await _require_special_replay_admission(
            worker, dataset_id, document_id, document, dataset, action="reprocess",
        )
        _require_dataset_index_writable(
            await svc.require_dataset_access(user, dataset_id, required="editor")
        )
        execution_id = await _submit_route_execution(
            svc, worker, dataset_id, document_id, action="reprocess",
        )
        logger.info("Reprocess queued for document %s (dataset=%s)", document_id, dataset_id)
        return _pipeline_execution_receipt(
            {
                "status": "queuing",
                "document_id": document_id,
                "action": "reprocess",
            },
            dataset_id=dataset_id,
            document_id=document_id,
            execution_id=execution_id,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.post("/knowledge/{dataset_id}/documents/{document_id}/recover")
async def recover_document(
    dataset_id: str,
    document_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
):
    """PRD T1 item 4: manual incremental continuation of a dead generation.

    The replay branch is derived from the furthest stage timestamp: a death
    in 'indexing' re-embeds persisted chunks in place; deaths in
    'parsing'/'splitting' redo the full pipeline. Documents still mid-flight
    are owned by automatic crash recovery and are rejected here.
    """

    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        document = await _get_route_document_or_404(svc, dataset_id, document_id)
        status = str(document.get("status") or "")
        if status == "waiting":
            raise HTTPException(
                status_code=409,
                detail="Document is already queued; automatic crash recovery owns it",
            )
        if status in ("parsing", "splitting", "indexing"):
            raise HTTPException(
                status_code=409,
                detail="Document is still processing; automatic crash recovery owns it",
            )
        await _require_special_replay_admission(
            worker, dataset_id, document_id, document, dataset, action="recover",
        )
        _require_dataset_index_writable(
            await svc.require_dataset_access(user, dataset_id, required="editor")
        )
        recover_stage = _latest_stage_reached(document)
        execution_id = await _submit_route_execution(
            svc, worker, dataset_id, document_id, action="recover",
            recover_stage=recover_stage,
        )
        logger.info(
            "Recover queued for document %s (dataset=%s, stage=%s)",
            document_id,
            dataset_id,
            recover_stage,
        )
        receipt: dict[str, Any] = {
            "status": "queuing",
            "document_id": document_id,
            "action": "recover",
        }
        if recover_stage:
            receipt["recover_stage"] = recover_stage
        return _pipeline_execution_receipt(
            receipt,
            dataset_id=dataset_id,
            document_id=document_id,
            execution_id=execution_id,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.post("/knowledge/{dataset_id}/documents/{document_id}/retry")
async def retry_document(
    dataset_id: str,
    document_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
):
    """PRD T1 item 4: rerun from a pinned snapshot with atomic publication.

    Unlike recover-from-indexing, retry re-enters parsing/chunking. The prior
    serving rows and points remain readable until the replacement commits;
    failure or cancellation restores the old generation.
    """

    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        document = await _get_route_document_or_404(svc, dataset_id, document_id)
        status = str(document.get("status") or "")
        if status == "waiting":
            raise HTTPException(
                status_code=409,
                detail="Document is already queued; wait for it to run or be recovered",
            )
        if status in ("parsing", "splitting", "indexing"):
            raise HTTPException(
                status_code=409,
                detail="Document is still processing; wait for it to finish or be recovered",
            )
        await _require_special_replay_admission(
            worker, dataset_id, document_id, document, dataset, action="retry",
        )
        _require_dataset_index_writable(
            await svc.require_dataset_access(user, dataset_id, required="editor")
        )
        execution_id = await _submit_route_execution(
            svc, worker, dataset_id, document_id, action="retry",
        )
        logger.info("Retry queued for document %s (dataset=%s)", document_id, dataset_id)
        return _pipeline_execution_receipt(
            {"status": "queuing", "document_id": document_id, "action": "retry"},
            dataset_id=dataset_id,
            document_id=document_id,
            execution_id=execution_id,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.get(
    "/knowledge/{dataset_id}/documents/{document_id}/executions/{execution_id}"
)
async def get_document_pipeline_execution(
    dataset_id: str,
    document_id: str,
    execution_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Return one scope-checked durable execution receipt."""

    try:
        await svc.require_dataset_access(user, dataset_id, required="viewer")
        getter = getattr(svc.db, "get_pipeline_execution", None)
        if not callable(getter):
            raise HTTPException(
                status_code=503,
                detail="pipeline execution store unavailable",
            )
        execution = await getter(execution_id)
        if (
            not isinstance(execution, dict)
            or str(execution.get("dataset_id") or "") != dataset_id
            or str(execution.get("document_id") or "") != document_id
        ):
            raise HTTPException(status_code=404, detail="pipeline execution not found")
        return execution
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc)) from exc


@router.delete("/knowledge/{dataset_id}/documents/{document_id}")
async def delete_document(
    dataset_id: str,
    document_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        ok = await svc.delete_document(user, dataset_id, document_id)
        return {"status": "success" if ok else "not_found", "document_id": document_id}
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.get("/knowledge/{dataset_id}/segments")
async def list_segments(
    dataset_id: str,
    response: Response,
    document_id: str | None = Query(default=None),
    q: str | None = Query(default=None),
    # D4 (frontend handoff): pagination; total exposed via X-Total-Count.
    limit: int = Query(default=500, ge=1, le=500),
    offset: int = Query(default=0, ge=0),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        page = await svc.list_segments_page(
            user,
            dataset_id,
            document_id=document_id,
            q=q,
            limit=limit,
            offset=offset,
        )
        response.headers["X-Total-Count"] = str(page["total"])
        return page["items"]
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        if str(exc) == "dataset content generation changed during read; retry the request":
            raise HTTPException(
                status_code=409,
                detail=str(exc),
                headers={"Retry-After": "1"},
            ) from exc
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@router.put("/knowledge/{dataset_id}/segments/{segment_id}")
async def update_segment(
    dataset_id: str,
    segment_id: str,
    payload: SegmentUpdateSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await svc.update_segment(
            user,
            dataset_id,
            segment_id,
            new_text=payload.text,
            new_answer=payload.answer,
            new_keywords=payload.keywords,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.delete("/knowledge/{dataset_id}/segments/{segment_id}")
async def delete_segment(
    dataset_id: str,
    segment_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        ok = await svc.delete_segment(user, dataset_id, segment_id)
        return {"status": "success" if ok else "not_found", "segment_id": segment_id}
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


async def _run_hierarchical_retrieval(
    *,
    dataset_id: str,
    query: str,
    top_k: int,
    strategy: str,
    l1_top_k: int,
    l2_top_k: int,
    include_context: bool,
    score_threshold: float | None,
    svc: KnowledgeService,
    user: UserContext,
) -> tuple[list[Any], Any]:
    """Run hierarchical retrieval with the dataset's authorized embedding setup."""

    dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
    from ...services.knowledge.retrieval_service import (
        dataset_retrieval_generation,
        require_shadow_only_dataset,
    )

    require_shadow_only_dataset(dataset)
    if svc._is_multimodal_dataset(dataset):
        raise ValidationFailedError("multimodal dataset retrieval is not enabled for this release")
    retrieval_generation = dataset_retrieval_generation(dataset)
    collection_name = str(dataset.get("collection_name") or "").strip()
    dataset_tenant_id = str(dataset.get("tenant_id") or "").strip()
    if not collection_name:
        raise ValidationFailedError("dataset retrieval requires a persisted Qdrant collection")
    collection_guard = getattr(
        svc.vector_store,
        "require_hierarchical_collections_readable",
        None,
    )
    if not callable(collection_guard):
        raise ValidationFailedError("vector store collection-read authority is unavailable")
    try:
        await collection_guard(
            collection_name,
            tenant_id=dataset_tenant_id,
            dataset_id=dataset_id,
        )
    except Exception as exc:
        raise ValidationFailedError(f"dataset collection is not readable: {exc}") from exc
    embedding_config = dataset.get("embedding_config")
    if not isinstance(embedding_config, dict):
        embedding_config = {}

    from ...services.knowledge.embedding import get_cached_embedder

    embedding_provider = str(dataset.get("embedding_provider") or "local")
    embedding_model = str(dataset.get("embedding_model") or "hash-384")
    embedding_dimension = int(dataset.get("embedding_dimension") or 0) or None
    embedding_settings = await maybe_await(
        svc._resolve_embedding_config(
            provider=embedding_provider,
            model=embedding_model,
            embedding_config=embedding_config,
            tenant_id=dataset_tenant_id,
        )
    )
    embedder: Any = await get_cached_embedder(
        embedding_settings,
        dimension=embedding_dimension,
    )

    from ...services.knowledge.hierarchical_retriever import hierarchical_retrieve

    hierarchical_results, hierarchical_meta = await hierarchical_retrieve(
        query=query,
        dataset_id=dataset_id,
        vector_store=svc.vector_store,
        embedder=embedder,
        database=svc.db,
        top_k=top_k,
        strategy=strategy,
        base_collection=collection_name,
        tenant_id=dataset_tenant_id,
        l1_top_k=l1_top_k,
        l2_top_k=l2_top_k,
        include_context=include_context,
        score_threshold=score_threshold,
    )
    text_only_results: list[Any] = []
    for item in hierarchical_results:
        if _is_unreleased_multimodal_result(item):
            continue
        projected_item = copy.copy(item)
        projected_item.metadata = _strip_unreleased_image_metadata(getattr(item, "metadata", {}))
        text_only_results.append(projected_item)
    hierarchical_results = text_only_results
    document_ids = [
        str(getattr(item, "document_id", "") or "").strip()
        for item in hierarchical_results
        if str(getattr(item, "document_id", "") or "").strip()
    ]
    if hierarchical_results:
        filter_documents = getattr(svc.db, "filter_active_document_ids", None)
        if not callable(filter_documents):
            raise ValidationFailedError("active-document database authority is unavailable")
        try:
            active_document_ids = await filter_documents(
                dataset_id=dataset_id,
                tenant_id=dataset_tenant_id,
                document_ids=document_ids,
            )
        except Exception as exc:
            raise ValidationFailedError(
                f"active-document database authority failed: {exc}"
            ) from exc
        normalized_active_documents = {
            str(document_id or "").strip()
            for document_id in (active_document_ids or set())
            if str(document_id or "").strip()
        }
        if not normalized_active_documents.issubset(set(document_ids)):
            raise ValidationFailedError(
                "active-document database authority returned an unexpected document"
            )
        hierarchical_results = [
            item
            for item in hierarchical_results
            if str(getattr(item, "document_id", "") or "").strip() in normalized_active_documents
        ]
    segment_results = [
        item for item in hierarchical_results if int(getattr(item, "level", 3) or 3) != 1
    ]
    segment_ids = [
        str(getattr(item, "segment_id", "") or "").strip()
        for item in segment_results
        if str(getattr(item, "segment_id", "") or "").strip()
    ]
    if segment_results:
        filter_segments = getattr(svc.db, "filter_active_segment_ids", None)
        if not callable(filter_segments):
            raise ValidationFailedError("active-segment database authority is unavailable")
        try:
            active_segment_ids = await filter_segments(
                dataset_id=dataset_id,
                tenant_id=dataset_tenant_id,
                segment_ids=segment_ids,
            )
        except Exception as exc:
            raise ValidationFailedError(f"active-segment database authority failed: {exc}") from exc
        normalized_active_segments = {
            str(segment_id or "").strip()
            for segment_id in (active_segment_ids or set())
            if str(segment_id or "").strip()
        }
        if not normalized_active_segments.issubset(set(segment_ids)):
            raise ValidationFailedError(
                "active-segment database authority returned an unexpected segment"
            )
        hierarchical_results = [
            item
            for item in hierarchical_results
            if int(getattr(item, "level", 3) or 3) == 1
            or str(getattr(item, "segment_id", "") or "").strip() in normalized_active_segments
        ]
    authoritative = await svc.require_dataset_access(
        user,
        dataset_id,
        required="viewer",
    )
    if dataset_retrieval_generation(authoritative) != retrieval_generation:
        raise ValidationFailedError(
            "dataset index generation changed during retrieval; retry the request"
        )
    return hierarchical_results, hierarchical_meta


@router.post("/knowledge/{dataset_id}/retrieve")
async def retrieve(
    dataset_id: str,
    payload: RetrieveRequestSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        _require_authenticated_user(user)
        source_generation = None
        require_access = getattr(svc, "require_dataset_access", None)
        if callable(require_access):
            source_dataset = await require_access(user, dataset_id, required="viewer")
            source_generation = _dataset_content_generation(source_dataset)
        # Use hierarchical retrieval if enabled
        if payload.hierarchical:
            hierarchical_results, hierarchical_meta = await _run_hierarchical_retrieval(
                query=payload.query,
                dataset_id=dataset_id,
                top_k=payload.top_k,
                strategy=payload.hierarchical_strategy.value,
                l1_top_k=payload.l1_top_k,
                l2_top_k=payload.l2_top_k,
                include_context=payload.include_context,
                score_threshold=payload.score_threshold,
                svc=svc,
                user=user,
            )

            hierarchical_metadata = svc.record_external_retrieval_observation(
                user=user,
                dataset_id=dataset_id,
                query=payload.query,
                mode="hybrid",
                top_k=payload.top_k,
                results=hierarchical_results,
                meta={
                    "strategy": hierarchical_meta.strategy,
                    "l1_candidates": hierarchical_meta.l1_candidates,
                    "l2_candidates": hierarchical_meta.l2_candidates,
                    "l3_results": hierarchical_meta.l3_results,
                    "total_time_ms": hierarchical_meta.total_time_ms,
                    "filtered_documents": hierarchical_meta.filtered_documents,
                    "timings_ms": {"total_ms": hierarchical_meta.total_time_ms},
                },
                source="hierarchical",
            )
            identities = await _published_source_identity_map(
                svc, user, dataset_id, [r.document_id for r in hierarchical_results],
                source_generation,
            )
            return {
                "results": [
                    {
                        "segment_id": r.segment_id,
                        "document_id": r.document_id,
                        "score": r.score,
                        "text": r.text,
                        "level": r.level,
                        "metadata": _source_metadata(r.metadata, r.document_id, identities),
                        "source_version": (_source_identity(r.metadata, r.document_id, identities) or (None, None))[0],
                        "source_hash": (_source_identity(r.metadata, r.document_id, identities) or (None, None))[1],
                        "parent_context": r.parent_context,
                        "document_summary": r.document_summary,
                    }
                    for r in hierarchical_results
                ],
                "metadata": hierarchical_metadata,
                "trace_id": hierarchical_metadata["trace_id"],
                "query_fingerprint": hierarchical_metadata["query_fingerprint"],
            }

        # Use multimodal retrieval if include_associated_images is requested
        if payload.include_associated_images:
            retrieval_results, retrieval_meta = await svc.retrieve_with_images(
                user=user,
                dataset_id=dataset_id,
                query=payload.query,
                top_k=payload.top_k,
                mode=payload.mode,
                document_id=payload.document_id,
                dense_weight=payload.dense_weight,
                bm25_weight=payload.bm25_weight,
                fusion_method=payload.fusion_method,
                alpha=payload.alpha,
                score_threshold=payload.score_threshold,
                vector_top_k=payload.vector_top_k,
                keyword_top_k=payload.keyword_top_k,
                candidate_top_k=payload.candidate_top_k,
                keyword_candidate_k=payload.keyword_candidate_k,
                fusion=payload.fusion,
                rrf_k=payload.rrf_k,
                rrf_weights=payload.rrf_weights,
                rerank=payload.rerank,
                rerank_model=payload.rerank_model,
                rerank_top_n=payload.rerank_top_n,
                mmr=payload.mmr,
                mmr_lambda=payload.mmr_lambda,
                mmr_threshold=payload.mmr_threshold,
                include_images=payload.include_images,
                content_type_filter=payload.content_type_filter,
                multimodal_rerank=payload.multimodal_rerank,
                source_type_filter=payload.source_type_filter,
                language_filter=payload.language_filter,
                metadata_filter=payload.metadata_filter,
                # Advanced multimodal parameters
                image_search_enabled=payload.image_search_enabled,
                vlm_rerank_weight=payload.vlm_rerank_weight,
                image_boost=payload.image_boost,
                image_score_threshold=payload.image_score_threshold,
                use_separate_thresholds=payload.use_separate_thresholds,
            )
        else:
            retrieval_results, retrieval_meta = await svc.retrieve(
                user=user,
                dataset_id=dataset_id,
                query=payload.query,
                top_k=payload.top_k,
                mode=payload.mode,
                document_id=payload.document_id,
                dense_weight=payload.dense_weight,
                bm25_weight=payload.bm25_weight,
                fusion_method=payload.fusion_method,
                alpha=payload.alpha,
                score_threshold=payload.score_threshold,
                vector_top_k=payload.vector_top_k,
                keyword_top_k=payload.keyword_top_k,
                candidate_top_k=payload.candidate_top_k,
                keyword_candidate_k=payload.keyword_candidate_k,
                fusion=payload.fusion,
                rrf_k=payload.rrf_k,
                rrf_weights=payload.rrf_weights,
                rerank=payload.rerank,
                rerank_model=payload.rerank_model,
                rerank_top_n=payload.rerank_top_n,
                mmr=payload.mmr,
                mmr_lambda=payload.mmr_lambda,
                mmr_threshold=payload.mmr_threshold,
                source_type_filter=payload.source_type_filter,
                language_filter=payload.language_filter,
                metadata_filter=payload.metadata_filter,
            )

        identities = await _published_source_identity_map(
            svc, user, dataset_id, [r.document_id for r in retrieval_results],
            source_generation,
        )
        # Build response with multimodal and source traceability fields.
        return {
            "results": [
                {
                    "segment_id": r.segment_id,
                    "document_id": r.document_id,
                    "score": r.score,
                    "text": r.text,
                    "metadata": _source_metadata(r.metadata, r.document_id, identities),
                    "source_version": (_source_identity(r.metadata, r.document_id, identities) or (None, None))[0],
                    "source_hash": (_source_identity(r.metadata, r.document_id, identities) or (None, None))[1],
                    # P3: Multimodal fields
                    "content_type": getattr(r, "content_type", "text"),
                    "image_url": getattr(r, "image_url", None),
                    "vlm_description": getattr(r, "vlm_description", None),
                    "associated_images": getattr(r, "associated_images", []),
                    # Source traceability fields
                    "source_type": (r.metadata or {}).get("source_type"),
                    "citation_text": (r.metadata or {}).get("citation_text"),
                    "source_reference": (r.metadata or {}).get("source_reference", {}),
                }
                for r in retrieval_results
            ],
            "metadata": retrieval_meta,
            "trace_id": retrieval_meta.get("trace_id"),
            "query_fingerprint": retrieval_meta.get("query_fingerprint"),
        }
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except IndexLeaseUnavailableError as exc:
        raise _index_lease_conflict(exc) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.post("/knowledge/{dataset_id}/retrieve_batch")
async def retrieve_batch(
    dataset_id: str,
    payload: BatchRetrieveRequestSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Multi-query retrieval with one global ranking pipeline.

    Supports two formats:
    - queries: List of queries ["query1", "query2", "query3"]
    - query: Comma-separated queries "query1,query2,query3"

    The first query is the original rerank query. Rewrites only expand recall;
    the response contains one globally fused Top-K result group.
    """
    try:
        _require_authenticated_user(user)
        # Parse queries from either format
        queries: list[Any] = []
        if payload.queries:
            queries = [
                item.model_dump(exclude_none=True) if hasattr(item, "model_dump") else item
                for item in payload.queries
            ]
        elif payload.query:
            # Support comma-separated queries
            queries = [q.strip() for q in payload.query.split(",") if q.strip()]

        if not queries:
            raise ValidationFailedError(
                "No queries provided. Use 'queries' list or comma-separated 'query' string."
            )

        batch_results, meta = await svc.retrieve_batch(
            user=user,
            dataset_id=dataset_id,
            queries=queries,
            top_k=payload.top_k,
            mode=payload.mode,
            document_id=payload.document_id,
            dense_weight=payload.dense_weight,
            bm25_weight=payload.bm25_weight,
            fusion_method=payload.fusion_method,
            alpha=payload.alpha,
            score_threshold=payload.score_threshold,
            vector_top_k=payload.vector_top_k,
            keyword_top_k=payload.keyword_top_k,
            candidate_top_k=payload.candidate_top_k,
            keyword_candidate_k=payload.keyword_candidate_k,
            fusion=payload.fusion,
            rrf_k=payload.rrf_k,
            rrf_weights=payload.rrf_weights or {},
            rerank=payload.rerank,
            rerank_model=payload.rerank_model,
            rerank_top_n=payload.rerank_top_n,
            mmr=payload.mmr,
            mmr_lambda=payload.mmr_lambda,
            mmr_threshold=payload.mmr_threshold,
            source_type_filter=payload.source_type_filter,
            language_filter=payload.language_filter,
            include_images=payload.include_images,
            include_associated_images=payload.include_associated_images,
            max_parallel=payload.max_parallel,
            dedupe_results=payload.dedupe_results,
        )

        return {
            "batch_results": batch_results,
            "total_queries": meta.get("total_queries", len(queries)),
            "total_results": meta.get("total_results", 0),
            "execution_time_ms": meta.get("execution_time_ms", 0),
            "trace_id": meta.get("trace_id"),
            "query_fingerprint": meta.get("query_fingerprint"),
        }
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except IndexLeaseUnavailableError as exc:
        raise _index_lease_conflict(exc) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.post("/knowledge/{dataset_id}/hit_test")
async def hit_test(
    dataset_id: str,
    payload: RetrieveRequestSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Retrieve preview endpoint for debugging (includes raw scores in metadata)."""
    try:
        source_dataset = await _require_authenticated_dataset_editor(svc, user, dataset_id)
        source_generation = _dataset_content_generation(source_dataset)
        payload.mode = payload.mode or "hybrid"
        results, meta = await svc.retrieve(
            user=user,
            dataset_id=dataset_id,
            query=payload.query,
            top_k=payload.top_k,
            mode=payload.mode,
            document_id=payload.document_id,
            dense_weight=payload.dense_weight,
            bm25_weight=payload.bm25_weight,
            fusion_method=payload.fusion_method,
            alpha=payload.alpha,
            score_threshold=payload.score_threshold,
            vector_top_k=payload.vector_top_k,
            keyword_top_k=payload.keyword_top_k,
            candidate_top_k=payload.candidate_top_k,
            keyword_candidate_k=payload.keyword_candidate_k,
            fusion=payload.fusion,
            rrf_k=payload.rrf_k,
            rrf_weights=payload.rrf_weights or {},
            rerank=payload.rerank,
            rerank_model=payload.rerank_model,
            rerank_top_n=payload.rerank_top_n,
            mmr=payload.mmr,
            mmr_lambda=payload.mmr_lambda,
            mmr_threshold=payload.mmr_threshold,
            telemetry_source="hit_test",
        )
        identities = await _published_source_identity_map(
            svc, user, dataset_id, [r.document_id for r in results],
            source_generation,
        )
        return {
            "results": [
                {
                    "segment_id": r.segment_id,
                    "document_id": r.document_id,
                    "score": r.score,
                    "text": r.text,
                    "metadata": _source_metadata(r.metadata, r.document_id, identities),
                    "source_version": (
                        _source_identity(r.metadata, r.document_id, identities) or (None, None)
                    )[0],
                    "source_hash": (
                        _source_identity(r.metadata, r.document_id, identities) or (None, None)
                    )[1],
                }
                for r in results
            ],
            "metadata": meta,
            "trace_id": meta.get("trace_id"),
            "query_fingerprint": meta.get("query_fingerprint"),
        }
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except IndexLeaseUnavailableError as exc:
        raise _index_lease_conflict(exc) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))
    except Exception as exc:
        logger.warning("Knowledge hit-test failed; refusing fallback response", exc_info=True)
        raise HTTPException(status_code=500, detail="Knowledge hit-test failed") from exc


# ============================================================
# Document Enable/Disable/Archive Endpoints (Dify-style)
# ============================================================


@router.patch("/knowledge/{dataset_id}/documents/{document_id}/status")
async def update_document_status(
    dataset_id: str,
    document_id: str,
    payload: DocumentEnableDisableSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Enable or disable a document."""
    try:
        doc = await svc.set_document_enabled(user, dataset_id, document_id, payload.enabled)
        return doc
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.patch("/knowledge/{dataset_id}/documents/{document_id}/archive")
async def archive_document(
    dataset_id: str,
    document_id: str,
    payload: DocumentArchiveSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Archive or unarchive a document."""
    try:
        doc = await svc.set_document_archived(
            user, dataset_id, document_id, payload.archived, payload.reason
        )
        return doc
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


def _bounded_eval_metadata(
    value: Any,
    *,
    depth: int = 0,
    budget: list[int] | None = None,
) -> Any:
    """Return bounded, JSON-safe retrieval evidence with secrets redacted."""

    from ai_gateway_core.security import SENSITIVE_KEY_RE, redact_trace_text

    budget = budget if budget is not None else [512]
    if budget[0] <= 0:
        return "[truncated]"
    budget[0] -= 1
    if depth >= 5:
        return "[truncated]"
    if value is None or isinstance(value, (bool, int)):
        return value
    if isinstance(value, float):
        return value if math.isfinite(value) else None
    if isinstance(value, str):
        return redact_trace_text(value, limit=500)
    if isinstance(value, Mapping):
        bounded: dict[str, Any] = {}
        for index, (raw_key, nested) in enumerate(value.items()):
            if index >= 64 or budget[0] <= 0:
                bounded["__truncated_entries__"] = len(value) - index
                break
            budget[0] -= 1
            key = redact_trace_text(raw_key, limit=128)
            bounded[key] = (
                "[redacted]"
                if SENSITIVE_KEY_RE.search(key)
                else _bounded_eval_metadata(nested, depth=depth + 1, budget=budget)
            )
        return bounded
    if isinstance(value, Sequence) and not isinstance(value, (str, bytes, bytearray)):
        bounded_items = []
        for index in range(min(len(value), 64)):
            if budget[0] <= 0:
                break
            bounded_items.append(
                _bounded_eval_metadata(value[index], depth=depth + 1, budget=budget)
            )
        if len(value) > len(bounded_items):
            bounded_items.append(f"[truncated {len(value) - len(bounded_items)} entries]")
        return bounded_items
    return redact_trace_text(value, limit=500)


async def _retrieve_eval_case(
    *,
    dataset_id: str,
    query: str,
    fetch_k: int,
    payload: RetrievalEvalRequestSchema,
    svc: KnowledgeService,
    user: UserContext,
) -> tuple[list[Any], dict[str, Any]]:
    """Run the same public retrieval branch selected by ``/retrieve``."""

    if payload.hierarchical:
        hierarchical_results, hierarchy_meta = await _run_hierarchical_retrieval(
            query=query,
            dataset_id=dataset_id,
            top_k=fetch_k,
            strategy=payload.hierarchical_strategy.value,
            l1_top_k=payload.l1_top_k,
            l2_top_k=payload.l2_top_k,
            include_context=payload.include_context,
            score_threshold=payload.score_threshold,
            svc=svc,
            user=user,
        )
        metadata = {
            "pipeline": "hierarchical",
            "strategy": hierarchy_meta.strategy,
            "l1_candidates": hierarchy_meta.l1_candidates,
            "l2_candidates": hierarchy_meta.l2_candidates,
            "l3_results": hierarchy_meta.l3_results,
            "total_time_ms": hierarchy_meta.total_time_ms,
            "filtered_documents": hierarchy_meta.filtered_documents,
        }
        return list(hierarchical_results), metadata

    if payload.include_associated_images:
        multimodal_results, raw_metadata = await svc.retrieve_with_images(
            user=user,
            dataset_id=dataset_id,
            query=query,
            top_k=fetch_k,
            mode=payload.mode,
            document_id=payload.document_id,
            dense_weight=payload.dense_weight,
            bm25_weight=payload.bm25_weight,
            fusion_method=payload.fusion_method,
            alpha=payload.alpha,
            score_threshold=payload.score_threshold,
            vector_top_k=payload.vector_top_k,
            keyword_top_k=payload.keyword_top_k,
            candidate_top_k=payload.candidate_top_k,
            keyword_candidate_k=payload.keyword_candidate_k,
            fusion=payload.fusion,
            rrf_k=payload.rrf_k,
            rrf_weights=payload.rrf_weights,
            rerank=payload.rerank,
            rerank_model=payload.rerank_model,
            rerank_top_n=payload.rerank_top_n,
            mmr=payload.mmr,
            mmr_lambda=payload.mmr_lambda,
            mmr_threshold=payload.mmr_threshold,
            include_images=payload.include_images,
            content_type_filter=payload.content_type_filter,
            multimodal_rerank=payload.multimodal_rerank,
            source_type_filter=payload.source_type_filter,
            language_filter=payload.language_filter,
            metadata_filter=payload.metadata_filter,
            image_search_enabled=payload.image_search_enabled,
            vlm_rerank_weight=payload.vlm_rerank_weight,
            image_boost=payload.image_boost,
            image_score_threshold=payload.image_score_threshold,
            use_separate_thresholds=payload.use_separate_thresholds,
        )
        metadata = dict(raw_metadata or {})
        metadata["pipeline"] = "multimodal"
        return list(multimodal_results), metadata

    standard_results, raw_metadata = await svc.retrieve(
        user=user,
        dataset_id=dataset_id,
        query=query,
        top_k=fetch_k,
        mode=payload.mode,
        document_id=payload.document_id,
        dense_weight=payload.dense_weight,
        bm25_weight=payload.bm25_weight,
        fusion_method=payload.fusion_method,
        alpha=payload.alpha,
        score_threshold=payload.score_threshold,
        vector_top_k=payload.vector_top_k,
        keyword_top_k=payload.keyword_top_k,
        candidate_top_k=payload.candidate_top_k,
        keyword_candidate_k=payload.keyword_candidate_k,
        fusion=payload.fusion,
        rrf_k=payload.rrf_k,
        rrf_weights=payload.rrf_weights,
        rerank=payload.rerank,
        rerank_model=payload.rerank_model,
        rerank_top_n=payload.rerank_top_n,
        mmr=payload.mmr,
        mmr_lambda=payload.mmr_lambda,
        mmr_threshold=payload.mmr_threshold,
        source_type_filter=payload.source_type_filter,
        language_filter=payload.language_filter,
        metadata_filter=payload.metadata_filter,
    )
    metadata = dict(raw_metadata or {})
    metadata["pipeline"] = "standard"
    return list(standard_results), metadata


@router.post(
    "/knowledge/{dataset_id}/retrieve_evaluate",
    dependencies=[Depends(require_admin_user)],
)
async def retrieve_evaluate(
    dataset_id: str,
    payload: RetrievalEvalRequestSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Evaluate retrieval quality against a labelled test set.

    Runs the *same* retrieval pipeline as ``/retrieve`` for each labelled case
    and scores the ranked results with deterministic IR metrics
    (hit-rate / precision / recall / MRR / nDCG / MAP) at the requested K values.
    Because every retrieval knob (mode, fusion, weights, rerank, mmr,
    thresholds) is inherited from the request, calling this endpoint twice with
    two configurations yields directly A/B-comparable metric sets — this is the
    backend primitive for the KB retrieval evaluation workbench.
    """
    await require_admin_user(user)

    from ...services.eval.retrieval_metrics import (
        QueryRetrievalJudgement,
        evaluate_retrieval,
    )

    try:
        # Schema validation guarantees this, but retain a defensive route
        # boundary for direct/internal calls built with ``model_construct``.
        if not 1 <= len(payload.cases) <= RETRIEVAL_EVAL_MAX_CASES:
            raise ValidationFailedError(
                f"cases must contain between 1 and {RETRIEVAL_EVAL_MAX_CASES} items"
            )
        if (
            isinstance(payload.top_k, bool)
            or not isinstance(payload.top_k, int)
            or not 1 <= payload.top_k <= 100
        ):
            raise ValidationFailedError("top_k must be an integer in 1..100")
        if not payload.k_values or any(
            isinstance(k, bool) or not isinstance(k, int) or not 1 <= k <= 100
            for k in payload.k_values
        ):
            raise ValidationFailedError("k_values must contain at least one value in 1..100")
        k_values = sorted(set(payload.k_values))
        fetch_k = max(max(k_values), payload.top_k)
        await _require_authenticated_dataset_editor(svc, user, dataset_id)
        deadline = asyncio.get_running_loop().time() + RETRIEVAL_EVAL_TIMEOUT_SECONDS

        judgements: list[QueryRetrievalJudgement] = []
        per_case_results: list[dict[str, Any]] = []
        per_case_metadata: list[dict[str, Any]] = []
        metadata_budget = [4096]

        for idx, case in enumerate(payload.cases):
            remaining = deadline - asyncio.get_running_loop().time()
            if remaining <= 0:
                raise TimeoutError
            results, retrieval_metadata = await asyncio.wait_for(
                _retrieve_eval_case(
                    dataset_id=dataset_id,
                    query=case.query,
                    fetch_k=fetch_k,
                    payload=payload,
                    svc=svc,
                    user=user,
                ),
                timeout=remaining,
            )
            provider_retrieved_count = len(results)
            results = results[:fetch_k]
            retrieval_metadata = {
                **retrieval_metadata,
                "evaluation_window": {
                    "requested_fetch_k": fetch_k,
                    "provider_retrieved_count": provider_retrieved_count,
                    "evaluated_retrieved_count": len(results),
                    "truncated": provider_retrieved_count > len(results),
                },
            }

            ranked_ids = [str(r.segment_id) for r in results]
            unique_ranked_ids = list(dict.fromkeys(ranked_ids))
            duplicate_ids = list(
                dict.fromkeys(
                    segment_id
                    for position, segment_id in enumerate(ranked_ids)
                    if segment_id in ranked_ids[:position]
                )
            )

            # Merge binary + graded relevance (graded takes precedence).
            relevance: dict[str, float] = dict.fromkeys(case.relevant_segment_ids, 1.0)
            for sid, grade in case.relevance.items():
                relevance[sid] = float(grade)

            case_id = case.case_id or f"case_{idx}"
            judgements.append(
                QueryRetrievalJudgement(
                    query_id=case_id,
                    retrieved=ranked_ids,
                    relevance=relevance,
                )
            )
            bounded_metadata = _bounded_eval_metadata(
                retrieval_metadata,
                budget=metadata_budget,
            )
            per_case_metadata.append(
                {
                    "case_id": case_id,
                    "provider_retrieved_count": provider_retrieved_count,
                    "retrieved_count": len(ranked_ids),
                    "unique_retrieved_count": len(unique_ranked_ids),
                    "duplicate_segment_ids": duplicate_ids,
                    "retrieval_metadata": bounded_metadata,
                }
            )
            if payload.return_retrieved:
                per_case_results.append(
                    {
                        "case_id": case_id,
                        "query": case.query,
                        "retrieved": [
                            {
                                "segment_id": str(r.segment_id),
                                "document_id": str(r.document_id),
                                "score": r.score,
                                "relevant": relevance.get(str(r.segment_id), 0.0) > 0,
                                "relevance_grade": relevance.get(str(r.segment_id), 0.0),
                                "metadata": _bounded_eval_metadata(
                                    getattr(r, "metadata", None) or {},
                                    budget=metadata_budget,
                                ),
                                "content_type": getattr(r, "content_type", "text"),
                                "level": getattr(r, "level", None),
                            }
                            for r in results
                        ],
                    }
                )

        report = evaluate_retrieval(judgements, k_values=k_values)
        response: dict[str, Any] = {
            "dataset_id": dataset_id,
            "num_cases": len(judgements),
            "k_values": report.k_values,
            "metrics": {str(k): m.to_dict() for k, m in report.metrics_at_k.items()},
            "requested_config": _bounded_eval_metadata(
                payload.model_dump(
                    exclude={"cases", "query", "k_values", "return_retrieved"},
                    exclude_none=True,
                    mode="json",
                )
            ),
            "case_metadata": per_case_metadata,
        }
        primary = report.primary()
        if primary is not None:
            response["primary_metrics"] = primary.to_dict()
        if payload.return_retrieved:
            response["cases"] = per_case_results
            response["per_query"] = report.per_query
        return response
    except TimeoutError:
        raise HTTPException(status_code=504, detail="Retrieval evaluation timed out") from None
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.get("/knowledge/retrieval/presets")
async def list_retrieval_presets():
    """Return the built-in retrieval presets with recommended-config copy.

    Powers the one-click preset dropdown in the retrieval-test and evaluation
    workbench UIs. Each preset maps to a full ``RetrievalConfig`` so the frontend
    can hydrate every control (mode / fusion / weights / rerank / mmr /
    threshold) from a single selection.
    """
    from ...services.knowledge.retrieval_config import DEFAULT_CONFIGS

    descriptions = {
        "fast": {
            "label": "快速 (Fast)",
            "summary": "仅向量检索，无重排。延迟最低，适合高 QPS / 低延迟场景。",
            "recommended_for": "实时问答、聊天补全、对延迟敏感的在线服务",
        },
        "balanced": {
            "label": "均衡 (Balanced)",
            "summary": "混合检索 (RRF) + 重排。质量与延迟的推荐平衡点。",
            "recommended_for": "大多数企业知识库的默认选择",
        },
        "accurate": {
            "label": "精确 (Accurate)",
            "summary": "混合检索 + 重排 + 更高阈值。牺牲少量召回换取更高精度。",
            "recommended_for": "对准确性要求高、可容忍略低召回的场景",
        },
        "diverse": {
            "label": "多样 (Diverse)",
            "summary": "混合 + 重排 + MMR 去重多样化。减少冗余、覆盖更多角度。",
            "recommended_for": "多文档摘要、浏览式探索、需要多角度信息",
        },
        "sota": {
            "label": "最优 (SOTA)",
            "summary": "混合 + 重排(top_n=10) + 轻度 MMR(λ=0.7)。最高质量配置。",
            "recommended_for": "离线评测基线、对质量要求最高的关键场景",
        },
    }

    presets = []
    for name, cfg in DEFAULT_CONFIGS.items():
        meta = descriptions.get(name, {})
        presets.append(
            {
                "name": name,
                "label": meta.get("label", name),
                "summary": meta.get("summary", ""),
                "recommended_for": meta.get("recommended_for", ""),
                "config": cfg.to_dict(),
            }
        )

    return {
        "presets": presets,
        "recommended_default": "balanced",
        "notes": {
            "rrf_k": "RRF 常数默认 60（Elastic/Cormack'09，近优且不敏感）。注意不同向量库口径不同（如 Qdrant 默认 k=2 且零基 rank）。",
            "mmr": "MMR 默认关闭；仅建议用于多文档摘要/浏览类查询。lambda=0.5（LangChain 默认，0=最多多样、1=最相关）。",
            "rerank_top_n": "建议一阶段召回 50-100，重排后保留 final top_k（5-10）。",
            "score_threshold": "相似度阈值建议 0.2-0.35；仅在有重排时生效更稳妥。",
        },
    }


@router.get("/knowledge/{dataset_id}/metadata-schema")
async def get_document_metadata_schema(
    dataset_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await _document_metadata_manager(svc).get_registry(user, dataset_id)
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc)) from exc


@router.put("/knowledge/{dataset_id}/metadata-schema")
async def update_document_metadata_schema(
    dataset_id: str,
    payload: DocumentMetadataRegistryUpdateSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await _document_metadata_manager(svc).update_registry(
            user,
            dataset_id,
            expected_revision=payload.expected_revision,
            fields=[field.model_dump(exclude_none=True) for field in payload.fields],
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except MetadataRegistryRevisionConflict as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@router.patch("/knowledge/{dataset_id}/documents/metadata/batch")
async def batch_update_document_metadata(
    dataset_id: str,
    payload: DocumentMetadataBatchUpdateSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await _document_metadata_manager(svc).patch_documents(
            user,
            dataset_id,
            payload.document_ids,
            metadata_patch=payload.metadata_patch,
            metadata_remove=payload.metadata_remove,
            metadata_schema_revision=payload.metadata_schema_revision,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except MetadataRegistryRevisionConflict as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    except IndexLeaseUnavailableError as exc:
        raise _index_lease_conflict(exc) from exc


@router.patch("/knowledge/{dataset_id}/documents/{document_id}")
async def update_document(
    dataset_id: str,
    document_id: str,
    payload: DocumentUpdateSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Update document metadata."""
    try:
        if payload.metadata_patch is not None or payload.metadata_remove is not None:
            return await _document_metadata_manager(svc).patch_document(
                user,
                dataset_id,
                document_id,
                metadata_patch=payload.metadata_patch or {},
                metadata_remove=payload.metadata_remove or [],
                metadata_schema_revision=int(payload.metadata_schema_revision or 0),
            )
        doc = await svc.update_document(
            user, dataset_id, document_id, payload.model_dump(exclude_none=True)
        )
        return doc
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except MetadataRegistryRevisionConflict as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from exc
    except LookupError as exc:
        raise HTTPException(status_code=404, detail="document not found") from exc
    except IndexLeaseUnavailableError as exc:
        raise _index_lease_conflict(exc) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@router.post("/knowledge/preview")
async def preview_chunking_generic(
    payload: ChunkPreviewRequestSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """
    Generic preview endpoint (no dataset context required).
    """
    try:
        _require_authenticated_user(user)
        _require_safe_chunk_preview_config(payload.config)
        # Use a dummy dataset ID since we don't have one yet
        chunks = await svc.preview_chunking(
            user,
            "temp_preview",
            text=payload.text,
            config=payload.config.model_dump() if payload.config else None,
        )
        return {"chunks": chunks, "total_chunks": len(chunks)}
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.post("/knowledge/{dataset_id}/chunk/preview")
async def preview_chunking(
    dataset_id: str,
    payload: ChunkPreviewRequestSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """
    Preview chunking results for a given text and configuration.
    Does not save anything. useful for testing chunking strategies.
    """
    try:
        await _require_authenticated_dataset_editor(svc, user, dataset_id)
        _require_safe_chunk_preview_config(payload.config)
        chunks = await svc.preview_chunking(
            user,
            dataset_id,
            text=payload.text,
            config=payload.config.model_dump() if payload.config else None,
        )
        return {"chunks": chunks, "total_chunks": len(chunks)}
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


# ============================================================
# Batch Operations (Dify-style)
# ============================================================


@router.post("/knowledge/{dataset_id}/documents/batch")
async def batch_create_documents(
    dataset_id: str,
    payload: DocumentBatchCreateSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
):
    """Batch create documents from text."""
    try:
        results = await svc.batch_create_documents(
            user,
            dataset_id,
            documents=payload.documents,
            process_rule=payload.process_rule.model_dump() if payload.process_rule else None,
            batch_name=payload.batch_name,
        )
        queued_documents = []
        skipped_document_ids = []
        for doc in results.get("documents", []):
            if await _try_enqueue_document(worker, dataset_id, doc["document_id"]):
                queued_documents.append(doc)
            else:
                skipped_document_ids.append(doc["document_id"])
        results["documents"] = queued_documents
        results["queued_count"] = len(queued_documents)
        results["skipped_document_ids"] = skipped_document_ids
        results["status"] = "partial" if skipped_document_ids else "queuing"
        return results
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.post("/knowledge/{dataset_id}/documents/batch-reindex", status_code=202)
async def batch_reindex_documents(
    dataset_id: str,
    payload: BatchReindexSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Persist a fair, restart-safe reembed operation."""
    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        return await _document_batch_store(svc).create_operation(
            tenant_id=str(dataset.get("tenant_id") or ""),
            dataset_id=dataset_id,
            operation="reembed",
            created_by=user.user_id,
            actor_roles=user.roles or [],
            document_ids=payload.document_ids,
            all_documents=payload.all_documents,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    except (RuntimeError, ValueError) as exc:
        raise HTTPException(status_code=503, detail=str(exc)) from exc


@router.post("/knowledge/{dataset_id}/documents/batch-delete", status_code=202)
async def batch_delete_documents(
    dataset_id: str,
    payload: BatchDeleteSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Persist a fair, restart-safe batch delete operation."""
    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        return await _document_batch_store(svc).create_operation(
            tenant_id=str(dataset.get("tenant_id") or ""),
            dataset_id=dataset_id,
            operation="delete",
            created_by=user.user_id,
            actor_roles=user.roles or [],
            document_ids=payload.document_ids,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    except (RuntimeError, ValueError) as exc:
        raise HTTPException(status_code=503, detail=str(exc)) from exc


@router.get("/knowledge/{dataset_id}/document-batches/{operation_id}")
async def get_document_batch_operation(
    dataset_id: str,
    operation_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        result = await _document_batch_store(svc).get_operation(
            operation_id=operation_id,
            tenant_id=str(dataset.get("tenant_id") or ""),
            dataset_id=dataset_id,
        )
        if result is None:
            raise HTTPException(status_code=404, detail="document batch not found")
        return result
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc)) from exc
    except ValueError as exc:
        raise HTTPException(status_code=404, detail="document batch not found") from exc


# ============================================================
# Segment Enable/Disable Endpoints
# ============================================================


@router.patch("/knowledge/{dataset_id}/segments/{segment_id}/status")
async def update_segment_status(
    dataset_id: str,
    segment_id: str,
    payload: SegmentEnableDisableSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Enable or disable a segment."""
    try:
        seg = await svc.set_segment_enabled(user, dataset_id, segment_id, payload.enabled)
        return seg
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


# ---------------------------------------------------------------------------
# Compat routes: frontend sends POST /enable, KB Service has PATCH /status
# ---------------------------------------------------------------------------


@router.post("/knowledge/{dataset_id}/documents/{document_id}/enable")
async def enable_document_compat(
    dataset_id: str,
    document_id: str,
    payload: DocumentEnableDisableSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Compat: POST /enable → delegates to PATCH /status."""
    return await update_document_status(dataset_id, document_id, payload, svc, user)


@router.post("/knowledge/{dataset_id}/segments/{segment_id}/enable")
async def enable_segment_compat(
    dataset_id: str,
    segment_id: str,
    payload: SegmentEnableDisableSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Compat: POST /enable → delegates to PATCH /status."""
    return await update_segment_status(dataset_id, segment_id, payload, svc, user)


@router.post("/knowledge/{dataset_id}/segments/batch/enable")
async def batch_enable_segments(
    dataset_id: str,
    payload: SegmentBatchEnableDisableSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Batch enable/disable segments."""
    try:
        _require_authenticated_user(user)
        return await svc.set_segments_enabled_batch(
            user,
            dataset_id,
            payload.segment_ids,
            payload.enabled,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc


@router.post("/knowledge/{dataset_id}/documents/{document_id}/segments")
async def create_segment(
    dataset_id: str,
    document_id: str,
    payload: SegmentCreateSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Create a new segment manually."""
    try:
        seg = await svc.create_segment(
            user,
            dataset_id,
            document_id,
            content=payload.content,
            answer=payload.answer,
            keywords=payload.keywords,
        )
        return seg
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


# ============================================================
# Statistics Endpoints
# ============================================================


@router.get("/knowledge/{dataset_id}/statistics")
async def get_dataset_statistics(
    dataset_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Get dataset statistics (document count, segment count, etc.)."""
    try:
        stats = await svc.get_dataset_statistics(user, dataset_id)
        return stats
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.get("/knowledge/{dataset_id}/debug")
async def debug_dataset(
    dataset_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Debug endpoint to check dataset status."""
    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        stats = await svc.get_dataset_statistics(user, dataset_id)

        # Debug output is still a content read. Fetch a bounded candidate pool,
        # then apply the same PostgreSQL active-state authority as retrieval so
        # disabled, archived, pending-lifecycle, and foreign rows never leak.
        sample_candidates = await svc.db.list_segments(
            dataset_id=dataset_id,
            limit=100,
            offset=0,
        )
        sample_segments = (
            await _filter_active_route_segments(
                svc,
                dataset=dataset,
                dataset_id=dataset_id,
                segments=sample_candidates,
            )
        )[:3]

        result = {
            "dataset": {
                "id": dataset_id,
                "name": dataset.get("name"),
                "embedding_provider": dataset.get("embedding_provider"),
                "embedding_model": dataset.get("embedding_model"),
                "embedding_dimension": dataset.get("embedding_dimension"),
                "collection_name": dataset.get("collection_name"),
            },
            "statistics": stats,
            "sample_segments": [
                {
                    "segment_id": s.get("segment_id"),
                    "document_id": s.get("document_id"),
                    "text_preview": (s.get("text") or "")[:100] + "..." if s.get("text") else None,
                    "token_count": s.get("token_count"),
                    "vector_id": s.get("vector_id"),
                }
                for s in sample_segments
            ],
            "has_segments": len(sample_segments) > 0,
            "has_collection": bool(dataset.get("collection_name")),
        }
        await _require_unchanged_dataset_content(
            svc,
            user,
            dataset_id,
            generation,
        )
        return result
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


@router.get("/knowledge/{dataset_id}/documents/{document_id}/statistics")
async def get_document_statistics(
    dataset_id: str,
    document_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Get document statistics."""
    try:
        stats = await svc.get_document_statistics(user, dataset_id, document_id)
        return stats
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


# ============================================================
# Query observability and feedback
# ============================================================


@router.get(
    "/knowledge/{dataset_id}/queries",
    response_model=QueryHistoryListSchema,
)
async def list_query_history(
    dataset_id: str,
    limit: int = Query(default=50, ge=1, le=100),
    zero_results: bool | None = Query(default=None),
    mode: str | None = Query(default=None, max_length=32),
    cursor: str | None = Query(default=None, max_length=1024),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await svc.list_query_history(
            user,
            dataset_id,
            limit=limit,
            zero_results=zero_results,
            mode=mode,
            cursor=cursor,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    except Exception as exc:
        logger.exception("Knowledge query-history read failed")
        raise HTTPException(status_code=503, detail="Query history is unavailable") from exc


@router.put(
    "/knowledge/{dataset_id}/feedback",
    response_model=QueryFeedbackSchema,
)
async def upsert_query_feedback(
    dataset_id: str,
    payload: QueryFeedbackUpsertSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    trace_id = str(payload.trace_id)
    target_id = payload.segment_id if payload.target_type == "retrieval_hit" else trace_id
    try:
        return await svc.upsert_query_feedback(
            user,
            dataset_id,
            trace_id=trace_id,
            query_fingerprint=payload.query_fingerprint,
            target_type=payload.target_type,
            target_id=str(target_id),
            rating=payload.rating,
            reason_code=payload.reason_code,
            comment=payload.comment,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except QueryObservationConflictError as exc:
        raise HTTPException(status_code=409, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    except Exception as exc:
        logger.exception("Knowledge query-feedback write failed")
        raise HTTPException(status_code=503, detail="Query feedback is unavailable") from exc


@router.get(
    "/knowledge/{dataset_id}/feedback",
    response_model=QueryFeedbackListSchema,
)
async def list_query_feedback(
    dataset_id: str,
    limit: int = Query(default=50, ge=1, le=100),
    rating: str | None = Query(default=None, pattern="^(positive|negative)$"),
    reason_code: str | None = Query(default=None, max_length=32),
    target_type: str | None = Query(
        default=None, pattern="^(retrieval_hit|qa_answer)$"
    ),
    trace_id: uuid.UUID | None = Query(default=None),
    cursor: str | None = Query(default=None, max_length=1024),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    try:
        return await svc.list_query_feedback(
            user,
            dataset_id,
            limit=limit,
            rating=rating,
            reason_code=reason_code,
            target_type=target_type,
            trace_id=str(trace_id) if trace_id else None,
            cursor=cursor,
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc
    except Exception as exc:
        logger.exception("Knowledge query-feedback read failed")
        raise HTTPException(status_code=503, detail="Query feedback is unavailable") from exc


# ============================================================
# QA Testing Endpoints
# ============================================================


@router.post(
    "/knowledge/{dataset_id}/qa",
    dependencies=[Depends(require_admin_user)],
)
async def qa_query(
    request: Request,
    dataset_id: str,
    payload: QAQuerySchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
    settings: Settings = Depends(get_settings),
):
    """
    Execute a QA query: retrieve → context → LLM answer.

    This endpoint provides a complete RAG flow for testing retrieval quality.
    """
    await require_admin_user(user)

    try:
        from ...services.knowledge.qa_service import QAService

        await _require_authenticated_dataset_editor(svc, user, dataset_id)
        llm_config = _build_server_qa_llm_config(payload.llm_config, settings)

        # Create QA service
        qa_service = QAService(svc, llm_config)

        try:
            result = await qa_service.query(
                user_context=user,
                dataset_id=dataset_id,
                query=payload.query,
                top_k=payload.top_k,
                mode=payload.mode,
                document_id=payload.document_id,
                rerank=payload.rerank,
                rerank_top_n=payload.rerank_top_n,
                mmr=payload.mmr,
                mmr_lambda=payload.mmr_lambda,
                fusion_method=payload.fusion_method,
                dense_weight=payload.dense_weight,
                bm25_weight=payload.bm25_weight,
                score_threshold=payload.score_threshold,
                include_raw_results=payload.include_raw_results,
            )

            return {
                "query": result.query,
                "answer": result.answer,
                "context_segments": result.context_segments,
                "retrieval_metadata": result.retrieval_metadata,
                "timing": {
                    "retrieval_ms": result.retrieval_time_ms,
                    "llm_ms": result.llm_time_ms,
                    "total_ms": result.total_time_ms,
                },
                "model": result.model,
                "tokens_used": result.tokens_used,
                "trace_id": result.trace_id,
                "query_fingerprint": result.query_fingerprint,
            }
        finally:
            await qa_service.close()

    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except IndexLeaseUnavailableError as exc:
        raise _index_lease_conflict(exc) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))
    except Exception as exc:
        raise HTTPException(status_code=500, detail=f"QA query failed: {str(exc)}")


@router.post(
    "/knowledge/{dataset_id}/qa/stream",
    dependencies=[Depends(require_admin_user)],
)
async def qa_query_stream(
    request: Request,
    dataset_id: str,
    payload: QAQuerySchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
    settings: Settings = Depends(get_settings),
):
    """
    Stream QA query: retrieve → stream LLM answer.
    """
    await require_admin_user(user)

    from ...services.knowledge.qa_service import QAService
    from ...services.knowledge.retrieval_service import dataset_retrieval_generation

    try:
        dataset = await _require_authenticated_dataset_editor(svc, user, dataset_id)
        dataset_retrieval_generation(dataset)
        llm_config = _build_server_qa_llm_config(payload.llm_config, settings)
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc)) from exc
    except IndexLeaseUnavailableError as exc:
        raise _index_lease_conflict(exc) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc)) from exc

    qa_service = QAService(svc, llm_config)

    async def event_generator():
        try:
            async for event in qa_service.query_stream(
                user_context=user,
                dataset_id=dataset_id,
                query=payload.query,
                top_k=payload.top_k,
                mode=payload.mode,
                document_id=payload.document_id,
                rerank=payload.rerank,
                rerank_top_n=payload.rerank_top_n,
                mmr=payload.mmr,
                mmr_lambda=payload.mmr_lambda,
                fusion_method=payload.fusion_method,
                dense_weight=payload.dense_weight,
                bm25_weight=payload.bm25_weight,
                score_threshold=payload.score_threshold,
                include_raw_results=payload.include_raw_results,
            ):
                payload_json = json.dumps(event, ensure_ascii=False)
                yield f"data: {payload_json}\n\n"
        except PermissionDeniedError as exc:
            payload_json = json.dumps(
                {"event": "error", "data": {"message": str(exc)}}, ensure_ascii=False
            )
            yield f"data: {payload_json}\n\n"
        except IndexLeaseUnavailableError as exc:
            payload_json = json.dumps(
                {
                    "event": "error",
                    "data": {
                        "message": str(exc),
                        "status": 409,
                        "retry_after": 1,
                    },
                },
                ensure_ascii=False,
            )
            yield f"data: {payload_json}\n\n"
        except ValidationFailedError as exc:
            payload_json = json.dumps(
                {"event": "error", "data": {"message": str(exc)}}, ensure_ascii=False
            )
            yield f"data: {payload_json}\n\n"
        except Exception as exc:
            payload_json = json.dumps(
                {"event": "error", "data": {"message": str(exc)}}, ensure_ascii=False
            )
            yield f"data: {payload_json}\n\n"
        finally:
            await qa_service.close()

    return StreamingResponse(
        event_generator(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
            "X-Accel-Buffering": "no",
        },
    )


@router.post(
    "/knowledge/{dataset_id}/qa/batch",
    dependencies=[Depends(require_admin_user)],
)
async def qa_batch_test(
    request: Request,
    dataset_id: str,
    payload: QABatchTestSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
    settings: Settings = Depends(get_settings),
):
    """
    Run batch QA tests for evaluation.

    Executes multiple test cases and returns aggregated results.
    """
    await require_admin_user(user)

    try:
        from ...services.knowledge.qa_service import (
            QAService,
            QATestCase,
        )

        await _require_authenticated_dataset_editor(svc, user, dataset_id)
        llm_config = _build_server_qa_llm_config(payload.llm_config, settings)

        # Create QA service
        qa_service = QAService(svc, llm_config)

        try:
            # Convert test cases
            test_cases = [
                QATestCase(
                    query=tc.query,
                    expected_answer=tc.expected_answer,
                    expected_segments=tc.expected_segments,
                )
                for tc in payload.test_cases
            ]

            # Run batch test
            results = await asyncio.wait_for(
                qa_service.run_test_batch(
                    user_context=user,
                    dataset_id=dataset_id,
                    test_cases=test_cases,
                    top_k=payload.top_k,
                    mode=payload.mode,
                    rerank=payload.rerank,
                    mmr=payload.mmr,
                ),
                timeout=settings.ragas_eval.request_timeout_seconds,
            )

            # Aggregate results
            summary = qa_service.aggregate_test_results(results)

            return {
                "results": [r.to_dict() for r in results],
                "summary": summary,
            }
        finally:
            await qa_service.close()

    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except IndexLeaseUnavailableError as exc:
        raise _index_lease_conflict(exc) from exc
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))
    except Exception as exc:
        raise HTTPException(status_code=500, detail=f"Batch QA test failed: {str(exc)}")


# ============================================================
# Configuration Endpoints
# ============================================================


@router.get("/knowledge/{dataset_id}/config")
async def get_dataset_config(
    dataset_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Get dataset chunking and retrieval configuration."""
    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        dataset = svc.sanitize_dataset_for_response(dataset)

        # Extract configurations from index_config
        index_config = dataset.get("index_config", {}) or {}

        from ...services.knowledge.parsing.config_validation import parsing_config_report

        parser_report = parsing_config_report(index_config)
        # Also get statistics
        try:
            stats = await svc.get_dataset_statistics(user, dataset_id)
        except Exception:
            stats = {}

        return {
            "dataset_id": dataset_id,
            "chunking": index_config.get(
                "chunking",
                {
                    "mode": "automatic",
                    "chunk_size": 500,
                    "chunk_overlap": 50,
                },
            ),
            "retrieval": index_config.get(
                "retrieval",
                {
                    "mode": "hybrid",
                    "top_k": 5,
                    "rerank": {"enabled": False},
                    "mmr": {"enabled": False},
                },
            ),
            "parsing": parser_report,
            "configuration_warnings": parser_report["warnings"],
            "embedding": {
                "provider": dataset.get("embedding_provider"),
                "model": dataset.get("embedding_model"),
                "dimension": dataset.get("embedding_dimension"),
                "collection_name": dataset.get("collection_name"),
            },
            "statistics": stats,
        }
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc))


@router.put("/knowledge/{dataset_id}/config")
async def update_dataset_config(
    dataset_id: str,
    payload: DatasetConfigUpdateSchema = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """Update dataset chunking and retrieval configuration."""
    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="owner")
        _require_dataset_index_writable(dataset)

        # Get current config
        index_config = dict(dataset.get("index_config", {}) or {})

        # Update chunking config
        if payload.chunking_config:
            index_config["chunking"] = payload.chunking_config.model_dump(exclude_none=True)

        # Update retrieval config
        if payload.retrieval_config is not None:
            # This endpoint is a config patch even though it uses PUT. Pydantic
            # model defaults must not become writes for fields the caller did
            # not send, and nested lexical/fusion/rerank/MMR objects must retain
            # their unspecified settings.
            retrieval_patch = payload.retrieval_config.model_dump(
                exclude_none=True,
                exclude_unset=True,
            )
            existing_retrieval = index_config.get("retrieval", {}) or {}
            if not isinstance(existing_retrieval, Mapping):
                existing_retrieval = {}
            index_config["retrieval"] = _merge_config_patch(
                existing_retrieval,
                retrieval_patch,
            )

        # Build dataset updates
        dataset_updates: dict[str, Any] = {"index_config": index_config}

        # Handle embedding config updates if provided
        # Note: embedding changes require dimension check to avoid breaking existing vectors
        embedding_provider = getattr(payload, "embedding_provider", None)
        embedding_model = getattr(payload, "embedding_model", None)
        embedding_dimension = getattr(payload, "embedding_dimension", None)

        # Validate embedding dimension changes don't break existing vectors
        current_dimension = dataset.get("embedding_dimension")
        if embedding_dimension is not None and embedding_dimension != current_dimension:
            # Check if dataset has existing segments
            stats = await svc.get_dataset_statistics(user, dataset_id)
            segment_count = stats.get("segment_count", 0)
            if segment_count > 0:
                raise HTTPException(
                    status_code=400,
                    detail=(
                        f"Cannot change embedding dimension when {segment_count} "
                        "segments exist. Please create a new dataset or delete all "
                        "existing documents first."
                    ),
                )

        if embedding_provider is not None:
            dataset_updates["embedding_provider"] = embedding_provider
        if embedding_model is not None:
            dataset_updates["embedding_model"] = embedding_model
        if embedding_dimension is not None:
            dataset_updates["embedding_dimension"] = embedding_dimension

        # Save updated config
        updated = await svc.update_dataset(user, dataset_id, dataset_updates)

        return {
            "status": "success",
            "dataset_id": dataset_id,
            "index_config": updated.get("index_config"),
            "embedding": {
                "provider": updated.get("embedding_provider"),
                "model": updated.get("embedding_model"),
                "dimension": updated.get("embedding_dimension"),
            },
        }
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


# ============================================================
# Chunk Preview Endpoint
# ============================================================


class ChunkPreviewRequest(BaseModel):
    """Request for chunk preview."""

    text: str = Field(..., min_length=1, max_length=200_000, description="Text to chunk")
    chunking_config: dict[str, Any] | None = Field(
        default=None, description="Chunking configuration. Uses dataset defaults if not provided."
    )

    @model_validator(mode="after")
    def _reject_unsafe_regex_preview(self) -> ChunkPreviewRequest:
        try:
            _require_safe_chunk_preview_config(self.chunking_config)
        except ValidationFailedError as exc:
            raise ValueError(str(exc)) from exc
        return self


class ChunkPreviewItem(BaseModel):
    """Single chunk preview item."""

    index: int
    text: str
    char_count: int
    token_count: int
    word_count: int


class ChunkPreviewResponse(BaseModel):
    """Response for chunk preview."""

    total_chunks: int
    chunks: list[ChunkPreviewItem]
    config_used: dict[str, Any]


@router.post("/knowledge/{dataset_id}/preview-chunks")
async def preview_chunks(
    dataset_id: str,
    payload: ChunkPreviewRequest = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """
    Preview how text would be chunked with the given configuration.

    This is useful for testing chunking settings before processing documents.
    """
    try:
        dataset = await _require_authenticated_dataset_editor(svc, user, dataset_id)

        # Import chunking module from KS's own services package — gateway src/
        # is NOT bundled into the KS image, so the legacy `from src.services...`
        # path crashed every preview-chunks call with ModuleNotFoundError.
        from ...services.knowledge.chunking import (
            ChunkingConfig,
            flatten_chunks,
            merge_small_chunks,
            process_document,
        )

        # Get chunking config - use provided or fall back to dataset defaults
        if payload.chunking_config:
            config_dict = payload.chunking_config
        else:
            index_config = dataset.get("index_config", {}) or {}
            config_dict = index_config.get("chunking", {})
        _require_safe_chunk_preview_config(config_dict)

        # Parse config
        config = ChunkingConfig.from_dict(config_dict)

        # Process text
        chunks = process_document(payload.text, config)
        flat_chunks = flatten_chunks(chunks)
        flat_chunks = merge_small_chunks(
            flat_chunks,
            min_size=config.min_chunk_size,
            max_size=config.max_chunk_size,
            min_tokens=config.min_chunk_tokens if config.use_token_count else None,
            max_tokens=config.max_chunk_tokens if config.use_token_count else None,
        )

        # Format response
        preview_items = [
            ChunkPreviewItem(
                index=i,
                text=chunk.text,
                char_count=chunk.char_count,
                token_count=chunk.token_count,
                word_count=chunk.word_count,
            )
            for i, chunk in enumerate(flat_chunks)
        ]

        return ChunkPreviewResponse(
            total_chunks=len(preview_items),
            chunks=preview_items,
            config_used=config.to_dict(),
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))
    except Exception as exc:
        raise HTTPException(status_code=500, detail=f"Chunking error: {exc}")


@router.get("/knowledge/{dataset_id}/sources", summary="Get dataset source statistics")
async def get_dataset_sources(
    request: Request,
    dataset_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """
    Get dataset source statistics including file uploads, URL imports, and Confluence sync.

    Returns counts by source type and details of any Confluence bindings.
    """
    try:
        # Verify access
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)

        # Exact aggregate: a paginated document page must never become a
        # dataset-wide source count.
        source_counts = await svc.db.count_documents_by_source_type(dataset_id)

        # Get Confluence bindings if service is available
        confluence_bindings = []
        confluence_svc = getattr(request.app.state, "confluence_sync_service", None)
        connector_status = "not_configured" if confluence_svc is None else "available"
        if confluence_svc:
            try:
                bindings = await confluence_svc.list_bindings(user, dataset_id=dataset_id)
                confluence_bindings = [
                    {
                        "binding_id": b.get("binding_id"),
                        "manual_sync_available": b.get("manual_sync_available") is True,
                        "current_sync_task": b.get("current_sync_task"),
                        "space_name": b.get("space_name"),
                        "page_count": b.get("synced_page_count") or 0,
                        "status": b.get("status"),
                        "last_success_at": b.get("last_sync_at"),
                        "has_problem": (
                            str(b.get("status") or "").lower() not in {"completed", "success"}
                            and bool(b.get("last_error"))
                        ),
                    }
                    for b in bindings
                ]
            except Exception:
                logger.warning("Confluence bindings unavailable for dataset %s", dataset_id)
                connector_status = "unavailable"

        result = {
            "file_uploads": {
                "count": source_counts.get("upload", 0),
            },
            "url_imports": {
                "count": source_counts.get("url", 0),
            },
            "confluence_bindings": confluence_bindings,
            "connector_status": connector_status,
            "source_binding_available": bool(
                connector_status == "available"
                and getattr(confluence_svc, "source_bindings", None)
                and await confluence_svc.source_bindings.available(user, dataset_id)
            ),
            "source_connection_controls_available": bool(
                connector_status == "available"
                and getattr(confluence_svc, "source_connections", None)
                and await confluence_svc.source_connections.controls_available(user, dataset_id)
            ),
            "source_connection_reconnect": (
                await confluence_svc.source_connections.capability(user, dataset_id, reconnect=True)
                if connector_status == "available" and getattr(confluence_svc, "source_connections", None)
                else {"available": False, "reason": "source_setup_unavailable"}
            ),
            "source_connection_setup": (
                await confluence_svc.source_connections.capability(user, dataset_id)
                if connector_status == "available" and getattr(confluence_svc, "source_connections", None)
                else {"available": False, "reason": "source_setup_unavailable"}
            ),
            "total_documents": sum(source_counts.values()),
        }
        await _require_unchanged_dataset_content(
            svc,
            user,
            dataset_id,
            generation,
        )
        return result
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


# =============================================================================
# Maintenance Endpoints (Internal Use Only)
# =============================================================================


@router.post("/knowledge/{dataset_id}/maintenance/dedupe")
async def dedupe_segments(
    dataset_id: str,
    request: Request,
    dry_run: bool = Query(
        default=True, description="If true, only report duplicates without deleting"
    ),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
    settings: Settings = Depends(get_settings),
):
    """
    Remove duplicate segments from a dataset.

    Duplicates are identified by content_hash. The oldest segment is kept,
    newer duplicates are deleted from both database and vector store.

    Use dry_run=true to preview what would be deleted.

    Requires owner access to the dataset.
    """
    from collections import defaultdict

    try:
        # Always require owner access for maintenance operations
        # Security: No bypass allowed - maintenance operations should be properly authenticated
        dataset = await svc.require_dataset_access(user, dataset_id, required="owner")
        try:
            pending_delete = dataset_index_deletion_fence(dataset)
        except RuntimeError as exc:
            raise ValidationFailedError(str(exc)) from exc

        recovered_count = 0
        if pending_delete is not None:
            if dry_run or pending_delete["operation"] != "segment_delete":
                _require_dataset_index_readable(dataset)
            recovered = await svc.delete_segment(
                user,
                dataset_id,
                pending_delete["target_id"],
            )
            recovered_count = int(bool(recovered))
            dataset = await svc.require_dataset_access(
                user,
                dataset_id,
                required="owner",
            )
        generation = _dataset_content_generation(dataset)

        # Get all segments grouped by content_hash
        segments = await svc.db.list_segments(dataset_id, limit=10001)
        if len(segments) > 10000:
            raise HTTPException(status_code=422, detail={
                "code": "KB_DEDUPE_INCOMPLETE", "incomplete": True,
                "scan_limit": 10000, "observed_at_least": len(segments),
                "message": "Dataset exceeds the synchronous dedupe limit.",
            })

        # Group by content_hash
        hash_to_segments = defaultdict(list)
        for seg in segments:
            content_hash = seg.get("content_hash") or ""
            if content_hash:
                hash_to_segments[content_hash].append(seg)

        # Find duplicates (keep oldest, delete rest)
        duplicates_to_delete = []
        for _content_hash, segs in hash_to_segments.items():
            if len(segs) > 1:
                # Sort by created_at, keep oldest
                segs.sort(key=lambda x: x.get("created_at") or "")
                duplicates_to_delete.extend(segs[1:])

        result = {
            "dataset_id": dataset_id,
            "total_segments": len(segments),
            "incomplete": False,
            "unique_content": len(hash_to_segments),
            "duplicates_found": len(duplicates_to_delete),
            "dry_run": dry_run,
        }

        if dry_run:
            # Return preview of duplicates
            result["duplicates_preview"] = [
                {
                    "segment_id": seg.get("segment_id"),
                    "content_preview": (seg.get("content") or "")[:100],
                }
                for seg in duplicates_to_delete[:10]
            ]
            await _require_unchanged_dataset_content(
                svc,
                user,
                dataset_id,
                generation,
                required="owner",
            )
            return result

        _require_dataset_index_writable(dataset)

        # Actually delete duplicates
        deleted_count = recovered_count
        errors = []

        for seg in duplicates_to_delete:
            seg_id = str(seg.get("segment_id") or "").strip()
            try:
                if not seg_id:
                    raise ValidationFailedError("duplicate segment has no durable segment identity")
                # Keep maintenance deletion on the same durable lifecycle path
                # as the public segment API.  That path holds the exclusive
                # dataset fence, sweeps every owned Qdrant generation, commits
                # PostgreSQL on the same lease, and only then clears the marker.
                ok = await svc.delete_segment(user, dataset_id, seg_id)
                if ok:
                    deleted_count += 1
            except Exception as e:
                errors.append({"segment_id": seg_id, "error": str(e)})

        result["deleted_count"] = deleted_count
        if errors:
            result["errors"] = errors[:10]

        authoritative = await svc.require_dataset_access(
            user,
            dataset_id,
            required="owner",
        )
        _require_dataset_index_readable(authoritative)
        return result

    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))


# ============================================================
# Document Version Control APIs
# ============================================================


class VersionListResponse(BaseModel):
    """Version list response schema"""

    versions: list[dict[str, Any]]
    total: int
    current_version: int | None = None


class VersionCompareResponse(BaseModel):
    """Version compare response schema"""

    from_version: int
    to_version: int
    diff: list[dict[str, Any]]
    stats: dict[str, int]


class VersionRestoreRequest(BaseModel):
    """Version restore request schema"""

    reason: str | None = Field(None, description="Reason for restoring this version")


@router.get("/knowledge/{dataset_id}/documents/{document_id}/versions")
async def list_document_versions(
    dataset_id: str,
    document_id: str,
    limit: int = Query(20, ge=1, le=100),
    offset: int = Query(0, ge=0),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """
    List version history for a document.

    Returns a list of versions without full content (for performance).
    Use GET .../versions/{version_number} to get full content.
    """
    try:
        # Verify access
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)

        # Version metadata is content-derived and must follow the same active
        # document authority as retrieval and full-content version reads.
        doc = await _require_active_document(
            svc,
            dataset=dataset,
            dataset_id=dataset_id,
            document_id=document_id,
        )

        # Get versions
        versions = await svc.db.list_document_versions(document_id, limit, offset)
        total = await svc.db.get_document_version_count(document_id)

        result = VersionListResponse(
            versions=versions,
            total=total,
            current_version=doc.get("current_version", 1),
        )
        await _require_unchanged_dataset_content(
            svc,
            user,
            dataset_id,
            generation,
        )
        return result

    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc))


@router.get("/knowledge/{dataset_id}/documents/{document_id}/versions/{version_number:int}")
async def get_document_version(
    dataset_id: str,
    document_id: str,
    version_number: int,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """
    Get a specific version of a document with full content.
    """
    try:
        # Verify access
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)

        await _require_active_document(
            svc,
            dataset=dataset,
            dataset_id=dataset_id,
            document_id=document_id,
        )

        # Get specific version
        version = await svc.db.get_document_version(document_id, version_number)
        if not version:
            raise ValidationFailedError(f"Version {version_number} not found")

        await _require_unchanged_dataset_content(
            svc,
            user,
            dataset_id,
            generation,
        )
        return version

    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc))


@router.get("/knowledge/{dataset_id}/documents/{document_id}/versions/{version_number:int}/source")
async def get_historical_document_source(
    dataset_id: str,
    document_id: str,
    version_number: int,
    source_hash: str = Query(..., alias="hash", min_length=64, max_length=64),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
) -> dict[str, Any]:
    """Return only an exact saved source version under current document rights."""

    normalized_hash = source_hash.strip().lower()
    if version_number <= 0 or not re.fullmatch(r"[0-9a-f]{64}", normalized_hash):
        raise HTTPException(status_code=404, detail={"code": "SOURCE_VERSION_UNAVAILABLE"})
    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)
        await _require_active_document(
            svc, dataset=dataset, dataset_id=dataset_id, document_id=document_id,
        )
        version = await svc.db.get_document_version(document_id, version_number)
        content = (version or {}).get("content")
        if (
            not version
            or str(version.get("change_type") or "").startswith("pending_")
            or not isinstance(content, str)
            or str(version.get("content_hash") or "").lower() != normalized_hash
            or hashlib.sha256(content.encode("utf-8")).hexdigest() != normalized_hash
        ):
            raise ValidationFailedError("source version is unavailable")
        await _require_unchanged_dataset_content(svc, user, dataset_id, generation)
        await _require_active_document(
            svc, dataset=dataset, dataset_id=dataset_id, document_id=document_id,
        )
        return {
            "document_id": document_id,
            "version_number": version_number,
            "source_hash": normalized_hash,
            "title": str(version.get("title") or ""),
            "content": content,
        }
    except (PermissionDeniedError, ValidationFailedError):
        raise HTTPException(status_code=404, detail={"code": "SOURCE_VERSION_UNAVAILABLE"}) from None


@router.get("/knowledge/{dataset_id}/documents/{document_id}/versions/compare")
async def compare_document_versions(
    dataset_id: str,
    document_id: str,
    from_version: int = Query(..., description="Version to compare from"),
    to_version: int = Query(..., description="Version to compare to"),
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(get_user_context),
):
    """
    Compare two versions of a document.

    Returns a unified diff showing additions, deletions, and changes.
    """
    import difflib

    try:
        # Verify access
        dataset = await svc.require_dataset_access(user, dataset_id, required="viewer")
        generation = _dataset_content_generation(dataset)

        await _require_active_document(
            svc,
            dataset=dataset,
            dataset_id=dataset_id,
            document_id=document_id,
        )

        # Get both versions
        version_from = await svc.db.get_document_version(document_id, from_version)
        version_to = await svc.db.get_document_version(document_id, to_version)

        if not version_from:
            raise ValidationFailedError(f"Version {from_version} not found")
        if not version_to:
            raise ValidationFailedError(f"Version {to_version} not found")

        # Get content
        content_from = (version_from.get("content") or "").splitlines(keepends=True)
        content_to = (version_to.get("content") or "").splitlines(keepends=True)

        # Generate unified diff
        diff_lines = list(
            difflib.unified_diff(
                content_from,
                content_to,
                fromfile=f"Version {from_version}",
                tofile=f"Version {to_version}",
                lineterm="",
            )
        )

        # Parse diff into structured format
        diff_items = []
        additions = 0
        deletions = 0

        for line in diff_lines:
            if line.startswith("+++") or line.startswith("---") or line.startswith("@@"):
                continue
            elif line.startswith("+"):
                diff_items.append({"type": "insert", "content": line[1:]})
                additions += 1
            elif line.startswith("-"):
                diff_items.append({"type": "delete", "content": line[1:]})
                deletions += 1
            else:
                diff_items.append({"type": "equal", "content": line})

        result = VersionCompareResponse(
            from_version=from_version,
            to_version=to_version,
            diff=diff_items[:500],  # Limit to 500 lines
            stats={
                "additions": additions,
                "deletions": deletions,
                "changes": additions + deletions,
            },
        )
        await _require_unchanged_dataset_content(
            svc,
            user,
            dataset_id,
            generation,
        )
        return result

    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=404, detail=str(exc))


@router.post("/knowledge/{dataset_id}/documents/{document_id}/versions/{version_number}/restore")
async def restore_document_version(
    dataset_id: str,
    document_id: str,
    version_number: int,
    payload: VersionRestoreRequest = Body(default=VersionRestoreRequest()),
    svc: KnowledgeService = Depends(get_knowledge_service),
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(get_user_context),
):
    """
    Restore a document to a specific version.

    This creates a new version with the content from the specified version,
    then re-indexes the document.
    """
    import hashlib

    try:
        # Verify access (need editor permission to restore)
        dataset = await svc.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)

        lease_factory = getattr(svc.db, "document_index_update_lease", None)
        if not callable(lease_factory):
            raise ValidationFailedError("Document restore serialization is unavailable")

        # Keep the current source serving while a restored historical version
        # is indexed. The candidate version and its marker are durable in one
        # transaction; a crash before local enqueue remains recoverable from
        # the waiting row without exposing a mixed source/segment generation.
        async with lease_factory(dataset_id, document_id) as lease_connection:
            # A queued request that waited for another document writer must
            # still have editor access when it acquires the write lease.
            fresh_access = await svc.require_dataset_access(
                user, dataset_id, required="editor"
            )
            authoritative_dataset = await svc.db.get_dataset(
                dataset_id,
                connection=lease_connection,
            )
            if not authoritative_dataset or str(
                authoritative_dataset.get("tenant_id") or ""
            ) != str(fresh_access.get("tenant_id") or ""):
                raise ValidationFailedError("Dataset identity changed; retry the restore")
            _require_dataset_index_writable(authoritative_dataset)

            async with lease_connection.transaction():
                doc = await svc.db.get_document(
                    document_id,
                    connection=lease_connection,
                )
                metadata = doc.get("metadata") if doc else None
                if (
                    not doc
                    or str(doc.get("dataset_id") or "") != dataset_id
                    or not bool(doc.get("enabled", True))
                    or bool(doc.get("archived", False))
                    or str(doc.get("status") or "") != "completed"
                    or DOCUMENT_LIFECYCLE_REINDEX_KEY
                    in (metadata if isinstance(metadata, dict) else {})
                ):
                    raise ValidationFailedError("Document is not active and restore-ready")

                # Read version content only after exact dataset/tenant/lifecycle
                # ownership has been revalidated under the document lease.
                version_to_restore = await svc.db.get_document_version(
                    document_id,
                    version_number,
                )
                if not version_to_restore:
                    raise ValidationFailedError(f"Version {version_number} not found")

                target_metadata = version_to_restore.get("metadata")
                target_metadata = target_metadata if isinstance(target_metadata, dict) else {}
                target_source = target_metadata.get(SOURCE_MANIFEST_KEY)
                source_kind = (
                    str(target_source.get("source_kind") or "")
                    if isinstance(target_source, dict) else ""
                )
                special_target = target_source is not None or (
                    target_metadata.get("processing_mode") == "scanned"
                    or bool(target_metadata.get("l1_segments"))
                )
                if special_target:
                    restored_text = str(version_to_restore.get("content") or "")
                    digest = hashlib.sha256(restored_text.encode("utf-8")).hexdigest()
                    if (
                        not isinstance(target_source, dict)
                        or source_kind not in {"text", "hierarchy", "vision"}
                        or str(version_to_restore.get("content_hash") or "") != digest
                        or target_source.get("content_hash") != digest
                        or not re.fullmatch(r"[0-9a-f]{64}", str(target_source.get("source_hash") or ""))
                        or not isinstance(target_source.get("index_config"), dict)
                        or (source_kind in {"text", "hierarchy"} and target_source["source_hash"] != digest)
                        or (source_kind == "text" and (
                            target_source.get("objects") != {}
                            or target_source.get("page_texts") != []
                        ))
                        or (source_kind == "vision" and (
                            not str(target_source.get("original_source_key") or "").startswith(
                                f"knowledge/documents/{authoritative_dataset['tenant_id']}/{document_id}/original/"
                            )
                            or not isinstance(target_source.get("page_texts"), list)
                            or not target_source["page_texts"]
                        ))
                    ):
                        raise ValidationFailedError(SPECIALIZED_REBUILD_UNAVAILABLE)
                    if target_source["index_config"] != (
                        authoritative_dataset.get("index_config") or {}
                    ):
                        raise ValidationFailedError(SPECIALIZED_REBUILD_UNAVAILABLE)
                else:
                    await worker.require_safe_restore_admission(
                        KnowledgeIngestTask(dataset_id=dataset_id, document_id=document_id),
                        authoritative_dataset,
                        connection=lease_connection,
                    )

                current_content = str(doc.get("content") or "")
                current_hash = hashlib.sha256(current_content.encode("utf-8")).hexdigest()
                before_snapshot = await svc.db.create_document_version(
                    document_id=document_id,
                    content=current_content,
                    content_hash=current_hash,
                    change_type="pending_before_restore",
                    title=doc.get("title"),
                    metadata=metadata if isinstance(metadata, dict) else {},
                    change_reason=f"Before restore to version {version_number}",
                    changed_by=user.user_id,
                    confluence_version=doc.get("confluence_version"),
                    connection=lease_connection,
                    activate=False,
                )
                previous_version = (
                    int(before_snapshot.get("version_number") or 0)
                    if isinstance(before_snapshot, dict) else 0
                )
                if previous_version <= 0:
                    raise RuntimeError("pre-restore source snapshot could not be saved")

                restored_content = str(version_to_restore.get("content") or "")
                await svc.db.update_document_status(
                    document_id,
                    status="waiting",
                    progress=0,
                    error="",
                    connection=lease_connection,
                )
                restored_hash = hashlib.sha256(restored_content.encode("utf-8")).hexdigest()
                new_version = await svc.db.create_document_version(
                    document_id=document_id,
                    content=restored_content,
                    content_hash=restored_hash,
                    change_type="pending_restore",
                    title=version_to_restore.get("title") or doc.get("title"),
                    metadata=(
                        version_to_restore.get("metadata")
                        if isinstance(version_to_restore.get("metadata"), dict)
                        else {}
                    ),
                    change_reason=payload.reason or f"Restored from version {version_number}",
                    changed_by=user.user_id,
                    confluence_version=version_to_restore.get("confluence_version"),
                    connection=lease_connection,
                    activate=False,
                )
                candidate_version = (
                    int(new_version.get("version_number") or 0)
                    if isinstance(new_version, dict) else 0
                )
                if candidate_version <= 0:
                    raise RuntimeError("restore candidate version could not be saved")
                if candidate_version <= previous_version:
                    raise RuntimeError("restore candidate version order is invalid")
                index_config = copy.deepcopy(
                    target_source["index_config"] if special_target
                    else authoritative_dataset.get("index_config") or {}
                )
                if not isinstance(index_config, dict):
                    raise ValidationFailedError("Restore index configuration is invalid")
                chunking = index_config.get("chunking") or {}
                if not isinstance(chunking, dict):
                    raise ValidationFailedError("Restore chunking configuration is invalid")
                validate_persisted_chunking_config(chunking)
                replay_snapshot = {
                    "index_config": index_config,
                    "chunking": copy.deepcopy(chunking),
                    "processing_mode": "scanned" if source_kind == "vision" else "text_only",
                    "restore_source_version": version_number,
                }
                if special_target:
                    replay_snapshot["restore_source_kind"] = source_kind
                    replay_snapshot["restore_source_hash"] = target_source["source_hash"]
                rule_id = await svc.db.record_process_rule(
                    dataset_id,
                    mode=str(chunking.get("mode") or "automatic"),
                    rules=replay_snapshot,
                    created_by=user.user_id,
                    connection=lease_connection,
                )
                if not rule_id or not await svc.db.pin_document_process_rule(
                    document_id, rule_id, connection=lease_connection,
                ):
                    raise RuntimeError("restore replay rule could not be pinned")
                execution_id = await svc.db.record_pipeline_execution(
                    document_id,
                    dataset_id,
                    action="reprocess",
                    triggered_by=user.user_id,
                    process_rule_id=rule_id,
                    input_snapshot=replay_snapshot,
                    connection=lease_connection,
                )
                if not execution_id:
                    raise RuntimeError("restore replay execution could not be saved")
                bound = await svc.db.set_pending_restore_version(
                    document_id,
                    dataset_id,
                    previous_version,
                    candidate_version,
                    execution_id,
                    connection=lease_connection,
                )
                if not bound:
                    raise RuntimeError("restore candidate could not bind to queued document")

        enqueue_claimed = getattr(worker, "enqueue_claimed", None)
        if not callable(enqueue_claimed):
            raise HTTPException(
                status_code=503,
                detail="Durable claimed-queue publisher is unavailable",
            )
        await enqueue_claimed(dataset_id, document_id)

        return {
            "status": "pending",
            "document_id": document_id,
            "restored_from_version": version_number,
            "new_version": new_version.get("version_number") if new_version else None,
            "message": f"Restore of version {version_number} was queued; the previous version remains available until indexing succeeds.",
        }

    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        if str(exc) == SPECIALIZED_REBUILD_UNAVAILABLE:
            raise HTTPException(
                status_code=409,
                detail={"code": SPECIALIZED_REBUILD_UNAVAILABLE,
                        "message": "Specialized document restore requires administrator review"},
            ) from None
        raise HTTPException(status_code=404, detail=str(exc))


@router.get("/knowledge/worker/status")
async def get_worker_status(
    worker: KnowledgeWorker = Depends(get_knowledge_worker),
    user: UserContext = Depends(require_admin_user),
):
    """Diagnostic endpoint to check worker status."""
    _ = user
    return {
        "running": worker._running,
        "queue_size": worker.queue.qsize(),
        "worker_count": len(worker._workers),
        "workers_alive": [not t.done() for t in worker._workers],
    }


@router.post("/knowledge/{dataset_id}/documents/{document_id}/force-complete")
async def force_complete_document(
    dataset_id: str,
    document_id: str,
    svc: KnowledgeService = Depends(get_knowledge_service),
    user: UserContext = Depends(require_admin_user),
):
    """Force complete a stuck document (admin/debug only)."""
    try:
        dataset = await svc.require_dataset_access(user, dataset_id, required="editor")
        _require_dataset_index_writable(dataset)
        # Completing an unfinished generation without proving its Qdrant/DB
        # writes would expose partial content. Keep this debug endpoint as an
        # idempotent acknowledgement for already-active documents only; queued,
        # processing, failed, disabled, archived, or lifecycle-pending rows must
        # be repaired through the durable worker path.
        await _require_active_document(
            svc,
            dataset=dataset,
            dataset_id=dataset_id,
            document_id=document_id,
        )
        return {"status": "completed", "document_id": document_id}
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    except ValidationFailedError as exc:
        raise HTTPException(status_code=400, detail=str(exc))
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


# ---------------------------------------------------------------------------
# Internal gateway endpoints (HMAC v2 signature-bound identity)
# ---------------------------------------------------------------------------


class DatasetAuthorizeRequest(BaseModel):
    dataset_ids: list[str] = Field(
        default_factory=list,
        max_length=200,
        description="Candidate dataset ids from the agent's KB bindings",
    )
    is_tenant_admin: bool = Field(
        default=False,
        description=(
            "Advisory copy of the caller's tenant-admin state. KS enforces "
            "admin scope from the signature-bound identity headers only; "
            "this flag can never widen access."
        ),
    )


class DatasetAuthorizeResponse(BaseModel):
    allowed_dataset_ids: list[str]


class DocumentAuthorizeRequest(BaseModel):
    dataset_id: str
    document_ids: list[str] = Field(default_factory=list, max_length=500)


class DocumentAuthorizeResponse(BaseModel):
    allowed_document_ids: list[str]


class DocumentSourceReference(BaseModel):
    document_id: str
    source_version: int = Field(gt=0)
    source_hash: str = Field(pattern=r"^[0-9a-fA-F]{64}$")


class DocumentSourceAuthorizeRequest(BaseModel):
    dataset_id: str
    references: list[DocumentSourceReference] = Field(default_factory=list, max_length=500)


class DocumentSourceAuthorizeResponse(BaseModel):
    allowed_references: list[DocumentSourceReference]


@router.post(
    "/internal/knowledge/datasets/authorize",
    response_model=DatasetAuthorizeResponse,
    dependencies=[Depends(require_verified_gateway)],
)
async def authorize_gateway_datasets(
    request: Request,
    body: DatasetAuthorizeRequest = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
) -> DatasetAuthorizeResponse:
    """Batch dataset ACL for the gateway's agent runtime (PRD T8.2).

    The gateway resolves agent KB bindings through this endpoint instead of
    reading KB tables directly ("gateway 零 KB 表直读"). Identity arrives in
    the signature-bound trusted headers verified by the gateway-secret
    middleware; fail-closed by construction:

    * a verified signature without X-User-Id/X-Tenant-Id is a contract
      violation -> 401 (never an empty 200 the caller could mistake for a
      completed check);
    * ``is_tenant_admin`` from the body is advisory only — admin scope is
      granted exclusively by the signed X-User-Tier/X-User-Roles headers,
      matching the _effective_dataset_permission contract every other KB
      surface uses;
    * unknown, soft-deleted, or denied datasets are silently dropped, so the
      runtime only ever sees a (possibly empty) subset of what it asked for.
    """

    user_id = request.headers.get("X-User-Id", "").strip()
    tenant_id = request.headers.get("X-Tenant-Id", "").strip()
    user_type = request.headers.get("X-User-Type", "").strip()
    if not user_id or not tenant_id or not user_type:
        raise HTTPException(
            status_code=401,
            detail={
                "code": "AUTH_DENIED",
                "message": "Signature-bound identity headers are required",
            },
        )
    roles_raw = request.headers.get("X-User-Roles", "").strip()
    roles = [role.strip() for role in roles_raw.split(",") if role.strip()] or [
        "user"
    ]
    user = UserContext(
        user_id=user_id,
        tenant_id=tenant_id,
        user_tier=request.headers.get("X-User-Tier", "normal").strip(),
        user_type=user_type,
        roles=roles,
        is_authenticated=user_type.lower() not in {"anonymous", "guest"},
    )
    try:
        allowed = await svc.dataset_service.authorize_datasets(
            user, body.dataset_ids
        )
    except PermissionDeniedError as exc:
        raise HTTPException(status_code=403, detail=str(exc))
    return DatasetAuthorizeResponse(allowed_dataset_ids=allowed)


@router.post(
    "/internal/knowledge/documents/authorize",
    response_model=DocumentAuthorizeResponse,
    dependencies=[Depends(require_verified_gateway)],
)
async def authorize_gateway_documents(
    request: Request,
    body: DocumentAuthorizeRequest = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
) -> DocumentAuthorizeResponse:
    """Check live document access for assistant history and derived artifacts."""
    user_id = request.headers.get("X-User-Id", "").strip()
    tenant_id = request.headers.get("X-Tenant-Id", "").strip()
    if not user_id or not tenant_id:
        raise HTTPException(status_code=401, detail={"code": "AUTH_DENIED"})
    roles_raw = request.headers.get("X-User-Roles", "").strip()
    user = UserContext(
        user_id=user_id,
        tenant_id=tenant_id,
        user_tier=request.headers.get("X-User-Tier", "normal").strip(),
        user_type=request.headers.get("X-User-Type", "user").strip(),
        roles=[role.strip() for role in roles_raw.split(",") if role.strip()] or ["user"],
    )
    try:
        await svc.require_dataset_access(user, body.dataset_id, required="viewer")
    except (PermissionDeniedError, ValidationFailedError):
        return DocumentAuthorizeResponse(allowed_document_ids=[])
    requested = sorted({str(item).strip() for item in body.document_ids if str(item).strip()})
    if not requested:
        return DocumentAuthorizeResponse(allowed_document_ids=[])
    allowed = await svc.db.filter_active_document_ids(
        body.dataset_id, tenant_id, requested,
    )
    return DocumentAuthorizeResponse(allowed_document_ids=sorted(allowed))


@router.post(
    "/internal/knowledge/document-sources/authorize",
    response_model=DocumentSourceAuthorizeResponse,
    dependencies=[Depends(require_verified_gateway)],
)
async def authorize_gateway_document_sources(
    request: Request,
    body: DocumentSourceAuthorizeRequest = Body(...),
    svc: KnowledgeService = Depends(get_knowledge_service),
) -> DocumentSourceAuthorizeResponse:
    """Authorize exact immutable source versions under current viewer rights."""

    user_id = request.headers.get("X-User-Id", "").strip()
    tenant_id = request.headers.get("X-Tenant-Id", "").strip()
    if not user_id or not tenant_id:
        raise HTTPException(status_code=401, detail={"code": "AUTH_DENIED"})
    roles_raw = request.headers.get("X-User-Roles", "").strip()
    user = UserContext(
        user_id=user_id,
        tenant_id=tenant_id,
        user_tier=request.headers.get("X-User-Tier", "normal").strip(),
        user_type=request.headers.get("X-User-Type", "user").strip(),
        roles=[role.strip() for role in roles_raw.split(",") if role.strip()] or ["user"],
    )
    try:
        await svc.require_dataset_access(user, body.dataset_id, required="viewer")
    except (PermissionDeniedError, ValidationFailedError):
        return DocumentSourceAuthorizeResponse(allowed_references=[])
    requested = [
        (item.document_id, item.source_version, item.source_hash.lower())
        for item in body.references
    ]
    allowed = await svc.db.authorize_document_source_versions(
        body.dataset_id, tenant_id, requested,
    )
    return DocumentSourceAuthorizeResponse(allowed_references=[
        DocumentSourceReference(
            document_id=document_id,
            source_version=version,
            source_hash=source_hash,
        )
        for document_id, version, source_hash in sorted(allowed)
    ])
