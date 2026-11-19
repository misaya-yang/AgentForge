package com.aigateway.ai;

/**
 * Callback interface for handling SSE streaming events from the AI Gateway.
 *
 * <p>Implement the methods you care about — all have no-op defaults, so you
 * only need to override the events relevant to your use case.
 *
 * <p>Typical usage:
 * <pre>{@code
 * ai.chat().stream("Explain Zakat", new StreamHandler() {
 *     @Override public void onText(String text) {
 *         System.out.print(text);     // incremental token
 *     }
 *     @Override public void onDone(ChatResponse response) {
 *         System.out.println("\n--- Done (tokens: " + response.usage() + ")");
 *     }
 *     @Override public void onError(GatewayAIException error) {
 *         System.err.println("Stream error: " + error.getMessage());
 *     }
 * });
 * }</pre>
 *
 * <p>All callbacks are invoked on the SSE reader thread. If you need to
 * update UI, post to your own dispatcher from within the callback.
 */
public interface StreamHandler {

    /**
     * Incremental text token from the model.
     * Called for each {@code text_delta} SSE event.
     */
    default void onText(String text) {}

    /**
     * The model is invoking a tool / function call.
     *
     * @param toolName  Name of the tool being called.
     * @param arguments JSON-encoded argument string.
     */
    default void onToolCall(String toolName, String arguments) {}

    /**
     * A tool execution completed.
     *
     * @param toolName Name of the tool that finished.
     * @param success  Whether the tool executed successfully.
     */
    default void onToolResult(String toolName, boolean success) {}

    /**
     * A sub-agent has been spawned within the orchestration loop.
     *
     * @param agentId     Unique ID of the sub-agent instance.
     * @param type        Sub-agent type (e.g. "research", "code", "review").
     * @param description Human-readable description of the sub-agent's task.
     */
    default void onSubAgentStarted(String agentId, String type, String description) {}

    /**
     * An artifact (file, image, document) was generated.
     *
     * @param artifactId Unique artifact identifier.
     * @param type       Artifact type: "image", "document", "code", "chart", "file".
     * @param title      Human-readable title.
     */
    default void onArtifact(String artifactId, String type, String title) {}

    /**
     * A raw {@link StreamEvent} that does not map to any typed callback above.
     *
     * <p>Override this to handle custom or less common event types
     * (search progress, phase telemetry, quiz events, etc.).
     *
     * @param event The full parsed event with type and data map.
     */
    default void onEvent(StreamEvent event) {}

    /**
     * An error occurred during the stream.
     *
     * <p>After this callback, no further events will be delivered and
     * {@link #onDone} will NOT be called.
     */
    default void onError(GatewayAIException error) {}

    /**
     * The stream completed successfully.
     *
     * <p>The response contains the accumulated full text, session ID,
     * usage statistics, and duration.
     */
    default void onDone(ChatResponse response) {}
}
