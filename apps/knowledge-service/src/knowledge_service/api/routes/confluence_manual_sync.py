"""Explicit saved-binding manual synchronization behind signed Knowledge ingress."""

from __future__ import annotations

from typing import Literal
from uuid import UUID

from fastapi import APIRouter, Depends, HTTPException, Query, Request
from pydantic import BaseModel, ConfigDict, Field, SecretStr, ValidationError, field_validator

from ...auth.user_context import UserContext
from ...core.exceptions import PermissionDeniedError, ValidationFailedError
from ..deps import get_user_context

router = APIRouter()


class ManualSyncRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    request_id: UUID


def _service(request):
    service = getattr(request.app.state, "confluence_sync_service", None)
    if service is None or not callable(getattr(service, "start", None)):
        raise HTTPException(
            503,
            {
                "code": "CONFLUENCE_SYNC_NOT_ACCEPTED",
                "accepted": False,
                "message": "Manual synchronization is unavailable",
            },
        )
    return service


@router.post(
    "/knowledge/{dataset_id}/sources/confluence/{binding_id}/sync", status_code=202
)
async def start_manual_sync(
    request: Request,
    dataset_id: str,
    binding_id: str,
    payload: ManualSyncRequest,
    user: UserContext = Depends(get_user_context),
):
    service = _service(request)
    try:
        return await service.start(
            user, dataset_id, binding_id, str(payload.request_id)
        )
    except PermissionDeniedError as exc:
        raise HTTPException(
            403,
            {
                "code": "CONFLUENCE_SYNC_NOT_ACCEPTED",
                "accepted": False,
                "message": "Source owner and current dataset editor access required",
            },
        ) from exc
    except ValidationFailedError as exc:
        raise HTTPException(
            400,
            {
                "code": "CONFLUENCE_SYNC_NOT_ACCEPTED",
                "accepted": False,
                "message": "Source synchronization prerequisites are not satisfied",
            },
        ) from exc
    except HTTPException as exc:
        if exc.status_code in {400, 401, 403, 404, 409, 422}:
            raise HTTPException(
                exc.status_code,
                {
                    "code": "CONFLUENCE_SYNC_NOT_ACCEPTED",
                    "accepted": False,
                    "message": "Synchronization request was not accepted; refresh source status",
                },
            ) from exc
        raise


@router.get("/knowledge/{dataset_id}/sources/confluence/{binding_id}/sync/{request_id}")
async def get_manual_sync(
    request: Request,
    dataset_id: str,
    binding_id: str,
    request_id: UUID,
    user: UserContext = Depends(get_user_context),
):
    try:
        return await _service(request).status(
            user, dataset_id, binding_id, str(request_id)
        )
    except PermissionDeniedError as exc:
        raise HTTPException(403, "Current source and dataset access required") from exc
    except ValidationFailedError as exc:
        raise HTTPException(
            400, "Source status prerequisites are not satisfied"
        ) from exc


class CreateSourceBindingRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    request_id: UUID
    connection_id: str = Field(min_length=1, max_length=255)
    space_key: str = Field(min_length=1, max_length=255, pattern=r"^[A-Za-z0-9_~-]+$")
    space_id: str = Field(min_length=1, max_length=255)
    root_page_id: str = Field(min_length=1, max_length=255)


def _bindings(request):
    service = getattr(request.app.state, "confluence_sync_service", None)
    bindings = getattr(service, "source_bindings", None)
    if bindings is None or getattr(service, "_closing", False):
        raise HTTPException(503, "Saved source binding is unavailable")
    return bindings


@router.get("/knowledge/{dataset_id}/sources/confluence/connections")
async def list_source_connections(
    request: Request,
    dataset_id: str,
    offset: int = Query(default=0, ge=0, le=100000),
    user: UserContext = Depends(get_user_context),
):
    return await _bindings(request).connections(user, dataset_id, offset=offset)


@router.get(
    "/knowledge/{dataset_id}/sources/confluence/connections/{connection_id}/space"
)
async def get_source_space(
    request: Request,
    dataset_id: str,
    connection_id: str,
    space_key: str = Query(min_length=1, max_length=255, pattern=r"^[A-Za-z0-9_~-]+$"),
    user: UserContext = Depends(get_user_context),
):
    return await _bindings(request).space(user, dataset_id, connection_id, space_key)


@router.post("/knowledge/{dataset_id}/sources/confluence/bindings", status_code=201)
async def create_source_binding(
    request: Request,
    dataset_id: str,
    payload: CreateSourceBindingRequest,
    user: UserContext = Depends(get_user_context),
):
    try:
        return await _bindings(request).create(
            user,
            dataset_id,
            str(payload.request_id),
            payload.connection_id,
            payload.space_key,
            payload.space_id,
            payload.root_page_id,
        )
    except (HTTPException, PermissionDeniedError, ValidationFailedError) as exc:
        # These explicit checks happen before INSERT or within its rolled-back
        # transaction. Unknown database/transport errors remain ambiguous.
        status = exc.status_code if isinstance(exc, HTTPException) else 403
        raise HTTPException(
            status,
            {
                "code": "CONFLUENCE_BINDING_NOT_ACCEPTED",
                "accepted": False,
                "message": "Binding was not accepted; verify access and source selection",
            },
        ) from exc


@router.get("/knowledge/{dataset_id}/sources/confluence/bindings/{binding_id}")
async def get_source_binding(
    request: Request,
    dataset_id: str,
    binding_id: UUID,
    user: UserContext = Depends(get_user_context),
):
    return await _bindings(request).status(user, dataset_id, str(binding_id))


class CreateSourceConnectionRequest(BaseModel):
    model_config = ConfigDict(extra="forbid", hide_input_in_errors=True)
    request_id: UUID
    name: str = Field(min_length=1, max_length=255)
    domain: str = Field(
        min_length=15,
        max_length=255,
        pattern=r"^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.atlassian\.net$",
    )
    email: SecretStr
    api_token: SecretStr

    @field_validator("name")
    @classmethod
    def clean_name(cls, value):
        if not value.strip() or any(ord(c) < 32 for c in value):
            raise ValueError("Invalid connection name")
        return value

    @field_validator("email", "api_token")
    @classmethod
    def valid_secret(cls, value, info):
        text = value.get_secret_value()
        if not text or len(text) > 4096 or any(ord(c) < 32 for c in text):
            raise ValueError("Invalid credential field")
        if info.field_name == "email" and (":" in text or "@" not in text or len(text) > 320):
            raise ValueError("Invalid credential field")
        return value


def _connections(request):
    service = getattr(request.app.state, "confluence_sync_service", None)
    owner = getattr(service, "source_connections", None)
    if owner is None or getattr(service, "_closing", False):
        raise HTTPException(503, "Connection setup is unavailable")
    return owner


@router.post("/knowledge/{dataset_id}/sources/confluence/connections", status_code=201)
async def create_source_connection(
    request: Request, dataset_id: str, user: UserContext = Depends(get_user_context)
):
    # FastAPI's default validation response includes raw input. Parse inside a
    # sanitized boundary so even malformed credentials are never echoed.
    try:
        try:
            payload = CreateSourceConnectionRequest.model_validate(await request.json())
        except (ValidationError, ValueError, TypeError):
            raise HTTPException(
                422,
                "Invalid connection fields; use an Atlassian Cloud hostname, account email and API token",
            ) from None
        return await _connections(request).create(user, dataset_id, payload)
    except HTTPException as exc:
        raise HTTPException(
            exc.status_code,
            {
                "code": "CONFLUENCE_CONNECTION_NOT_ACCEPTED",
                "accepted": False,
                "message": exc.detail,
                "reason": "encryption_unavailable"
                if exc.status_code == 503 and "encrypt" in str(exc.detail).lower()
                else None,
            },
        ) from None


class ReplaceSourceCredentialsRequest(BaseModel):
    model_config = ConfigDict(extra="forbid", hide_input_in_errors=True)
    expected_revision: str = Field(min_length=1, max_length=80)
    expected_status: Literal["active", "disabled"]
    email: SecretStr
    api_token: SecretStr

    valid_secret = field_validator("email", "api_token")(CreateSourceConnectionRequest.valid_secret.__func__)


@router.post("/knowledge/{dataset_id}/sources/confluence/connections/{connection_id}/credentials")
async def replace_source_credentials(
    request: Request, dataset_id: str, connection_id: str,
    user: UserContext = Depends(get_user_context),
):
    # Never allow FastAPI validation to serialize submitted secrets or extra keys.
    try:
        payload = ReplaceSourceCredentialsRequest.model_validate(await request.json())
    except (ValidationError, ValueError, TypeError):
        raise HTTPException(422, "Invalid replacement credential fields") from None
    return await _connections(request).reconnect(user, dataset_id, connection_id, payload)


class SourceConnectionControlRequest(BaseModel):
    model_config = ConfigDict(extra="forbid")
    expected_revision: str = Field(min_length=1, max_length=80)
    expected_status: Literal["active", "disabled"]
    status: Literal["active", "disabled"]


@router.get("/knowledge/{dataset_id}/sources/confluence/connections/manage")
async def list_managed_connections(
    request: Request, dataset_id: str,
    offset: int = Query(default=0, ge=0, le=100000),
    user: UserContext = Depends(get_user_context),
):
    return await _connections(request).managed(user, dataset_id, offset=offset)


@router.get("/knowledge/{dataset_id}/sources/confluence/connections/{connection_id}/control")
async def get_connection_impact(
    request: Request, dataset_id: str, connection_id: str,
    offset: int = Query(default=0, ge=0, le=100000),
    user: UserContext = Depends(get_user_context),
):
    return await _connections(request).impact(user, dataset_id, connection_id, offset=offset)


@router.patch("/knowledge/{dataset_id}/sources/confluence/connections/{connection_id}/control")
async def control_source_connection(
    request: Request, dataset_id: str, connection_id: str,
    payload: SourceConnectionControlRequest,
    user: UserContext = Depends(get_user_context),
):
    return await _connections(request).control(user, dataset_id, connection_id, payload)


@router.get("/knowledge/{dataset_id}/sources/confluence/connections/{connection_id}")
async def get_source_connection(
    request: Request,
    dataset_id: str,
    connection_id: UUID,
    user: UserContext = Depends(get_user_context),
):
    return await _connections(request).status(user, dataset_id, str(connection_id))
