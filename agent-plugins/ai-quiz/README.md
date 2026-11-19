# ai-quiz compiled plugin 1.1.0

This package owns the quiz domain. `native/` contains deterministic scoring,
question validation, execution-scoped PostgreSQL generation persistence, nine
quiz HTTP routes, frozen-share attempts and source/audience guards. Foundation
provides generic database/rate/identity primitives; Worker only registers and
invokes this compiled plugin through the existing governed execution machinery.
The compiled implementation ships in the reviewed immutable OCI release. The
USTAR package binds that exact implementation; it never delivers native or WASM
code to execute. No executable-plugin ADR or WASM loader is introduced.

`generate_quiz` keeps its ID, input schema hash, question validation, scoring,
source-derived scope, SQL ownership, advisory-lock idempotency and saved receipt
behavior. There is no model call in persistence. Host-verified execution context
must carry the current plugin activation, archive digest and approved publisher;
the plugin also checks those bindings against the durable execution descriptor.
A leftover built-in executor/untagged execution cannot write a quiz through it. The execution
context carries the exact dataset set from the verified Runtime snapshot, serialized
in the durable resource binding and copied by Worker into the plugin. Missing,
malformed, duplicate or wildcard grants fail closed; tool arguments cannot supply
or replace them. Before insert or receipt replay the plugin compares the set and
owner/revision against the locked execution row. An explicitly empty snapshot grant
means zero dataset provenance/access for a generic quiz, never tenant-wide access.
Public quiz reads omit correct answers, explanations and arbitrary option fields.
Question keys remain private until grading. Shared/internal source permission and
attempt-token rules remain in force; moving code grants no tenant permissions.

The package descriptor lives at `native/capability.json`. Host registration must
include it as the compiled plugin blueprint, then advertise/dispatch it only from
an admitted signed activation. Package skills and agents remain inert Markdown
under the current pack installer; their presence is not a claim that prompts were
loaded into the kernel. The existing plugin lifecycle and signed per-turn ceiling
still control discovery and calls.

Generate an unsigned candidate locally (new external file only):

```text
node agent-plugins/ai-quiz/package.mjs /release/new-ai-quiz-1.1.0.tar
```

A release owner signs those exact bytes with the reviewed minisign key, in the
supported raw-message Ed profile (`minisign -l`). Store the detached signature in
the deployment trust policy v2 alongside the existing exact archive, Worker and
compiled-descriptor digest pins. The installer verifies primary and trusted-comment
signatures with its separately pinned publisher key. It rejects unsigned packs,
unknown/wrong/revoked publishers, altered bytes and unsupported prehash algorithms.
The manifest publisher ID is `agentforge-maintainers`; it grants no trust without
a separately reviewed key pin and explicit tenant admission. No production key or
trusted-publisher enrollment is supplied by source fixtures.

Policy v2 explicitly maps tenant IDs to approved publisher IDs; there is no `*`
or implicit tenant fallback. Pack startup/dispatch must invoke the host's tenant
admission gate as well as its existing descriptor/lease/fence checks. Disabling a
pack retains the managed-ID tombstone. New policy/activation generations bind
publisher signatures and tenant policy through the existing trust SHA and catalog
handshake; old activation paths do not become a new deployment automatically.
Immutable archive identity records retain their original id/version/digest format,
so adding signatures never overwrites a previously pinned artifact/version.

Source tests cover scoring/Unicode/pending answers, answer redaction, submission
idempotency and owner/tenant conflicts, plugin execution bindings and publisher
signature/tenant-policy rejection. PostgreSQL persistence, a governed full quiz
run, tenant lifecycle/activation, sharing and rollback still require their real
service receipts. Neither source tests nor a signed archive establish those gates.
