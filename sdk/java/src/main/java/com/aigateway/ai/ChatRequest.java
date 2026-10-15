package com.aigateway.ai;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

/**
 * Parameters for a chat completion (streaming or non-streaming).
 *
 * <p>Use the {@link Builder} to construct instances fluently:
 * <pre>{@code
 * ChatRequest req = ChatRequest.builder("Summarize the onboarding checklist")
 *     .sessionId("sess_abc123")
 *     .temperature(0.5)
 *     .maxTokens(2048)
 *     .kbDatasetIds(List.of("ds_product_docs", "ds_support"))
 *     .build();
 * }</pre>
 *
 * <p>Omit {@code modelId} to let the server apply its deployment default.
 *
 * <p>Mirrors the Python SDK's {@code ChatRequest} dataclass.
 */
public final class ChatRequest {

    private final String message;
    private String sessionId;
    private String modelId;
    private double temperature = 0.7;
    private Integer maxTokens;
    private String systemPrompt;
    private List<String> kbDatasetIds;
    private String kbMode;
    private Integer kbTopK;
    private Double kbScoreThreshold;
    private boolean webSearchEnabled = false;
    private Integer webSearchMaxResults;
    private String executionProfile;
    private String memoryMode;
    private boolean stream = false;

    private ChatRequest(String message) {
        if (message == null || message.isBlank()) {
            throw new IllegalArgumentException("message must not be null or blank");
        }
        this.message = message;
    }

    // -------------------------------------------------------------------------
    // Builder
    // -------------------------------------------------------------------------

    /** Create a new builder with the required message. */
    public static Builder builder(String message) {
        return new Builder(message);
    }

    /** Create a minimal request for the given message with all defaults. */
    public static ChatRequest of(String message) {
        return builder(message).build();
    }

    public static final class Builder {
        private final ChatRequest req;

        private Builder(String message) {
            this.req = new ChatRequest(message);
        }

        /** Existing session to continue. Omit to let the server create one. */
        public Builder sessionId(String sessionId) {
            req.sessionId = sessionId;
            return this;
        }

        /** LLM model identifier (omit for the server's deployment default). */
        public Builder modelId(String modelId) {
            req.modelId = modelId;
            return this;
        }

        /** Sampling temperature (default: 0.7). */
        public Builder temperature(double temperature) {
            req.temperature = temperature;
            return this;
        }

        /** Maximum tokens to generate. */
        public Builder maxTokens(int maxTokens) {
            req.maxTokens = maxTokens;
            return this;
        }

        /** Custom system prompt override. */
        public Builder systemPrompt(String systemPrompt) {
            req.systemPrompt = systemPrompt;
            return this;
        }

        /** Knowledge-base dataset IDs for RAG retrieval. */
        public Builder kbDatasetIds(List<String> kbDatasetIds) {
            req.kbDatasetIds = kbDatasetIds;
            return this;
        }

        /** KB mode: "auto", "tool", or "off". */
        public Builder kbMode(String kbMode) {
            req.kbMode = kbMode;
            return this;
        }

        /** Number of KB chunks to retrieve. */
        public Builder kbTopK(int kbTopK) {
            req.kbTopK = kbTopK;
            return this;
        }

        /** Minimum KB similarity score threshold. */
        public Builder kbScoreThreshold(double kbScoreThreshold) {
            req.kbScoreThreshold = kbScoreThreshold;
            return this;
        }

        /** Enable web search augmentation. */
        public Builder webSearchEnabled(boolean webSearchEnabled) {
            req.webSearchEnabled = webSearchEnabled;
            return this;
        }

        /** Maximum web search results to include. */
        public Builder webSearchMaxResults(int webSearchMaxResults) {
            req.webSearchMaxResults = webSearchMaxResults;
            return this;
        }

        /** Execution profile: "safe", "balanced", or "power". */
        public Builder executionProfile(String executionProfile) {
            req.executionProfile = executionProfile;
            return this;
        }

        /** Memory mode: "auto", "strict", or "off". */
        public Builder memoryMode(String memoryMode) {
            req.memoryMode = memoryMode;
            return this;
        }

        /** Build the immutable {@link ChatRequest}. */
        public ChatRequest build() {
            return req;
        }
    }

    // -------------------------------------------------------------------------
    // Accessors
    // -------------------------------------------------------------------------

    public String getMessage() { return message; }
    public String getSessionId() { return sessionId; }
    public String getModelId() { return modelId; }
    public double getTemperature() { return temperature; }
    public Integer getMaxTokens() { return maxTokens; }
    public String getSystemPrompt() { return systemPrompt; }
    public List<String> getKbDatasetIds() { return kbDatasetIds; }
    public boolean isWebSearchEnabled() { return webSearchEnabled; }
    public boolean isStream() { return stream; }

    // -------------------------------------------------------------------------
    // Serialization
    // -------------------------------------------------------------------------

    /** Package-private: used by {@link ChatClient} to set the stream flag. */
    void setStream(boolean stream) {
        this.stream = stream;
    }

    /**
     * Serialize to a JSON-safe map, omitting {@code null} values.
     * Matches the Python SDK's {@code ChatRequest.to_dict()}.
     */
    public Map<String, Object> toMap() {
        var map = new LinkedHashMap<String, Object>();
        map.put("message", message);
        putIfNotNull(map, "session_id", sessionId);
        putIfNotNull(map, "model_id", modelId);
        map.put("temperature", temperature);
        putIfNotNull(map, "max_tokens", maxTokens);
        putIfNotNull(map, "system_prompt", systemPrompt);
        putIfNotNull(map, "kb_dataset_ids", kbDatasetIds);
        putIfNotNull(map, "kb_mode", kbMode);
        putIfNotNull(map, "kb_top_k", kbTopK);
        putIfNotNull(map, "kb_score_threshold", kbScoreThreshold);
        if (webSearchEnabled) map.put("web_search_enabled", true);
        putIfNotNull(map, "web_search_max_results", webSearchMaxResults);
        putIfNotNull(map, "execution_profile", executionProfile);
        putIfNotNull(map, "memory_mode", memoryMode);
        map.put("stream", stream);
        return map;
    }

    private static void putIfNotNull(Map<String, Object> map, String key, Object value) {
        if (value != null) map.put(key, value);
    }
}
