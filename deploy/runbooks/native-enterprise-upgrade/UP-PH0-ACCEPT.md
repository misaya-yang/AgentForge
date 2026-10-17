# UP-PH0-ACCEPT — Current native build acceptance

Scope/state: [loop-state.json](loop-state.json). Required journeys and phase boundary
come from the [adopted upgrade plan](../../../docs/plans/platform-upgrade-2026-10/README.md#6-phases-and-work-package-map)
and existing enterprise program. This package fills the previously referenced
Ph0 acceptance dependency; it does not create a second status ledger.

## Environment preparation — 2026-10-17

The owner supplied a clipboard provider credential and a Singapore workspace API
Host for local development. The credential was captured without printing it, then
the repository initializer created ignored `.env.dev` with mode 0600 and generated
private infrastructure secrets. The selected project stays
`agentforge-dev-7801d4a9a4`. No model key, DSN or private provider capture belongs in
Git or this receipt.

Configured primary model: `qwen3.8-flash`, Responses wire. Configured embedding:
`text-embedding-v4`, dimension 1024. Official current paths are
`/compatible-mode/v1/responses` and
`/api/v1/services/embeddings/text-embedding/text-embedding` on the workspace domain.
The provider's [Responses reference](https://www.alibabacloud.com/help/en/model-studio/qwen-api-via-openai-responses)
states the former `/api/v2/apps/protocols/...` path is no longer maintained; no
request used that legacy path here.

Actual direct probes on the owner-selected workspace both returned HTTP 401
`InvalidApiKey`. They sent only synthetic acceptance text. Raw captures are private;
no model response or embedding success is claimed. Full-token extraction was
checked and was not truncated. After the owner's correction, the clipboard was
re-read as one exact plain key value; it matched the original saved value. Both
probes still returned 401. A separate curl request with `x-api-key` to the
owner-provided Anthropic `/apps/anthropic/v1/messages` path also returned 401
`InvalidApiKey`. Key validity/workspace binding remains unresolved; do not retry
unchanged credentials or weaken authentication to proceed. Independent work continues.

`make doctor ENV_FILE=.env.dev` passed its current preflight. This proves its
implemented checks only: the separately observed Node/pnpm versions still differ
from repository pins. Docker currently has no project containers or volumes and
only its two built-in docker-driver builders; the historical named builder is
absent. No containers, volumes or builders have been created or removed in this
checkpoint.

Next: provision private local S3/scoped-role inputs, inspect and start only this
project's infrastructure serially, run the native authority's frozen and privilege
verification, build current native services with the external one-job target,
and prepare source-locked Runtime/Worker artifacts using existing builder capacity.
Measure real POST/Redis regressions and the original Assistant, Knowledge and
Local Node journeys after the required services are admitted. Compilation, direct
provider probes and preflight remain distinct from final acceptance.
