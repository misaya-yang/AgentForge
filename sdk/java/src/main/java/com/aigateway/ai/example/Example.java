package com.aigateway.ai.example;

import com.aigateway.ai.*;

import java.util.List;

/**
 * Example usage of the AI Gateway Java SDK.
 *
 * <p>Demonstrates non-streaming chat, streaming with callbacks,
 * advanced request configuration, and error handling.
 *
 * <p>Run with:
 * <pre>{@code
 * mvn compile exec:java -Dexec.mainClass="com.aigateway.ai.example.Example"
 * }</pre>
 */
public class Example {

    public static void main(String[] args) {
        // Read API key from environment or use a placeholder
        String apiKey = System.getenv("AI_GATEWAY_API_KEY");
        if (apiKey == null || apiKey.isBlank()) {
            apiKey = "gw_xxx";
        }

        try (var ai = new GatewayAI(apiKey, "http://localhost:8080", "default")) {
            System.out.println("SDK initialized: " + ai);

            // ─────────────────────────────────────────────────────────────────
            // 1. Non-streaming chat
            // ─────────────────────────────────────────────────────────────────
            System.out.println("\n=== Non-streaming ===");
            ChatResponse response = ai.chat().send("Summarize the onboarding checklist").join();
            System.out.println("Response: " + response.content());
            System.out.println("Session: " + response.sessionId());
            System.out.println("Model:   " + response.modelId());
            System.out.println("Usage:   " + response.usage());
            System.out.println("Time:    " + response.durationMs() + "ms");

            // ─────────────────────────────────────────────────────────────────
            // 2. Streaming with handler
            // ─────────────────────────────────────────────────────────────────
            System.out.println("\n=== Streaming ===");
            ai.chat().stream("Explain our refund policy", new StreamHandler() {
                @Override
                public void onText(String text) {
                    System.out.print(text);
                }

                @Override
                public void onToolCall(String toolName, String arguments) {
                    System.out.println("\n[Tool] " + toolName + " called with: " + arguments);
                }

                @Override
                public void onToolResult(String toolName, boolean success) {
                    System.out.println("[Tool] " + toolName + " → " + (success ? "OK" : "FAILED"));
                }

                @Override
                public void onSubAgentStarted(String agentId, String type, String description) {
                    System.out.println("\n[SubAgent] " + type + ": " + description);
                }

                @Override
                public void onArtifact(String artifactId, String type, String title) {
                    System.out.println("\n[Artifact] " + type + ": " + title + " (id=" + artifactId + ")");
                }

                @Override
                public void onEvent(StreamEvent event) {
                    // Catch-all for unhandled event types
                    System.out.println("\n[Event] " + event.eventType() + ": " + event.data());
                }

                @Override
                public void onError(GatewayAIException error) {
                    System.err.println("\n[Error] " + error.getMessage());
                }

                @Override
                public void onDone(ChatResponse resp) {
                    System.out.println("\n\n--- Stream complete ---");
                    System.out.println("Total length: " + resp.content().length() + " chars");
                    System.out.println("Session: " + resp.sessionId());
                }
            });

            // ─────────────────────────────────────────────────────────────────
            // 3. Advanced request with builder
            // ─────────────────────────────────────────────────────────────────
            System.out.println("\n=== Advanced request ===");
            ChatRequest advancedReq = ChatRequest.builder("Compare the free and enterprise plan limits.")
                    .modelId("gemini-3-flash-preview")
                    .temperature(0.3)
                    .maxTokens(1024)
                    .systemPrompt("You are a product support assistant. Answer concisely in bullet points.")
                    .kbDatasetIds(List.of("ds_product_docs", "ds_pricing"))
                    .kbMode("auto")
                    .webSearchEnabled(false)
                    .build();

            ChatResponse advancedResp = ai.chat().send(advancedReq).join();
            System.out.println(advancedResp.content());

            // ─────────────────────────────────────────────────────────────────
            // 4. Error handling
            // ─────────────────────────────────────────────────────────────────
            System.out.println("\n=== Error handling example ===");
            try {
                var badAi = new GatewayAI("invalid_key", "http://localhost:8080", "");
                badAi.chat().send("Hello").join();
            } catch (Exception e) {
                Throwable cause = e.getCause() != null ? e.getCause() : e;
                if (cause instanceof AuthException auth) {
                    System.out.println("Auth error (expected): " + auth.getMessage());
                } else if (cause instanceof RateLimitException rate) {
                    System.out.println("Rate limited, retry after: " + rate.getRetryAfterSeconds() + "s");
                } else if (cause instanceof GatewayAIException gatewayError) {
                    System.out.println("API error [" + gatewayError.getStatusCode() + "]: " + gatewayError.getMessage());
                } else {
                    System.out.println("Unexpected error: " + cause.getMessage());
                }
            }

            // ─────────────────────────────────────────────────────────────────
            // 5. Async streaming with CompletableFuture
            // ─────────────────────────────────────────────────────────────────
            System.out.println("\n=== Async streaming ===");
            ChatRequest streamReq = ChatRequest.builder("What is Hajj?")
                    .temperature(0.5)
                    .build();

            ai.chat().streamAsync(streamReq, new StreamHandler() {
                @Override
                public void onText(String text) {
                    System.out.print(text);
                }

                @Override
                public void onDone(ChatResponse resp) {
                    System.out.println("\n--- Async stream done ---");
                }
            }).join(); // Wait for completion in this example

        } catch (Exception e) {
            System.err.println("Fatal error: " + e.getMessage());
            e.printStackTrace();
        }
    }
}
