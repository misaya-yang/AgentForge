# Fixed successor 028 → 029: Gateway worker leases and gated provider Batch

The canonical **source package is activated at epoch 029** from the reviewed activation-b
bundle. This is source activation, not a main database migration or service deployment.
The primary measured the real isolated clone `schema_successor_verify_upgrade029b`, including
strict fingerprints, exact ACL and `post_migration_noop=true`; the main application database
was last reported at 025 and was not touched by this integration. Batch submission remains
disabled: `gateway.task_batch_contract` was required to remain empty.

The measurement runner requires the exact complete 028 ledger prefix and cannot skip
026–028 or infer database state from the source epoch. Frozen ≤028 SQL, references, receipts
and predecessor archives remain byte-identical. The active manifest and distribution lock
extend the actual archived 028 package.

## Fixed inputs

The factory in `platform-migration-authority::source_tools::next_successor_029` accepts only
these actual predecessor identities:

| Artifact | SHA-256 |
| --- | --- |
| `database/authority/package-authority-lock.json` | `1d45dc6faae144ab72cbb64eaa4b4eb849fc4c649acbdd74ad9e2921aed38f01` |
| epoch-028 `manifest.yml` | `988f445e7faf70e604938d98eeaf798c1ac99a242c0c22ded6c478108e81fab9` |
| `028_verification.json` | `a0ae5dee780ca0fc429b8523502bd8c1f337f5e5d3c08d65ad33c54070c8372a` |
| `SUCCESSOR_028_MEASUREMENT.json` | `c2c8e2b2552480a0c8fae3a8558f8133a127bde3f52e73f2daa5718039745851` |
| `SUCCESSOR_028_ACTIVATION.json` | `2d045bb350077ed94e0027527ca5af3e7f4dddaa0f0b6a68f83eeb8f30a5ee1b` |
| measured 028 source-plan identity, from its actual activation receipt | `404e6f581fa90974d85d8872dcb9dc13187a69e8a1bd2d2e70a7a422364fc55c` |

It recursively verifies activated 028 and the existing distribution before making a candidate.
No caller-supplied SQL, hash, epoch, fingerprint or predecessor override is supported.

| Proposal, in dependency order | Actual original SHA-256 |
| --- | --- |
| [Gateway singleton worker leases](proposals/gateway-singleton-worker-leases.sql) | `1f45d0020ecf7ef80aab5c4967972fe2955f4021016dcadf3d3a124eeef9dbf7` |
| [Provider Batch reconciliation](proposals/generic-task-batch-reconciliation.sql) | `e34b2704f094d2a5bdf15c4646b87e8eada38085737e9fb7a5dd22c8b834ba4e` |

The generated [029 bundle](proposals/gateway-workers-and-batch-029.sql) has SHA-256
`8652f2b46e760a6c37947f5e56540219037a0b90eb456a73d438fc7f9ce96745`.
The factory independently reconstructs and compares these bytes every time. It preserves
both complete proposal bodies except one exact transformation: the worker proposal's one
terminal hardcoded Gateway `GRANT EXECUTE` is replaced with a fixed `DO` block deriving
`prefix || 'gateway'` from `current_user`, quoting the resulting role with `%I`. Missing,
duplicate, nonterminal or altered grant anchors are rejected. No unrelated 027 price-book
transformation is carried into this factory.

## Transaction, ownership and ACL

One transactional change, `platform_upgrade_gateway_workers_batch_029`, adds six tables and
six typed functions. The owner must match the native canonical-prefix contract, own all three
application schemas, and have no LOGIN, SUPERUSER, CREATEDB, CREATEROLE, REPLICATION or BYPASSRLS.
Existing application data and all epoch ≤028 source/reference/measurement bytes are retained.

Only Gateway receives these new privileges:

- Worker leases: execute exactly acquire/renew/assert/release functions; no table DML.
- Batch inputs and activation contract: SELECT only.
- Provider intents and reconciliation audit: SELECT/INSERT; provider intents additionally
  permit UPDATE only on `provider_job_id,confirmed_at`.
- Quota holds: SELECT/INSERT, with UPDATE only on `state,settled_at,released_at`.

The binding-trigger functions have owner EXECUTE only. Runtime, Capability Worker, Knowledge
and PUBLIC receive no new object access. Input-grant INSERT and marker INSERT are deliberately
not granted to Gateway. This successor does not invent an upload authority or activate Batch.

Measurement requires the entire prior raw ACL set plus **exactly 38** new owner/table/column/
function lines. Extra, missing or duplicate lines are refused, including PUBLIC EXECUTE,
lease-table access, arbitrary provider-intent UPDATE or new actor access. Compatibility-ledger
metadata remains exact; extension/reference-data fingerprints must remain unchanged, while the
structural and ACL fingerprints must change. The reference is reconstructed from actual
measured raw fingerprints and ACL lines, not inferred from desired SQL.

Postconditions verify canonical ownership, all typed functions and tables, four SECURITY
DEFINER lease functions with `search_path=pg_catalog`, two invoker binding functions with
`search_path=pg_catalog, pg_temp`, enabled trigger bindings, measured input count, retained
028 features, and all four validated Batch CHECK replacements. The proposal's exact token/
request branches remain, and only their narrower predecessor checks are dropped after the
new named checks validate.

The Batch activation table must be **empty**. The SQL contains only a commented marker example.
Actual schema, role-grant and price-contract acceptance is a separate operator gate, before
any separately authorized native-authority marker write. That later activation must preserve
frozen reference-data contracts; this factory never inserts it, during source activation or
application startup.

## CLI and primary-owned verification

`schema-admission-successor-029` supports `validate`, `prepare`, `verify-preview`, `measure`,
`verify-measured`, `bundle`, and `verify-activated`, following the existing authority CLI.
Outputs must be new external directories, disjoint from source and preview. No overwrite or
symlink-based source substitution is accepted. Prepare copies the exact predecessor database
inputs and adds only the candidate SQL/manifest/marker in the external preview; it changes no
canonical source. The preview marker explicitly leaves replay/no-op evidence null until it
has actually been measured.

Only `measure` accepts database arguments, and it requires a named
`schema_successor_verify_<approved suffix>` database and explicit
`--run-approved-isolated-measurements`. Existing `measure_single_step` remains unchanged: it
requires development source mode, loopback migrator DSN, exact safe role/ledger ACL state,
advisory lock and actual 028 applied prefix; applies exactly 029 through the owner boundary;
measures raw fingerprints; verifies the resulting state; and proves replay is a no-op.
The verified reference, measurement receipt and source-plan identity bind one another.
Unmeasured/fabricated receipts cannot produce an activation review bundle.

The activated source includes the reviewed SQL, reference, measurement/activation receipts,
canonical manifest and package lock, plus exact archived 028 manifest/lock. The compiled
release lock is `b8580e5d3ee6d8fa25966e9857d5372082e664788616db91cac2f678fb10f87b`;
the exact 028 lock remains N-1 and the exact 027–021 proof packages remain available only
through their unchanged recursive ancestry checks. No arbitrary old lock is admitted.

The 029 verifier, generic source checker and role-probe source validation dispatch through
the complete measured chain. Native application admission requires epoch 029 with all 29
exact checksums; historical compiled contracts retain their original windows. Runtime and
Capability Worker `schema_admission_expected.json` are the same compiled native export.
A database below 029 remains refused by these current binaries until separately authorized
native-authority migration and actual service-role acceptance complete.

Reviewed activation identities:

| Artifact | SHA-256 |
| --- | --- |
| 029 source plan | `fac120210f2b72e4df1050e167864d1be986b5643ee19d9a1499a3a889ae2239` |
| 029 active manifest | `2780972d37314780d96012a8bdb1805b4e3a50118b8d61956ce645c10f4832fa` |
| `029_verification.json` | `358f374d5bbe79a946d2aba860fbe07c6b2e87df3775d01d9f4d408c50fa07a0` |
| `SUCCESSOR_029_MEASUREMENT.json` | `d8e766e1fa1141ffab3d8f13ab692d4b475a9b3dd0d78688025cc011d34a47b9` |
| `SUCCESSOR_029_ACTIVATION.json` | `847086885f59742d1f68ccd12c91cb93c88b20a8ac512db13ec9f785e8c0a430` |

The external review bundle is retained at
`~/.cache/agentforge-upgrade/authority029-artifacts/activation-b/ACTIVATION_029_REVIEW.json`.
Source activation verification and controlled native test evidence are retained in
`~/.cache/agentforge-upgrade/authority-service-completion/activation029/`.
No main database or application was started/migrated for this source integration.
Actual-role lease contention, provider-Batch contract acceptance and live service startup
are separate acceptance gates; source checks do not replace them.

Source tests cover real fixed predecessor/proposal pins, exact deterministic grant replacement,
transaction/owner/empty-marker checks, tampering with every input, early candidate files,
external preview/no overwrite, unmeasured and fabricated evidence, exact minimum ACL and
negative extra/missing/duplicate privileges, metadata drift, and symlink substitution. They
are source-contract tests, not actual-role lease contention or provider-Batch acceptance.

## Independent delivery gate and source identification

The pre-integration executed linter report rejected029; that historical evidence
is retained unchanged in the activation029 cache. The subsequent source-owned
schema integration adds exact SQL/complete-ChangeSpec recognition for its three
safe quoted grant bodies, four already validated monotonic CHECK widenings and
two alterations of newly created empty tables. Any SQL/spec/precondition
byte change loses recognition. General dynamic SQL, populated-table NOT NULL/
UNIQUE, mixed expand/contract, permissions and transaction rules remain intact.
See [the service-successor source plan](canonical-schema-service-successors-source-plan.md).

The source changes and new negative tests require primary's controlled rerun;
this writer did not run Cargo or claim a new executed lint pass. N-1 remains
`not_run`, `release_eligible=false`, and no source recognition substitutes for a
real old-application compatibility driver. No frozen029 input or receipt was
rewritten. Source activation remains distinct from main DB/service acceptance.
