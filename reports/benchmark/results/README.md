# Benchmark result storage

Live benchmark receipts, raw candidate output, SSE events, and ephemeral
candidate state are generated here and are ignored by Git by default.  A
release report may link to a separately sealed evidence bundle, but raw state
must not be committed because it can contain session or credential metadata.

The exploratory `enterprise-agent-three-way-2026-08-12` runs were invalidated:
the initial suite was a single-turn JSON smoke set, did not enforce equivalent
sampling/runtime budgets, and did not exercise the declared enterprise hard
gates.  Their outputs are not completion-rate evidence.
