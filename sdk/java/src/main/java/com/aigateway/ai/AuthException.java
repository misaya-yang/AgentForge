package com.aigateway.ai;

/**
 * Authentication or authorization failure (HTTP 401 / 403).
 *
 * <p>Thrown when the API key is missing, invalid, expired, or the caller
 * does not have permission for the requested resource or tenant.
 */
public class AuthException extends GatewayAIException {

    public AuthException(String message) {
        super(message, 401, null);
    }

    public AuthException(String message, Integer statusCode, String requestId) {
        super(message, statusCode, requestId);
    }
}
