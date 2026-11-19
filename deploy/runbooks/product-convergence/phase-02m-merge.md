# PC-02M — Merge tracks C→D→B

Merge the three worktree branches into `product-convergence/main` in order C → D → B.

## Steps

1. `git merge` each track branch in order; resolve conflicts per the conflict map:
   `src/api/router.py` (one-line includes), `src/config/settings.py` (two fields),
   `CHANGELOG.md` (append sections), `ci.yml` (one step).
2. After each merge: `uv run --all-packages --extra test pytest -q --no-cov tests/api
   tests/services/quiz` + `pnpm -C web type-check && pnpm -C web i18n:check`.
3. After B: `make validate-example-config` + `make harness-check`.

## Evidence (fill on verify)

- [ ] merge commits + conflict resolutions listed
- [ ] smoke outputs green after each merge
