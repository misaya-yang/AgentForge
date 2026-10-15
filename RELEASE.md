# Release Checklist

This repository publishes an open-source AI Gateway platform with Docker images
and SDK packages. A release is ready only when the checks below pass on the
exact commit being tagged.

## Pre-Release Gates

Run these checks locally before creating a tag:

```bash
make validate-example-config
docker compose --env-file .env.example config --quiet
pnpm -C web type-check
pnpm -C web lint
pnpm -C web build
pnpm -C web e2e:opensource
uv run --extra dev --extra test pytest -q --no-cov \
  tests/scripts/test_validate_env_quickstart.py \
  tests/scripts/test_web_runtime_config_entrypoint.py \
  tests/scripts/test_seed_demo_data.py
scripts/new/seed-demo-data.sh --dry-run
```

`make validate-example-config` is the public repository gate for `.env.example`.
Do not replace the real deployment gate with it. Before deploying a shared or
production environment, run `make validate-config ENV_FILE=/path/to/.env` and
`make validate ENV_FILE=/path/to/.env` against a populated env file with real
secrets, domains, provider keys, and CORS origins.
The open-source env-readiness TODO is tracked in
`deploy/runbooks/open-source-env-readiness-todo.md`.

## Versioning

- Application releases use tags like `v2.0.0`.
- SDK package releases use tags like `sdk-v1.0.2`.
- Update `CHANGELOG.md` before tagging.
- Keep `pyproject.toml`, `web/package.json`, SDK package metadata, and image
  tags aligned when the public API or package surface changes.

## Artifact Workflows

- `.github/workflows/docker-publish.yml` runs on `v*` tags and publishes:
  - `ghcr.io/<owner>/ai-gateway`
  - `ghcr.io/<owner>/ai-gateway-web`
  - `ghcr.io/<owner>/islamic-content-service`
- `.github/workflows/publish-sdk.yml` runs on `sdk-v*` tags and publishes:
  - `sdk/python` to PyPI
  - `sdk/cli` to npm

Before relying on these workflows, confirm the target repository has the
required package permissions and registry tokens configured in GitHub Actions.
Do not commit tokens or registry credentials.

## Tagging

```bash
git status --short
git log --oneline -5
git tag -a v2.0.0 -m "Release v2.0.0"
git push origin v2.0.0
```

Use an SDK tag only when the SDK package metadata has changed:

```bash
git tag -a sdk-v1.0.2 -m "Release SDK v1.0.2"
git push origin sdk-v1.0.2
```

## Post-Release Smoke

After images or packages publish, verify:

```bash
docker pull ghcr.io/<owner>/ai-gateway:<version>
docker pull ghcr.io/<owner>/ai-gateway-web:<version>
docker pull ghcr.io/<owner>/islamic-content-service:<version>
```

Then run the local quickstart against the release image set or a clean checkout:

```bash
make validate-config
make quickstart
make seed-demo
make status
```

## Rollback

If release validation fails before tagging, do not tag. Fix forward on the
branch or revert the release-preparation commit.

If a tag has been pushed but artifacts fail:

1. Leave the failed tag in place unless the repository owner explicitly approves
   deleting it.
2. Document the failed workflow URL in `CHANGELOG.md` or a GitHub release note.
3. Commit the fix.
4. Publish a new patch tag such as `v2.0.1`.

If a deployed environment is already using a bad image:

1. Stop rollout automation.
2. Restore the previous known-good image tag.
3. Run `make validate` and `make status` against the environment.
4. Keep traffic closed until gateway readiness, frontend health, and dependency
   checks are all passing.

## Release Blockers

- Missing or placeholder secrets in a non-local env file.
- CI failing on the release commit.
- Docker Compose config failing to render from `.env.example`.
- Frontend build/typecheck/lint failure.
- Demo seed dry-run failure.
- Undocumented production migration or destructive data operation.
- Unresolved security report affecting the release surface.


## Native platformctl management candidates

The SDK-tag workflow also builds the `platformctl` management binary on native
Linux x64/arm64, macOS x64/arm64 and Windows x64 runners. The canonical target
matrix is `deploy/release/platformctl-targets.json`. Existing independent Agent
CLI npm packaging and protected runtime supply-chain inputs are unchanged.

Each `platformctl-<target>` workflow artifact contains a versioned archive, its
SHA-256 companion and an artifact receipt with commit, source material hashes,
binary target/hash and native `--help` result. Outputs are staged outside the
checkout. The packager refuses dirty inputs, architecture mismatches, failed
native smoke, repository-local output and overwrites. The native workspace
version names the archive; the SDK tag identifies the coordinating workflow.

These are downloadable CI candidates, not automatically published release assets
or installers. They do not claim live API regression, macOS notarization, Windows
signing, update support or publisher authenticity from a checksum alone. Before
public distribution, satisfy unified client/API acceptance, verify all required
targets and reconcile the existing repository MIT LICENSE versus native workspace
Apache-2.0 declaration. Packaging preserves LICENSE unchanged and records this
mismatch; it does not relicense code. Do not declare a target supported until its
native build, packaged smoke and authenticated acceptance actually pass.

For a clean already-built candidate, invoke:

```bash
python scripts/harness/package_platformctl.py --target linux-x64 \
  --binary /absolute/out/x86_64-unknown-linux-gnu/release/platformctl \
  --output /absolute/release-candidates/platformctl
```

The packaging command executes only the supplied native binary's `--help`; it
never builds, downloads, signs, publishes, launches a GUI or reads credentials.


## Native desktop candidates

`.github/workflows/native-desktop.yml` is separate from SDK publication. Relevant
pull requests and explicit manual dispatch build five native candidates from
`deploy/release/desktop-targets.json`: Linux x64/arm64 portable archives, macOS
x64/arm64 app-bundle archives and a Windows x64 portable ZIP. It requires no
publisher credentials, uses read-only repository permissions and only uploads
run artifacts. No tag or package publication is needed to run this workflow.

`scripts/harness/package_desktop.py` packages an already-built candidate with
compiled-in source commit/material digest/target identity. It refuses dirty or
untracked inputs, unidentified/stale binaries, architecture mismatches,
repository-local output and overwrites. Native and extracted `--package-info`
and `--help` checks verify offline process identity; they do not open a window or
establish authenticated acceptance. Developer builds without the clean build
metadata cannot be promoted by this packager.

See [native desktop distribution](docs/architecture/native-desktop-distribution.md)
for exact build commands, prerequisites, archive layout, launch/removal and
mandatory release gates. In particular, the current Linux x64 sign-in-window
launch does not validate Windows/macOS, minimum OS versions or full desktop use.
Candidate artifacts have no publisher signature/notarization and remain blocked
from public promotion by per-target GUI/API acceptance, license reconciliation,
third-party notice/SBOM review and approved signing/provenance requirements.
Signing changes bytes: create separate final hashes/receipts after approved
signing/notarization; preserve the original candidate receipt as historical fact.
