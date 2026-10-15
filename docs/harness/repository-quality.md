# Repository Quality and Evidence Lifecycle

> Rules for deleting dead code, retiring stale documentation and tests, controlling large modules,
> and keeping browser evidence useful without turning the repository into an artifact dump.

**Schema:** `harness/repository-quality/v1`
**Status:** policy and mechanical hygiene/evidence/LOC/shim enforcement active. Fact baseline is
the post-RAG inventory rooted at `1d25e679`; historical 2026-08-13 counts remain leads only.

---

## 1. Quality work is evidence-driven

Age, line count, zero static references, or a previous audit is not enough to delete something.
Dynamic registries, framework-required dependencies, migration history, accepted ADRs, failure
reports, and release receipts can be live contracts without ordinary imports.

Every candidate is classified before action:

| Class | Meaning | Default action |
| --- | --- | --- |
| Confirmed dead | No static/dynamic/entrypoint/contract consumer and direct gates stay equivalent | Delete code and exports; Git history is the archive |
| Needs dynamic proof | Plugin, reflection, route, CLI, provider, framework, or optional-runtime use is plausible | Keep until its consumer gate is run |
| Superseded guidance | No longer a current instruction but retains design provenance | Mark successor and move out of active reading path |
| Protected evidence | ADR, migration, loop state, critic verdict, release/security/rollback receipt | Preserve; append or supersede, never rewrite as new evidence |
| Scratch artifact | Re-creatable local output with no release-manifest reference | Ignore and clean through a narrow dry-run tool |
| Restricted raw evidence | May contain account, session, trace, provider, or credential metadata | Never commit; seal with retention policy or destroy explicitly |

Repository-quality work is its own work package and commits separately from product behaviour.

## 2. Deletion proof by artifact type

### Production code

Deletion requires all of:

1. repository-wide reference and import search;
2. route/CLI/Compose/workflow/entrypoint/plugin registry inspection;
3. no public compatibility contract or a deliberate successor contract;
4. direct tests before and after deletion;
5. package/application import or build proof;
6. removal of exports, configuration, docs, and now-invalid tests in the same logical change.

Do not create an `old/` source directory. Git already preserves deleted code.

### Dependencies

A text search is insufficient. Frameworks such as FastAPI load some packages indirectly. Remove one
dependency at a time, update the lockfile, and run real imports, type-check/build, focused tests, and
the affected runtime journey. Every direct dependency must have a service owner, purpose, entry
path, and deletion condition.

### Tests

Delete or repair tests that:

- contain an empty `pass` body;
- only assert behaviour of a test mock without exercising production code;
- pin a contract whose successor ADR/migration **and implementation** are complete, whose replacement
  executable test passes, and whose old-client/schema support window is closed;
- can never collect or always skip in the environment where their gate claims to run;
- duplicate another test without adding a failure mode, boundary, or implementation-independent
  assertion.

Do not delete a failing test merely because the implementation changed. First decide whether the
test expresses the current contract. Oversized test files may be split by scenario, but test
movement and production refactoring use separate commits.

Do not turn a failing required test into `skip` or `xfail` to complete cleanup. Allowed skip/xfail
requires gate allowlist, owner, reason and expiry; xfail is strict. A deletion receipt records where
each removed failure mode remains covered. “Self-proving” can be mechanically flagged, but a reviewer
makes the semantic decision.

### Documentation

Documents use four machine lifecycle states:

- `active` — the current implementation or execution instruction;
- `queued` — accepted next work with an explicit prerequisite;
- `superseded` — replaced, with a named successor;
- `archived` — provenance/evidence, not an instruction.

At most one active program may own a product domain. Prompt handoffs and one-time session notes
cannot remain active plans after execution. Accepted ADRs are not edited to rewrite history; a new
ADR supersedes precise clauses and the old ADR receives an implementation/supersession note.

Every current/queued plan or program declares `status`, `domain_id`, `owner`, `last_verified`, and
`successor`; queued work also declares `prerequisite`. `blocked` is an execution state for a package
or program, while “Historical” is only an index display group for `superseded/archived` material.
Archive moves run a repository-wide committed-Markdown link check; stale-source checks apply only to
current instructions, not historical examples or ADR provenance.

### Migrations and release evidence

Applied migrations, checksum ledgers, rollback receipts, security findings, and critic verdicts are
never removed merely because the related code is old. Migration compaction creates an immutable
baseline while retaining the supported upgrade chain and audit archive.

## 3. Test trust

A gate is trustworthy only when it proves that it exercised the intended implementation.

- TypeScript must report a non-zero application and Node/config file set.
- Offline OpenAPI comparison must build the application in-process; an unreachable live Gateway is
  not a reason to skip the offline contract.
- Release-required Playwright projects declare expected scenario counts and zero unexpected skip.
- Release Playwright freezes scenario ids/names as well as counts, so empty or renamed placeholder
  tests cannot satisfy the gate numerically.
- Optional live/provider tests may skip only when the gate explicitly allows it, and the result is
  reported `SKIPPED` rather than `PASS`.
- RAG fixture replay, live provider quality, and live service integration are three different
  claims with three different gate names.
- CI must cover changed Gateway, Knowledge, DB, Rust, Web, and SDK paths rather than only checking
  scripts and documentation.

The architecture program must introduce diff-to-gate validation so every changed path maps to at
least one real gate and every required CI result is present.

## 4. Browser and Playwright artifacts

Three classes have different retention rules:

| Class | Location | Retention |
| --- | --- | --- |
| Run scratch | Re-creatable screenshots/report HTML under `web/test-results/`, `web/playwright-report/`, `tmp/browser/`, `.playwright-mcp/` | Ignored; cleanup only when content classification remains scratch |
| Durable release evidence | Selected, redacted artifact bundle referenced by a committed manifest | Retained with SHA-256, source SHA, command, scenario, viewport and policy |
| Restricted raw receipt | Traces/HAR/video/raw logs that may contain sensitive metadata | Never committed; sealed external bundle or explicit limited retention |

Classification follows content, not directory. Trace, video, HAR, console/network logs and any
unreviewed screenshot default to restricted raw even when stored under a scratch path.
`web/.playwright` auth state is never ordinary scratch.

Screenshots are not durable merely because a report calls them durable. The manifest must be
portable and its artifact must actually exist at the recorded URI or allowed repository path.
Conversely, a screenshot named by a feature oracle must not be deleted until its evidence is
promoted or the oracle is deliberately superseded.

Durable manifests record owner, evidence tier, media type/size, generation time, retention, source
SHA, command/scenario/viewport, hash, redaction reviewer and storage access policy. A sealed external
bundle defines encryption, authorized readers and expiry. CI validates manifest/schema/hash only;
authorized evidence verification—not public CI—checks private bundle access.

Cleanup tools must default to dry-run, operate on an allowlist, report age and size, and dynamically
read `git worktree list --porcelain`. They canonicalize every target and refuse every worktree root,
the repository root, Git common dir, symlink, path traversal, unresolved glob, external mount,
`.env*`, auth state, committed files, and referenced evidence. Ignored/untracked local data may be
removed only when created by the current run or explicitly authorized by the user. Ordinary files
prefer quarantine/trash; irreversible destruction of sensitive raw evidence requires its owner.
`hygiene-check` never fails merely because user-owned ignored files exist. `artifact-status` is
read-only; `artifact-cleanup` is separate from CI. Broad recursive deletion of `tmp/` is prohibited.

## 5. Size and ownership

Line count is a guardrail, not an architecture metric.

- Record a post-RAG LOC baseline for production and test files.
- Existing files over the threshold may not grow without a dated exception, owner, reason, and
  removal condition.
- New Python modules should remain below 800 lines; new TS/TSX modules below 500 lines.
- A split is successful only when each module has one owner/use-case group, the import graph becomes
  clearer, and behaviour contracts remain stable.
- Pinned upstream/fork code is classified separately; do not churn it solely to satisfy local LOC.

The quality ledger records oversized file, bounded context, owning package, planned action,
exception expiry, and verification gate. `harness.yml` points to one immutable, base-SHA-bound
baseline under `reports/repository-quality/`; a normal feature diff may not regenerate it to absorb
growth. Baseline updates are independent reviewed packages, and expired exceptions fail the gate.
A raw list of “large files” is not an implementation plan.

## 6. Compatibility shims

A shim is allowed only when it names:

- old and new import/API;
- current consumers;
- owner;
- deletion condition;
- latest removal release or date;
- a compatibility test for the supported window and a consumer-inventory/removal trigger.

During the window, the gate verifies compatibility, dynamic consumers, owner and deadline. When the
consumer count reaches zero or the support policy ends, a removal package deletes the shim and its
compatibility test and adds an absence test. Public removal also follows semver/support policy; a
date alone is not authorization. Unbounded shims are dead architecture.

## 7. Quality gates

`make hygiene-check`, `make loc-no-growth-gate`, `make core-boundary-gate`, and
`make harness-check` are the executable L0 checks. The hygiene gate rejects new skip/xfail markers
relative to the immutable post-RAG inventory; removing an inherited skip is allowed. The evidence
gate binds durable artifacts to the checked policy, an ancestor source commit, a clean committed
artifact or portable sealed URI, and rejects feature-oracle references to scratch output.

Together the quality gates cover:

- empty/self-proving tests and forbidden `only/fixme` markers;
- real TypeScript project coverage;
- active/queued/superseded/archived document semantics;
- one active program per domain;
- ignored versus durable artifact consistency;
- size no-growth and dated exceptions;
- direct dependency ownership;
- high-confidence dead exports and compatibility-shim expiry;
- stale source paths in active instructions;
- release documentation versus actual distributed services.

The first post-RAG run creates a new baseline. Do not copy the counts from the 2026-08-13 hygiene
report: most of its largest dead paths have already been removed.

ARC-00 makes the minimal hygiene gate, evidence policy, size no-growth, dependency and shim ledger
effective before ARC-01 changes code. ARC-07 re-audits those gates and clears the historical backlog;
it does not postpone quality rules until the end.

Every deletion produces a receipt with candidate/class, tracked-versus-local ownership, static and
dynamic checks, public-contract decision, successor test, before/after commands and counts, removed
paths, recovery method and reviewer verdict.
