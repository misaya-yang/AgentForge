package com.aigateway.ai;

import java.util.Collections;
import java.util.Map;

/**
 * Response from a non-streaming chat completion, or the final accumulated
 * result of a streaming chat (delivered via {@link StreamHandler#onDone}).
 *
 * <p>Mirrors the Python SDK's {@code ChatResponse} dataclass.
 *
 * @param content    The assistant's full text reply.
 * @param sessionId  Session ID (created or continued).
 * @param modelId    Model that generated this response (e.g. {@code "gemini-3-flash-preview"}).
 * @param usage      Token usage map (typically keys: {@code prompt_tokens}, {@code completion_tokens}, {@code total_tokens}).
 * @param durationMs Server-side processing duration in milliseconds.
 */
public record ChatResponse(
        String content,
        String sessionId,
        String modelId,
        Map<String, Integer> usage,
        double durationMs
) {

    /** Compact constructor — null-safe defaults. */
    public ChatResponse {
        if (content == null) content = "";
        if (sessionId == null) sessionId = "";
        if (modelId == null) modelId = "";
        if (usage == null) usage = Collections.emptyMap();
    }

    /**
     * Construct a {@code ChatResponse} from a raw JSON map returned by the gateway.
     *
     * <p>Tolerant of missing fields — every value falls back to a safe default.
     */
    @SuppressWarnings("unchecked")
    public static ChatResponse fromJson(Map<String, Object> json) {
        String content = stringOr(json, "content", "");
        String sessionId = stringOr(json, "session_id", "");
        String modelId = stringOr(json, "model_id", "");

        Map<String, Integer> usage = Collections.emptyMap();
        Object rawUsage = json.get("usage");
        if (rawUsage instanceof Map<?, ?> m) {
            try {
                usage = (Map<String, Integer>) m;
            } catch (ClassCastException e) {
                // Non-integer values — skip
            }
        }

        double durationMs = 0.0;
        Object rawDuration = json.get("duration_ms");
        if (rawDuration instanceof Number n) {
            durationMs = n.doubleValue();
        }

        return new ChatResponse(content, sessionId, modelId, usage, durationMs);
    }

    // -------------------------------------------------------------------------
    // Internal helpers
    // -------------------------------------------------------------------------

    private static String stringOr(Map<String, Object> map, String key, String fallback) {
        Object val = map.get(key);
        return val != null ? val.toString() : fallback;
    }
}
