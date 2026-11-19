/// Data models for the AI Gateway SDK.
///
/// All models are simple Dart classes with `fromJson` factory constructors
/// for safe construction from raw maps — extra/missing fields never crash.

// ---------------------------------------------------------------------------
// Event types — mirrors the gateway's SSE event vocabulary
// ---------------------------------------------------------------------------

/// Canonical SSE event type constants.
///
/// Use these instead of raw strings for reliable pattern-matching.
abstract class EventType {
  // Core stream events
  static const started = 'started';
  static const status = 'status';
  static const textDelta = 'text_delta';
  static const thinkingDelta = 'thinking_delta';
  static const thinkingEnd = 'thinking_end';
  static const usage = 'usage';
  static const finish = 'finish';
  static const done = 'done';
  static const error = 'error';

  // Retrieval and files
  static const contextRetrieved = 'context_retrieved';
  static const webSearchResults = 'web_search_results';
  static const fileProcessed = 'file_processed';
  static const ragEvaluation = 'rag_evaluation';
  static const cacheMetrics = 'cache_metrics';

  // Session metadata
  static const sessionCreated = 'session_created';
  static const sessionUpdated = 'session_updated';

  // Validation / warnings
  static const outputWarnings = 'output_warnings';

  // Run lifecycle
  static const runStarted = 'run_started';
  static const runFinished = 'run_finished';
  static const runError = 'run_error';
  static const cancelled = 'cancelled';

  // Step lifecycle
  static const stepStarted = 'step_started';
  static const stepFinished = 'step_finished';

  // Tool lifecycle
  static const toolCallStart = 'tool_call_start';
  static const toolCallArgs = 'tool_call_args';
  static const toolCallEnd = 'tool_call_end';
  static const toolCallResult = 'tool_call_result';
  static const toolError = 'tool_error';

  // File generation
  static const fileCreating = 'file_creating';
  static const fileCreated = 'file_created';
  static const artifactCreated = 'artifact_created';
  static const outlineReady = 'outline_ready';

  // Search progress
  static const searchStarted = 'search_started';
  static const searchProgress = 'search_progress';
  static const searchCompleted = 'search_completed';

  // Code / image / document execution
  static const codeExecutionStart = 'code_execution_start';
  static const codeExecutionOutput = 'code_execution_output';
  static const codeExecutionResult = 'code_execution_result';
  static const imageGenerationStart = 'image_generation_start';
  static const imageGenerationResult = 'image_generation_result';
  static const documentGenerationStart = 'document_generation_start';
  static const documentGenerationResult = 'document_generation_result';

  // Sub-Agent lifecycle
  static const subagentStarted = 'subagent_started';
  static const subagentStep = 'subagent_step';
  static const subagentTextDelta = 'subagent_text_delta';
  static const subagentFinished = 'subagent_finished';

  // Quiz events
  static const quizStatus = 'quiz:status';
  static const quizReady = 'quiz:ready';
  static const quizError = 'quiz:error';
}

// ---------------------------------------------------------------------------
// StreamEvent
// ---------------------------------------------------------------------------

/// A single server-sent event parsed from the chat stream.
class StreamEvent {
  /// Event name, e.g. `"text_delta"` or `"done"`.
  final String eventType;

  /// Parsed JSON payload of the event.
  final Map<String, dynamic> data;

  /// Server-assigned epoch timestamp (seconds), if present.
  final double? timestamp;

  const StreamEvent({
    required this.eventType,
    this.data = const {},
    this.timestamp,
  });

  /// Whether this is a text content delta.
  bool get isText => eventType == EventType.textDelta;

  /// Whether this signals end-of-stream.
  bool get isDone =>
      eventType == EventType.done || eventType == EventType.runFinished;

  /// Whether this is an error event.
  bool get isError =>
      eventType == EventType.error || eventType == EventType.runError;

  /// Whether the stream was cancelled.
  bool get isCancelled => eventType == EventType.cancelled;

  /// Whether this event terminates the stream.
  bool get isTerminal => isDone || isError || isCancelled;

  /// Convenience accessor for the text content of a `text_delta` event.
  String get textContent =>
      (data['content'] ?? data['text'] ?? '').toString();

  /// Construct from a parsed JSON map.
  factory StreamEvent.fromJson(Map<String, dynamic> json) {
    return StreamEvent(
      eventType: (json['event_type'] ?? json['event'] ?? 'message').toString(),
      data: Map<String, dynamic>.from(json)
        ..remove('event_type')
        ..remove('event')
        ..remove('timestamp'),
      timestamp: json['timestamp'] != null
          ? (json['timestamp'] as num).toDouble()
          : null,
    );
  }

  @override
  String toString() => 'StreamEvent($eventType, data=$data)';
}

// ---------------------------------------------------------------------------
// ChatResponse
// ---------------------------------------------------------------------------

/// Response from a non-streaming chat completion.
class ChatResponse {
  /// The assistant's response text.
  final String content;

  /// Session ID (existing or newly created).
  final String sessionId;

  /// Model used for generation.
  final String? modelId;

  /// Unique run identifier for this request.
  final String? runId;

  /// Token usage breakdown (e.g. `{"prompt_tokens": 10, "completion_tokens": 50}`).
  final Map<String, int>? usage;

  /// Server-side generation time in milliseconds.
  final double? durationMs;

  /// Retrieved RAG contexts, if any.
  final List<Map<String, dynamic>>? contexts;

  const ChatResponse({
    required this.content,
    required this.sessionId,
    this.modelId,
    this.runId,
    this.usage,
    this.durationMs,
    this.contexts,
  });

  /// Construct from a parsed JSON map.
  factory ChatResponse.fromJson(Map<String, dynamic> json) {
    return ChatResponse(
      content: (json['content'] ?? '').toString(),
      sessionId: (json['session_id'] ?? '').toString(),
      modelId: json['model_id']?.toString(),
      runId: json['run_id']?.toString(),
      usage: json['usage'] != null
          ? Map<String, int>.from(
              (json['usage'] as Map).map(
                (k, v) => MapEntry(k.toString(), (v as num).toInt()),
              ),
            )
          : null,
      durationMs: json['duration_ms'] != null
          ? (json['duration_ms'] as num).toDouble()
          : null,
      contexts: json['contexts'] != null
          ? List<Map<String, dynamic>>.from(
              (json['contexts'] as List).map(
                (c) => Map<String, dynamic>.from(c as Map),
              ),
            )
          : null,
    );
  }

  @override
  String toString() =>
      'ChatResponse(sessionId=$sessionId, contentLength=${content.length})';
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

/// An assistant conversation session.
class Session {
  final String sessionId;
  final String userId;
  final String tenantId;
  final String? serviceId;
  final String? createdAt;
  final String? updatedAt;
  final Map<String, dynamic>? metadata;
  final int messageCount;

  const Session({
    required this.sessionId,
    this.userId = '',
    this.tenantId = '',
    this.serviceId,
    this.createdAt,
    this.updatedAt,
    this.metadata,
    this.messageCount = 0,
  });

  /// Human-readable title from metadata, if present.
  String? get title {
    if (metadata != null && metadata!.containsKey('title')) {
      return metadata!['title'].toString();
    }
    return null;
  }

  /// Construct from a parsed JSON map.
  factory Session.fromJson(Map<String, dynamic> json) {
    return Session(
      sessionId: (json['session_id'] ?? '').toString(),
      userId: (json['user_id'] ?? '').toString(),
      tenantId: (json['tenant_id'] ?? '').toString(),
      serviceId: json['service_id']?.toString(),
      createdAt: json['created_at']?.toString(),
      updatedAt: json['updated_at']?.toString(),
      metadata: json['metadata'] != null
          ? Map<String, dynamic>.from(json['metadata'] as Map)
          : null,
      messageCount: (json['message_count'] ?? 0) as int,
    );
  }

  @override
  String toString() => 'Session($sessionId)';
}

// ---------------------------------------------------------------------------
// Message
// ---------------------------------------------------------------------------

/// A single message in a session's history.
class Message {
  final String role;
  final String content;
  final String? timestamp;
  final Map<String, dynamic>? metadata;

  const Message({
    required this.role,
    required this.content,
    this.timestamp,
    this.metadata,
  });

  /// Construct from a parsed JSON map.
  factory Message.fromJson(Map<String, dynamic> json) {
    return Message(
      role: (json['role'] ?? '').toString(),
      content: (json['content'] ?? '').toString(),
      timestamp: json['timestamp']?.toString(),
      metadata: json['metadata'] != null
          ? Map<String, dynamic>.from(json['metadata'] as Map)
          : null,
    );
  }

  @override
  String toString() => 'Message($role: ${content.length} chars)';
}

// ---------------------------------------------------------------------------
// Artifact
// ---------------------------------------------------------------------------

/// A generated or uploaded artifact (image, document, chart, code, file).
class Artifact {
  final String artifactId;
  final String sessionId;

  /// Type: image, document, chart, code, file.
  final String type;

  /// Format: png, jpg, pdf, json, html, etc.
  final String format;

  final String title;
  final String filename;
  final int sizeBytes;
  final String? mimeType;

  /// Source: ai, user, code_execution, image_generation, document_generation.
  final String? source;

  final String? storageKey;
  final String? messageId;
  final String? tenantId;
  final String? userId;
  final Map<String, dynamic>? metadata;
  final String? downloadUrl;
  final String? createdAt;
  final String? updatedAt;

  const Artifact({
    required this.artifactId,
    required this.sessionId,
    required this.type,
    required this.format,
    required this.title,
    this.filename = '',
    this.sizeBytes = 0,
    this.mimeType,
    this.source,
    this.storageKey,
    this.messageId,
    this.tenantId,
    this.userId,
    this.metadata,
    this.downloadUrl,
    this.createdAt,
    this.updatedAt,
  });

  /// Construct from a parsed JSON map.
  factory Artifact.fromJson(Map<String, dynamic> json) {
    return Artifact(
      artifactId: (json['artifact_id'] ?? '').toString(),
      sessionId: (json['session_id'] ?? '').toString(),
      type: (json['type'] ?? '').toString(),
      format: (json['format'] ?? '').toString(),
      title: (json['title'] ?? '').toString(),
      filename: (json['filename'] ?? '').toString(),
      sizeBytes: (json['size_bytes'] ?? 0) as int,
      mimeType: json['mime_type']?.toString(),
      source: json['source']?.toString(),
      storageKey: json['storage_key']?.toString(),
      messageId: json['message_id']?.toString(),
      tenantId: json['tenant_id']?.toString(),
      userId: json['user_id']?.toString(),
      metadata: json['metadata'] != null
          ? Map<String, dynamic>.from(json['metadata'] as Map)
          : null,
      downloadUrl: json['download_url']?.toString(),
      createdAt: json['created_at']?.toString(),
      updatedAt: json['updated_at']?.toString(),
    );
  }

  @override
  String toString() => 'Artifact($artifactId, $type/$format, "$title")';
}
