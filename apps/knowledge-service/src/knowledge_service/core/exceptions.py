"""Shim: exception classes compatible with gateway's core.exceptions."""


class GatewayError(Exception):
    """Base exception."""
    def __init__(self, message: str = "", status_code: int = 500):
        self.message = message
        self.status_code = status_code
        super().__init__(message)


class NotFoundError(GatewayError):
    def __init__(self, message: str = "Resource not found"):
        super().__init__(message, status_code=404)


class PermissionDeniedError(GatewayError):
    def __init__(self, message: str = "Permission denied"):
        super().__init__(message, status_code=403)


class ValidationFailedError(GatewayError):
    def __init__(self, message: str = "Validation failed"):
        super().__init__(message, status_code=422)


class RateLimitError(GatewayError):
    def __init__(self, message: str = "Rate limit exceeded"):
        super().__init__(message, status_code=429)


class AuthenticationRequiredError(GatewayError):
    def __init__(self, message: str = "Authentication required"):
        super().__init__(message, status_code=401)


class ResourceNotFoundError(NotFoundError):
    pass
