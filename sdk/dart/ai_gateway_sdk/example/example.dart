// ignore_for_file: avoid_print
/// Example usage of the AI Gateway SDK for Flutter/Dart.
///
/// Run with: dart run example/example.dart

import 'dart:io';

import 'package:ai_gateway_sdk/ai_gateway_sdk.dart';

void main() async {
  // -----------------------------------------------------------------------
  // 1. Initialise the client
  // -----------------------------------------------------------------------
  final ai = GatewayAI(
    apiKey: 'gw_xxx', // Replace with your actual API key
    baseUrl: 'http://localhost:8080', // Or your gateway URL
    tenantId: 'default', // Replace with your tenant identifier.
  );

  try {
    // ---------------------------------------------------------------------
    // 2. Non-streaming chat
    // ---------------------------------------------------------------------
    print('--- Non-streaming chat ---');
    // modelId is omitted — the server applies its deployment default.
    final response = await ai.chat.send(
      'Summarize the onboarding checklist',
      temperature: 0.7,
    );
    print('Session: ${response.sessionId}');
    print('Response: ${response.content}');
    if (response.usage != null) {
      print('Usage: ${response.usage}');
    }
    print('');

    // ---------------------------------------------------------------------
    // 3. Streaming chat — the core value of this SDK
    // ---------------------------------------------------------------------
    print('--- Streaming chat ---');
    String? sessionId;

    await for (final event in ai.chat.stream(
      'Explain our refund policy',
      sessionId: response.sessionId, // Continue same session
      temperature: 0.5,
    )) {
      switch (event.eventType) {
        case EventType.started:
          print('[Stream started]');
          break;

        case EventType.sessionCreated:
          sessionId = event.data['session_id']?.toString();
          print('[Session: $sessionId]');
          break;

        case EventType.textDelta:
          // Stream text content token by token
          stdout.write(event.textContent);
          break;

        case EventType.usage:
          print('\n[Usage: ${event.data}]');
          break;

        case EventType.artifactCreated:
          final artifact = Artifact.fromJson(event.data);
          print('\n[Artifact: ${artifact.title} (${artifact.type})]');
          break;

        case EventType.contextRetrieved:
          final count = (event.data['contexts'] as List?)?.length ?? 0;
          print('[Retrieved $count KB contexts]');
          break;

        case EventType.error:
          print('\n[ERROR: ${event.data}]');
          break;

        case EventType.done:
          print('\n[Stream complete]');
          break;

        default:
          // Other event types (status, thinking, tool calls, etc.)
          // can be handled as needed
          break;
      }
    }
    print('');

    // ---------------------------------------------------------------------
    // 4. Streaming with Knowledge Base (RAG)
    // ---------------------------------------------------------------------
    print('--- Streaming with KB ---');
    await for (final event in ai.chat.stream(
      'Compare the free and enterprise plan limits.',
      kbDatasetIds: ['product_docs_v1'],
      temperature: 0.3,
    )) {
      if (event.isText) {
        stdout.write(event.textContent);
      } else if (event.isDone) {
        print('\n');
      }
    }

    // ---------------------------------------------------------------------
    // 5. Error handling
    // ---------------------------------------------------------------------
    print('--- Error handling demo ---');
    try {
      await ai.chat.send('test', modelId: 'nonexistent-model');
    } on AuthException catch (e) {
      print('Auth error: $e');
    } on RateLimitException catch (e) {
      print('Rate limited! Retry after: ${e.retryAfter}s');
    } on GatewayAIException catch (e) {
      print('API error [${e.statusCode}]: ${e.message}');
    }
  } finally {
    // Always dispose the client to release HTTP connections
    ai.dispose();
  }
}
