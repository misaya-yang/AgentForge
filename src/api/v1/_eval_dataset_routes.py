"""Eval dataset and trace-feedback route family."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Annotated, Any

from ai_gateway_core.persistence.repositories.agent_trace_repository import (
    AgentTraceRepository,
    EvalCaseRevisionConflict,
)
from fastapi import APIRouter, Depends, HTTPException, Query, Request

from ...core.auth.user_resolver import UserContext
from ...services.assistant_entry.source_access import (
    visible_dataset_names,
    visible_source_version_keys,
)
from ...services.eval.golden import validate_case
from ...services.eval.trace_feedback import (
    build_harness_profile_proposal,
    build_redacted_dataset_case,
    classify_trace_failure,
    cluster_failure_patterns,
)
from ..deps import AuthContext, get_auth_context, get_user_context
from ..eval_export import EXPORT_REDACTION_POLICY
from ..schemas.eval import (
    EvalDataset,
    EvalDatasetCreate,
    EvalDatasetListResponse,
    EvalExample,
    EvalExampleFromTraceCreate,
    EvalExampleListResponse,
    EvalExamplesExportResponse,
    EvalExamplesImportRequest,
    EvalExamplesImportResponse,
    EvalExampleUpdate,
    EvalKbFailureSave,
    EvalKbFailureSaveResponse,
    EvalTraceFailurePattern,
    EvalTraceFeedbackRequest,
    EvalTraceFeedbackResponse,
)

_KB_REVISION_METADATA_KEYS = frozenset({
    "source_kind", "kb_source_versions", "kb_source_versions_verified",
    "case_revision", "supersedes_example_id",
})


def _reject_reserved_kb_metadata(metadata: dict[str, Any]) -> None:
    if _KB_REVISION_METADATA_KEYS & metadata.keys():
        raise HTTPException(
            status_code=422,
            detail="KB failure provenance must use the dedicated revision endpoint",
        )


async def _require_kb_example_read_access(
    request: Request,
    auth: AuthContext,
    user: UserContext,
    dataset: dict[str, Any],
    examples: list[dict[str, Any]],
) -> None:
    kb_dataset_ids: set[str] = set()
    linked_id = (dataset.get("metadata") or {}).get("kb_dataset_id")
    if isinstance(linked_id, str) and linked_id:
        kb_dataset_ids.add(linked_id)
    references: set[tuple[str, str, int, str]] = set()
    for example in examples:
        metadata = example.get("metadata") if isinstance(example.get("metadata"), dict) else {}
        if metadata.get("source_kind") != "kb_failure":
            continue
        kb_dataset_id = metadata.get("kb_dataset_id")
        if not isinstance(kb_dataset_id, str) or not kb_dataset_id:
            raise HTTPException(status_code=403, detail="KB case source is unavailable")
        kb_dataset_ids.add(kb_dataset_id)
        source_versions = metadata.get("kb_source_versions") or []
        if not isinstance(source_versions, list):
            raise HTTPException(status_code=403, detail="KB case source is unavailable")
        observed_ids = metadata.get("kb_observed_segment_ids") or []
        if (
            not isinstance(observed_ids, list)
            or any(not isinstance(item, str) for item in observed_ids)
            or sorted(observed_ids) != sorted(
                str(source.get("segment_id") or "")
                for source in source_versions if isinstance(source, dict)
            )
        ):
            raise HTTPException(status_code=403, detail="KB case source is incomplete")
        for source in source_versions:
            if not isinstance(source, dict):
                raise HTTPException(status_code=403, detail="KB case source is unavailable")
            document_id = source.get("document_id")
            version = source.get("source_version")
            source_hash = source.get("source_hash")
            if (
                source.get("kb_dataset_id") != kb_dataset_id
                or not isinstance(document_id, str) or not document_id
                or type(version) is not int or version <= 0
                or not isinstance(source_hash, str)
                or len(source_hash) != 64
                or any(char not in "0123456789abcdef" for char in source_hash.lower())
            ):
                raise HTTPException(status_code=403, detail="KB case source is unavailable")
            references.add((kb_dataset_id, document_id, version, source_hash.lower()))
    if not kb_dataset_ids:
        return
    if user.user_id != auth.user_id or user.tenant_id != auth.tenant_id:
        raise HTTPException(status_code=403, detail="Eval and Knowledge identities differ")
    visible = await visible_dataset_names(request, user)
    if not kb_dataset_ids <= visible.keys():
        raise HTTPException(status_code=403, detail="Knowledge dataset is not accessible")
    if references and await visible_source_version_keys(request, user, frozenset(references)) != references:
        raise HTTPException(status_code=403, detail="KB case source is unavailable")


@dataclass(frozen=True)
class EvalDatasetRouteDependencies:
    get_trace_repository: Callable[[Request], AgentTraceRepository]
    require_trace_access: Callable[[Request, AuthContext], None]
    require_run_access: Callable[[Request, AuthContext], None]
    require_supported_family: Callable[[str], None]
    scoped_user_id: Callable[[AuthContext, str | None], str | None]


@dataclass(frozen=True)
class EvalDatasetRouteFamily:
    router: APIRouter
    list_eval_datasets: Callable[..., Any]
    get_eval_dataset: Callable[..., Any]
    list_eval_examples: Callable[..., Any]
    update_eval_example: Callable[..., Any]
    delete_eval_example: Callable[..., Any]
    import_eval_examples: Callable[..., Any]
    export_eval_examples: Callable[..., Any]
    create_eval_dataset: Callable[..., Any]
    create_eval_example_from_trace: Callable[..., Any]
    get_latest_kb_failure_example: Callable[..., Any]
    save_kb_failure_example: Callable[..., Any]
    preview_eval_trace_feedback: Callable[..., Any]


def build_eval_dataset_routes(
    dependencies: EvalDatasetRouteDependencies,
) -> EvalDatasetRouteFamily:
    """Build dataset routes against facade-owned dependency seams."""
    router = APIRouter()
    _get_trace_repository = dependencies.get_trace_repository
    _require_eval_trace_access = dependencies.require_trace_access
    _require_eval_run_access = dependencies.require_run_access
    _require_supported_family = dependencies.require_supported_family
    _scoped_user_id = dependencies.scoped_user_id

    @router.get("/datasets", response_model=EvalDatasetListResponse)
    async def list_eval_datasets(
        request: Request,
        limit: Annotated[int, Query(ge=1, le=200)] = 50,
        offset: Annotated[int, Query(ge=0)] = 0,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalDatasetListResponse:
        _require_eval_trace_access(request, auth)
        manifest = await _get_trace_repository(request).list_dataset_manifest(
            tenant_id=auth.tenant_id,
        )
        has_linked = any((row.get("metadata") or {}).get("kb_dataset_id") for row in manifest)
        if has_linked and (user.user_id != auth.user_id or user.tenant_id != auth.tenant_id):
            raise HTTPException(status_code=403, detail="Eval and Knowledge identities differ")
        visible_kb_ids = (
            (await visible_dataset_names(request, user)).keys() if has_linked else set()
        )
        visible = [
            row for row in manifest
            if not (row.get("metadata") or {}).get("kb_dataset_id")
            or (row.get("metadata") or {}).get("kb_dataset_id") in visible_kb_ids
        ]
        rows = visible[offset:offset + limit]
        return EvalDatasetListResponse(
            datasets=[EvalDataset(**row) for row in rows],
            total=len(visible),
            limit=limit,
            offset=offset,
        )

    @router.get("/datasets/{dataset_id}", response_model=EvalDataset)
    async def get_eval_dataset(
        dataset_id: str,
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalDataset:
        _require_eval_trace_access(request, auth)
        dataset = await _get_trace_repository(request).get_dataset(
            tenant_id=auth.tenant_id,
            dataset_id=dataset_id,
        )
        if not dataset:
            raise HTTPException(status_code=404, detail="Dataset not found")
        await _require_kb_example_read_access(request, auth, user, dataset, [])
        return EvalDataset(**dataset)

    @router.get("/datasets/{dataset_id}/examples", response_model=EvalExampleListResponse)
    async def list_eval_examples(
        dataset_id: str,
        request: Request,
        split: Annotated[str | None, Query()] = None,
        limit: Annotated[int, Query(ge=1, le=500)] = 200,
        offset: Annotated[int, Query(ge=0)] = 0,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalExampleListResponse:
        _require_eval_trace_access(request, auth)
        repo = _get_trace_repository(request)
        dataset = await repo.get_dataset(tenant_id=auth.tenant_id, dataset_id=dataset_id)
        if not dataset:
            raise HTTPException(status_code=404, detail="Dataset not found")
        rows, total = await repo.list_examples(
            tenant_id=auth.tenant_id,
            dataset_id=dataset_id,
            split=split,
            limit=limit,
            offset=offset,
        )
        await _require_kb_example_read_access(request, auth, user, dataset, rows)
        return EvalExampleListResponse(
            examples=[EvalExample(**row) for row in rows],
            total=total,
            limit=limit,
            offset=offset,
        )

    @router.patch("/datasets/{dataset_id}/examples/{example_id}", response_model=EvalExample)
    async def update_eval_example(
        dataset_id: str,
        example_id: str,
        body: EvalExampleUpdate,
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalExample:
        _require_eval_run_access(request, auth)
        patch = body.model_dump(exclude_unset=True)
        _reject_reserved_kb_metadata(patch.get("metadata") or {})
        repo = _get_trace_repository(request)
        existing = await repo.get_example(
            tenant_id=auth.tenant_id, dataset_id=dataset_id, example_id=example_id,
        )
        if not existing:
            raise HTTPException(status_code=404, detail="Example not found")
        dataset = await repo.get_dataset(tenant_id=auth.tenant_id, dataset_id=dataset_id)
        if not dataset:
            raise HTTPException(status_code=404, detail="Dataset not found")
        await _require_kb_example_read_access(request, auth, user, dataset, [existing])
        if (existing.get("metadata") or {}).get("source_kind") == "kb_failure":
            if set(patch) - {"review_status", "split", "metadata"} or (
                set(patch.get("metadata") or {}) - {"reviewed_from"}
            ):
                raise HTTPException(
                    status_code=409,
                    detail="KB failure content is immutable; save a new revision",
                )
            status = patch.get("review_status")
            if status not in {"approved", "rejected", "needs_fix", "pending"}:
                raise HTTPException(status_code=422, detail="Review status is required")
            expected_split = "review"
            if patch.get("split") not in {None, expected_split}:
                raise HTTPException(status_code=422, detail="Review split does not match status")
            try:
                reviewed = await repo.review_kb_failure_example(
                    tenant_id=auth.tenant_id,
                    dataset_id=dataset_id,
                    example_id=example_id,
                    review_status=status,
                    reviewed_from=str((patch.get("metadata") or {}).get("reviewed_from") or "eval_console"),
                )
            except EvalCaseRevisionConflict as exc:
                raise HTTPException(
                    status_code=409,
                    detail={"error": "case_revision_conflict", "current_revision": exc.current_revision},
                ) from exc
            except ValueError as exc:
                raise HTTPException(status_code=409, detail=str(exc)) from exc
            if not reviewed:
                raise HTTPException(status_code=404, detail="Example not found")
            return EvalExample(**reviewed)
        current_metadata = existing.get("metadata") if isinstance(existing.get("metadata"), dict) else {}
        patch_metadata = dict(patch.get("metadata") or {})
        for key in (
            "expected_trajectory", "assertions", "tags", "difficulty", "owner", "review_status",
        ):
            if key in patch:
                patch_metadata[key] = patch[key]
        merged_metadata = {**current_metadata, **patch_metadata}
        validation_case = {
            "case_id": merged_metadata.get("case_id") or example_id,
            "split": patch.get("split") or existing.get("split") or "regression",
            "input": patch.get("input") if patch.get("input") is not None else existing.get("input") or {},
            "expected_output": patch.get("expected_output") if patch.get("expected_output") is not None else existing.get("expected_output") or {},
            "expected_trajectory": merged_metadata.get("expected_trajectory") or {},
            "assertions": merged_metadata.get("assertions") or [],
            "metadata": merged_metadata,
        }
        errors = validate_case(validation_case)
        if errors:
            raise HTTPException(status_code=422, detail={"case_id": example_id, "errors": errors})
        example = await repo.update_example(
            tenant_id=auth.tenant_id,
            dataset_id=dataset_id,
            example_id=example_id,
            payload=patch,
        )
        if not example:
            raise HTTPException(status_code=404, detail="Example not found")
        return EvalExample(**example)

    @router.delete(
        "/datasets/{dataset_id}/examples/{example_id}",
        status_code=204,
    )
    async def delete_eval_example(
        dataset_id: str,
        example_id: str,
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> None:
        """Delete one tenant-scoped example; a missing or repeated target is 404."""
        _require_eval_run_access(request, auth)
        repo = _get_trace_repository(request)
        dataset = await repo.get_dataset(tenant_id=auth.tenant_id, dataset_id=dataset_id)
        existing = await repo.get_example(
            tenant_id=auth.tenant_id, dataset_id=dataset_id, example_id=example_id,
        )
        if not dataset or not existing:
            raise HTTPException(status_code=404, detail="Example not found")
        await _require_kb_example_read_access(request, auth, user, dataset, [existing])
        if (existing.get("metadata") or {}).get("source_kind") == "kb_failure":
            raise HTTPException(
                status_code=409,
                detail="KB failure revisions are immutable and cannot be deleted",
            )
        deleted = await repo.delete_example(
            tenant_id=auth.tenant_id,
            dataset_id=dataset_id,
            example_id=example_id,
        )
        if not deleted:
            raise HTTPException(status_code=404, detail="Example not found")

    @router.post(
        "/datasets/{dataset_id}/examples:import",
        response_model=EvalExamplesImportResponse,
        status_code=201,
    )
    async def import_eval_examples(
        dataset_id: str,
        body: EvalExamplesImportRequest,
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalExamplesImportResponse:
        _require_eval_run_access(request, auth)
        repo = _get_trace_repository(request)
        dataset = await repo.get_dataset(tenant_id=auth.tenant_id, dataset_id=dataset_id)
        if not dataset:
            raise HTTPException(status_code=404, detail="Dataset not found")
        await _require_kb_example_read_access(request, auth, user, dataset, [])
        examples = [example.model_dump() for example in body.examples]
        for example in examples:
            _reject_reserved_kb_metadata(example.get("metadata") or {})
        validation_errors = [
            {"case_id": example.get("case_id"), "errors": errors}
            for example in examples
            if (errors := validate_case(example))
        ]
        if validation_errors:
            raise HTTPException(status_code=422, detail={"cases": validation_errors})
        result = await repo.import_examples(
            tenant_id=auth.tenant_id,
            dataset_id=dataset_id,
            created_by=auth.user_id,
            examples=examples,
            mode=body.mode,
        )
        return EvalExamplesImportResponse(
            imported=result.get("imported", 0),
            skipped=result.get("skipped", 0),
            examples=[EvalExample(**example) for example in result.get("examples", [])],
        )

    @router.get("/datasets/{dataset_id}/examples:export", response_model=EvalExamplesExportResponse)
    async def export_eval_examples(
        dataset_id: str,
        request: Request,
        split: Annotated[str | None, Query()] = None,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalExamplesExportResponse:
        _require_eval_trace_access(request, auth)
        repo = _get_trace_repository(request)
        dataset = await repo.get_dataset(tenant_id=auth.tenant_id, dataset_id=dataset_id)
        if not dataset:
            raise HTTPException(status_code=404, detail="Dataset not found")
        rows = await repo.list_example_manifest(
            tenant_id=auth.tenant_id, dataset_id=dataset_id,
        )
        if split:
            rows = [row for row in rows if row.get("split") == split]
        await _require_kb_example_read_access(request, auth, user, dataset, rows)
        export_items = []
        for row in rows:
            metadata = row.get("metadata") or {}
            export_items.append(
                {
                    "case_id": metadata.get("case_id") or row.get("example_id"),
                    "split": row.get("split") or "regression",
                    "input": row.get("input") or {},
                    "expected_output": row.get("expected_output") or {},
                    "expected_trajectory": metadata.get("expected_trajectory") or {},
                    "assertions": metadata.get("assertions") or [],
                    "metadata": {
                        key: value
                        for key, value in metadata.items()
                        if key not in {"case_id", "expected_trajectory", "assertions"}
                    },
                    "source_trace_id": row.get("source_trace_id"),
                    "source_span_id": row.get("source_span_id"),
                }
            )
        return EvalExamplesExportResponse(
            dataset=EvalDataset(**dataset),
            examples=export_items,
        )

    @router.post("/datasets", response_model=EvalDataset, status_code=201)
    async def create_eval_dataset(
        body: EvalDatasetCreate,
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalDataset:
        _require_eval_run_access(request, auth)
        linked_id = body.metadata.get("kb_dataset_id")
        if linked_id and (
            not isinstance(linked_id, str)
            or linked_id not in await visible_dataset_names(request, user)
        ):
            raise HTTPException(status_code=403, detail="Knowledge dataset is not accessible")
        dataset = await _get_trace_repository(request).create_dataset(
            tenant_id=auth.tenant_id,
            created_by=auth.user_id,
            payload=body.model_dump(by_alias=True),
        )
        return EvalDataset(**dataset)

    @router.post(
        "/datasets/{dataset_id}/examples:from-trace", response_model=EvalExample, status_code=201
    )
    async def create_eval_example_from_trace(
        dataset_id: str,
        body: EvalExampleFromTraceCreate,
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalExample:
        _require_supported_family(body.trace_family)
        _require_eval_run_access(request, auth)
        _reject_reserved_kb_metadata(body.metadata)
        repo = _get_trace_repository(request)
        dataset = await repo.get_dataset(tenant_id=auth.tenant_id, dataset_id=dataset_id)
        if not dataset:
            raise HTTPException(status_code=404, detail="Dataset not found")
        await _require_kb_example_read_access(request, auth, user, dataset, [])
        example = await repo.create_example_from_trace(
            tenant_id=auth.tenant_id,
            dataset_id=dataset_id,
            created_by=auth.user_id,
            user_id=_scoped_user_id(auth),
            trace_family=body.trace_family,
            payload=body.model_dump(),
        )
        if not example:
            raise HTTPException(status_code=404, detail="Trace not found")
        return EvalExample(**example)

    @router.get(
        "/datasets/{dataset_id}/kb-failures/{case_id}", response_model=EvalExample
    )
    async def get_latest_kb_failure_example(
        dataset_id: str,
        case_id: str,
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalExample:
        _require_eval_trace_access(request, auth)
        example = await _get_trace_repository(request).get_latest_kb_failure_example(
            tenant_id=auth.tenant_id, dataset_id=dataset_id, case_id=case_id,
        )
        if not example:
            raise HTTPException(status_code=404, detail="KB failure case not found")
        dataset = await _get_trace_repository(request).get_dataset(
            tenant_id=auth.tenant_id, dataset_id=dataset_id,
        )
        if not dataset:
            raise HTTPException(status_code=404, detail="Dataset not found")
        await _require_kb_example_read_access(request, auth, user, dataset, [example])
        return EvalExample(**example)

    @router.post(
        "/datasets/{dataset_id}/kb-failures:save",
        response_model=EvalKbFailureSaveResponse,
    )
    async def save_kb_failure_example(
        dataset_id: str,
        body: EvalKbFailureSave,
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
        user: UserContext = Depends(get_user_context),
    ) -> EvalKbFailureSaveResponse:
        _require_eval_run_access(request, auth)
        if user.user_id != auth.user_id or user.tenant_id != auth.tenant_id:
            raise HTTPException(status_code=403, detail="Eval and Knowledge identities differ")
        repo = _get_trace_repository(request)
        dataset = await repo.get_dataset(tenant_id=auth.tenant_id, dataset_id=dataset_id)
        if not dataset or (dataset.get("metadata") or {}).get("kb_dataset_id") != body.kb_dataset_id:
            raise HTTPException(status_code=404, detail="Linked Eval dataset not found")
        if body.kb_dataset_id not in await visible_dataset_names(request, user):
            raise HTTPException(status_code=403, detail="Knowledge dataset is not accessible")
        if any(source.kb_dataset_id != body.kb_dataset_id for source in body.source_versions):
            raise HTTPException(status_code=422, detail="Source dataset does not match KB case")
        if sorted(body.observed_segment_ids) != sorted(
            source.segment_id for source in body.source_versions
        ):
            raise HTTPException(
                status_code=422,
                detail="Every observed segment needs a stable source version",
            )
        references = frozenset(
            (
                source.kb_dataset_id,
                source.document_id,
                source.source_version,
                source.source_hash.lower(),
            )
            for source in body.source_versions
        )
        if references and await visible_source_version_keys(request, user, references) != references:
            raise HTTPException(
                status_code=422,
                detail="Source version is unavailable or not authorized",
            )
        if body.source_trace_id:
            if body.kb_trace_id and body.kb_trace_id != body.source_trace_id:
                raise HTTPException(status_code=422, detail="Source trace IDs do not match")
            detail = await repo.get_trace_detail(
                tenant_id=auth.tenant_id,
                trace_id=body.source_trace_id,
                user_id=_scoped_user_id(auth),
                trace_family="rag",
            )
            if not detail:
                raise HTTPException(status_code=422, detail="Source trace is not accessible")
        try:
            example, created = await repo.save_kb_failure_revision(
                tenant_id=auth.tenant_id,
                dataset_id=dataset_id,
                created_by=auth.user_id,
                payload={
                    **body.model_dump(),
                    "source_versions_verified": bool(references),
                },
            )
        except EvalCaseRevisionConflict as exc:
            raise HTTPException(
                status_code=409,
                detail={"error": "case_revision_conflict", "current_revision": exc.current_revision},
            ) from exc
        except ValueError as exc:
            raise HTTPException(status_code=404, detail=str(exc)) from exc
        return EvalKbFailureSaveResponse(
            example=EvalExample(**example),
            created=created,
            revision=example["metadata"]["case_revision"],
        )

    @router.post("/trace-feedback:preview", response_model=EvalTraceFeedbackResponse)
    async def preview_eval_trace_feedback(
        body: EvalTraceFeedbackRequest,
        request: Request,
        auth: AuthContext = Depends(get_auth_context),
    ) -> EvalTraceFeedbackResponse:
        _require_supported_family(body.trace_family)
        _require_eval_run_access(request, auth)
        repo = _get_trace_repository(request)
        if body.dataset_id:
            dataset = await repo.get_dataset(
                tenant_id=auth.tenant_id,
                dataset_id=body.dataset_id,
            )
            if not dataset:
                raise HTTPException(status_code=404, detail=f"Dataset not found: {body.dataset_id}")
        proposed_by = body.proposed_by or f"eval-feedback:{auth.user_id or 'system'}"
        patterns = []
        dataset_cases = []
        seen_trace_ids: set[str] = set()
        for trace_id in body.trace_ids:
            if trace_id in seen_trace_ids:
                continue
            seen_trace_ids.add(trace_id)
            detail = await repo.get_trace_detail(
                tenant_id=auth.tenant_id,
                trace_id=trace_id,
                user_id=_scoped_user_id(auth),
                trace_family=body.trace_family,
            )
            if not detail:
                raise HTTPException(status_code=404, detail=f"Trace not found: {trace_id}")
            pattern = classify_trace_failure(
                detail,
                low_score_threshold=body.low_score_threshold,
                latency_threshold_ms=body.latency_threshold_ms,
            )
            patterns.append(pattern)
            dataset_cases.append(
                build_redacted_dataset_case(
                    detail,
                    pattern,
                    split=body.split,
                )
            )

        clusters = cluster_failure_patterns(patterns)
        import_request = (
            EvalExamplesImportRequest(examples=dataset_cases) if body.dataset_id else None
        )
        return EvalTraceFeedbackResponse(
            trace_family=body.trace_family,
            dataset_id=body.dataset_id,
            patterns=[
                EvalTraceFailurePattern(
                    trace_id=pattern.trace_id,
                    trace_family=pattern.trace_family,
                    failure_mode=pattern.failure_mode,
                    reasons=pattern.reasons,
                    severity=pattern.severity,
                )
                for pattern in patterns
            ],
            clusters=clusters,
            dataset_cases=dataset_cases,
            import_request=import_request,
            proposals=[
                build_harness_profile_proposal(cluster, proposed_by=proposed_by)
                for cluster in clusters
            ],
            redaction_policy=EXPORT_REDACTION_POLICY,
        )

    return EvalDatasetRouteFamily(
        router=router,
        list_eval_datasets=list_eval_datasets,
        get_eval_dataset=get_eval_dataset,
        list_eval_examples=list_eval_examples,
        update_eval_example=update_eval_example,
        delete_eval_example=delete_eval_example,
        import_eval_examples=import_eval_examples,
        export_eval_examples=export_eval_examples,
        create_eval_dataset=create_eval_dataset,
        create_eval_example_from_trace=create_eval_example_from_trace,
        get_latest_kb_failure_example=get_latest_kb_failure_example,
        save_kb_failure_example=save_kb_failure_example,
        preview_eval_trace_feedback=preview_eval_trace_feedback,
    )
