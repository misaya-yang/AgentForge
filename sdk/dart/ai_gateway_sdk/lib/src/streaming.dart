/// Server-Sent Events (SSE) parser for Dart/Flutter.
///
/// Converts the raw byte stream from an `http.Client.send()` streamed
/// response into a sequence of [StreamEvent] objects. Handles:
///
/// * `data: {json}\n\n` — standard single-line payloads
/// * Multi-line `data:` fields (concatenated with `\n`)
/// * `event:` field (mapped to eventType)
/// * `[DONE]` sentinel signalling end-of-stream
/// * Empty keep-alive lines (silently skipped)
/// * `: comment` lines (SSE keep-alive, silently skipped)

import 'dart:async';
import 'dart:convert';

import 'models.dart';
import 'exceptions.dart';

/// Stateless SSE parser that transforms a raw byte stream into [StreamEvent]s.
///
/// Usage:
/// ```dart
/// final parser = SSEParser();
/// await for (final event in parser.parse(response.stream)) {
///   if (event.isText) {
///     stdout.write(event.textContent);
///   }
/// }
/// ```
class SSEParser {
  /// Parse a byte stream (from `http.Client.send()`) into [StreamEvent]s.
  ///
  /// The stream is expected to be a standard SSE byte stream where events
  /// are separated by double newlines (`\n\n`). Each event may contain
  /// `data:`, `event:`, and `: comment` fields.
  ///
  /// The parser yields a [StreamEvent] for each complete SSE event. When
  /// a `[DONE]` sentinel is encountered, a final done event is yielded
  /// and the stream ends.
  Stream<StreamEvent> parse(Stream<List<int>> byteStream) async* {
    // Buffer for incomplete lines across chunk boundaries
    String lineBuffer = '';

    // Current event being assembled
    String? currentEventField;
    List<String> currentDataLines = [];

    try {
      await for (final chunk in byteStream) {
        // Decode bytes to string and append to buffer
        lineBuffer += utf8.decode(chunk, allowMalformed: true);

        // Process complete lines (split on \n)
        while (lineBuffer.contains('\n')) {
          final newlineIndex = lineBuffer.indexOf('\n');
          final line = lineBuffer.substring(0, newlineIndex).trimRight();
          lineBuffer = lineBuffer.substring(newlineIndex + 1);

          // Empty line = event boundary (double newline)
          if (line.isEmpty) {
            if (currentDataLines.isNotEmpty) {
              final event = _flush(currentEventField, currentDataLines);
              if (event != null) {
                yield event;
                if (event.isTerminal) return;
              }
            }
            currentEventField = null;
            currentDataLines = [];
            continue;
          }

          // Parse SSE field
          if (line.startsWith('data:')) {
            // data: value (strip the space after colon if present)
            final value = line.length > 5 ? line.substring(5).trimLeft() : '';
            currentDataLines.add(value);
          } else if (line.startsWith('event:')) {
            currentEventField = line.substring(6).trim();
          } else if (line.startsWith(':')) {
            // SSE comment / keep-alive — skip silently
            continue;
          }
          // Ignore unrecognised fields per SSE spec
        }
      }

      // Flush any trailing event not terminated by a blank line
      if (currentDataLines.isNotEmpty) {
        final event = _flush(currentEventField, currentDataLines);
        if (event != null) {
          yield event;
        }
      }
    } on StreamException {
      rethrow;
    } catch (e) {
      throw StreamException('SSE parse failure: $e');
    }
  }

  /// Parse buffered data lines into a [StreamEvent], or `null` for sentinels.
  static StreamEvent? _flush(
    String? eventField,
    List<String> dataLines,
  ) {
    final raw = dataLines.join('\n');

    // [DONE] sentinel
    if (raw.trim() == '[DONE]') {
      return StreamEvent(
        eventType: EventType.done,
        data: const {},
        timestamp: DateTime.now().millisecondsSinceEpoch / 1000.0,
      );
    }

    // Parse JSON payload
    Map<String, dynamic> payload;
    try {
      payload = Map<String, dynamic>.from(
        jsonDecode(raw) as Map,
      );
    } on FormatException {
      // Non-JSON data line — wrap it
      payload = {'raw': raw};
    }

    // Determine event type:
    //   explicit event: field > payload event_type > payload event > "message"
    final eventType = eventField ??
        payload.remove('event_type')?.toString() ??
        payload.remove('event')?.toString() ??
        'message';

    final timestamp = payload.remove('timestamp');
    final ts = timestamp != null
        ? (timestamp is num
            ? timestamp.toDouble()
            : double.tryParse(timestamp.toString()))
        : DateTime.now().millisecondsSinceEpoch / 1000.0;

    final hasInner = payload.containsKey('data');
    final inner = payload.remove('data');
    final Map<String, dynamic> data;
    if (!hasInner) {
      data = payload;
    } else if (inner is String) {
      data = {'content': inner};
    } else if (inner is Map) {
      data = Map<String, dynamic>.from(inner);
    } else {
      data = {'value': inner};
    }

    return StreamEvent(
      eventType: eventType,
      data: data,
      timestamp: ts,
    );
  }
}
