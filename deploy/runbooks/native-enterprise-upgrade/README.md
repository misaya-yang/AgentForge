# Native enterprise upgrade

This program implements the user-requested Rust migration and enterprise upgrade.
The capability status and next action are recorded in `loop-state.json`.

Implementation breadth takes priority: complete a connected first version across
Agent, framework/provider integrations, desktop/CLI/Web, knowledge, scheduled
tasks, plugins/connectors, governance, and frontend design. Resolve blocking
errors immediately; consolidate remaining regression and refinement afterward.
Do not equate implementation with verification or wait for full per-module
acceptance before implementing independent capabilities.

Existing completed programs remain historical; this program does not reopen or
rewrite their evidence. Build caches, fixture data and temporary checks stay
outside the repository. Root integrates externally staged proposals into one
worktree. No publication or production changes are implied.

## Evaluation comparison navigation

Comparison readbacks are bound to the exact request and current selection/navigation
visit. Changing a baseline, candidate, experiment or route abandons the old readback;
returning to the same run pair does not make an earlier response fresh. Late success
and failure cannot replace a newer comparison or hide its error. Loading belongs
to the current request: abandoning a pending readback immediately restores the
Compare action, and an old completion cannot end a newer request’s spinner. Returned run IDs
are also checked before displaying a comparison. Run IDs identify persisted rows;
the comparison endpoint re-reads summaries and cases, so IDs alone do not prove
readback freshness.

The production-page Node regression executes real TanStack MutationObserver
callbacks and observed pending state with controlled React/router/query fixtures. It covers selection changes,
reopening a persisted run, repeated same-pair success/failure and Back navigation.
This is offline state-transition evidence, not database persistence, real router
rendering, provider execution or authenticated browser acceptance.
