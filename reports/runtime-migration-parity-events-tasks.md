# Runtime event and task lifecycle test migration

Status: authored for root's serialized compilation; no Cargo or live infrastructure execution in this lane. Ignored Redis tests are runnable opt-in assertions, not passing evidence. No whole source file is claimed retired by this batch.

## Actual production Redis event lifecycle

Target `runtime_migration_parity_events`, test `publish_envelope_duplicate_poison_retry_and_dlq_lifecycle` uses the real UsageEventBus publisher and supervised consumer. Opt-in requires `AI_PLATFORM_RUNTIME_EVENT_TEST_REDIS_URL`, loopback, nonzero and initially empty database. Cleanup deletes only reserved fixture keys, never flushes a database.

Original assertions mapped:
- `tests/events/test_envelope.py::test_envelope_default_factories_populate_id_and_timestamp`: actual emitted UUID version4 and UTC RFC3339 time.
- `test_envelope_round_trip_via_parse_envelope`: real consumer preserves event identity/type/producer/tenant/request/schema/trace, input100/output200, metadata and timestamp. Native serde values replace Python model-class identity checks.
- `test_parse_envelope_rejects_unknown_event_type` and `test_parse_envelope_rejects_malformed_json`: unknown, invalid JSON and absent discriminator never reach handler; actual consumer acknowledges and dead-letters them. Private Python exception wording is not asserted; rejected event discriminator remains in raw DLQ payload.
- `tests/events/test_bus_publish.py::test_publish_writes_envelope_to_correct_stream`: actual registered stream and stored Redis ID/envelope checked. Native public publisher returns unit, so a Python return-value assertion is replaced by checking the actual Redis entry ID.
- `test_close_is_idempotent`: close twice succeeds.
- `tests/events/test_consumer_idempotency.py::test_duplicate_event_id_calls_handler_once`: duplicate identical envelope invokes once and pending count is zero.
- `tests/events/test_consumer_dlq.py::test_handler_failures_route_to_dlq`: failures release claims; three consumer restarts reclaim the same pending ID; terminal DLQ retains original ID, attempts>=3 and bounded error category; pending is zero. XCLAIM changes only fixture idle age to exercise real production XAUTOCLAIM without waiting60 seconds. Rust bounded error category replaces Python exception class disclosure.
- `tests/events/test_bus_publish_consume_e2e.py::test_publish_consume_round_trip`: real publish-consume handler payload, identity, trace and ACK are asserted. Payload model class check is replaced by concrete UsageRecord input plus parsed output field assertions.

Residual source behavior requiring explicit retirement decision: generic publish of an arbitrary unknown event type is impossible through the typed native UsageRecord publisher; configurable MAXLEN3 source constructor has no equivalent native parameter (native fixed50000). Do not mark complete source bus_publish suite equivalent. The producer name is native usage-recorder, and group is the actual production group rather than a configurable Python fixture group.

## Memory task authority

Target `runtime_migration_parity_task_lifecycle` is ordinary, non-network execution.
- `tests/services/task/test_task_transitions.py::test_all_edges` -> `all_twenty_five_status_edges_preserve_source_timestamps_and_rejected_rows`: full25-edge matrix, allowed transitions and timestamps, unchanged rejected rows, direct CAS conflict.
- `test_memory_compare_rejects_identity_replacement` -> `every_immutable_identity_field_rejects_compare_and_preserves_row`: all5 immutable identifiers, no changed row.
- `test_memory_all_boundaries_are_detached_and_patch_preserves_current_fields` -> `detached_save_get_list_and_lifecycle_patch_keep_concurrent_metadata`: save/get/list detached nested values; stale lifecycle update preserves concurrent metadata and progress; candidate mutation cannot mutate stored result; repeat CAS loses.

Existing foundation `task_manager_tests.rs` contains related but narrower lifecycle tests, inspected before adding this matrix. Remaining source task-transition race/worker/callback cases are not retired by these3 tests; follow-on inspection will map or port them individually.

### Cancellation defect uncovered and fixed
The source tests `test_cancel_reloads_after_claim_and_preserves_repeat_timestamp`, `test_cancel_never_adopts_replacement_identity_even_when_already_cancelled`, and `test_cancel_bounded_when_broken_adapter_reports_perpetual_conflict` now map to `cancellation_reloads_claim_but_never_adopts_replacement_identity` and `cancellation_conflict_retry_is_bounded_to_three`. Minimal native cancel fix retains initial identity before accepting reloaded status and caps CAS attempts at3. Held-CAS fixture deterministically introduces the race; broken adapter errors after a fourth attempt so a regression fails rather than hanging. No production worker architecture changed.

## Shared trace redaction
Target `runtime_migration_parity_redaction` ports all9 parameter rows of `tests/services/assistant/test_redaction.py::test_redact_trace_text_covers_union_of_both_original_pattern_sets`, explicit/no-limit truncation, empty input and safe exception-description preservation. Python arbitrary-object/None conversion is language-specific: native API requires a string, so no None coercion API is claimed. Source suite is eligible for retirement after tests pass and this typed-input rationale is accepted; no Python pattern assertion is dropped.

## Newly identified residual
Legacy/test TaskWorker currently observes only Cancelled status using its constructor storage. Source cancellation ownership suites additionally require identity/other terminal status loss and manager-claim authority. Durable production worker has a separate ledger path; that does not prove source legacy worker equivalence. These suites remain unretired pending bounded corrective work.

### Legacy worker correction and additional mapping
`test_task_cancellation_ownership.py::test_observed_authority_loss_stops_owned_work_without_callback` loss cases cancel/identity/completed now have actual owned-future cancellation and unchanged-authority-row assertions in `owned_invocation_stops_on_cancel_identity_or_terminal_loss_using_manager_authority`; read_error maps to `uncertain_authority_read_stops_dispatch_without_overwriting_processing`. The former also maps `test_authority_observer_uses_claim_storage_not_legacy_constructor_storage` by passing a deliberately different empty constructor store. Callback-count assertions remain residual (these fixtures use no callback URL); no full-suite retirement claim.

Minimal correction: legacy worker reads manager claim authority, validates identity/Processing before invocation, observes all ownership loss, and commits completion/fallback against the captured claim using existing CAS rather than reloading/adopting another task identity. Durable production path is unchanged. Full-read observation remains functionally safe but does not implement the source optimized six-field authority projection; `test_task_authority_projection.py` remains a distinct residual, including malformed unrelated payload isolation.

## Runtime timing schema
`runtime_migration_parity_timing` ports `test_timing.py` fake-clock additive identity, missing-as-None, first-write-wins, real-clock <=5ms identity and all10 client-residual boundary rows. Native test passes explicit monotonic stamps instead of injecting a Python clock callable. Real-clock arithmetic needs no intentional sleeps. This is partial suite coverage: controlled-delay provider integration remains unported; source components() six-decimal rendering is performed by native TimingEvidence logging rather than ModelPlaneTiming::components and has not been asserted here. Do not retire the entire timing source suite.

## Model tool stream scope batch
Target `runtime_migration_parity_model_tools` adds5 ordinary tests. Exact mappings:
- `test_model_tool_projection.py::test_named_tool_output_must_match_call_namespace` -> `named_output_must_match_original_call_namespace`.
- `test_native_tool_validator_rejects_wrong_namespace_and_unadvertised_search` -> `native_stream_rejects_wrong_namespace_unadvertised_tools_and_search`, all4 input rows and exact scope error.
- `test_chat_split_tool_name_waits_for_provider_identity` -> `split_tool_name_waits_for_provider_call_identity_and_rejects_duplicate_id`, checks empty premature output, stable final identity/name, and duplicate-ID error.
- `test_tool_argument_limit_counts_utf8_bytes` -> same native test name; exact byte overflow and error. Original private projector buffer empty assertion remains unverified; no whole-test retirement claim yet.
- `test_model_plane_unsupported_history.py` accepted-rejection/fake-success cases -> `unadvertised_call_allows_only_exact_runtime_rejection_receipt` tests shared validator for tools enabled/disabled. This is partial: provider body replay and tools-disabled distinct error classification remain unverified, so source file must stay.

No providers, DBs or sockets used in these model tests.

## Request-id middleware
`runtime_migration_parity_request_context` uses real Axum request_context and task-local bridge layers. `test_request_id_propagation.py` middleware preservation, no-header service UUID, all5 malformed-ID rows, response echo and context reset map to the two named native tests. FastAPI Request.state and Python ContextVar are replaced by Axum RequestContext extension and scoped Tokio task-local. Original ServiceProxy header-precedence/no-context cases remain separate and unretired by this target.

## Safe-fetch negative preflight batch
`runtime_migration_parity_safe_fetch` ports original16 unsafe/IP-parse rows and4 public-address rows, mixed-public/private DNS rejection, empty DNS, resolver error, host exact/subdomain/case/suffix-overlap precedence, non-HTTP schemes and literal loopback/link-local rejection. Actual PinnedImageFetch is exercised with an injected resolver; every case fails before constructing/sending HTTP, with resolver-call counts proving allowlist ordering. Original network-success/stream cap/manual redirect/per-hop revalidation/callback URL cases remain unported here; no positive network assertion is replaced with a weaker negative assertion. Whole `test_safe_fetch.py` remains unretired.

## Session deletion and retained audit references
`runtime_migration_parity_session_tombstone::unreferenced_delete_and_restricted_tombstone_preserve_runtime_audit_chain` ports all3 observable cases in `tests/persistence/test_session_delete_tombstone.py`: unreferenced row is really removed; actual FK RESTRICT triggers deleted-status fallback while retaining audit row; normal get hides tombstoned row. Python mock SQL substring/call counts are replaced with real persisted outcomes through production PgSessionStorage. Ignored opt-in requires an empty loopback database named runtime_parity_* via AI_PLATFORM_RUNTIME_PERSISTENCE_TEST_DATABASE_URL; no shared schema replacement and only its created schema is cleaned after drain. Source is eligible after explicit opt-in verification; ignored status is not passing evidence.

## Verified checkpoint (root-owned execution)
Read root's `gateway-parity74.log` at2026-10-15 14:56UTC: redaction2 passed, task_lifecycle7 passed, timing4 passed. Assignment3 and events1 compiled and remained ignored, not executed. Newer model_tools/request_context/image_history/safe_fetch/session_tombstone targets were not present in that checkpoint and remain pending. This lane did not invoke Cargo or infrastructure.

### Model projection completion batch
Extended model_tools with5 tests covering remaining observable assertions from all10 `test_model_tool_projection.py` functions:
- `test_chat_completions_flattens_namespaces_and_omits_native_search` and `test_native_tool_choice_none_omits_runtime_tools_on_first_call` -> `chat_flattens_namespace_without_native_search_and_native_none_omits_tools`.
- `test_chat_namespace_history_roundtrip_preserves_parallel_calls` -> `namespace_parallel_history_roundtrip_is_detached` (two namespace aliases, three messages, matching parallel call IDs and unmodified history).
- `test_chat_tool_projector_restores_namespace_and_rejects_undeclared_tools` -> `projected_items_restore_namespace_without_private_byte_counters` (restored namespace/name, private counter absent, unadvertised tool refused).
- `test_native_current_tool_subset_keeps_completed_history` -> `current_tool_subset_keeps_completed_history`, both auto/none choices.
- `test_mcp_multiple_text_outputs_replay_on_both_wires` -> `multiple_text_tool_outputs_replay_on_both_wires_and_image_output_fails_closed` (native array preserved, chat concatenation, image output rejection).

Fixture supplies an already-authorized model snapshot with explicit flatten/native-search tool settings and inert reasoning adapter; tests exercise actual provider_body and actual projectors, no mocked wire transformation. Source's private tool_calls buffer empty check after oversized input has no public native accessor; retained observable limit/error assertion covers refusal, and no provider network emission exists in projector. Whole source projection suite is now a retirement candidate after root verification and acceptance of this private-buffer rationale. Unsupported-history remains a separate partial suite.

### Unsupported-history completion
`unadvertised_history_provider_wire_preserves_rejection_and_exact_disabled_error` now covers both original `test_model_plane_unsupported_history.py` functions and their enabled/disabled parameter rows through actual provider_body: rejection receipt preserved, no function tool when disabled, fake success fails with TRANSCRIPT when enabled and TOOLS_NOT_ENABLED when disabled. The latter exposed a native error-code mismatch, corrected narrowly after Gateway owner coordination: native/no-function invalid transcript returns source RUNTIME_TOOLS_NOT_ENABLED_FOR_PHASE; valid rejection receipts and enabled-tools failures retain their existing paths. Both source functions are retirement candidates after root test verification.

### Terminal callback race matrix
Task target now adds `each_terminal_winner_preserves_timestamps_result_and_exact_callback_count`, mapping all4 `test_task_transitions.py::test_terminal_winner_and_callbacks` rows (Completed/Failed × cancellation first/last). Actual TaskManager with injected non-network callback transport proves no losing-terminal callback, exactly one winning callback with original result/error, unchanged started timestamp, final completion stamp and preserved winning row. This eighth task test awaits root verification; earlier seven-test checkpoint remains accurate.

### Duplicate dispatch
Task target now totals10 tests: `duplicate_claims_dispatch_once_and_retain_typed_result` ports `test_worker_duplicate_claims_invoke_once_and_keep_typed_memory_result` with actual memory queue/worker, deterministic held invocation, concurrent duplicate and post-completion duplicate. `terminal_queue_duplicates_never_dispatch_or_mutate` ports all3 `test_worker_never_dispatches_terminal_duplicate` rows. No provider calls; injected dispatch returns a real UnifiedResponse. These three additions after checkpoint74 await root verification.

### Shared envelope parser supplement
Foundation owner added exported typed event_envelope and delegated actual usage_event_bus publish/decode to it. Supplementary pure tests are `event_envelope::tests::typed_payload_defaults_and_unknown_optional_fields_are_source_compatible` and `event_envelope::tests::missing_or_malformed_typed_payload_is_distinct_from_poison_envelope`: payload defaults/null metadata/unknown-field drop, required fields, absent/nonobject payload, fractional token refusal and poison-vs-payload error classification. This supplements, not replaces, ignored actual Redis lifecycle assertions. Await root verification before claiming these additions passed. Generic schema-evolution matrix belongs to Gateway owner's contract target.

## Task uncertain-commit continuation
New independent `runtime_migration_parity_task_uncertainty` target maps4 additional source functions:
- `test_uncertain_claim_never_dispatches` -> `uncertain_claim_never_dispatches_whether_commit_won_or_not`, both commit-before-error rows, no invocation/callback and preserved pending/processing.
- `test_uncertain_completion_only_conditionally_projects_failure` -> `uncertain_completion_projects_failure_only_if_original_claim_still_owns_row`, both rows, exactly one invocation, completed/failed winner, callback0/1 respectively.
- `test_completion_error_after_cancellation_cannot_project_failure` -> same native name with `_or_callback`, verifies cancelled row keeps no result/error and callback absent.
- `test_callback_failure_cannot_trigger_failed_projection_and_runs_outside_lock` -> `failed_callback_runs_after_unlocked_completion_and_never_projects_failure`, injected callback reads completed row under1s deadlock bound then fails; row remains completed. Native transport configured one attempt to match original one mocked _send_callback failure; this test is not a retry-policy assertion.

Fault adapter delegates all normal operations to actual MemoryTaskStorage, injecting uncertain acknowledgment only at original CAS boundaries; worker and manager are production. No production changes in this continuation.

## Observer lifetime and bounded full-row reads (final frozen batch)
`runtime_migration_parity_task_observer` adds three actual worker contracts:
- `test_finished_invocation_cancels_stalled_authority_read_before_commit` -> `finished_invocation_drops_stalled_authority_read_before_commit_and_callback`: a pending storage read is dropped before terminal CAS/callback; invocation completion is not blocked by its pending observer.
- `test_read_cost_bounded_and_no_observer_survives_into_callback` -> `source_poll_budget_and_no_observer_survives_into_callback`: initial ownership check plus bounded periodic reads, and no observer reads while the terminal callback is held.
- `test_cancel_after_claim_before_invoke_never_starts_adapter` -> `cancellation_after_claim_before_invoke_never_starts_adapter`: cancellation between claim and dispatch prevents adapter invocation and callback.

The legacy observer now waits one second before subsequent authority reads. This is an explicit accepted latency/load choice: the current adapter reads the complete row/result, so 250 ms amplified payload reads fourfold. The initial ownership check and terminal compare-and-set remain immediate; the existing cancellation observation bound is 1.6 seconds. There is no public cadence knob to preserve or introduce. The durable worker is unchanged. Root accepted this choice before the final freeze. No claim is made that the optimized authority-only source projection is implemented.

At freeze: this batch has 53 ordinary tests and 2 ignored infrastructure tests, plus the separately reported assignment target's 3 ignored tests. Only the checkpoint74 subset above is confirmed executed here; later targets await root verification. Production and tests are frozen for compile/test/archive, with no further expansion.

### Closure distinctions
- Implemented necessary corrections: immutable cancellation authority including route-authorized expected identity; claim-bound terminal CAS; cancellation observation stops on ownership loss; response request-ID echo; native disabled-tools history error contract; exported image signature append/inflate/replay helpers. No source production caller exists for those image helper exports, so none was invented.
- Known remaining implementation differences: legacy authority observer uses full-row decoding rather than the source authority-only projection; legacy worker claim-error/lane-recovery and shutdown-generation behavior has not been fully reconciled. These remain explicit recovery work, not completed parity.
- Verification gaps: real Redis lifecycle and PostgreSQL tombstone/assignment tests remain opt-in and unrun; positive HTTP safe-fetch/redirect/cap behavior, timing provider integration, proxy header precedence, remaining persistence and unlisted assigned suites are not claimed migrated or retired. Root retains remaining original test references externally for follow-on work.

## Final verification evidence and archive freeze
Read actual root logs at 2026-10-15 15:58 UTC. `gateway98.log` confirms task_lifecycle 10 passed, task_observer 3 passed, task_uncertainty 4 passed, zero failures. `workspace96.log` ends with a successful all-target dev check; it compiles remaining newly authored targets but does not establish their execution. Gateway owner's task authorization target reports 4 ignored, not passed. Earlier checkpoint74 redaction/timing evidence remains as recorded. Reports now reflect these distinctions. All lane files are frozen for ZIP delivery; remaining issues and unported references are follow-on recovery work.
