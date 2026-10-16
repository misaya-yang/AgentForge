# Shared endpoint and event contracts

2026-10-15. Latest additions are formatted; coordinator compilation/tests pending.

## Endpoint domains

`provider_startup_seeder::ChatEndpointResolver` now exposes typed `EndpointDomain`
methods for Chat, Image, Embedding and Ocr. Existing chat trait methods delegate to
the shared resolver. The actual Gateway bootstrap image projection and
`ImageGenerationConfig::from_values` also delegate their DashScope/Google endpoint
selection. Provider-specific image model/protocol/custom submit paths remain in
those owners. Knowledge owner wired both actual service and embedded host process constructors for embedding/OCR task key/base resolution, preserving nested explicit Knowledge configuration precedence.

`foundation_migration_parity_endpoints` now has four tests, covering every original
`tests/config/test_endpoints_resolver.py` case: domain override independence,
general fallback, bare/native/compatible/full-chat URL normalization, OCR-native
URL, Google backend selection and unknown-backend fallback, per-domain Vertex key
precedence, shared Gemini fallback, and legacy Google key fallback. Earlier two
chat tests passed foundation81; the added two domain matrices await execution.

## Event envelope and registries

`event_envelope` exports generic `EventEnvelope<T>`, `UsageRecordedV1`, independent
payload and stream registries, schema lookup, stream lookup, DLQ suffix derivation
and registered-payload parsing. Unknown optional fields are ignored, required
payload fields validated, default metadata populated, and UUID/time generated when
absent. Numeric payload parsing retains accepted source scalar coercions.

Actual `UsageEventBus` production publication and consumer decoding use this
parser; the stream constant binds the shared registry. Existing handler Value
projection and Redis lifecycle are unchanged. Malformed/unknown envelopes retain
DLQ classification; typed payload validation remains the distinct error that
stops the source consumer without acknowledging its pending message.

Two foundation unit cases cover defaults/unknown-field dropping and required
payload validation/error classification. Gateway owns three schema-evolution
assertions; runtime owns the explicitly ignored live Redis event fixture. No live
Redis, DB or provider request was performed for these changes.

## Tracing initialization options

The approved `initialize_with_options` helper accepts an explicit endpoint and
native OpenTelemetry resource attributes. Existing `initialize` delegates to the
default options. Explicit empty endpoint overrides an environment endpoint;
first-initialization idempotence and optional nonfatal export remain unchanged.
The tracing-init target now checks these options and actual Resource construction
in addition to its previously passing no-endpoint/idempotence cases. A malformed
URI is tested offline; unreachable remote collector behavior is not exercised.
