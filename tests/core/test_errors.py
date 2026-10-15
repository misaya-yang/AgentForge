"""
错误处理测试

测试内容：
- 错误码定义
- 网关异常
- 具体异常类
"""

from src.core.errors.base import GatewayException
from src.core.errors.codes import ErrorCategory, ErrorCode
from src.core.errors.exceptions import (
    AuthenticationError,
    RateLimitError,
    ResourceNotFoundError,
    ValidationError,
)


class TestErrorCodes:
    """错误码测试"""

    def test_error_categories(self):
        """测试错误类别"""
        assert ErrorCode.INVALID_REQUEST.category == ErrorCategory.CLIENT
        assert ErrorCode.INTERNAL_ERROR.category == ErrorCategory.SERVER
        assert ErrorCode.SESSION_ERROR.category == ErrorCategory.BUSINESS
        assert ErrorCode.ADAPTER_ERROR.category == ErrorCategory.EXTERNAL

    def test_http_status_codes(self):
        """测试 HTTP 状态码"""
        assert ErrorCode.AUTHENTICATION_REQUIRED.http_status == 401
        assert ErrorCode.RATE_LIMIT_EXCEEDED.http_status == 429
        assert ErrorCode.SERVICE_NOT_FOUND.http_status == 404

    def test_retryable_errors(self):
        """测试可重试错误"""
        assert ErrorCode.TIMEOUT.retryable
        assert not ErrorCode.PERMISSION_DENIED.retryable


class TestGatewayException:
    """网关异常测试"""

    def test_exception_creation(self):
        """测试异常创建"""
        exc = GatewayException(
            code=ErrorCode.SERVICE_NOT_FOUND,
            message="Service xyz not found",
            details={"service_id": "xyz"},
        )

        assert exc.code == ErrorCode.SERVICE_NOT_FOUND
        assert exc.message == "Service xyz not found"
        assert exc.http_status == 404
        assert exc.details["service_id"] == "xyz"

    def test_exception_to_dict(self):
        """测试异常转换为字典"""
        exc = GatewayException(
            code=ErrorCode.SERVICE_NOT_FOUND,
            message="Service xyz not found",
            details={"service_id": "xyz"},
        )

        result = exc.to_dict()
        assert result["error"]["code"] == ErrorCode.SERVICE_NOT_FOUND.value
        assert result["error"]["message"] == "Service xyz not found"


class TestSpecificExceptions:
    """具体异常类测试"""

    def test_validation_error(self):
        """测试验证错误"""
        err = ValidationError(message="Invalid input", field="name")
        assert err.http_status == 400
        assert err.details["field"] == "name"

    def test_authentication_error(self):
        """测试认证错误"""
        err = AuthenticationError.token_expired()
        assert err.code == ErrorCode.TOKEN_EXPIRED

    def test_rate_limit_error(self):
        """测试限流错误"""
        err = RateLimitError(dimension="user", limit=100, retry_after=30)
        assert err.http_status == 429
        assert err.retryable

    def test_resource_not_found_error(self):
        """测试资源不存在错误"""
        err = ResourceNotFoundError(resource_type="service", resource_id="xyz")
        assert err.code == ErrorCode.SERVICE_NOT_FOUND
