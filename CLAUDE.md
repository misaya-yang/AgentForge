# CLAUDE.md

**Read [`AGENTS.md`](AGENTS.md) first — it is the shared contract for every agent in this repo
(repo map, canonical commands, verification, safety).** This file adds only what is specific to
Claude Code.

## Session start

1. `AGENTS.md` — the contract.
2. [`docs/harness/README.md`](docs/harness/README.md) — architecture boundaries, gates, task loop.
3. [`docs/README.md`](docs/README.md) — find the doc that owns the area you are touching.
4. For work already in flight: the program's `loop-state.json` under `deploy/runbooks/`.

Do not carry knowledge forward from a prior session in your head. If it matters, it is in a file.

## Repo-local tooling

| Surface | What it is |
| --- | --- |
| `.claude/commands/` | `/review`, `/review-file`, `/test`, `/test-gen` — repo-local slash commands |
| `.claude/settings.json` | Enabled plugins and permissions; `settings.local.json` is untracked |
| `agent-plugins/` | Agent Plugin 1.0.0 packages shipped **by** this platform — product surface, not your tooling |
| `skills/` | Skill packages served by the platform — treat as vendored data, do not refactor |
| `web/e2e/` | The **only** home for Playwright specs — see [`web/e2e/README.md`](web/e2e/README.md) |

`agent-plugins/`, `skills/`, and `sdk/` are product artifacts this platform ships to its users.
Read them to understand behaviour; change them only when that is the task.

## Working preferences

- Use plan mode for anything touching more than ~3 files or crossing a service boundary.
- Prefer scoped test runs (`pytest -q --no-cov <paths>`) over the full suite; the full suite is long
  and `--cov-fail-under=25` makes unscoped partial runs fail for the wrong reason.
- Parallelize independent reads. Do not parallelize Docker actions — `COMPOSE_PARALLEL_LIMIT=1`
  exists because this stack is memory-tight.
- Reach for `Explore`/`Task` subagents only when the user asks for them.
- After a review finds something that would recur, encode it as a rule or a gate
  (`docs/harness/workflow.md` §5) rather than only fixing the instance.

## Do not

- Do not commit, push, or open a PR unless asked.
- Do not run `docker compose up --build`, `build`, `prune`, or `down -v` for routine verification.
  Use `make hot-update`, then `make status`.
- Do not add a rule to this file or `AGENTS.md` unless an agent has actually got it wrong.
  These files have line budgets enforced by `make harness-check`; length costs accuracy.
