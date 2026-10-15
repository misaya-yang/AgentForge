"""Tenant-scoped, instruction-only Skill artifact API."""

from __future__ import annotations

import logging
from dataclasses import replace
from typing import Any

from ai_gateway_core.skills import (
    DatabaseSkillArtifactRepository,
    SkillArtifactConflictError,
    SkillArtifactError,
    SkillArtifactNotFoundError,
    SkillArtifactUnavailableError,
    SkillBuilder,
    UserSkillPolicyError,
    manifest_from_artifact,
    parse_user_skill_md,
)
from fastapi import APIRouter, Depends, File, HTTPException, Query, Request, UploadFile
from pydantic import BaseModel, Field

from ...core.auth.permissions import Capability
from ...core.auth.user_resolver import UserContext
from ..deps import AuthContext, get_auth_context, get_user_context, require_gateway_capability

logger = logging.getLogger(__name__)
router = APIRouter(prefix="/skills", tags=["skills"])


class SkillUpdateRequest(BaseModel):
    title: str | None = None
    description: str | None = None
    tags: list[str] | None = None
    permissions: list[str] | None = None
    enabled: bool | None = None


class SkillTestRequest(BaseModel):
    input: str = Field(..., description="Test input for the skill")


def _repository(request: Request) -> Any:
    repository = getattr(request.app.state, "skill_artifact_repository", None)
    if repository is not None:
        return repository
    database = getattr(request.app.state, "database", None)
    if database is None:
        raise HTTPException(
            status_code=503,
            detail={"code": "SKILL_STORAGE_UNAVAILABLE", "message": "Skill storage unavailable"},
        )
    repository = DatabaseSkillArtifactRepository(database)
    request.app.state.skill_artifact_repository = repository
    return repository


def _reserved_names() -> frozenset[str]:
    from ai_gateway_core.skills.builtin.skill_create import SKILL_CREATE_MANIFEST

    return frozenset({SKILL_CREATE_MANIFEST.name})


def _platform_skill(name: str) -> Any | None:
    from ai_gateway_core.skills.builtin.skill_create import SKILL_CREATE_MANIFEST

    if name == SKILL_CREATE_MANIFEST.name:
        return SKILL_CREATE_MANIFEST
    return None


def _platform_record(*, include_content: bool) -> dict[str, Any]:
    from ai_gateway_core.skills.builtin.skill_create import SKILL_CREATE_MANIFEST

    record = {
        **SKILL_CREATE_MANIFEST.to_dict(),
        "status": "active",
        "revision": 0,
        "revoked": False,
    }
    return _catalog_item(record, include_content=include_content)


def _reject_platform_mutation(name: str) -> None:
    if _platform_skill(name) is not None:
        _error(404, "SKILL_NOT_FOUND", "Skill not found")


def _error(status_code: int, code: str, message: str, *, field: str | None = None) -> None:
    detail: dict[str, Any] = {"code": code, "message": message}
    if field:
        detail["field"] = field
    raise HTTPException(status_code=status_code, detail=detail)


def _map_artifact_error(exc: Exception) -> None:
    if isinstance(exc, SkillArtifactNotFoundError):
        _error(404, "SKILL_NOT_FOUND", "Skill not found")
    if isinstance(exc, SkillArtifactConflictError):
        _error(409, exc.code, "Skill artifact conflicts with current state")
    if isinstance(exc, SkillArtifactUnavailableError):
        status = 409 if exc.code == "SKILL_VERSION_UNAVAILABLE" else 503
        _error(status, exc.code, "Skill artifact is unavailable")
    if isinstance(exc, SkillArtifactError):
        _error(503, exc.code, "Skill storage unavailable")
    logger.exception("Skill persistence operation failed")
    _error(503, "SKILL_STORAGE_UNAVAILABLE", "Skill storage unavailable")


def _catalog_item(record: dict[str, Any], *, include_content: bool) -> dict[str, Any]:
    result = dict(record)
    if not include_content:
        result.pop("content", None)
        result.pop("instructions", None)
    return result


@router.post("/upload", status_code=201)
async def upload_skill(
    request: Request,
    file: UploadFile = File(...),
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    """Persist a full immutable Skill version or fail without registry mutation."""

    require_gateway_capability(request, auth, Capability.GATEWAY_SKILL_WRITE)
    if not file.filename or not file.filename.endswith(".md"):
        _error(400, "SKILL_FILE_TYPE_INVALID", "File must be a .md SKILL.md artifact")
    raw = await file.read()
    if len(raw) > 50_000:
        _error(400, "SKILL_FILE_TOO_LARGE", "SKILL.md exceeds the 50KB limit")
    try:
        content = raw.decode("utf-8")
    except UnicodeDecodeError:
        _error(400, "SKILL_ENCODING_INVALID", "SKILL.md must be valid UTF-8")
    try:
        normalized, manifest = parse_user_skill_md(
            content,
            reserved_names=_reserved_names(),
        )
    except UserSkillPolicyError as exc:
        _error(422, exc.code, exc.message, field=exc.field)
    except (TypeError, ValueError) as exc:
        _error(422, "SKILL_MANIFEST_INVALID", str(exc), field="file")

    validation_errors = SkillBuilder().validate_manifest(manifest)
    if validation_errors:
        _error(
            422,
            "SKILL_MANIFEST_INVALID",
            "; ".join(validation_errors),
            field="manifest",
        )
    try:
        record = await _repository(request).create_version(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            content=normalized,
            manifest=manifest,
            created_by=user.user_id,
        )
    except HTTPException:
        raise
    except Exception as exc:
        _map_artifact_error(exc)
    return _catalog_item(record, include_content=True)


@router.get("")
async def list_skills(
    request: Request,
    enabled_only: bool = Query(True),
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    require_gateway_capability(request, auth, Capability.GATEWAY_SKILL_READ)
    try:
        rows = await _repository(request).list_for_actor(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            enabled_only=enabled_only,
        )
    except Exception as exc:
        _map_artifact_error(exc)
    items = [
        _platform_record(include_content=False),
        *(_catalog_item(row, include_content=False) for row in rows),
    ]
    return {"skills": items, "total": len(items)}


@router.get("/{name}")
async def get_skill(
    name: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    require_gateway_capability(request, auth, Capability.GATEWAY_SKILL_READ)
    if _platform_skill(name) is not None:
        return _platform_record(include_content=True)
    try:
        row = await _repository(request).get_for_actor(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            name=name,
        )
    except Exception as exc:
        _map_artifact_error(exc)
    return _catalog_item(row, include_content=True)


@router.patch("/{name}")
async def update_skill(
    name: str,
    body: SkillUpdateRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    require_gateway_capability(request, auth, Capability.GATEWAY_SKILL_WRITE)
    _reject_platform_mutation(name)
    changes = body.model_dump(exclude_unset=True)
    enabled = changes.pop("enabled", None)
    try:
        if changes:
            if enabled is not None:
                changes["enabled"] = enabled
            null_field = next(
                (key for key, value in changes.items() if value is None),
                None,
            )
            if null_field is not None:
                _error(
                    422,
                    "SKILL_MANIFEST_INVALID",
                    "Skill metadata fields cannot be null",
                    field=null_field,
                )
            repository = _repository(request)
            current = await repository.get_for_actor(
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                name=name,
            )
            candidate = replace(manifest_from_artifact(current), **changes)
            validation_errors = SkillBuilder().validate_manifest(candidate)
            if validation_errors:
                _error(
                    422,
                    "SKILL_MANIFEST_INVALID",
                    "; ".join(validation_errors),
                    field="manifest",
                )
            if candidate.enabled and candidate.review_required():
                _error(
                    409,
                    "SKILL_ACTIVATION_GATES_REQUIRED",
                    "Skill activation gates are required",
                )
            row = await repository.update_metadata(
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                name=name,
                changes=changes,
                updated_by=user.user_id,
            )
        else:
            row = await _repository(request).get_for_actor(
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                name=name,
            )
        if enabled is not None and not changes:
            row = await _repository(request).set_enabled(
                tenant_id=user.tenant_id,
                user_id=user.user_id,
                name=name,
                enabled=enabled,
            )
    except HTTPException:
        raise
    except Exception as exc:
        _map_artifact_error(exc)
    return _catalog_item(row, include_content=True)


@router.delete("/{name}")
async def delete_skill(
    name: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    require_gateway_capability(request, auth, Capability.GATEWAY_SKILL_WRITE)
    _reject_platform_mutation(name)
    try:
        await _repository(request).delete(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            name=name,
        )
    except Exception as exc:
        _map_artifact_error(exc)
    return {"deleted": True, "name": name}


@router.post("/{name}/test")
async def test_skill(
    name: str,
    body: SkillTestRequest,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    """Return instruction content only; tenant artifacts never dispatch code."""

    require_gateway_capability(request, auth, Capability.GATEWAY_SKILL_WRITE)
    platform = _platform_skill(name)
    if platform is not None:
        from ai_gateway_core.skills.builtin.skill_create import handle_skill_create

        return await handle_skill_create({"input": body.input}, platform)
    try:
        row = await _repository(request).get_for_actor(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            name=name,
        )
        manifest = manifest_from_artifact(row)
    except Exception as exc:
        _map_artifact_error(exc)
    from ai_gateway_core.skills.executor import SkillExecutor

    return await SkillExecutor().execute(manifest, {"input": body.input})


async def _set_enabled(
    *,
    name: str,
    enabled: bool,
    request: Request,
    user: UserContext,
    auth: AuthContext,
) -> dict[str, Any]:
    require_gateway_capability(request, auth, Capability.GATEWAY_SKILL_WRITE)
    _reject_platform_mutation(name)
    try:
        row = await _repository(request).set_enabled(
            tenant_id=user.tenant_id,
            user_id=user.user_id,
            name=name,
            enabled=enabled,
        )
    except Exception as exc:
        _map_artifact_error(exc)
    return _catalog_item(row, include_content=False)


@router.post("/{name}/enable")
async def enable_skill(
    name: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    return await _set_enabled(
        name=name,
        enabled=True,
        request=request,
        user=user,
        auth=auth,
    )


@router.post("/{name}/disable")
async def disable_skill(
    name: str,
    request: Request,
    user: UserContext = Depends(get_user_context),
    auth: AuthContext = Depends(get_auth_context),
) -> dict[str, Any]:
    return await _set_enabled(
        name=name,
        enabled=False,
        request=request,
        user=user,
        auth=auth,
    )
