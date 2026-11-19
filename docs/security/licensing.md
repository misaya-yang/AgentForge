# Repository and distribution licensing

The owner authorized Apache-2.0 for repository-owned code on 2026-10-17. The root
[LICENSE](../../LICENSE) contains the unmodified Apache License 2.0 text, including
its standard appendix. Project copyright and attribution belong in
[NOTICE](../../NOTICE), not in a modified license template. CLI and Web declare
`Apache-2.0`; native crates inherit the existing workspace Apache-2.0 declaration.
New owned source files should use `SPDX-License-Identifier: Apache-2.0`.

This choice does not relicense third-party material. Retain existing file headers,
license texts and notices. The Runtime's pinned upstream materials are described
in [its notice](../../deploy/agent-runtime-source/NOTICE.md); include the actual
upstream LICENSE and NOTICE with Runtime distributions. The two community plugin
notices linked by the root NOTICE remain authoritative for those imports.

## Repeatable dependency notice generation

The Rust `platform-tools third-party-notices` command consumes real Cargo metadata
and installed Web, CLI and external A2A agent dependency trees. It generates `inventory.json` (component
identity, declared license, each notice's SHA-256 and verbatim text, lockfile
hashes) and `THIRD_PARTY_NOTICES.txt`. Ordering is deterministic; no timestamps or
machine-specific package-cache paths are included. It handles npm and pnpm's
virtual store and rejects conflicting license text for a repeated package ID.
License/NOTICE/COPYING/COPYRIGHT and bundled font materials are collected
recursively without following notice-tree symlinks. Complete MIT/BSD grants
embedded in README files and verbatim copyright/license headers on bundled
XML/proto data are retained with their source-file hashes. Vendored third-party
path dependencies remain individual components; workspace-owned packages are
covered by the root declaration. Unknown declarations and missing text are
explicit review states. No license is approved by generation.

After installing the exact locked dependencies for Web and CLI, the primary
builder runs these commands from the repository root (external scratch paths):

```sh
cargo metadata --locked --offline --all-features --format-version 1 \
  --manifest-path rust/platform-native/Cargo.toml \
  > /tmp/agentforge-cargo-metadata.json
platform-tools third-party-notices --repo-root "$PWD" \
  --cargo-metadata /tmp/agentforge-cargo-metadata.json \
  --check-all --require-zero \
  --output /tmp/agentforge-third-party-notices
```

Run native compilation/tests with `scripts/new/native-container.sh`; the commands
above describe the tool inputs, not permission for a host service build. The
canonical operational controller may project only the public metadata, registry,
vendored source, installed dependency, legal text and lock inputs read-only, with
an external output directory writable. Never mount the entire private checkout
or create another dependency/target cache for notice generation.

The output directory must not already exist. `web/node_modules`,
`sdk/cli/node_modules` and `apps/external-a2a-agent/node_modules`, and their
corresponding lockfiles, are required. A declared application cannot be silently
omitted when its dependencies have not been installed. The supply-chain workflow
installs the locked source trees and calls `scripts/harness/check_third_party_notices.sh`
to generate the complete bundle as a CI artifact. The tool
includes development dependencies present in those installations; its output is
a source dependency inventory, not an assertion that every component ships in a
particular image. Use the actual image/package SBOM to select distribution scope.
Compare its components with the notice inventory and resolve missing entries,
missing texts and all policy rejections before distributing. Include generated
notices and the root LICENSE/NOTICE in each applicable package or image.

The Runtime upstream dependency graph, operating-system packages, externally
installed utilities (including Poppler), and their licenses require their own
actual distribution SBOM. Native metadata and JavaScript inventory do not cover
those graphs. Do not treat a successful generation as a complete release gate.
Run the existing cargo-deny gate separately; this work does not change its policy.
The generator itself is Rust and requires no additional Cargo dependency.

### Exact-source supplements and completeness checks

[third-party-license-sources.json](third-party-license-sources.json) and
[the original legal materials](../../licenses/third-party/README.md) preserve
actual upstream files omitted from published packages. A supplement must match
the complete component identity, declared license and actual package-manifest
SHA-256. Cargo VCS metadata or exact npm release/source provenance binds its
immutable commit. Text hashes, relative-path containment, symlinks, revision/URL
agreement and repeated identities are checked before generation. No network
request occurs in the generator. Changed package manifests, declared terms or
supplemental source material require a reviewed source-index update; a package name alone never selects a
license. Upstream text is not replaced with a guessed copyright/template.
For MPL-2.0 source references and an explicit original CC0-1.0 declaration, a
separate narrow binding retains the original grant-file hash and the verbatim
matching standard text at an immutable SPDX license-list revision. The tool
rechecks the actual grant and rejects copyright templates or unknown grants;
this does not authorize any policy exception.

`--check-all` validates the exact unmodified root Apache-2.0 LICENSE, retained
Runtime/plugin NOTICE references, Web/CLI/reference-agent declarations and npm
lock roots, native workspace declaration and owned native crate inheritance.
It does not inspect or change third-party declarations.

`--require-zero` fails before writing any bundle when a declaration or text is
missing. For source investigation, omit that option once and inspect the explicit
`review_required` entries and generated `SOURCE_REQUESTS.json`; this incomplete
collection cannot be used as a passing completeness receipt. The latter contains
real package-manifest hashes, upstream metadata and Cargo VCS data, including
packages available only in the canonical Docker registry cache.

To check a previously generated bundle, add
`--check-bundle /path/to/bundle` while using a fresh `--output` directory. The tool
reconstructs the real inventory and text and rejects any byte difference, stale
lock/source index, altered notice or missing component. Deterministic equality
is a collection check only. Keep the current metadata/lock identities and the
actual distribution SBOM with the evidence; historical counts are not an
assertion about the current resolved graph. The external reference agent needs
its own installed SDK dependency inventory before it is packaged; its absent
optional installation is not silently treated as a verified SDK distribution.

## Dependency decisions still required

The historical scan in
[license-decision-inventory.json](../../reports/platform-upgrade-20261017/license-decision-inventory.json)
records ten denied package/version entries. It remains an unchanged observation
of the earlier state, not the current first-party licensing decision. The root
license change grants no additional permission for these dependencies.

| Packages observed | Terms | Ownership / fulfillment material to retain | Decision required |
| --- | --- | --- | --- |
| `borrow-or-share 0.2.4` | MIT-0 | Crate `LICENSE`, Cargo authors/repository metadata | Explicitly assess MIT-0 against distribution policy; currently denied |
| `cssparser 0.34.0`, `cssparser-macros 0.6.1`, `dtoa-short 0.3.5`, `selectors 0.26.0` | MPL-2.0 | Servo/CSS source copyrights and license text; keep notices and corresponding covered source for distributed covered files and modifications | Decide file-level MPL obligations and source delivery. `selectors 0.26.0` was published without a standalone LICENSE: the source index now binds its original MPL header to the full MPL-2.0 standard text; retain the corresponding covered source as well |
| `fernet 0.2.2` | MPL-2.0 | Crate LICENSE, authors' source copyrights, exact covered source and changes | Same explicit MPL source/notice decision; do not relabel as Apache |
| `epaint_default_fonts 0.33.3` | `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` | `fonts/OFL.txt`, `fonts/UFL.txt`, `fonts/emoji-icon-font-mit-license.txt`, font copyright and reserved names in the actual assets | Assess all conjunctive font terms, embedding/redistribution and modified font names; preserve bundled font notices independently of the crate code license |
| `webpki-root-certs 1.0.9`, `webpki-roots 0.26.11`, `webpki-roots 1.0.9` | CDLA-Permissive-2.0 | Each crate LICENSE and certificate/root-store provenance | Decide data-license acceptance and certificate redistribution notices; retain the data license rather than applying the project Apache declaration |

For each accepted exception, record exact package versions, SPDX expression,
distribution scope, approval authority, license/notice text hashes, source or font
fulfillment artifacts, and conditions. If rejected, replace/remove the dependency
and regenerate the actual SBOM. The primary writer owns any subsequent reviewed
cargo-deny policy change; there is no automatic allowlist expansion here.
