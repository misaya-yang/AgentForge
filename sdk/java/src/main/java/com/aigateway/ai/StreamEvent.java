package com.aigateway.ai;

import java.util.Collections;
import java.util.Map;

/**
 * A single server-sent event parsed from the chat stream.
 *
 * <p>Mirrors the Python SDK's {@code StreamEvent} dataclass. The
 * {@link #eventType()} matches one of the {@link EventType} constants and
 * {@link #data()} contains the parsed JSON payload.
 *
 * @param eventType Event name, e.g. {@code "text_delta"} or {@code "done"}.
 * @param data      Parsed JSON payload of the event.
 */
public record StreamEvent(String eventType, Map<String, Object> data) {

    /** Compact constructor — ensures data is never null. */
    public StreamEvent {
        if (eventType == null) eventType = "message";
        if (data == null) data = Collections.emptyMap();
    }

    /**
     * Convenience accessor for text content from a {@code text_delta} event.
     *
     * <p>Looks for {@code "content"} first, then {@code "text"} in the payload.
     *
     * @return the text fragment, or an empty string if not present.
     */
    public String textContent() {
        Object val = data.get("content");
        if (val == null) val = data.get("text");
        return val != null ? val.toString() : "";
    }

    /** True if this is a {@code text_delta} event. */
    public boolean isText() {
        return EventType.TEXT_DELTA.equals(eventType);
    }

    /** True if this is a {@code done} event signalling end of stream. */
    public boolean isDone() {
        return EventType.DONE.equals(eventType) || EventType.RUN_FINISHED.equals(eventType);
    }

    /** True if this is an {@code error} event. */
    public boolean isError() {
        return EventType.ERROR.equals(eventType) || EventType.RUN_ERROR.equals(eventType);
    }

    /** True if the stream was cancelled. */
    public boolean isCancelled() {
        return EventType.CANCELLED.equals(eventType);
    }

    /** True for all canonical terminal stream events. */
    public boolean isTerminal() {
        return isDone() || isError() || isCancelled();
    }

    /** Return the raw value for any key, or null. */
    @SuppressWarnings("unchecked")
    public <T> T get(String key) {
        return (T) data.get(key);
    }
}
