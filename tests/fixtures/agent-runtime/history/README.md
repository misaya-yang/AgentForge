# Authenticated Runtime history corpus

`corpus.json` is intentionally **blocked**: no authenticated raw N-1 PostgreSQL
rollout stream was found in the tracked repository. Existing recovery reports
contain projections/counts, and public Assistant history is not native rollout
history. Neither can be relabelled as a decoder corpus. `previous: null` and no
cases prevent accidental release acceptance. The candidate identity comes from
the current source lock; it must be refreshed with the compiled writer identity
when the controlled upstream/schema pin changes.

The separate crate fixture `tests/history-fixtures/upstream-legacy-response-line.json`
is an exact existing upstream regression value with source provenance. It is
used only by decoder unit tests; it is not a recorded N-1 turn and cannot satisfy
this gate.

Primary should supply an external private corpus through `AI_PLATFORM_HISTORY_CORPUS`.
Do not add private prompts, tool output, paths or tenant data to Git. A manifest
has schema `ai-platform/history-corpus/v1`, `candidate` and `previous` writer
identities, and `cases`. Writer identities contain exactly:

- `format`: `ai-platform/rollout-writer/v1`
- `upstream_revision`: lowercase 40-character actual upstream commit
- `schema_sha256`: lowercase 64-character actual compiled App Server schema hash

Every case has exactly these fields:

| Field | Meaning |
| --- | --- |
| `id` | Unique case identifier, no private content |
| `direction` | `forward` (N reads N-1) or `rollback` (frozen N-1 decoder reads N) |
| `scenario` | One required scenario below |
| `file`, `sha256` | Relative NDJSON path and SHA256 of its exact bytes |
| `item_count` | Exact positive number of raw native rollout payloads |
| `source_receipt`, `source_receipt_sha256` | Relative source-release receipt and pinned SHA256; source.upstream_sha and schema_bundle.sha256 must match the exporting release |
| `capture_receipt`, `capture_receipt_sha256` | Relative capture receipt and pinned SHA256 |

All paths resolve within the manifest directory. Files are capped at 16 MiB and
unknown keys, wrong hashes, path escapes and missing records fail closed. Each
NDJSON line is the exact native `assistant_runtime_items.payload` for
`event_type='rollout/item'`, ordered by root sequence; it is not a public event or
a wrapper containing a payload field. Keep the raw stream immutable. For legacy
rows without a stamp, retain their absence and use the authenticated source and
capture receipts; never backfill a writer by guesswork. Full histories may carry
several valid older writer stamps. Rollback samples must include actual items
written by the candidate, rather than an unchanged all-legacy history.

Capture receipt has exactly: `schema` (`ai-platform/history-capture/v1`), `method`
(`postgresql_rollout_items`), RFC3339 `captured_at`, the exporting release's
`writer` identity, immutable `image_id` (`sha256:...`), `history_sha256`,
`source_receipt_sha256` and `item_count`. The operator must capture it against the
admitted real image/source and retain the external original release evidence.
Hashes provide integrity; an authored receipt alone does not prove a real capture.
No tool in this change manufactures capture receipts or historical records.

Both directions require all eight scenarios: `session_start`, `ordinary_turn`,
`tool_roundtrip`, `approval_pause`, `cancelled_turn`, `failed_turn`,
`compacted_resume`, `subagent_history`. The gate checks corresponding native
structure, including call/output pairs, an unresolved call for approval pause,
a real terminal error for failure and root attribution for child history. A label
without the required records fails. Normalization must roundtrip without changing
the current decoder's native semantics. Unknown native variants, malformed
metadata and incompatible payloads block promotion. Successful decode alone does
not establish provider replay, approval semantics or database restore acceptance.
