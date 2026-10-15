package com.aigateway.ai;

/**
 * Base exception for all AI Gateway SDK errors.
 *
 * <p>Carries structured metadata (HTTP status code, message, request ID) so
 * callers can log, retry, or surface errors without parsing strings.
 */
public class GatewayAIException extends RuntimeException {

    private final Integer statusCode;
    private final String requestId;

    public GatewayAIException(String message) {
        this(message, null, null, null);
    }

    public GatewayAIException(String message, Throwable cause) {
        this(message, null, null, cause);
    }

    public GatewayAIException(String message, Integer statusCode, String requestId) {
        this(message, statusCode, requestId, null);
    }

    public GatewayAIException(String message, Integer statusCode, String requestId, Throwable cause) {
        super(message, cause);
        this.statusCode = statusCode;
        this.requestId = requestId;
    }

    /** HTTP status code from the gateway, or {@code null} for client-side errors. */
    public Integer getStatusCode() {
        return statusCode;
    }

    /** Gateway-assigned request ID for tracing, or {@code null} if unavailable. */
    public String getRequestId() {
        return requestId;
    }

    @Override
    public String toString() {
        var sb = new StringBuilder(getClass().getSimpleName());
        sb.append("(message=").append(getMessage());
        if (statusCode != null) sb.append(", statusCode=").append(statusCode);
        if (requestId != null) sb.append(", requestId=").append(requestId);
        sb.append(")");
        return sb.toString();
    }

    // -------------------------------------------------------------------------
    // Factory — maps HTTP status codes to typed exceptions
    // -------------------------------------------------------------------------

    /**
     * Create the appropriate exception subclass for a given HTTP status code.
     */
    public static GatewayAIException fromStatus(int statusCode, String message, String requestId) {
        return switch (statusCode) {
            case 401, 403 -> new AuthException(message, statusCode, requestId);
            case 429 -> new RateLimitException(message, statusCode, requestId, null);
            case 404 -> new GatewayAIException("Not found: " + message, statusCode, requestId);
            case 422 -> new GatewayAIException("Validation error: " + message, statusCode, requestId);
            case 500, 502, 503, 504 -> new GatewayAIException("Server error: " + message, statusCode, requestId);
            default -> new GatewayAIException(message, statusCode, requestId);
        };
    }
}
