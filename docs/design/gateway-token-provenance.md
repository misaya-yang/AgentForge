# Gateway token provenance

Owner: generic Gateway presentation in Python `GatewayDispatcher` and native
`stream_statistics` / `usage_parser::sync_response_stats`. This is an additive
metadata contract, `gateway-token-provenance/v1`; it does not version or replace
the public request, response, stream or accounting protocols.

## Wire contract

New generic `stream_end.metadata.usage` and new assistant history
`metadata.stats` may contain `token_provenance`, an object keyed by the existing
canonical fields `input_tokens`, `output_tokens`, and `total_tokens`. Each entry
labels only its corresponding present count:

- `upstream`: a nonnegative integer received at the adapter-to-dispatcher
  boundary. This describes where Gateway observed it, not proof that a provider
  measured it. An adapter or upstream service can itself normalize counts.
- `estimated`: a missing component filled by the existing Gateway text estimator,
  or an explicit valid upstream estimate annotation preserved with that count.
- `derived`: a locally formed total whose nonnegative integer components add to
  the final total, or an explicit valid upstream derivation annotation.
- `unknown`: invalid count/annotation, insufficient origin evidence, or a legacy
  error-path zero placeholder. Null, boolean, string, fractional and negative
  counts are preserved but are never labelled as valid reported counts.

`token_provenance` is a reserved presentation field at these new projection
boundaries. Its incoming value is validated and replaced by the canonical
per-count object: unsupported keys/types are not forwarded as provenance.
All other usage fields remain untouched.

The source enum is closed for v1. Consumers treat an absent/unsupported label as
unknown and validate the numeric value separately. Labels are descriptive only;
none proves a provider outcome, settled charge, quota refund, successful request
or remote cancellation. SDK usage remains governed by `sdk_usage`; these generic
helpers do not add `token_provenance` to SDK stats or fill SDK native counts.
The retained Python generic dispatcher has no new SDK execution path.

Sources accumulate per field as stream usage updates arrive. A new count without
a source label is a new adapter observation; the earlier label for that field
is discarded. A label-only update cannot relabel a previously observed count.
Explicit upstream `unknown` remains unknown. Timing and other usage metadata are
not token observations. Original streamed provider/adapter chunks pass through
unchanged; annotations are emitted with Gateway's terminal/history projection.
Sync history retains its existing prompt/completion alias mapping; streaming
retains its existing independent canonical estimates when only aliases exist.

## Compatibility and preservation

All existing amounts, missing fields, timing, error behavior, quota/accounting
inputs and lifecycle states stay unchanged. In particular:

- Explicit reported zero stays zero and is distinct from a missing count.
- When a normal stream has both components but no total, the total stays absent.
- Partial sync usage can calculate a total before estimating a missing component.
  Example: input 8 plus a later estimated output 3 can retain total 8. This
  legacy inconsistency is not repaired here; that locally formed total is labelled
  unknown because it is not the sum of the displayed components.
- Error before output/usage retains zero placeholders in the unchanged private
  recorder payload; no public statistics event is manufactured. Its absent source
  remains unknown. Cancellation history labels existing estimates without claiming
  settlement.
- Invalid token values that already caused arithmetic errors still do so. Source
  annotation alone never makes invalid metadata throw, sanitize amounts or succeed.
- Original historical rows are not rewritten. Numeric historical stats without
  this annotation must remain source unknown in clients. Provider measurement
  cannot be inferred from amount, an old billing `token_source`, or mere presence.

Private recorder stats are captured before presentation annotation; recorder-only
error/cancellation fallbacks remain unannotated. Existing recursive usage and
identity extraction can inspect any nested object, including an incoming field
that collides with `token_provenance`. The exact original subtree is retained in
the accounting payload, even when it contains larger nested counts or identity
fields. This preserves existing accounting amounts and attribution rather than
silently correcting that legacy extraction behavior.

No DB migration, ledger write, price change, new provider call, configuration,
public auth scope or dependency is introduced. Rollback is old-reader compatible:
old clients ignore the added object, and old producers simply omit it.

## Verification

`tests/fixtures/gateway/token_provenance_v1.json` is the shared Python/native
fixture authority. Real Python invoke/stream functions and native stream/sync
production functions exercise missing, zero, partial, reported, estimated, invalid
values/labels, aliases, derived totals and deliberately absent totals. Additional
tests cover overwritten values, label-only updates, errors, cancellation and SDK
independence. External baseline comparison preserves all non-timing presentation
values and recorder inputs, using in-memory adapters and recording/session doubles.
These tests establish source behavior, not authenticated browser, live provider,
ledger settlement or deployed-binary acceptance.
