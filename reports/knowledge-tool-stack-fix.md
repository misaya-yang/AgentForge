# Knowledge tool future stack fix

## Evidence

Root run100 passed all13other Knowledge parity integration targets (76tests),
but the two-test tools target aborted on the default test-thread stack before
reporting a result. A larger-stack rerun passed, prompting investigation of
public future layout rather than changes to input-validation assertions.

Diagnostic run109 measured public future types using size_of::<F>() through
uncalled closures. The fixture closes its lazy PostgreSQL pool before graph
construction. Measurement never constructs or polls retrieval futures and
performs no DB, model or provider calls.

| Public future | Before109 bytes | After111 bytes |
| --- | ---: | ---: |
| RetrievalService::retrieve |311320|1544|
| KnowledgeRetriever::retrieve |311552|1776|
| KnowledgeBaseTool::arun |311512|1736|
| MultiKnowledgeBaseTool::arun |240|240|
| DifyCompatibleKBAPI::retrieve |311696|1920|

The single-tool/retriever/Dify async state embedded the complete retrieval
pipeline even when an outer validation check returned before retrieval. This
was production-facing future-size exposure, not recursion in invalid-input
validation. The multi-tool already isolates work in spawned tasks and its
public future was small.

## Minimal change

`platform-gateway-integrations/src/migration_knowledge_query/service.rs` now
passes `Box::pin(self.retrieve_within_budget(...))` into the existing
`with_interactive_budget` scope. This removes the large inner pipeline from
callers' inline async state. It does not introduce a task, retry, authorization
shortcut, changed deadline, or altered cancellation ownership. Dropping the
outer future still drops the owned inner future in the existing task-local
budget scope.

`platform-knowledge-service/tests/knowledge_tool_future_layout.rs` now asserts
an upper bound of16KiB for all five measured public futures. This is a generous
regression bound rather than a compiler-specific exact layout assertion.

## Verification and limits

Root run111 explicitly unset RUST_MIN_STACK:

- Original `knowledge_migration_parity_tools` target: both unchanged tests pass.
- `knowledge_tool_future_layout`: measurement and upper-bound assertions pass.

The proof covers default-stack public input-validation tests and future layout.
It is not live DB/provider acceptance or a claim that all successful retrieval
paths have been stack-profiled. Original v126 ZIP was already delivered before
this repair; archive regeneration remains root-owned. No Cargo, manifest,
lockfile or ZIP changes were made by the Knowledge worker.
