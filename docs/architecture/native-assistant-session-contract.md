# Source-owned native assistant sessions and history

Five existing operations share an exportable declaration with their real handlers:
GET/POST `/api/v1/assistant/sessions`, GET/DELETE `/api/v1/assistant/sessions/{session_id}`,
and GET `/api/v1/assistant/sessions/{session_id}/history`.
Web history and the hosted CLI history/resume client consume this assistant namespace.
Generic `/api/v1/sessions` is a separate API and is not declared by this increment.

## Production ownership

`native_sessions_contract.rs` supplies route paths, limits/defaults, the actual byte-body
create parser and DTO, detail/create response DTO, history message DTO and envelope
builder. The list keeps its existing SQLx row DTO and timestamp serializer; its closed
public fields match the shared session shape. Metadata stays unconstrained JSON, including
historical non-object values. History identity remains skipped during serialization.
The exporter embeds current handler hashes as well as declaration hashes. The independent
common-native manifest checks mounted-router and bootstrap evidence. Neither published
OpenAPI nor Python/common ownership changes; publication is not an exporter schema input.

- Account JWT is required; API keys are rejected. Native auth requirements stay explicit
  even where publication omits them. Errors retain native detail-string responses,
  including the distinct history 503 source-check detail object.
- List defaults to 50, maximum 200; history defaults to 100, maximum 500. Invalid,
  duplicate or non-integer limit values fail through existing Serde query parsing.
- List total is returned row count, not a global count; message_count remains zero.
  List metadata retains its four-key projection. Item reads count legacy object messages.
- Create accepts empty/null/omitted body, ignores unknown fields, and accepts metadata
  objects or null. Pinned Serde also accepts a one-element sequence whose sole value is
  metadata (object or null); zero/two-element sequences and other item types fail.
  The native declaration retains this actual behavior even if Python differs. Existing byte parsing is not gated on Content-Type. Creation and
  single-kernel assignment commit atomically; ambiguous acknowledgement is not safe retry.
- Item/history IDs are arbitrary strings; tenant/user and deletion checks hide other owners.
  Expired sessions can have TTL renewed. History rechecks source visibility and preserves
  existing Runtime, legacy and image projection/total semantics. Dynamic metadata remains open.
- Create timestamps intentionally omit UTC offset; later reads include it. No new format
  restriction is declared. Host/extractor errors beyond bounded schemas stay undeclared.

## Assistant DELETE declaration and bounded verification (2026-10-13)

DELETE shares `ITEM` and `delete_response` with the mounted handler. Its closed
success envelope is `{session_id: string, status: "deleted"}`. There is no consumed
request body or query field, UUID restriction, or invented 422 response. Existing
account JWT-only authentication and detail-string 401/503 responses are retained.
The concrete authenticate path has no 403 outcome; DELETE does not declare one.
Missing/deleted/foreign ownership maps to 404. Cleanup and barrier-readback errors
map to 500, including collaborator 503 errors; absent collaborators, false cleanup,
or an unconfirmed barrier map to 503. Other host/extractor errors remain undeclared.

The production host injects the same durable owner as generic SessionsApi. Success
requires committed logical deletion plus a confirmed permanent barrier, and may
leave asynchronous SDK cleanup pending. Required Runtime cleanup accepts only an
exact-session deleted/not_found acknowledgement or HTTP 404. Unknown or malformed
acknowledgements fail closed. A repeated completed DELETE returns 404. A failure
can follow committed deletion; it is not proof of rollback or immediate physical
erasure. See [durable cleanup](sdk-session-cleanup.md#native-assistant-delete-compatibility).

The common-native manifest checks the mounted DELETE, shared envelope, production
injection, durable owner and strict decoder. The existing compiled-source bindings
also cover their actual source files. Publication and Python ownership are unchanged;
no schema is copied from the published document to conceal incompatibility.

The isolated candidate was freshly compiled and its exact source bindings verified.
Five exporter tests passed: route/auth declaration, create parser/limits, existing
serializer envelopes, the new DELETE envelope/status schema, and aggregate incomplete
coverage. Thirteen Gateway tests passed: eight deletion collaborator outcomes, three
strict Runtime acknowledgement cases, and two retained serializer/query-extractor cases.
These are controlled in-process tests, not live authentication or network/SQL acceptance.

The actual independent export measured **38 paths / 58 operations**, leaving **394
of 452** publication operations absent. Two native producer processes and independent
owning-Python exports were deterministic. Only assistant DELETE was added to the prior
operation map; other operations, components and owning-Python schema were unchanged.
Fresh producer ELF, compiler inputs, handler/declaration and owner source hashes were
bound. The actual create-body schema passed meta-validation and seven accepted/five
rejected fixtures; the DELETE success schema passed meta-validation and two accepted/
five rejected fixtures, with exact statuses, JWT security and no request body checked.
188 Python wrapper/profile/owner-composition/response diagnostics passed. Two named
`create_app`-dependent tests were explicitly deselected; no unexpected skips occurred.

The required full-native comparison genuinely **FAILED with 456 violations**, versus
455 before this declaration. The missing DELETE method diagnostic was replaced by two
real differences: its explicit native JWT security requirement and absent 422 response.
The published baseline and native behavior were not weakened to conceal these findings.
Reduced absent-operation count is measured declaration coverage, not full compatibility.
Existing 23 Gateway compiler warnings and two pytest configuration warnings remain.

No live authentication, PostgreSQL concurrency, Runtime transport, SDK erasure,
provider, browser, deployment or release acceptance follows. Compilation and tests
used the existing locked offline toolchain/dependencies, one Cargo job, no debug or
incremental output, external snapshots and unchanged disk/memory guards. Each phase,
actual source copy and terminal was independently reviewed. Full Python application
composition remains unverified.

Declaration evidence identity:

- Tested seven-file source manifest:
  `77cad381d4c11eccc24db1989fc44fdbfddc605a4585de2efd0ee1028ee12c4e`
- Five exporter-test result:
  `2801bd3d2884f1b3820779f84cfcec74178743a2471990536ce601cfe292edbc`
- Thirteen Gateway-test result:
  `570900a85ecbe22e088ae86ad7df77ed3f4e34029a3a6fbf0c32df428ac40dd1`
- Fresh source-bound producer build result:
  `ba7419f37f01fc7483da30672f6e2af8f76bec1360bd23a5fbb594b746bc88bd`
- Actual generated native schema:
  `5b47b0da69e723c3f5cf70b7342f0113443c95ff5a20f47d3faee4f9cb0e249a`
- Actual coverage report:
  `dd5733b8be8eed4cd618baa79123dfcc318828ff5df8cfc185afaa3b9a26aa8a`
- Failed full-native comparison report:
  `43fc600c2f009cdf9a5b80886db3d563f33113295e71b331bc83c4b4a745f49c`
- 188-test Python result:
  `0f7fc5406d82b4742d3e26876ce74a9b8db63c0f8baa044d049cc61cc7633480`

This verification amendment changes documentation only after execution. All six
non-document delivery files retain their exact tested bytes; the tested snapshots,
source manifests and original preimages remain preserved for root integration.

## Historical four-operation bounded verification (2026-10-13)

The four-operation independent native export declared **38 paths / 57 operations**,
leaving **395 of 452** publication operations absent. Two native producer processes
and independent owning-Python hash-seed exports were deterministic. Current
compiled producer, native handlers, schemas and Python owner source hashes were
verified against the exact isolated candidate. Python/common ownership and the
published `sdk/openapi.json` remain unchanged; generated native schema/coverage
artifacts stay outside the repository and must not replace publication.

Completed controlled checks:

- Four exact Rust exporter tests, including the preserved Serde sequence-body
  parser, declaration shape and existing aggregate incomplete-coverage assertion
- Two actual Gateway serializer/query-extractor tests, with the candidate Gateway
  library compiled; no HTTP listener or database interaction
- Offline native producer build; its ELF, dep-info and compiler inputs were bound
- 174 Python wrapper/profile/owner-composition/response diagnostic tests passed;
  two create_app-dependent tests were explicitly deselected, not passed
- Actual exported create-body schema passed JSON Schema meta-validation plus
  seven accepted and five rejected instance fixtures

Existing 23 Gateway compiler warnings and two pytest async-configuration warnings
were retained. These results do not establish warning-clean, full Python app
composition, live authentication, SQL, provider, browser, deployment or release
acceptance. The runner used the existing Rust 1.99 toolchain, offline single-job
external target, no-debug/no-incremental settings, unchanged disk/memory guards
and independently reviewed source/proof copies and terminal results.

The required full-native compatibility gate genuinely **FAILED with 455
violations**, compared with 449 before this increment, using the same published
baseline. Three previously missing paths are now declared. This reveals four
actual JWT security differences, three native 422 detail-string differences, the
item GET's absent 422 declaration, and the still-missing assistant DELETE method.
These nine added diagnostics replace three missing-path diagnostics. They are
retained truthfully; no authentication/error behavior or schema constraint was
weakened to produce a passing gate. Bounded declarations are not full native
compatibility or end-to-end product completion.

Evidence identity:

- Tested nine-file source manifest:
  `fa162b337f58ad95be10acd1d4cb072bff07dc91bbb9989853da92adc7760db7`
- Actual generated native schema:
  `396dc101577a5ca140321aac07b0bd0dbe8946cbb09d90328c2832bf608ba7d9`
- Actual coverage report:
  `bb032e6878d1defe4ab38f6e2e6aee9ab1a7b7a16c1f188a82d2ce9981c1e28b`
- Failed full-native comparison report:
  `646a4537f427738f166744e7fcf979331974acf21e11b2cffbce363ab0a8fe4f`

This historical verification section records a documentation-only amendment after
that execution. At that point all eight non-document candidate files retained their
exact tested bytes. The subsequent DELETE declaration above changes source and
requires fresh validation; these earlier receipts do not certify its new bytes.

<a id="post-declaration-delete-reconciliation"></a>

## Historical DELETE implementation reconciliation (before its declaration)

Native assistant DELETE is now implemented on the shared ITEM route, using the
existing authenticated scope and durable generic-session cleanup owner. Its
success response requires the durable deletion barrier; it does not establish
physical erasure or execution cessation. Both Gateway control clients now accept
only the producer's `deleted`/`not_found` success vocabulary, retaining idempotent
HTTP 404 while rejecting malformed or unknown acknowledgement statuses.

The combined implementation was compiled and validated in a separate guarded,
exact-source window on 2026-10-13:

- 13 actual Gateway tests passed: eight DELETE response-adapter tests, three
  strict acknowledgement decoder tests and two retained serializer/query tests.
  Ownership collaborators are deterministic; this is not authenticated DELETE
  routing or real tenant/SQL race acceptance.
- Four actual native producer tests passed, followed by a source-bound offline
  producer build. Existing 23 integrations and 23 Gateway warnings were retained.
- Actual producer/owning-Python exports were independently deterministic. Relative
  to the preceding declaration result, only the embedded `session_history.rs`
  source hash changed; all operation/schema content remained unchanged.
- Actual create-body schema meta-validation and seven accepted/five rejected
  fixtures passed. The runtime parser was not rerun in this fixture phase.
- 174 Python wrapper/profile/composition/response checks passed. Two named
  `create_app`-dependent tests were explicitly deselected; two existing pytest
  async-configuration warnings were retained. No unexpected skips occurred.

**Historical implementation-only coverage, before the declaration above:** that
independent export contained four assistant operations, 38 paths / 57 operations
overall. It did not declare assistant DELETE. The genuine required full-native comparison again
**FAILED with 455 violations**, with its complete diagnostic list identical to the
preceding result, including the missing DELETE declaration. No requirements or
baseline were weakened, and published `sdk/openapi.json` remains unchanged.

Combined evidence identity:

- Tested ten-file candidate manifest:
  `0b30f4b9d7d966babafc8f52c91f006737ef1ddb7c6d0de757143315ebdbce4c`
- Gateway 13-test result:
  `053bc3fbba48ba74986b264dd94607ddf965446155482d9765aa5e86b040ce7b`
- Source-bound producer build result:
  `f46eee0d3717d925e7f8bf88b850bdae20fab4b5bf5ea69e20e4948e88148d9b`
- Actual combined native schema:
  `a594447045d8cdfca46ca8d5a4c2bc10254bf4497bfa23585f9173cb6b041b3a`
- Coverage report:
  `578f71a2da698ee9cd8351910868040b0fc3d46dd67011a45855054fe2536e4e`
- Failed full-native comparison report:
  `26b7c5d01d888c7badb245eddf0c3730da6440ec94da59553a44ec3f74895f05`
- Final 174-test result:
  `087d00791deb1b0c0de452d2213e2de1bdf02dcb6303fec3305c6c33ee3746e4`

This historical section and the cleanup-owner evidence update were documentation-only
changes following that execution. Its eight non-document delivery files retained
their exact tested bytes then; they do not certify the subsequent declaration.
No live authentication, database, Runtime transport,
provider, browser, deployment, physical-erasure or release acceptance follows.
