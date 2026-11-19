# Codex app-server diff, 2026-11-14

Upstream pin stays at `69231e119bfca0b14453515e97d99688ce1c47c9` (fork sha and git tree `536f9036c55e5861325476e58a9612de08ca3ca8`). The newest stable tag on `openai/codex` is `rust-v0.168.0` at `309da486f21e040868f4169723f4fdc8dc4218ec`, published 2026-11-13.

This checkout does not contain the upstream tree, and the VM has no Docker buildx. `docs/harness/codex-upstream-upgrade.md` requires a three-way merge plus a rebuilt runtime image. The overlay is 233 files bound to the locked image digest `sha256:0d99354adc57d8a053c8ff0ad19350e069c0eeacd80d410a5abcf34a16371d63`. Moving the pin without that merge would break `source-lock validate`. The upgrade is therefore recorded here and not applied.

## Methods the platform did not project

The locked app-server already implements `thread/fork` (`ThreadForkParams`, including `forked_from_id` persistence) and `turn/steer` (`TurnSteerParams` / `TurnSteerResponse`). The platform HTTP surface stopped at turn start, interrupt, recover, resume, archive, and unarchive.

This change projects those two methods without editing upstream app-server sources:

- Runtime overlay: `POST /internal/v1/threads/{id}/fork` and `POST /internal/v1/threads/{id}/turns/{id}/steer`, forwarded as `ClientRequest::ThreadFork` and `ClientRequest::TurnSteer`. Owner routing treats both as owner-affine.
- Gateway: `POST /api/v2/agent/threads/{id}/fork` and `POST .../turns/{id}:steer`.

The overlay content hash moved because those platform files changed. Upstream sha, fork sha, and image digests did not.
