/// Exception hierarchy for the AI Gateway SDK.
///
/// All exceptions carry structured metadata (statusCode, message, requestId)
/// so callers can log, retry, or surface errors without parsing strings.

/// Base exception for all SDK errors.
class GatewayAIException implements Exception {
  /// Human-readable error message.
  final String message;

  /// HTTP status code, if the error originated from an HTTP response.
  final int? statusCode;

  /// Server-assigned request ID for correlation, if available.
  final String? requestId;

  const GatewayAIException(
    this.message, {
    this.statusCode,
    this.requestId,
  });

  @override
  String toString() {
    final parts = <String>['GatewayAIException: $message'];
    if (statusCode != null) parts.add('statusCode=$statusCode');
    if (requestId != null) parts.add('requestId=$requestId');
    return parts.join(', ');
  }
}

/// Authentication or authorisation failure (HTTP 401 / 403).
class AuthException extends GatewayAIException {
  const AuthException(
    super.message, {
    super.statusCode,
    super.requestId,
  });

  @override
  String toString() {
    final parts = <String>['AuthException: $message'];
    if (statusCode != null) parts.add('statusCode=$statusCode');
    if (requestId != null) parts.add('requestId=$requestId');
    return parts.join(', ');
  }
}

/// Too many requests (HTTP 429).
class RateLimitException extends GatewayAIException {
  /// Seconds the server suggests waiting before retrying, if provided.
  final double? retryAfter;

  const RateLimitException(
    super.message, {
    super.statusCode = 429,
    super.requestId,
    this.retryAfter,
  });

  @override
  String toString() {
    final parts = <String>['RateLimitException: $message'];
    if (statusCode != null) parts.add('statusCode=$statusCode');
    if (retryAfter != null) parts.add('retryAfter=${retryAfter}s');
    if (requestId != null) parts.add('requestId=$requestId');
    return parts.join(', ');
  }
}

/// Requested resource does not exist (HTTP 404).
class NotFoundException extends GatewayAIException {
  const NotFoundException(
    super.message, {
    super.statusCode = 404,
    super.requestId,
  });
}

/// Request payload failed server-side validation (HTTP 422).
class ValidationException extends GatewayAIException {
  const ValidationException(
    super.message, {
    super.statusCode = 422,
    super.requestId,
  });
}

/// Internal server error (HTTP 5xx).
class ServerException extends GatewayAIException {
  const ServerException(
    super.message, {
    super.statusCode,
    super.requestId,
  });
}

/// SSE connection or parsing failure during streaming.
class StreamException extends GatewayAIException {
  const StreamException(
    super.message, {
    super.statusCode,
    super.requestId,
  });
}

/// Request exceeded the configured timeout.
class TimeoutException extends GatewayAIException {
  const TimeoutException(
    super.message, {
    super.statusCode,
    super.requestId,
  });
}

/// Map an HTTP status code to the appropriate exception type.
GatewayAIException exceptionFromStatus(
  int statusCode,
  String message, {
  String? requestId,
  double? retryAfter,
}) {
  switch (statusCode) {
    case 401:
    case 403:
      return AuthException(message,
          statusCode: statusCode, requestId: requestId);
    case 404:
      return NotFoundException(message,
          statusCode: statusCode, requestId: requestId);
    case 422:
      return ValidationException(message,
          statusCode: statusCode, requestId: requestId);
    case 429:
      return RateLimitException(message,
          statusCode: statusCode,
          requestId: requestId,
          retryAfter: retryAfter);
    case >= 500:
      return ServerException(message,
          statusCode: statusCode, requestId: requestId);
    default:
      return GatewayAIException(message,
          statusCode: statusCode, requestId: requestId);
  }
}
