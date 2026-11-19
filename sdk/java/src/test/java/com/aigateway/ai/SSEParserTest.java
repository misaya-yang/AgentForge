package com.aigateway.ai;

import com.fasterxml.jackson.core.type.TypeReference;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.Test;

import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;

import static org.junit.jupiter.api.Assertions.*;

/**
 * Tests for {@link SSEParser} — the critical SSE stream parsing logic.
 */
class SSEParserTest {

    private final SSEParser parser = new SSEParser();

    private Map<String, Map<String, Object>> sharedFixture() throws IOException {
        String json = Files.readString(Path.of("..", "fixtures", "sse_inner_envelopes.json"));
        return new ObjectMapper().readValue(json, new TypeReference<>() {});
    }

    private List<StreamEvent> parse(String sseText) {
        var events = new ArrayList<StreamEvent>();
        var stream = new ByteArrayInputStream(sseText.getBytes(StandardCharsets.UTF_8));
        parser.parse(stream, events::add);
        return events;
    }

    @Test
    void parseSingleTextDelta() {
        String sse = """
                data: {"event_type": "text_delta", "content": "Hello"}

                """;

        List<StreamEvent> events = parse(sse);
        assertEquals(1, events.size());
        assertEquals(EventType.TEXT_DELTA, events.get(0).eventType());
        assertEquals("Hello", events.get(0).textContent());
    }

    @Test
    void parseSharedInnerEnvelopeFixture() throws IOException {
        var fixture = sharedFixture();

        var text = parse(fixture.get("text_delta").get("sse").toString()).get(0);
        assertEquals(EventType.TEXT_DELTA, text.eventType());
        assertEquals("Hi", text.textContent());

        for (String name : List.of("done", "error", "cancelled", "run_finished", "run_error")) {
            var event = parse(fixture.get(name).get("sse").toString()).get(0);
            assertEquals(fixture.get(name).get("event_type"), event.eventType());
            assertTrue(event.isTerminal());
            if ("error".equals(name)) assertEquals("boom", event.data().get("message"));
            if ("cancelled".equals(name)) assertEquals("user_stop", event.data().get("reason"));
            if ("run_error".equals(name)) {
                assertTrue(event.isError());
                assertEquals("run failed", event.data().get("message"));
            }
        }

        for (String name : List.of("null_data", "number_data", "boolean_data", "array_data")) {
            var event = parse(fixture.get(name).get("sse").toString()).get(0);
            assertEquals(fixture.get(name).get("value"), event.data().get("value"));
        }
    }

    @Test
    void parseMultipleEvents() {
        String sse = """
                data: {"event_type": "started", "session_id": "sess_123"}

                data: {"event_type": "text_delta", "content": "Zakat "}

                data: {"event_type": "text_delta", "content": "is one of"}

                data: {"event_type": "done"}

                """;

        List<StreamEvent> events = parse(sse);
        assertEquals(4, events.size());
        assertEquals(EventType.STARTED, events.get(0).eventType());
        assertEquals("Zakat ", events.get(1).textContent());
        assertEquals("is one of", events.get(2).textContent());
        assertEquals(EventType.DONE, events.get(3).eventType());
    }

    @Test
    void parseEventField() {
        // When SSE has an explicit "event:" field, it should take precedence
        String sse = """
                event: text_delta
                data: {"content": "Hello from event field"}

                """;

        List<StreamEvent> events = parse(sse);
        assertEquals(1, events.size());
        assertEquals(EventType.TEXT_DELTA, events.get(0).eventType());
        assertEquals("Hello from event field", events.get(0).textContent());
    }

    @Test
    void parseDoneSentinel() {
        String sse = """
                data: [DONE]

                """;

        List<StreamEvent> events = parse(sse);
        assertEquals(1, events.size());
        assertEquals(EventType.DONE, events.get(0).eventType());
    }

    @Test
    void parseMultiLineData() {
        // Multi-line data fields should be concatenated with \n
        String sse = """
                data: {"event_type": "text_delta",
                data:  "content": "multi-line"}

                """;

        List<StreamEvent> events = parse(sse);
        assertEquals(1, events.size());
        assertEquals(EventType.TEXT_DELTA, events.get(0).eventType());
        assertEquals("multi-line", events.get(0).textContent());
    }

    @Test
    void skipComments() {
        String sse = """
                : this is a keep-alive comment
                data: {"event_type": "text_delta", "content": "after comment"}

                """;

        List<StreamEvent> events = parse(sse);
        assertEquals(1, events.size());
        assertEquals("after comment", events.get(0).textContent());
    }

    @Test
    void skipEmptyLines() {
        // Multiple empty lines should not produce events
        String sse = """


                data: {"event_type": "text_delta", "content": "only event"}


                """;

        List<StreamEvent> events = parse(sse);
        assertEquals(1, events.size());
    }

    @Test
    void parseToolCallEvent() {
        String sse = """
                data: {"event_type": "tool_call_start", "name": "search_kb", "arguments": "{\\"query\\": \\"Zakat\\"}"}

                """;

        List<StreamEvent> events = parse(sse);
        assertEquals(1, events.size());
        assertEquals(EventType.TOOL_CALL_START, events.get(0).eventType());
        assertEquals("search_kb", events.get(0).data().get("name"));
    }

    @Test
    void parseNonJsonData() {
        // Non-JSON data should be wrapped in a {raw: ...} map
        String sse = """
                data: just plain text

                """;

        List<StreamEvent> events = parse(sse);
        assertEquals(1, events.size());
        assertEquals("message", events.get(0).eventType());
        assertEquals("just plain text", events.get(0).data().get("raw"));
    }

    @Test
    void parseTrailingEventWithoutBlankLine() {
        // An event at the end of stream not terminated by blank line should still be flushed
        String sse = "data: {\"event_type\": \"text_delta\", \"content\": \"trailing\"}";

        List<StreamEvent> events = parse(sse);
        assertEquals(1, events.size());
        assertEquals("trailing", events.get(0).textContent());
    }

    @Test
    void chatResponseFromJson() {
        var json = new java.util.HashMap<String, Object>();
        json.put("content", "Zakat is a pillar of Islam.");
        json.put("session_id", "sess_abc");
        json.put("model_id", "gemini-3-flash-preview");
        json.put("duration_ms", 1234.5);

        ChatResponse resp = ChatResponse.fromJson(json);
        assertEquals("Zakat is a pillar of Islam.", resp.content());
        assertEquals("sess_abc", resp.sessionId());
        assertEquals("gemini-3-flash-preview", resp.modelId());
        assertEquals(1234.5, resp.durationMs(), 0.01);
    }

    @Test
    void chatRequestBuilder() {
        ChatRequest req = ChatRequest.builder("test message")
                .sessionId("sess_1")
                .modelId("claude-3")
                .temperature(0.5)
                .maxTokens(512)
                .kbDatasetIds(List.of("ds_1", "ds_2"))
                .webSearchEnabled(true)
                .build();

        assertEquals("test message", req.getMessage());
        assertEquals("sess_1", req.getSessionId());
        assertEquals("claude-3", req.getModelId());
        assertEquals(0.5, req.getTemperature(), 0.001);
        assertEquals(512, req.getMaxTokens());
        assertEquals(List.of("ds_1", "ds_2"), req.getKbDatasetIds());
        assertTrue(req.isWebSearchEnabled());

        // Verify toMap omits nulls
        var map = req.toMap();
        assertEquals("test message", map.get("message"));
        assertEquals("sess_1", map.get("session_id"));
        assertEquals("claude-3", map.get("model_id"));
        assertEquals(List.of("ds_1", "ds_2"), map.get("kb_dataset_ids"));
        assertTrue((boolean) map.get("web_search_enabled"));
        assertNull(map.get("system_prompt")); // null fields omitted
    }

    @Test
    void streamEventConvenienceMethods() {
        var textEvent = new StreamEvent(EventType.TEXT_DELTA, java.util.Map.of("content", "hello"));
        assertTrue(textEvent.isText());
        assertFalse(textEvent.isDone());
        assertFalse(textEvent.isError());
        assertEquals("hello", textEvent.textContent());

        var doneEvent = new StreamEvent(EventType.DONE, null);
        assertTrue(doneEvent.isDone());
        assertEquals("", doneEvent.textContent());

        var errorEvent = new StreamEvent(EventType.ERROR, java.util.Map.of("message", "fail"));
        assertTrue(errorEvent.isError());
    }

    @Test
    void exceptionHierarchy() {
        var auth = new AuthException("bad key", 401, "req_1");
        assertInstanceOf(GatewayAIException.class, auth);
        assertEquals(401, auth.getStatusCode());

        var rate = new RateLimitException("slow down", 429, null, 30.0);
        assertInstanceOf(GatewayAIException.class, rate);
        assertEquals(30.0, rate.getRetryAfterSeconds());

        var stream = new StreamException("parse error");
        assertInstanceOf(GatewayAIException.class, stream);

        // fromStatus factory
        assertInstanceOf(AuthException.class, GatewayAIException.fromStatus(401, "unauth", null));
        assertInstanceOf(AuthException.class, GatewayAIException.fromStatus(403, "forbidden", null));
        assertInstanceOf(RateLimitException.class, GatewayAIException.fromStatus(429, "limited", null));
    }
}
