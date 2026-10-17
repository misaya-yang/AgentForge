# Operator reranker measurements

`reranker-bakeoff` keeps its fixed golden candidates, identity baseline, slice metrics,
report schema and promotion rules. The original `bge` adapter remains a fully
offline Candle CPU benchmark over local pinned weights, with the original model
revision and artifact SHA verification. Its report adds `native_identity` containing
the Candle model/revision/SHA fingerprint. It needs neither actor credentials nor
`--allow-live`, never downloads weights, and missing or mismatched weights remain
real adapter failures. The three existing `KNOWLEDGE_NATIVE__*_MODEL_DIRECTORY`
settings select local directories; the pinned default paths remain unchanged.

Jina/Cohere/Dashscope adapters use the same signed Knowledge model-plane client as
the service. Direct vendor HTTP and provider-key discovery are removed.
`knowledge-eval-gate` remains an offline evidence gate and requires no credentials.

An external-model measurement requires `--allow-live`, explicit `--tenant-id`, `--user-id`, and
`--credential-file /absolute/operator-token`. The credential file must be a regular,
non-symlink, bounded UTF-8 file with Unix mode 0600. The default credential kind is
`bearer`; `--credential-kind api-key` sends `x-api-key` instead. The command first
checks `/api/v1/auth/me` and requires an exact current user/tenant match. It does
not derive authority from CLI roles or invent a system/admin actor.

Configure the same four public settings used by the service:

- `KNOWLEDGE_MODEL_PLANE__URL` (or `--model-plane-url`)
- `KNOWLEDGE_MODEL_PLANE__WORKLOAD_KEY_FILE` (or `--workload-key-file`)
- `KNOWLEDGE_MODEL_PLANE__GATEWAY_PUBLIC_KEYS_FILE` (or `--gateway-public-keys-file`)
- `KNOWLEDGE_MODEL_PLANE__MODEL_REVISIONS` (or `--model-revisions`), a JSON map from
  `provider/model` to the registered immutable model revision.

The key file is the Knowledge workload's own signing key; the verification file
contains only Gateway public keys. Provider credentials and Gateway private keys
are never inputs. Existing service key-file validation is reused. Credentials
are sent only to an HTTPS origin or literal loopback HTTP development origin;
proxies, redirects and transport retries are disabled.

Example (paths and IDs are operator-supplied, never defaults):

```sh
platform-knowledge-ops reranker-bakeoff \
  --cases tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl \
  --out /absolute/report.json --adapters identity,dashscope:qwen3-rerank \
  --allow-live --tenant-id ACTUAL_TENANT --user-id ACTUAL_USER \
  --credential-file /absolute/operator-token
```

Golden-input Rerank calls can omit Knowledge resources. `--dataset-id` instead
binds the operation to the existing dataset ACL and publication generation;
document/job/generation checks remain mandatory whenever present. Gateway model,
budget, lease and settlement authority applies in either case. Missing model-plane
configuration or actor admission fails before inference. Per-case model failures
still make the adapter ineligible; unknown dispatch is never promoted to success.

Primary-writer verification commands (not executed by this implementation task):

```sh
cargo test -p ai-platform-knowledge-ops --lib
cargo test -p ai-platform-knowledge-domain model_scope --lib
```

Real measured quality and paid-provider verification remain separate acceptance.

The original local benchmark remains:

```sh
platform-knowledge-ops reranker-bakeoff \
  --cases tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl \
  --out /absolute/local-report.json --adapters identity,bge
```
