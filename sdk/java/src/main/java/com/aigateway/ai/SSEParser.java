package com.aigateway.ai;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.io.BufferedReader;
import java.io.IOException;
import java.io.InputStream;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Map;
import java.util.function.Consumer;

/**
 * Server-Sent Events (SSE) parser.
 *
 * <p>Converts a raw {@link InputStream} (from {@code HttpResponse<InputStream>})
 * into a sequence of {@link StreamEvent} objects. Handles:
 *
 * <ul>
 *   <li>{@code data: {json}\n\n} — standard single-line payloads</li>
 *   <li>Multi-line {@code data:} fields (concatenated with {@code \n})</li>
 *   <li>{@code [DONE]} sentinel signalling end-of-stream</li>
 *   <li>{@code event:} field (mapped to {@code eventType})</li>
 *   <li>Empty keep-alive lines (silently skipped)</li>
 *   <li>SSE comments (lines starting with {@code :})</li>
 * </ul>
 *
 * <p>This parser is stateless and thread-safe — create one instance and
 * reuse it across calls.
 */
public class SSEParser {

    private static final TypeReference<Map<String, Object>> MAP_TYPE = new TypeReference<>() {};

    private final ObjectMapper objectMapper;

    public SSEParser() {
        this(new ObjectMapper());
    }

    public SSEParser(ObjectMapper objectMapper) {
        this.objectMapper = objectMapper;
    }

    /**
     * Parse an SSE stream and deliver events to the consumer.
     *
     * <p>Reads lines from the input stream, buffers until a blank line
     * (event boundary), then flushes the accumulated event. The consumer
     * receives each parsed {@link StreamEvent} in order.
     *
     * <p>Blocks until the stream is exhausted or closed.
     *
     * @param stream  Raw input stream from the HTTP response body.
     * @param handler Consumer that receives each parsed event.
     * @throws StreamException if an I/O or JSON parse error occurs.
     */
    public void parse(InputStream stream, Consumer<StreamEvent> handler) {
        try (var reader = new BufferedReader(new InputStreamReader(stream, StandardCharsets.UTF_8))) {
            String eventField = null;
            List<String> dataLines = new ArrayList<>();

            String line;
            while ((line = reader.readLine()) != null) {
                // Empty line = event boundary
                if (line.isEmpty()) {
                    if (!dataLines.isEmpty()) {
                        StreamEvent event = flush(eventField, dataLines);
                        if (event != null) {
                            handler.accept(event);
                            if (event.isTerminal()) return;
                        }
                    }
                    eventField = null;
                    dataLines = new ArrayList<>();
                    continue;
                }

                // Field parsing per SSE spec
                if (line.startsWith("data:")) {
                    String value = line.substring(5);
                    // Trim single leading space per SSE spec
                    if (value.startsWith(" ")) {
                        value = value.substring(1);
                    }
                    dataLines.add(value);
                } else if (line.startsWith("event:")) {
                    eventField = line.substring(6).strip();
                } else if (line.startsWith(":")) {
                    // SSE comment / keep-alive — skip
                } else if (line.startsWith("id:")) {
                    // Event ID — currently unused but valid SSE field
                } else if (line.startsWith("retry:")) {
                    // Reconnection time — currently unused but valid SSE field
                }
            }

            // Flush any trailing event not terminated by a blank line
            if (!dataLines.isEmpty()) {
                StreamEvent event = flush(eventField, dataLines);
                if (event != null) {
                    handler.accept(event);
                }
            }

        } catch (IOException e) {
            throw new StreamException("SSE stream I/O error: " + e.getMessage(), e);
        }
    }

    // -------------------------------------------------------------------------
    // Internal
    // -------------------------------------------------------------------------

    /**
     * Parse buffered data lines into a {@link StreamEvent}, or null for sentinels.
     */
    private StreamEvent flush(String eventField, List<String> dataLines) {
        String raw = String.join("\n", dataLines);

        // [DONE] sentinel
        if ("[DONE]".equals(raw.strip())) {
            return new StreamEvent(EventType.DONE, Map.of());
        }

        // Parse JSON payload (must be mutable for event_type removal below)
        Map<String, Object> payload;
        try {
            payload = objectMapper.readValue(raw, MAP_TYPE);
        } catch (Exception e) {
            // Non-JSON data line — wrap it (mutable map for consistency)
            payload = new HashMap<>();
            payload.put("raw", raw);
        }

        // Determine event type:
        // 1. Explicit event: field from SSE
        // 2. event_type key in JSON payload
        // 3. event key in JSON payload
        // 4. Fallback to "message"
        String eventType = eventField;
        if (eventType == null) {
            Object et = payload.remove("event_type");
            if (et == null) et = payload.remove("event");
            eventType = et != null ? et.toString() : "message";
        }

        boolean hasInner = payload.containsKey("data");
        Object inner = payload.remove("data");
        Map<String, Object> data;
        if (!hasInner) {
            payload.remove("timestamp");
            data = payload;
        } else if (inner instanceof String text) {
            data = new HashMap<>();
            data.put("content", text);
        } else if (inner instanceof Map<?, ?> innerMap) {
            data = new HashMap<>();
            innerMap.forEach((key, value) -> data.put(key.toString(), value));
        } else {
            data = new HashMap<>();
            data.put("value", inner);
        }

        return new StreamEvent(eventType, data);
    }
}
