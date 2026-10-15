"""
API Key 管理接口
"""

from datetime import datetime
from typing import Any

from fastapi import APIRouter, Depends, HTTPException, Request
from pydantic import BaseModel, Field

from ...core.auth.service_access_resolver import clear_service_access_constraint_cache
from ...core.auth.user_resolver import UserContext
from ...persistence.database import DatabaseStorage
from ...services.auth.api_key_service import APIKeyService
from ..deps import get_user_context

router = APIRouter(prefix="/api-keys", tags=["API Keys"])

# This is a self-service endpoint. Its scopes are stored as API-key
# permissions and later participate in RBAC resolution, so keep the public
# issuance surface deliberately limited to non-management knowledge access.
# Privileged, console, and administrative API keys must be issued through an
# administrator-controlled path instead.
SELF_SERVICE_API_KEY_SCOPES = frozenset({"knowledge:read", "knowledge:write"})
DEFAULT_SELF_SERVICE_API_KEY_SCOPES = ("knowledge:read", "knowledge:write")


def _tenant_scope(current_user: dict[str, Any]) -> str:
    """Return the authenticated tenant while preserving legacy default rows."""
    return str(current_user.get("tenant_id") or "default")


def get_database(request: Request) -> DatabaseStorage:
    """获取数据库连接"""
    db = getattr(request.app.state, "database", None)
    if not db:
        raise HTTPException(status_code=503, detail="Database not available")
    return db


async def require_authenticated_user(
    user: UserContext = Depends(get_user_context),
) -> dict:
    """要求认证用户，返回用户信息字典"""
    if not user.is_authenticated:
        raise HTTPException(status_code=401, detail="Authentication required")
    return {
        "id": user.user_id,
        "user_id": user.user_id,
        "tenant_id": user.tenant_id,
        "roles": user.roles or [],
        "tier": user.tier,
    }


# ============ Request/Response Schemas ============


class CreateAPIKeyRequest(BaseModel):
    name: str = Field(..., min_length=1, max_length=100, description="API Key 名称")
    description: str | None = Field(None, max_length=500, description="描述")
    scopes: list[str] | None = Field(
        default=list(DEFAULT_SELF_SERVICE_API_KEY_SCOPES),
        description="自助 Key 仅允许 knowledge:read 和 knowledge:write",
    )
    tier: str | None = Field(
        default="normal",
        description="已弃用且会被忽略；Key 层级始终继承已认证调用方",
    )
    expires_at: datetime | None = Field(None, description="过期时间，不填则永不过期")


class APIKeyResponse(BaseModel):
    key_id: str
    key_prefix: str | None = None
    name: str
    description: str | None = None
    user_id: str | None = None
    derived_user_id: str | None = None
    scopes: list[str] | None = None
    roles: list[str] | None = None
    tier: str | None = None
    rate_limit: Any | None = None
    is_active: bool
    created_at: datetime
    last_used_at: datetime | None = None
    expires_at: datetime | None = None
    use_count: int = 0

    class Config:
        from_attributes = True


class CreateAPIKeyResponse(APIKeyResponse):
    api_key: str = Field(..., description="完整的 API Key，只显示一次")
    warning: str


# ============ Endpoints ============


@router.post("", response_model=CreateAPIKeyResponse)
async def create_api_key(
    req: CreateAPIKeyRequest,
    db: DatabaseStorage = Depends(get_database),
    current_user: dict = Depends(require_authenticated_user),
):
    """
    创建 API Key

    返回的 api_key 只会显示一次，请妥善保存。
    """
    scopes = (
        list(req.scopes) if req.scopes is not None else list(DEFAULT_SELF_SERVICE_API_KEY_SCOPES)
    )
    disallowed_scopes = sorted(set(scopes) - SELF_SERVICE_API_KEY_SCOPES)
    if disallowed_scopes:
        raise HTTPException(
            status_code=403,
            detail={
                "message": "Requested API key scopes are not available for self-service issuance",
                "disallowed_scopes": disallowed_scopes,
                "allowed_scopes": sorted(SELF_SERVICE_API_KEY_SCOPES),
            },
        )

    service = APIKeyService(db)

    result = await service.create_api_key(
        name=req.name,
        user_id=current_user["user_id"],
        tenant_id=current_user.get("tenant_id", ""),
        description=req.description or "",
        scopes=scopes,
        # The request body is untrusted: self-service keys inherit their
        # authenticated caller's tier rather than accepting a client-supplied
        # value such as "admin".
        tier=str(current_user.get("tier") or "normal"),
        expires_at=req.expires_at,
    )

    clear_service_access_constraint_cache()
    return result


@router.get("", response_model=list[APIKeyResponse])
async def list_api_keys(
    include_inactive: bool = False,
    db: DatabaseStorage = Depends(get_database),
    current_user: dict = Depends(require_authenticated_user),
):
    """列出当前用户的所有 API Keys"""
    service = APIKeyService(db)

    # 管理员可以看所有的
    user_id = None if "admin" in current_user.get("roles", []) else current_user["user_id"]

    keys = await service.list_api_keys(
        user_id=user_id,
        tenant_id=_tenant_scope(current_user),
        include_inactive=include_inactive,
    )

    return keys


@router.get("/{key_id}", response_model=APIKeyResponse)
async def get_api_key_detail(
    key_id: str,
    db: DatabaseStorage = Depends(get_database),
    current_user: dict = Depends(require_authenticated_user),
):
    """获取 API Key 详情"""
    service = APIKeyService(db)
    key = await service.get_api_key(
        key_id,
        tenant_id=_tenant_scope(current_user),
    )

    if not key:
        raise HTTPException(status_code=404, detail="API Key not found")

    # 非管理员只能看自己的
    if (
        "admin" not in current_user.get("roles", [])
        and str(key["user_id"]) != str(current_user["user_id"])
    ):
        raise HTTPException(status_code=403, detail="Access denied")

    return key


@router.post("/{key_id}/revoke")
async def revoke_api_key(
    key_id: str,
    db: DatabaseStorage = Depends(get_database),
    current_user: dict = Depends(require_authenticated_user),
):
    """
    吊销 API Key

    吊销后 Key 将立即失效，但记录保留。
    """
    service = APIKeyService(db)

    # 非管理员只能吊销自己的
    user_id = None if "admin" in current_user.get("roles", []) else current_user["user_id"]

    success = await service.revoke_api_key(
        key_id,
        user_id=user_id,
        tenant_id=_tenant_scope(current_user),
    )

    if not success:
        raise HTTPException(status_code=404, detail="API Key not found")

    clear_service_access_constraint_cache()
    return {"message": "API Key revoked", "key_id": key_id}


@router.delete("/{key_id}")
async def delete_api_key(
    key_id: str,
    db: DatabaseStorage = Depends(get_database),
    current_user: dict = Depends(require_authenticated_user),
):
    """删除 API Key"""
    service = APIKeyService(db)

    # 非管理员只能删除自己的
    user_id = None if "admin" in current_user.get("roles", []) else current_user["user_id"]

    success = await service.delete_api_key(
        key_id,
        user_id=user_id,
        tenant_id=_tenant_scope(current_user),
    )

    if not success:
        raise HTTPException(status_code=404, detail="API Key not found")

    clear_service_access_constraint_cache()
    return {"message": "API Key deleted", "key_id": key_id}
