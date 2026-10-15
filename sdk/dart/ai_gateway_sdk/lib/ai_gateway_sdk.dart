/// Official AI Gateway Assistant SDK for Flutter/Dart.
///
/// Provides streaming and non-streaming chat completions, session management,
/// and artifact access backed by AI Gateway.
///
/// ```dart
/// import 'package:ai_gateway_sdk/ai_gateway_sdk.dart';
///
/// final ai = GatewayAI(apiKey: 'gw_xxx', tenantId: 'default');
///
/// // Non-streaming
/// final response = await ai.chat.send('Summarize the onboarding checklist');
/// print(response.content);
///
/// // Streaming
/// await for (final event in ai.chat.stream('Explain the support escalation process')) {
///   if (event.isText) {
///     stdout.write(event.textContent);
///   }
/// }
///
/// ai.dispose();
/// ```
library ai_gateway_sdk;

export 'src/client.dart';
export 'src/chat.dart';
export 'src/models.dart';
export 'src/streaming.dart';
export 'src/exceptions.dart';
