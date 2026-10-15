package com.aigateway.ai;

/**
 * Too many requests (HTTP 429).
 *
 * <p>The {@link #getRetryAfterSeconds()} method returns the number of seconds
 * the server recommends waiting before retrying, parsed from the
 * {@code Retry-After} header when available.
 */
public class RateLimitException extends GatewayAIException {

    private final Double retryAfterSeconds;

    public RateLimitException(String message) {
        this(message, 429, null, null);
    }

    public RateLimitException(String message, Integer statusCode, String requestId, Double retryAfterSeconds) {
        super(message, statusCode, requestId);
        this.retryAfterSeconds = retryAfterSeconds;
    }

    /**
     * Seconds the server suggests waiting before retrying,
     * or {@code null} if the header was absent.
     */
    public Double getRetryAfterSeconds() {
        return retryAfterSeconds;
    }

    @Override
    public String toString() {
        var base = super.toString();
        if (retryAfterSeconds != null) {
            return base.replace(")", ", retryAfter=" + retryAfterSeconds + ")");
        }
        return base;
    }
}
