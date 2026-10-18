# Generated TypeScript SDK

`generated.ts` is produced by the native OpenAPI 3.2 generator. It exports
`Models`, named model aliases, `ApiOperations`, `ApiRequest`, `ApiResponse`,
`ApiEvent`, operation metadata, and a transport-delegating `createClient` helper.
The source SHA and operation inventory are recorded in `sdk/generated-manifest.json`.

```sh
ai-platform-openapi codegen --out sdk
ai-platform-openapi codegen --out sdk --check
# Offline generation/checking from an explicitly selected release artifact:
ai-platform-openapi codegen --out sdk --candidate /path/to/openapi.json --check
```

Provide the existing application's authenticated transport to `createClient`.
The helper delegates requests and async streams; it does not create fetch calls,
URLs, credentials, CSRF headers, retries, redirects or event parsing. Console
adapters must continue using the console transport and its authority checks.
`request` accepts operations with ordinary successful responses; `stream`
accepts operations with OpenAPI 3.2 `itemSchema` events. Operations supporting
both representations expose both methods.

Types describe schema structure. Server validation still enforces ranges,
patterns, scopes and other business rules. Offline candidate references may use
JSON pointers and relative JSON files confined to the candidate directory tree.
Unsupported resource rebasing, reference anchors and conflicting `allOf` schemas
fail explicitly. Generation does not fetch external resources.

## Resuming event observation

V2 thread events are an observation GET. For header-based resume, omit the
`after_sequence` query parameter and supply the generated optional header:

```ts
const events = client.stream("GET /api/v2/agent/threads/{thread}/events", {
  path: { thread },
  header: { "Last-Event-ID": cursor },
  query: { limit: 100 },
});
```

An explicit numeric `after_sequence`, including `0`, takes precedence over the
header. `limit` controls the initial replay page: default `100`, range `1..1000`.
It does not cap the live stream. Reconnecting this GET does not submit another
turn. The separate Local Node JSON event feed accepts only `after_sequence`.
