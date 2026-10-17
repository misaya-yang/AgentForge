# Existing upstream regression sample

`upstream-legacy-response-line.json` copies the exact JSON value of `legacy_line`
from upstream `codex-rs/history/src/tests.rs`, test
`response_item_rollout_line_preserves_shape`, lines 234-249, at revision
`092d3acd6bec3e3a14bdc7e7a2810ab628ab759d`. Source file SHA256:
`8e09d905e693c5caa8ca20f45b8b3d74f20d67e0e1f1c6899132f38a8b078926`.

Only whitespace formatting changed. This is an existing upstream compatibility
regression sample, not a captured production turn, not N-1 evidence, and not an
accepted rollback corpus. Unit tests mutate it deliberately for negative cases;
those mutations are synthetic tests, never historical fixtures.
