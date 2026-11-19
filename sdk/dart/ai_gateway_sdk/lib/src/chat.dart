/// Chat module — streaming and non-streaming chat completions.
///
/// Usage:
/// ```dart
/// final ai = GatewayAI(apiKey: 'gw_xxx', tenantId: 'default');
///
/// // Non-streaming
/// final resp = await ai.chat.send('Hello!');
///
/// // Streaming
/// await for (final event in ai.chat.stream('Tell me a story')) {
///   if (event.isText) stdout.write(event.textContent);
/// }
/// ```

import 'dart:async';
import 'dart:convert';

import 'package:http/http.dart' as http;

import 'client.dart';
import 'exceptions.dart';
import 'models.dart';
import 'streaming.dart';

/// High-level chat operations backed by `/api/v1/assistant/chat`.
class ChatClient {
  static const _chatPath = '/api/v1/assistant/chat';
  static const _streamPath = '/api/v1/assistant/chat/stream';
  static const _cancelPath = '/api/v1/assistant/tasks/{task_id}/cancel';

  final GatewayAI _client;
  final SSEParser _sseParser = SSEParser();

  ChatClient(this._client);

  /// Send a non-streaming chat request and return the full response.
  ///
  /// Parameters:
  /// - [message] — The user message to send.
  /// - [sessionId] — Existing session to continue. Omit to create one.
  /// - [modelId] — LLM model identifier.
  /// - [temperature] — Sampling temperature (0.0 - 2.0).
  /// - [maxTokens] — Maximum tokens in the response.
  /// - [systemPrompt] — Optional system prompt override.
  /// - [kbDatasetIds] — Knowledge-base dataset IDs for RAG.
  /// - [webSearchEnabled] — Enable web search grounding.
  /// - [extra] — Additional key-value pairs forwarded to the API.
  Future<ChatResponse> send(
    String message, {
    String? sessionId,
    String? modelId,
    double temperature = 0.7,
    int? maxTokens,
    String? systemPrompt,
    List<String>? kbDatasetIds,
    bool webSearchEnabled = false,
    Map<String, dynamic>? extra,
  }) async {
    final body = _buildBody(
      message: message,
      sessionId: sessionId,
      modelId: modelId,
      temperature: temperature,
      maxTokens: maxTokens,
      systemPrompt: systemPrompt,
      kbDatasetIds: kbDatasetIds,
      webSearchEnabled: webSearchEnabled,
      stream: false,
      extra: extra,
    );

    final uri = Uri.parse('${_client.baseUrl}$_chatPath');
    final response = await _client.httpClient.post(
      uri,
      headers: _client.headers,
      body: jsonEncode(body),
    );

    _checkResponse(response);
    final json = jsonDecode(response.body) as Map<String, dynamic>;
    return ChatResponse.fromJson(json);
  }

  /// Start a streaming chat and yield [StreamEvent] objects.
  ///
  /// The returned stream stays open until the server sends a `done` event
  /// or closes the connection.
  ///
  /// Parameters are identical to [send].
  Stream<StreamEvent> stream(
    String message, {
    String? sessionId,
    String? modelId,
    double temperature = 0.7,
    int? maxTokens,
    String? systemPrompt,
    List<String>? kbDatasetIds,
    bool webSearchEnabled = false,
    Map<String, dynamic>? extra,
  }) async* {
    final body = _buildBody(
      message: message,
      sessionId: sessionId,
      modelId: modelId,
      temperature: temperature,
      maxTokens: maxTokens,
      systemPrompt: systemPrompt,
      kbDatasetIds: kbDatasetIds,
      webSearchEnabled: webSearchEnabled,
      stream: true,
      extra: extra,
    );

    final uri = Uri.parse('${_client.baseUrl}$_streamPath');

    // Use http.Client.send() for a streamed response so we can read
    // the body as a byte stream rather than buffering it all in memory.
    final request = http.Request('POST', uri)
      ..headers.addAll(_client.headers)
      ..body = jsonEncode(body);

    final streamedResponse = await _client.httpClient.send(request);

    if (streamedResponse.statusCode != 200) {
      // Read the full error body for diagnostics
      final errorBody = await streamedResponse.stream.bytesToString();
      final requestId = streamedResponse.headers['x-request-id'];
      final retryAfter = streamedResponse.headers['retry-after'];
      throw exceptionFromStatus(
        streamedResponse.statusCode,
        'Stream request failed: $errorBody',
        requestId: requestId,
        retryAfter:
            retryAfter != null ? double.tryParse(retryAfter) : null,
      );
    }

    // Pipe the raw byte stream through the SSE parser
    yield* _sseParser.parse(streamedResponse.stream);
  }

  /// Cancel a running chat task.
  ///
  /// [taskId] is obtained from the `run_started` stream event.
  /// Returns the server response with cancellation status.
  Future<Map<String, dynamic>> cancel(
    String taskId, {
    String? reason,
  }) async {
    final path = _cancelPath.replaceFirst('{task_id}', taskId);
    final uri = Uri.parse('${_client.baseUrl}$path');

    final body = <String, dynamic>{};
    if (reason != null) body['reason'] = reason;

    final response = await _client.httpClient.post(
      uri,
      headers: _client.headers,
      body: jsonEncode(body),
    );

    _checkResponse(response);
    return jsonDecode(response.body) as Map<String, dynamic>;
  }

  // -------------------------------------------------------------------------
  // Helpers
  // -------------------------------------------------------------------------

  /// Build the JSON request body, omitting null values.
  Map<String, dynamic> _buildBody({
    required String message,
    String? sessionId,
    String? modelId,
    required double temperature,
    int? maxTokens,
    String? systemPrompt,
    List<String>? kbDatasetIds,
    required bool webSearchEnabled,
    required bool stream,
    Map<String, dynamic>? extra,
  }) {
    final body = <String, dynamic>{
      'message': message,
      'temperature': temperature,
      'stream': stream,
    };

    if (modelId != null) body['model_id'] = modelId;
    if (sessionId != null) body['session_id'] = sessionId;
    if (maxTokens != null) body['max_tokens'] = maxTokens;
    if (systemPrompt != null) body['system_prompt'] = systemPrompt;
    if (kbDatasetIds != null) body['kb_dataset_ids'] = kbDatasetIds;
    if (webSearchEnabled) body['web_search_enabled'] = true;

    // Merge any extra fields
    if (extra != null) body.addAll(extra);

    return body;
  }

  /// Check a non-streaming HTTP response and throw on error.
  void _checkResponse(http.Response response) {
    if (response.statusCode >= 200 && response.statusCode < 300) return;

    final requestId = response.headers['x-request-id'];
    final retryAfter = response.headers['retry-after'];
    throw exceptionFromStatus(
      response.statusCode,
      response.body,
      requestId: requestId,
      retryAfter: retryAfter != null ? double.tryParse(retryAfter) : null,
    );
  }
}
