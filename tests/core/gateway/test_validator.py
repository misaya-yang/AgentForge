"""Tests for request validator."""

import pytest

from src.core.auth.rbac import RBAC
from ai_gateway_core.exceptions import (
    AuthenticationRequiredError,
    InvalidContentTypeError,
    PermissionDeniedError,
    ValidationFailedError,
)
from src.core.gateway.validator import RequestValidator
from src.models.request import ContentItem, ContentType, UnifiedRequest
from src.models.service import ServiceAuthConfig, ServiceConfig, ServiceDefinition


ROLE_PERMISSIONS = {
    "user": ["console:dashboard:view", "conversation:playground:access"],
    "admin": ["admin:*"],
}


@pytest.fixture
def rbac():
    return RBAC(ROLE_PERMISSIONS)


@pytest.fixture
def validator(rbac):
    return RequestValidator(rbac)


def _make_request(
    service_id: str = "test-svc",
    inputs: list | None = None,
    parameters: dict | None = None,
    user_id: str = "user_1",
) -> UnifiedRequest:
    if inputs is None:
        inputs = [ContentItem(type=ContentType.TEXT, data="hello")]
    return UnifiedRequest(
        request_id="req_test_001",
        service_id=service_id,
        inputs=inputs,
        parameters=parameters or {},
        user_id=user_id,
    )


def _make_service(
    service_id: str = "test-svc",
    accepted_content_types: list | None = None,
    input_schema: dict | None = None,
    auth_enabled: bool = False,
    auth_public: bool = False,
    auth_require_auth: bool = False,
    auth_allowed_roles: list | None = None,
) -> ServiceDefinition:
    auth_config = ServiceAuthConfig(
        enabled=auth_enabled,
        public=auth_public,
        require_auth=auth_require_auth,
        allowed_roles=auth_allowed_roles or [],
    )
    svc_config = ServiceConfig(auth=auth_config)
    return ServiceDefinition(
        service_id=service_id,
        name="Test Service",
        accepted_content_types=accepted_content_types or [],
        input_schema=input_schema or {},
        service_config=svc_config,
    )


class TestValidateRequiredFields:
    def test_missing_service_id(self, validator):
        req = _make_request(service_id="")
        with pytest.raises(ValidationFailedError, match="service_id"):
            validator._validate_required_fields(req)

    def test_missing_inputs(self, validator):
        req = _make_request()
        req.inputs = []
        with pytest.raises(ValidationFailedError, match="inputs"):
            validator._validate_required_fields(req)

    def test_valid_request_passes(self, validator):
        req = _make_request()
        validator._validate_required_fields(req)


class TestValidateContentItems:
    def test_item_with_data_passes(self, validator):
        inputs = [ContentItem(type=ContentType.TEXT, data="hello")]
        validator._validate_content_items(inputs)

    def test_item_with_url_passes(self, validator):
        inputs = [ContentItem(type=ContentType.TEXT, url="https://example.com")]
        validator._validate_content_items(inputs)

    def test_item_without_data_or_url_fails(self, validator):
        inputs = [ContentItem(type=ContentType.TEXT)]
        with pytest.raises(ValidationFailedError, match="data or url"):
            validator._validate_content_items(inputs)


class TestValidateSchema:
    def test_valid_schema_passes(self, validator):
        schema = {"type": "object", "properties": {"name": {"type": "string"}}}
        validator._validate_schema({"name": "test"}, schema)

    def test_invalid_data_fails(self, validator):
        schema = {"type": "object", "properties": {"count": {"type": "integer"}}, "required": ["count"]}
        with pytest.raises(ValidationFailedError):
            validator._validate_schema({}, schema)

    def test_empty_schema_skipped(self, validator):
        validator._validate_schema({"anything": True}, {})

    def test_string_schema_handled_gracefully(self, validator):
        validator._validate_schema({"anything": True}, '{"type": "object"}')
