from __future__ import annotations

from typing import Any

from jsonschema import ValidationError as JSONSchemaValidationError
from jsonschema import validate as jsonschema_validate

from ...models.request import ContentItem, UnifiedRequest
from ...models.service import ServiceDefinition
from ..auth.permissions import Capability, require_capability
from ..auth.rbac import RBAC
from ai_gateway_core.exceptions import (
    AuthenticationRequiredError,
    InvalidContentTypeError,
    PermissionDeniedError,
    ValidationFailedError,
)


class RequestValidator:
    def __init__(self, rbac: RBAC):
        self.rbac = rbac

    async def validate(
        self, request: UnifiedRequest, service: ServiceDefinition, roles: list[str]
    ) -> None:
        self._validate_required_fields(request)
        self._validate_content_types(request.inputs, service)
        self._validate_schema(request.parameters or {}, service.input_schema or {})
        self._validate_content_items(request.inputs)

        # 服务级别鉴权
        self._validate_service_auth(request, service, roles)

        # 全局 RBAC
        require_capability(
            rbac=self.rbac,
            roles=roles,
            permissions=None,
            capability=Capability.AGENT_INVOKE,
        )

    def _validate_service_auth(
        self, request: UnifiedRequest, service: ServiceDefinition, roles: list[str]
    ) -> None:
        """验证服务级别的鉴权配置"""
        config = service.get_service_config()
        auth_config = config.auth

        # 如果未启用服务级别鉴权，跳过
        if not auth_config.enabled:
            return

        # 如果服务是公开的，跳过鉴权
        if auth_config.public:
            return

        # 如果需要强制鉴权，检查用户是否已鉴权
        if auth_config.require_auth:
            is_anon = (request.user_id or "").startswith("anon:") or (
                roles and all(r == "guest" for r in roles)
            )
            if is_anon or not request.user_id:
                raise AuthenticationRequiredError(f"服务 {service.service_id} 需要鉴权")

        # 检查允许的角色
        if auth_config.allowed_roles:
            if not any(role in auth_config.allowed_roles for role in roles):
                raise PermissionDeniedError(
                    f"服务 {service.service_id} 需要以下角色之一: {auth_config.allowed_roles}"
                )

    def _validate_required_fields(self, request: UnifiedRequest) -> None:
        if not request.service_id:
            raise ValidationFailedError("service_id is required")
        if not request.inputs:
            raise ValidationFailedError("inputs is required")

    def _validate_content_types(
        self, inputs: list[ContentItem], service: ServiceDefinition
    ) -> None:
        accepted = set(service.accepted_content_types or [])
        if not accepted:
            return
        for item in inputs:
            if item.type not in accepted:
                raise InvalidContentTypeError(item.type.value, [a.value for a in accepted])

    def _validate_schema(self, parameters: dict[str, Any], schema: dict[str, Any]) -> None:
        if not schema:
            return
        # 防止 schema 为字符串的情况（数据库反序列化问题）
        if isinstance(schema, str):
            import json

            try:
                schema = json.loads(schema)
            except (json.JSONDecodeError, TypeError):
                return  # 无效的 schema 字符串，跳过验证
        if not isinstance(schema, dict):
            return  # 非字典类型的 schema，跳过验证
        try:
            jsonschema_validate(instance=parameters, schema=schema)
        except JSONSchemaValidationError as exc:
            raise ValidationFailedError(str(exc)) from exc

    def _validate_content_items(self, inputs: list[ContentItem]) -> None:
        for item in inputs:
            if item.data is None and not item.url:
                raise ValidationFailedError("content item must have data or url")
