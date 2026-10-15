# LangGraph tool outcomes across stream and history

Remote LangGraph message/update frames and supported local tool-end projections
preserve tool outcome independently of the overall run outcome. A LangChain
`ToolMessage.status` of `error` (or legacy `failed`) projects to tool status
`error`; a successful tool result projects to `completed`. A graph can handle a
tool error and finish its turn successfully. This does not retry or reexecute a
tool, change provider routing, or authorize side effects.

The Gateway collects terminal tool records even when a start frame was not
observed. Session tool metadata carries `status` and `result`. An empty-string
result is present data; null/missing means no result was observed. Tool-bearing
turns may have empty assistant text and are still persisted without fabricated
text. Textless successful turns without tools follow the retained Python empty-success
history contract below; failure/interruption persistence rules are unchanged.

The Playground preserves this distinction in live reduction, direct LangGraph
messages, wait/history fallback, manual session persistence and session replay.
Existing metadata without a status remains readable with the previous completed
fallback. Explicit existing event-status metadata remains authoritative over a
legacy tool-call object's default status.

## Bounded verification

`tests/fixtures/adapters/langchain_tool_roundtrip.json` was generated using the
installed `@langchain/core` 1.1.49 `AIMessage` and `ToolMessage` constructors and
`toDict()`. No dependency upgrade is required. Eight cases cover success, error,
empty success/error, tool-only runs and a result without a preceding start.

- `tests/adapters/test_langchain_tool_roundtrip.py` exercises the production
  Python adapter, dispatcher, memory session manager and history API with an
  HTTP mock transport, plus local tool-end and absent-result controls.
- Native `langchain_tool_outcomes_survive_stream_and_session_projection` runs
  the production SSE stream and statistics/session projection with the same
  message/update fixture, without a listener.
- `langgraphToolRoundtrip.test.ts` consumes the deterministic backend-generated
  replay fixture; `LANGCHAIN_GATEWAY_RESPONSES` can select a freshly exported
  backend response instead.
- `failoverFeedback.test.ts` additionally runs the actual Playground hook and
  SSE parser through live presentation and manual persistence. Existing routing
  observation tests remain intact.

These are controlled local checks. They do not establish live-provider, browser,
persistent-database or deployed-service acceptance.

## Stream cancellation ownership

The LangGraph adapter owns each remote stream or in-process graph event iterator
it creates. Its public stream closes and joins that iterator before returning
from early close at a yielded chunk or a producer failure. A configured adapter
bulkhead likewise closes its own producer before releasing the concurrency slot.
Cleanup preserves a primary failure or cancellation, and surfaces a close failure
on otherwise normal completion. Iterators without `aclose` remain supported.
This reuses the Gateway's cancellation-shielded cleanup policy; it adds no cleanup
deadline and does not promise termination of an uncooperative provider.

`tests/adapters/test_langgraph_stream_ownership.py` covers controlled HTTP body
closure, in-process generator cleanup, bulkhead slot ordering, error precedence,
and repeated cancellation during owned early-close cleanup. Repeated cancellation
inside an active upstream read can still interrupt the upstream iterator's own
finally block; this change does not shield individual reads. These tests do not
prove upstream remote-run stop or live provider acceptance.


## Session thread creation prerequisite (Python adapter)

On a session-to-thread cache miss, the retained Python LangGraph adapter now
requires a successful thread-creation acknowledgement before caching the mapping
or dispatching either a wait or streaming run. HTTP denial, transport uncertainty,
malformed JSON, contradictory returned identity and embedded thread errors stop
that invocation. Cancellation during creation or conflict readback stops admission without
publishing a mapping. Cancellation during the later best-effort Redis write can
leave the already-confirmed L1 mapping in place, as before. Errors use
fixed messages/status codes rather than logging upstream bodies or exception text.
There is no automatic create retry or model-run fallback after those failures.

The official [Create Thread API](https://docs.langchain.com/langsmith/agent-server-api/threads/create-thread)
and [Get Thread API](https://docs.langchain.com/langsmith/agent-server-api/threads/get-thread),
checked 2026-10-14, specify successful JSON responses with required UUID thread
identity; duplicate creation with `do_nothing` returns the existing thread.
Both fresh and existing acknowledgements must match the requested UUID (equivalent
UUID text forms are accepted). Empty/non-JSON acknowledgements are unconfirmed;
malformed JSON fails at the existing HTTPConnector boundary. For compatibility,
HTTP 409 performs one authenticated GET of that exact ID and proceeds only with
a matching JSON identity; failed or unconfirmed readback stops the invocation.
This readback policy is local recovery logic, not an upstream promise that all
conflicts mean the thread exists or belongs to the caller.
Existing confirmed L1/Redis cache paths and best-effort Redis availability are
unchanged. Historical cached mappings are not retroactively revalidated or migrated;
this change does not establish new tenant isolation or multi-process concurrency
semantics. A caller retry remains explicit and does not prove a prior timed-out
thread creation was undone.

`tests/adapters/test_langgraph_thread_admission.py` uses the production adapter and
HTTPConnector with in-memory httpx transport to cover invoke/stream admission,
creation failures, cancellation, conflict readback, cache publication and successful
reuse. Bounded offline validation passed 88 tests (42 new admission cases and 46
existing stream terminal/ownership cases), with no failures or skips. Both baseline
and candidate scoped Ruff checks passed with zero findings. The 99 existing
`datetime.utcnow()` deprecation warnings remain reported. An initial fixture failure
from ambient SOCKS proxy configuration is retained; clearing proxy variables only
inside the offline test child allowed the unchanged source to run. All 488 snapshot
files and the pinned dependency/tool identities were checked before and after each
gate, using one immutable snapshot and dependency inventory. These Python checks
do not establish native or live framework/provider acceptance.

### Native session admission parity

The native LangGraph adapter now applies the same cache-miss prerequisite before
both wait and streaming runs. It reuses its existing HTTP transport and response
ownership: require a successful JSON acknowledgement with matching UUID identity;
for HTTP 409 only, release the conflict response before one same-authority GET of
the exact requested thread ID. Invalid headers, transport failures, HTTP denials,
malformed/non-JSON/empty bodies, embedded `__error__` and contradictory identities
stop admission with fixed redacted validation errors. They cannot trigger automatic
model dispatch or a create retry. UUID-equivalent text forms remain accepted.

Only confirmed identity reaches L1 publication and the existing best-effort Redis
write. Dropping the future during creation or readback drops its owned response and
permit before any mapping is published; this is local cancellation, not proof that a
remote create was undone. Cancellation after confirmation during Redis publication
can leave confirmed L1 state, as before. Existing/historical cache hits, global
session-key scope and Redis availability behavior are unchanged. This is not a new
tenant isolation or concurrent-creation guarantee.

`migration_adapters/langgraph_thread_tests.rs` drives the production cache/admission
function through one private send-closure seam using actual reqwest requests and
in-memory response bodies. It covers request metadata, exact-ID authenticated 409
readback, failures/no replay, cancellation/permit release and retained cache hits;
real invoke/stream entrypoints also reject invalid authority before transport.
No Redis process, listener, provider or model is used. The current integrations
library passed an offline locked, single-job Cargo check. All eight focused native
production-path tests then passed with no failures or ignored tests; 58 unrelated
crate tests were filtered out. The first test compile caught two test-only enum
pattern mistakes (AdapterError wraps DispatchError); those two assertions were
corrected, with the failed receipt retained and production source unchanged.

The successful current compiler artifacts and dep-info explicitly include both
`migration_knowledge_query/qa.rs` and `qa_stream.rs`, closing the previously pending
QA terminal production assembly check in this crate. This does not execute the QA
provider flow or certify the full Gateway binary. Exact source, toolchain, dependency
and protected-artifact identities were verified before and after each phase with
the 8 GiB disk admission floor intact. Compiler warnings outside the changed
LangGraph source remain recorded; no unrelated warning cleanup is included.
Whole Gateway, live LangGraph/Redis/provider behavior and broader regression remain
separate acceptance boundaries. The existing response body read remains unbounded;
this admission fix makes no response-size-hardening claim.


## Python LangGraph Unicode stream continuity

Remote LangGraph streaming now keeps UTF-8 decoder state across HTTP chunks.
Split multibyte text, tool arguments and tool results survive the Python adapter,
Gateway stream and session-history projection. This reuses the existing Python
adapter and standard library; it adds no dependency or execution path. Malformed
bytes retain the prior ignore policy. Existing end/[DONE], ordinary EOF success,
line framing and cancellation ownership semantics remain unchanged.

The roundtrip regression reuses the existing official LangChain-generated message
shapes with synthetic Unicode content and whole/every-byte HTTP bodies in both
messages and updates modes, including handled tool errors. It checks one final
Gateway marker, response closure and exact restored text/tool data. This controlled
fixture does not establish provider, database, native adapter or browser acceptance.


## In-process LangChain message delta continuity

The local `astream_events(version="v2")` projection treats each
`on_chat_model_stream` chunk as a delta. LangChain assigns one parent message ID
to multiple chunks; that ID is not a delivery identifier. Repeated text, an empty
first chunk and later text/tool deltas must not be discarded on that basis.
Tool-start/tool-end run-event deduplication remains unchanged. Remote cumulative
message handling, UTF-8 framing, tool argument/identity assembly, cancellation and
terminal contracts are unchanged. The message-ID correction alone does not add local parallel-tool assembly.

`tests/adapters/test_langgraph_local_deltas.py` uses the installed locked
`langchain-core` 1.4.7 `RunnableLambda`, `FakeListChatModel` and real
`astream_events` machinery through the production Python adapter, dispatcher,
stream serialization and in-memory history. It checks complete repeated/Unicode
answers, failure after multiple chunks and early close; object/dict delta and
lifecycle controls cover the bounded projection. The model is the library's
synthetic fixture, with no provider or LangGraph server. These tests do not
establish network, persistent-database, native, live-provider or browser acceptance.

The message-ID regression alone did not close the local first-tool-only, missing
call-ID/index and fragmented-argument gaps. The official local SDK projection below
addresses those paths while retaining the dispatcher's cumulative-argument contract.

## Official local SDK tool roundtrip projection

For installed LangChain event streams, each request now keeps its own official
`AIMessageChunk` aggregate per model run. Index-associated fragments retain all
parallel tool calls, late IDs/names and cumulative argument strings. Only changed
calls are emitted, alongside the original text delta when a chunk contains both.
Model-end/chain snapshots do not replay text. Missing final IDs, duplicate call
IDs in an aggregate, and disagreements between callback-bound and explicit result
IDs fail explicitly, without inventing a successful result or a tool ID. Chunk
merging otherwise follows the installed SDK, including its treatment of differing
nonempty IDs at the same index as distinct calls.
Cumulative arguments use the existing dispatcher/session and client contract;
remote cumulative/SSE behavior and public schemas are unchanged.

A request-local inline SDK callback observes the exact tool run ID/provider call
ID relationship before the SDK publishes `on_tool_start`. It does not match by
name, arguments or completion order. Existing callback lists/managers are copied
and retained. Lifecycle duplicate filtering still uses the original SDK run ID;
projected starts, results and errors use the authoritative provider call ID.
Serialized ToolMessage results retain their content/status rather than storing the
whole message object; ordinary dictionary tool results remain JSON. Unhandled
local tool errors produce an empty redacted error result and retain overall graph
failure or recovery semantics. Closing an active stream leaves unfinished tools
running/unknown and the session interrupted, with no fabricated success terminal.

The optional SDK imports serve local in-process invocation and streaming only,
and reuse the existing lock.
If the root SDK package is absent, legacy duck-iterator projection remains; a
missing dependency inside an installed SDK still fails. Custom iterators that do
not dispatch the supplied callbacks retain their observed run-ID identity and are
not claimed to have provider-call correlation. No name/argument guess, external
execution callback, new provider, tool authorization or automatic retry is added.

`tests/adapters/test_langgraph_local_tools.py` exercises real installed
langchain-core 1.4.7 model aggregation, callback dispatch, `StructuredTool.ainvoke`
and `ToolMessage` output through the adapter, Gateway stream and in-memory history.
The controlled graph uses a synthetic fragment model and real bounded Python tool
functions. Cases include parallel identical-name/argument tools finishing in
reverse order, later fragments with missing IDs/names, multiple model runs reusing
indexes, mixed text/tools, object/dictionary events, empty/handled-error outcomes,
propagated/recovered tool errors and cancellation. Additional controls cover
callback preservation, root tool input envelopes, identity failures and legacy
projection. This is source-bound controlled SDK evidence, not provider, LangGraph
server, persistent-database, native, network, browser or deployed acceptance.


### Local SDK text content blocks

Local model-stream chunks may carry strings or lists of content blocks. For list
content, the installed SDK's `AIMessageChunk.text` accessor selects raw string
entries and standard `type: text` blocks in their original order. Reasoning,
images and other non-text blocks are not converted to assistant prose. Projection
uses each incoming delta, including mixed tool/text chunks; model-end and chain
snapshots do not replay an aggregate answer. String chunks, tool identity and
argument correlation, remote handling and the no-SDK legacy path are unchanged.

`tests/adapters/test_langgraph_local_text_blocks.py` drives the real installed
model/event machinery through the Python adapter, dispatcher, serialized stream
and in-memory history. It covers object/dict events, repeated and Unicode block
text, interleaved string/list chunks, non-text exclusion, parallel tool outcomes,
failure, early close and ignored snapshots. These controlled fixtures do not
establish live provider, LangGraph server, persistent-database, native or browser
acceptance.


### Blocking local SDK result text

The local `invoke` path projects a direct SDK `AIMessage`, an assistant/ai message
dictionary, or the latest assistant message in existing `messages`,
`values.messages` and `output.messages` envelopes through the installed SDK's
text accessor. String content and standard text blocks become assistant text;
reasoning, images and other non-text blocks are not rendered as Python objects
or saved as assistant prose. The latest empty/non-text assistant message remains
empty rather than falling back to an older answer or trailing tool result.

Unsupported nonmessage results and the absent-SDK path retain the existing
legacy extraction. Remote extraction, tool execution, retries and streaming are
unchanged. `tests/adapters/test_langgraph_local_invoke.py` exercises real local
`BaseChatModel.ainvoke` and `RunnableLambda` results through the adapter, dispatcher
and in-memory session/history, including serialized message envelopes, empty
text, legacy results and remote controls. This is controlled local SDK evidence,
not live provider, LangGraph server, persistent-database or deployed acceptance.


### Blocking local SDK message-usage observations

Blocking local invocation observes installed LangChain model callbacks in a
request-owned run-ID ledger. The first chat generation's validated SDK
`AIMessage.usage_metadata` counters are retained once per model run; repeated end
callbacks replace that run's observation. Different runs remain distinct even
when model/message identities coincide. Returned graph state and prior-turn
messages are never scanned for usage. This also accepts an SDK-aggregated
`AIMessageChunk` returned by a model streamed inside the blocking graph.

Response, assistant history and usage-record metadata expose `local_model_usage`.
`coverage_scope` is `observed_chat_model_first_generations`: alternate generations
and hidden/uninstrumented calls are outside this observation. `coverage` is complete only when every observed model run started as a chat model,
ended without an observed error and reported nonnegative integer input/output/total
counts with a consistent total. Some reports mean partial coverage; no reports mean
unknown. Missing callbacks are not proof that no models ran. Explicit reported
zeros remain zeros; missing or malformed reports never become invented counters.
Observed sums use `observed_input`, `observed_output` and `observed_total`, which
are deliberately outside the accounting parser's token-field names. Partial sums
cover reported runs only. `sdk_zero_cost_runs` retains explicit SDK zero-cost markers;
absence of a marker does not establish a cache miss.

These are SDK-reported message observations, not measured current provider
consumption. LangChain application-cache hits may replay the original message
counts with a zero-cost marker, and arbitrary local Python graphs have no enforced
per-model binding to the Gateway's single configured billing identity. Therefore
`accounting_eligible` remains false with `model_binding_unverified`; canonical
response usage is not populated. Existing estimated accounting and estimated
history statistics remain unchanged, alongside the separately labeled observation.
This does not introduce pricing, settle reported partial usage, or prove measured
billing. Token subcategory/cache pricing remains unresolved for arbitrary graphs.

Existing callback lists/managers are copied and preserved. A missing optional
root SDK preserves legacy invocation; a missing dependency inside an installed
SDK remains an error. An unhandled invocation error/cancellation keeps the existing
failure path and does not publish observations or a fabricated assistant response.
Recovered model failures remain partial/unknown in the successful response.
Remote invocation, streaming/custom metadata precedence, tools and retries are
unchanged.

`tests/adapters/test_langgraph_local_usage.py` exercises actual installed
langchain-core 1.4.7 `BaseChatModel`, `RunnableLambda`, callback managers, streamed
model aggregation and an in-memory SDK cache through the adapter, dispatcher,
response serialization, recorder boundary and in-memory session/history. The
synthetic models do not contact providers. This is controlled local SDK evidence,
not provider, persistent-database, native, browser or deployed acceptance.


## Empty successful history (retained Python, 2026-10-14)

A successful generic blocking response (`status == "success"` without an error payload) or normally
exhausted generic stream retains one assistant history row even when it has no
text or tools. Content is stored exactly, including empty or whitespace-only
text, with the existing `gateway_stream_status: completed` observation. No
placeholder prose, hidden reasoning or invented tool outcome is added. Repeated
close after normal exhaustion does not create another row. Session-disabled
services still create no history. This extends the former text/tool-only
persistence rule; prior evidence expecting absent empty rows is historical.

The marker records the existing local success observation, not provider-side
effect completion or durable delivery. Normal stream exhaustion remains the
existing dispatcher success criterion; this change does not infer a new upstream
terminal contract. Closing before exhaustion, including while suspended at the
final yielded chunk, retains the existing interruption status. Non-success blocking responses
do not acquire an empty completed row. Existing partial-text/tool error history
continues unchanged. Failed history storage is not repaired or retried.

The existing history API preserves empty content and the playground maps the
completed marker without creating new prose. Controlled tests run the actual
LangGraph adapter, dispatcher and in-memory history route, including actual SDK
message projection regressions. This is retained Python evidence only; native
Gateway empty-history behavior, live provider/database/browser recovery and full
AS-02 acceptance remain separate.


## First-frame exception history (retained Python, 2026-10-14)

Once an invoke adapter attempt or the stream producer loop is entered, caught
failures retain an assistant row even before any text or tool observation. The
content is exactly empty (or the exact observed stream text); exception messages,
provider payloads and credentials are never copied into the outcome metadata.
The existing gateway_stream_status is failed for local failures and interrupted
for cancellation, generator close or an unknown upstream outcome. This records
what the Gateway observed, not proof that an upstream operation was cancelled,
failed or completed. The Playground restores these existing markers using its
local failed/cancelled display statuses, without a completion notice.

Normal success, tool results/statuses and partial text remain unchanged. Closing
while suspended at the final yielded chunk now retains even an empty interrupted
row, once only. Invoke exceptions still propagate, with usage recording unchanged. New invoke
outcome persistence uses owned cleanup, joined before propagating the original
exception even when cancellation repeats during that write.
Session-disabled services create no rows. Validation/session/admission failures
outside these execution blocks, returned non-success response status semantics,
storage failures and process death are not made recoverable by this change. There is no persistence retry or durable
upstream cancellation guarantee. Regression scope is the actual retained Python
adapter, dispatcher and in-memory history API; no native, live database/provider
or browser acceptance is claimed.


## Stream finalization ownership (retained Python, 2026-10-14)

The dispatcher owns and joins one finalization task through producer close,
capacity release, history storage and usage recording. Repeated caller cancellation
cannot detach history writes or skip later finalization steps. Producer-close
failure still attempts capacity release and history; release failure still attempts
history. Cleanup failures are logged without exception payloads. The original
stream exception, cancellation or generator close takes precedence over cleanup
failure or later caller cancellation. Without an original stream exception, the first caller cancellation during
finalization takes precedence over cleanup failure and retains its message. With
neither, producer-close/release failure propagates after the remaining steps.
Ordinary history-storage failure remains logged, unretried, and does not fabricate
a stored row. No upstream cancellation or rollback is implied.

History retains the locally observed stream outcome and exact existing text/tool
projection. Cancellation after normal stream exhaustion during finalization does
not retroactively change its completion observation. Closing at a suspended yield
still records interruption. Finalization has cooperative owned-join semantics,
not a hard timeout: a producer or storage operation that never settles can hold
the caller indefinitely. A timeout cannot guarantee termination of an arbitrary
coroutine that ignores cancellation; no detached background task, retry, new
configuration or persistence protocol is introduced here. Process death, failed
storage, live provider/DB durability and native behavior remain outside this proof.
