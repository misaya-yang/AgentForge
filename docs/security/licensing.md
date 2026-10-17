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
and installed Web/CLI dependency trees. It generates `inventory.json` (component
identity, declared license, each notice's SHA-256 and verbatim text, lockfile
hashes) and `THIRD_PARTY_NOTICES.txt`. Ordering is deterministic; no timestamps or
machine-specific package-cache paths are included. It handles npm and pnpm's
virtual store and rejects conflicting license text for a repeated package ID.
License/NOTICE/COPYING/COPYRIGHT and bundled font license materials are collected
recursively without following notice-tree symlinks. Unknown declarations and
missing text are explicit review states. No license is approved by generation.

After installing the exact locked dependencies for Web and CLI, the primary
builder runs these commands from the repository root (external scratch paths):

```sh
cargo metadata --locked --offline --format-version 1 \
  --manifest-path rust/platform-native/Cargo.toml \
  > /tmp/agentforge-cargo-metadata.json
platform-tools third-party-notices --repo-root "$PWD" \
  --cargo-metadata /tmp/agentforge-cargo-metadata.json \
  --output /tmp/agentforge-third-party-notices
```

The output directory must not already exist. Both `web/node_modules` and
`sdk/cli/node_modules`, and their corresponding lockfiles, are required. The tool
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

## Dependency decisions still required

The historical scan in
[license-decision-inventory.json](../../reports/platform-upgrade-20261017/license-decision-inventory.json)
records ten denied package/version entries. It remains an unchanged observation
of the earlier state, not the current first-party licensing decision. The root
license change grants no additional permission for these dependencies.

| Packages observed | Terms | Ownership / fulfillment material to retain | Decision required |
| --- | --- | --- | --- |
| `borrow-or-share 0.2.4` | MIT-0 | Crate `LICENSE`, Cargo authors/repository metadata | Explicitly assess MIT-0 against distribution policy; currently denied |
| `cssparser 0.34.0`, `cssparser-macros 0.6.1`, `dtoa-short 0.3.5`, `selectors 0.26.0` | MPL-2.0 | Servo/CSS source copyrights and license text; keep notices and corresponding covered source for distributed covered files and modifications | Decide file-level MPL obligations and source delivery. `selectors 0.26.0` cache has no standalone LICENSE: retrieve/retain the exact upstream licensed source material before shipping, rather than inventing notice text |
| `fernet 0.2.2` | MPL-2.0 | Crate LICENSE, authors' source copyrights, exact covered source and changes | Same explicit MPL source/notice decision; do not relabel as Apache |
| `epaint_default_fonts 0.33.3` | `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0` | `fonts/OFL.txt`, `fonts/UFL.txt`, `fonts/emoji-icon-font-mit-license.txt`, font copyright and reserved names in the actual assets | Assess all conjunctive font terms, embedding/redistribution and modified font names; preserve bundled font notices independently of the crate code license |
| `webpki-root-certs 1.0.9`, `webpki-roots 0.26.11`, `webpki-roots 1.0.9` | CDLA-Permissive-2.0 | Each crate LICENSE and certificate/root-store provenance | Decide data-license acceptance and certificate redistribution notices; retain the data license rather than applying the project Apache declaration |

For each accepted exception, record exact package versions, SPDX expression,
distribution scope, approval authority, license/notice text hashes, source or font
fulfillment artifacts, and conditions. If rejected, replace/remove the dependency
and regenerate the actual SBOM. The primary writer owns any subsequent reviewed
cargo-deny policy change; there is no automatic allowlist expansion here.
