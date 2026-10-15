/// Main SDK entry point for the AI Gateway Assistant.
///
/// Usage:
/// ```dart
/// final ai = GatewayAI(apiKey: 'gw_xxx', tenantId: 'default');
///
/// final response = await ai.chat.send('Summarize the onboarding checklist');
/// print(response.content);
///
/// // Always dispose when done
/// ai.dispose();
/// ```

import 'package:http/http.dart' as http;

import 'chat.dart';

/// SDK version string included in every request header.
const sdkVersion = 'ai-gateway-dart/1.0.0';

/// The main entry point for the AI Gateway SDK.
///
/// Initialise once, then access sub-modules (`ai.chat`) for operations.
/// Call [dispose] when you're done to release HTTP resources.
class GatewayAI {
  /// API key issued by the AI Gateway.
  final String apiKey;

  /// Gateway base URL (no trailing slash).
  final String baseUrl;

  /// Multi-tenant identifier (empty = default tenant).
  final String tenantId;

  /// Optional user identifier for per-user session scoping.
  final String? userId;

  /// The underlying HTTP client. Exposed so [ChatClient] can use `send()`
  /// for streamed responses. You may inject your own for testing.
  final http.Client httpClient;

  /// Whether this instance owns the [httpClient] and should close it
  /// on [dispose].
  final bool _ownsClient;

  /// High-level chat operations (streaming + non-streaming).
  late final ChatClient chat;

  /// Create a new SDK instance.
  ///
  /// Parameters:
  /// - [apiKey] — Required. Your AI Gateway API key (e.g. `gw_xxx`).
  /// - [baseUrl] — Gateway base URL. Defaults to `http://localhost:8080`.
  /// - [tenantId] — Tenant identifier for multi-tenant routing.
  /// - [userId] — Optional user ID for per-user session scoping.
  /// - [httpClient] — Optional. Inject your own `http.Client` for testing
  ///   or custom configuration (e.g. pinned certificates). If provided,
  ///   the SDK will NOT close it on [dispose] — you manage its lifecycle.
  GatewayAI({
    required this.apiKey,
    this.baseUrl = 'http://localhost:8080',
    this.tenantId = '',
    this.userId,
    http.Client? httpClient,
  })  : httpClient = httpClient ?? http.Client(),
        _ownsClient = httpClient == null {
    chat = ChatClient(this);
  }

  /// Standard headers included in every request.
  Map<String, String> get headers {
    final h = <String, String>{
      'X-API-Key': apiKey,
      'Content-Type': 'application/json',
      'Accept': 'application/json',
      'X-SDK-Version': sdkVersion,
    };
    if (tenantId.isNotEmpty) {
      h['X-Tenant-Id'] = tenantId;
    }
    if (userId != null && userId!.isNotEmpty) {
      h['X-User-Id'] = userId!;
    }
    return h;
  }

  /// Release all underlying HTTP connections.
  ///
  /// If you injected your own [httpClient], this is a no-op — you manage
  /// that client's lifecycle yourself.
  void dispose() {
    if (_ownsClient) {
      httpClient.close();
    }
  }
}
