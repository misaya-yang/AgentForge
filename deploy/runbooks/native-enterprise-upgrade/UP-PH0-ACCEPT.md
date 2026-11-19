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

Credential resolution later in the same session: the owner corrected the clipboard
value, and the exact plain value replaced the private credential. The workspace
Responses probe then returned HTTP 200, `completed`, and exactly `OK`; native
`text-embedding-v4` returned HTTP 200 with 1024 finite dimensions. Current receipt:
[UP-PH0-provider-connectivity.json](../../../reports/platform-upgrade-20261017/UP-PH0-provider-connectivity.json).
The earlier 401 receipt remains historical evidence. This resolves direct provider
connectivity only; the platform journeys still require their real service evidence.

The initial `make doctor ENV_FILE=.env.dev` passed its implemented preflight checks.
At that point Node/pnpm differed from repository pins, Docker had no project containers
or volumes, and only its two built-in docker-driver builders existed. The dated initial
preflight receipt is preserved; the following measured checkpoint supersedes those
environment observations.

## Local bootstrap and rendered UI checkpoint — 2026-10-17

Node 26.11.1 and pnpm 12.11.2 are installed in the external task cache; the Node archive
was checked against official SHA-256 sums. Frozen frontend installation and type checking
passed. These paths are selected explicitly without modifying the system toolchain.

The private local-env helper now prepares S3 inputs and six independent role DSNs,
maps Gateway and Knowledge API to their scoped roles, and preserves existing inputs
atomically. It rejects tracked/insecure/foreign env files and conflicts. The selected-file
marker controls initialization; ambient values cannot opt in or bypass local checks.
Native authority validates its source checkout, the exact Compose project/config and
numeric loopback port before connecting. Only missing SCRAM passwords are initialized;
an existing mismatch aborts without rotation. Frozen SQL and fingerprints were not edited.

Serial image pulls and `make dev-start` / `make dev-setup ENV_FILE=.env.dev` completed in
the single owned project. PostgreSQL, Redis, Qdrant and RustFS are ready, with owned
volumes retained. Native initialization created six missing credentials and verified
seven independent connections; repeat initialization preserved all six. Frozen baseline
and all applied successor postconditions through epoch 022 verified successfully.

The explicit live regression checks password/role catalog preservation, rejects a wrong
password and an overridden source root, validates metadata-only Gateway/Knowledge
admission and real permission denials for direct ledger/password reads and owner escalation.
An initial test incorrectly expected direct ledger access; the existing 018 contract
explicitly denies it. The corrected test exercises the defined metadata function and
retains direct ledger denial; no database grant or acceptance contract was relaxed.

Rendered frontend baseline reproduced 50 passes and three stale-test failures. Current
product contracts require Apply filters/Enter and Workbench before Dashboard. Tests now
exercise both submit paths, verify edits leave applied results visible, and retain the full
navigation order. The complete suite passed 53/53 with one worker on Chromium using
fixture API responses. Browser plugin not available; repository Playwright was used.
Desktop and mobile captures were inspected and live outside the repository. This proves
rendered fixture journeys only; it does not establish real-service acceptance.

Next: finish the bootstrap source/review gates, build current native services with the external one-job target,
and prepare source-locked Runtime/Worker artifacts using existing builder capacity.
Measure real POST/Redis regressions and the original Assistant, Knowledge and
Local Node journeys after the required services are admitted. Compilation, direct
provider probes and preflight remain distinct from final acceptance.

Checkpoint `6ae194e` records the local bootstrap and rendered-UI evidence. The
current Gateway binary then built successfully with one host Cargo job; its
configuration projection and temporary loopback liveness passed. The resource-lock
runner stopped the process on command completion; no persistent Gateway service or
readiness acceptance is claimed. Knowledge process compilation is the next serial
native build. Runtime source was fetched from the official upstream at the locked
commit/tree, verified against overlay preimages and composed outside the checkout;
the architecture boundary gate passes with these explicit inputs. A direct gate
without composed inputs correctly failed on the partial overlay's missing dependency.

The next builder change removes automatic creation entirely and adds explicit
existing local docker-driver selection with context/endpoint and actual shared-daemon
capacity validation. Current daemon evidence is 4,107,141,120 bytes and four CPUs;
the selected ceiling will be 4 GiB/four CPUs, one Cargo job, serial builds and host
memory guard. This is a shared daemon limit, not an isolated builder limit. The
Runtime/Worker source lock still contains no admitted image; build, recovery tests,
image pair identity and live journeys are open.
