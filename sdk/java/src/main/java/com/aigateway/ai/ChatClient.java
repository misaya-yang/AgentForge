package com.aigateway.ai;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.io.InputStream;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;
import java.util.Collections;
import java.util.Map;
import java.util.concurrent.CompletableFuture;

/**
 * Chat client for non-streaming and SSE streaming chat completions.
 *
 * <p>Obtained via {@link GatewayAI#chat()}. Not intended for direct construction.
 *
 * <p>Non-streaming requests return a {@link CompletableFuture} that resolves
 * to a {@link ChatResponse} when the full reply is ready:
 * <pre>{@code
 * ChatResponse resp = ai.chat().send("What is Zakat?").join();
 * }</pre>
 *
 * <p>Streaming requests use an SSE connection with event callbacks:
 * <pre>{@code
 * ai.chat().stream("Explain fasting", new StreamHandler() {
 *     @Override public void onText(String text) { System.out.print(text); }
 *     @Override public void onDone(ChatResponse resp) { System.out.println("\nDone!"); }
 * });
 * }</pre>
 */
public class ChatClient {

    private static final String CHAT_PATH = "/api/v1/assistant/chat";
    private static final String STREAM_PATH = "/api/v1/assistant/chat/stream";

    private static final TypeReference<Map<String, Object>> MAP_TYPE = new TypeReference<>() {};

    private final HttpClient httpClient;
    private final String baseUrl;
    private final String apiKey;
    private final String tenantId;
    private final ObjectMapper objectMapper;
    private final SSEParser sseParser;
    private final Duration timeout;

    ChatClient(HttpClient httpClient, String baseUrl, String apiKey, String tenantId,
               ObjectMapper objectMapper, Duration timeout) {
        this.httpClient = httpClient;
        this.baseUrl = baseUrl;
        this.apiKey = apiKey;
        this.tenantId = tenantId;
        this.objectMapper = objectMapper;
        this.sseParser = new SSEParser(objectMapper);
        this.timeout = timeout;
    }

    // =========================================================================
    // Non-streaming
    // =========================================================================

    /**
     * Send a simple message and receive the full response asynchronously.
     *
     * <p>Creates a {@link ChatRequest} with default settings. Use
     * {@link #send(ChatRequest)} for full control over parameters.
     */
    public CompletableFuture<ChatResponse> send(String message) {
        return send(ChatRequest.of(message));
    }

    /**
     * Send a fully configured chat request and receive the full response asynchronously.
     *
     * @param request The chat request (built via {@link ChatRequest#builder(String)}).
     * @return A future that completes with the {@link ChatResponse}.
     */
    public CompletableFuture<ChatResponse> send(ChatRequest request) {
        request.setStream(false);

        return CompletableFuture.supplyAsync(() -> {
            try {
                String jsonBody = objectMapper.writeValueAsString(request.toMap());

                HttpRequest httpRequest = buildRequest(CHAT_PATH, jsonBody)
                        .build();

                HttpResponse<String> response = httpClient.send(
                        httpRequest, HttpResponse.BodyHandlers.ofString());

                checkStatus(response.statusCode(), response.body());

                Map<String, Object> json = objectMapper.readValue(response.body(), MAP_TYPE);
                return ChatResponse.fromJson(json);

            } catch (GatewayAIException e) {
                throw e;
            } catch (Exception e) {
                throw new GatewayAIException("Chat request failed: " + e.getMessage(), e);
            }
        });
    }

    // =========================================================================
    // Streaming
    // =========================================================================

    /**
     * Stream a simple message with SSE event callbacks.
     *
     * <p>This method blocks until the stream completes or an error occurs.
     * For non-blocking usage, call from a virtual thread or executor.
     */
    public void stream(String message, StreamHandler handler) {
        stream(ChatRequest.of(message), handler);
    }

    /**
     * Stream a fully configured chat request with SSE event callbacks.
     *
     * <p>Opens an SSE connection to the gateway and dispatches parsed events
     * to the appropriate {@link StreamHandler} methods. The stream is fully
     * consumed before this method returns.
     *
     * <p>Event dispatch:
     * <ul>
     *   <li>{@code text_delta} → {@link StreamHandler#onText}</li>
     *   <li>{@code tool_call_start} / {@code tool_call} → {@link StreamHandler#onToolCall}</li>
     *   <li>{@code tool_call_result} / {@code tool_result} → {@link StreamHandler#onToolResult}</li>
     *   <li>{@code subagent_started} → {@link StreamHandler#onSubAgentStarted}</li>
     *   <li>{@code artifact_created} → {@link StreamHandler#onArtifact}</li>
     *   <li>{@code error} → {@link StreamHandler#onError}</li>
     *   <li>{@code done} → {@link StreamHandler#onDone}</li>
     *   <li>All other events → {@link StreamHandler#onEvent}</li>
     * </ul>
     *
     * @param request The chat request (built via {@link ChatRequest#builder(String)}).
     * @param handler Callback handler for stream events.
     */
    public void stream(ChatRequest request, StreamHandler handler) {
        request.setStream(true);

        try {
            String jsonBody = objectMapper.writeValueAsString(request.toMap());

            HttpRequest httpRequest = buildRequest(STREAM_PATH, jsonBody)
                    .header("Accept", "text/event-stream")
                    .build();

            HttpResponse<InputStream> response = httpClient.send(
                    httpRequest, HttpResponse.BodyHandlers.ofInputStream());

            if (response.statusCode() != 200) {
                String errorBody;
                try (var is = response.body()) {
                    errorBody = new String(is.readAllBytes());
                }
                var ex = GatewayAIException.fromStatus(response.statusCode(), errorBody, null);
                handler.onError(ex);
                return;
            }

            // Accumulate full content for the final ChatResponse
            var contentBuilder = new StringBuilder();
            String[] sessionId = {request.getSessionId() != null ? request.getSessionId() : ""};
            String[] modelId = {request.getModelId()};
            @SuppressWarnings("unchecked")
            Map<String, Integer>[] usage = new Map[]{Collections.emptyMap()};
            double[] durationMs = {0.0};
            boolean[] unsuccessfulTerminal = {false};

            sseParser.parse(response.body(), event -> {
                if (event.isError() || event.isCancelled()) {
                    unsuccessfulTerminal[0] = true;
                }
                dispatchEvent(event, handler, contentBuilder, sessionId, modelId, usage, durationMs);
            });

            // Send final accumulated response via onDone
            if (!unsuccessfulTerminal[0]) {
                handler.onDone(new ChatResponse(
                        contentBuilder.toString(),
                        sessionId[0],
                        modelId[0],
                        usage[0],
                        durationMs[0]
                ));
            }

        } catch (GatewayAIException e) {
            handler.onError(e);
        } catch (Exception e) {
            handler.onError(new StreamException("Stream failed: " + e.getMessage(), e));
        }
    }

    /**
     * Stream a fully configured chat request with SSE event callbacks,
     * returning a {@link CompletableFuture} that completes when the stream finishes.
     *
     * <p>The streaming runs on a virtual thread (Java 21+) or the common pool,
     * allowing the caller to continue without blocking.
     */
    public CompletableFuture<Void> streamAsync(ChatRequest request, StreamHandler handler) {
        return CompletableFuture.runAsync(() -> stream(request, handler));
    }

    // =========================================================================
    // Internal helpers
    // =========================================================================

    private HttpRequest.Builder buildRequest(String path, String jsonBody) {
        var builder = HttpRequest.newBuilder()
                .uri(URI.create(baseUrl + path))
                .timeout(timeout)
                .header("Content-Type", "application/json")
                .header("X-API-Key", apiKey)
                .POST(HttpRequest.BodyPublishers.ofString(jsonBody));

        if (tenantId != null && !tenantId.isEmpty()) {
            builder.header("X-Tenant-ID", tenantId);
        }

        return builder;
    }

    @SuppressWarnings("unchecked")
    private void checkStatus(int statusCode, String responseBody) {
        if (statusCode >= 200 && statusCode < 300) return;

        // Try to extract error message from JSON body
        String message = "HTTP " + statusCode;
        String requestId = null;
        try {
            Map<String, Object> errorJson = objectMapper.readValue(responseBody, MAP_TYPE);
            Object detail = errorJson.get("detail");
            if (detail == null) detail = errorJson.get("message");
            if (detail == null) detail = errorJson.get("error");
            if (detail != null) message = detail.toString();
            Object rid = errorJson.get("request_id");
            if (rid != null) requestId = rid.toString();
        } catch (Exception ignored) {
            if (responseBody != null && !responseBody.isBlank()) {
                message = responseBody;
            }
        }

        throw GatewayAIException.fromStatus(statusCode, message, requestId);
    }

    @SuppressWarnings("unchecked")
    private void dispatchEvent(StreamEvent event, StreamHandler handler,
                               StringBuilder contentBuilder, String[] sessionId,
                               String[] modelId, Map<String, Integer>[] usage,
                               double[] durationMs) {

        String type = event.eventType();
        Map<String, Object> data = event.data();

        switch (type) {
            case EventType.TEXT_DELTA -> {
                String text = event.textContent();
                contentBuilder.append(text);
                handler.onText(text);
            }

            case EventType.TOOL_CALL_START, EventType.TOOL_CALL -> {
                String toolName = stringOr(data, "name", stringOr(data, "tool_name", "unknown"));
                String arguments = stringOr(data, "arguments", "{}");
                handler.onToolCall(toolName, arguments);
            }

            case EventType.TOOL_CALL_RESULT, EventType.TOOL_RESULT -> {
                String toolName = stringOr(data, "name", stringOr(data, "tool_name", "unknown"));
                boolean success = Boolean.TRUE.equals(data.get("success"));
                handler.onToolResult(toolName, success);
            }

            case EventType.SUBAGENT_STARTED -> {
                String agentId = stringOr(data, "agent_id", "");
                String agentType = stringOr(data, "type", "");
                String description = stringOr(data, "description", "");
                handler.onSubAgentStarted(agentId, agentType, description);
            }

            case EventType.ARTIFACT_CREATED -> {
                String artifactId = stringOr(data, "artifact_id", "");
                String artifactType = stringOr(data, "type", "");
                String title = stringOr(data, "title", "");
                handler.onArtifact(artifactId, artifactType, title);
            }

            case EventType.SESSION_CREATED, EventType.SESSION_UPDATED -> {
                Object sid = data.get("session_id");
                if (sid != null) sessionId[0] = sid.toString();
            }

            case EventType.USAGE -> {
                try {
                    Object raw = data.get("usage");
                    if (raw instanceof Map<?, ?> m) {
                        usage[0] = (Map<String, Integer>) m;
                    } else if (!data.isEmpty()) {
                        usage[0] = (Map<String, Integer>) (Map<?, ?>) data;
                    }
                } catch (ClassCastException ignored) {}
            }

            case EventType.FINISH, EventType.RUN_FINISHED -> {
                Object mid = data.get("model_id");
                if (mid != null) modelId[0] = mid.toString();
                Object dur = data.get("duration_ms");
                if (dur instanceof Number n) durationMs[0] = n.doubleValue();
            }

            case EventType.ERROR, EventType.RUN_ERROR -> {
                String msg = stringOr(data, "message", stringOr(data, "error", "Unknown stream error"));
                handler.onError(new GatewayAIException(msg));
                return; // Don't dispatch to onEvent after error
            }

            case EventType.DONE -> {
                // Done is handled after parse() returns — accumulation is complete
                return;
            }

            default -> {
                // Forward unhandled events to the generic onEvent callback
                handler.onEvent(event);
            }
        }
    }

    private static String stringOr(Map<String, Object> map, String key, String fallback) {
        Object val = map.get(key);
        return val != null ? val.toString() : fallback;
    }
}
