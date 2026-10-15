from __future__ import annotations


class GatewayError(Exception):
    pass


class ServiceNotFoundError(GatewayError):
    def __init__(self, service_id: str):
        super().__init__(f"Service not found: {service_id}")
        self.service_id = service_id


class AdapterNotFoundError(GatewayError):
    def __init__(self, adapter_type: str):
        super().__init__(f"Adapter not found: {adapter_type}")
        self.adapter_type = adapter_type


class InvalidContentTypeError(GatewayError):
    def __init__(self, content_type: str, accepted):
        super().__init__(f"Invalid content type {content_type}, accepted: {accepted}")
        self.content_type = content_type
        self.accepted = accepted


class ValidationFailedError(GatewayError):
    pass


class AuthError(GatewayError):
    pass


class AuthenticationRequiredError(GatewayError):
    """需要鉴权但未提供有效凭证"""

    pass


class PermissionDeniedError(GatewayError):
    pass


class SessionAlreadyExistsError(GatewayError):
    """Raised when a caller attempts to create an existing session id."""

    pass


class RateLimitExceededError(GatewayError):
    def __init__(self, key: str):
        super().__init__(f"Rate limit exceeded for {key}")
        self.key = key


class CircuitBreakerOpenError(GatewayError):
    def __init__(self, service_id: str):
        super().__init__(f"Circuit breaker open for {service_id}")
        self.service_id = service_id


class NoHealthyInstanceError(GatewayError):
    def __init__(self, service_id: str):
        super().__init__(f"No healthy instance for {service_id}")
        self.service_id = service_id


class TaskNotFoundError(GatewayError):
    def __init__(self, task_id: str):
        super().__init__(f"Task not found: {task_id}")
        self.task_id = task_id


class TaskCancelledError(GatewayError):
    def __init__(self, task_id: str):
        super().__init__(f"Task cancelled: {task_id}")
        self.task_id = task_id


class NotFoundError(GatewayError):
    """资源不存在错误"""

    def __init__(self, message: str = "Resource not found"):
        super().__init__(message)
