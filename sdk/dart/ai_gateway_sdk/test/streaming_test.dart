import 'dart:convert';
import 'dart:io';

import 'package:ai_gateway_sdk/ai_gateway_sdk.dart';
import 'package:test/test.dart';

Map<String, dynamic> loadFixture() {
  final file = File('../../fixtures/sse_inner_envelopes.json');
  return jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
}

Future<StreamEvent> parseFixture(String name, Map<String, dynamic> fixture) async {
  final entry = fixture[name] as Map<String, dynamic>;
  final events = await SSEParser()
      .parse(Stream.value(utf8.encode(entry['sse'] as String)))
      .toList();
  return events.single;
}

void main() {
  test('shared SSE inner-envelope fixture', () async {
    final fixture = loadFixture();

    final text = await parseFixture('text_delta', fixture);
    expect(text.eventType, EventType.textDelta);
    expect(text.textContent, 'Hi');

    for (final name in [
      'done',
      'error',
      'cancelled',
      'run_finished',
      'run_error',
    ]) {
      final event = await parseFixture(name, fixture);
      expect(event.eventType, (fixture[name] as Map)['event_type']);
      expect(event.isTerminal, isTrue);
      if (name == 'error') expect(event.data['message'], 'boom');
      if (name == 'cancelled') expect(event.data['reason'], 'user_stop');
      if (name == 'run_error') {
        expect(event.isError, isTrue);
        expect(event.data['message'], 'run failed');
      }
    }

    for (final name in [
      'null_data',
      'number_data',
      'boolean_data',
      'array_data',
    ]) {
      final event = await parseFixture(name, fixture);
      expect(event.data, {'value': (fixture[name] as Map)['value']});
    }
  });
}
