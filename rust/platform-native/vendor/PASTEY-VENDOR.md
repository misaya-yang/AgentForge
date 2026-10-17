# Complete native paste dependency replacement source

The nine direct paste parents in the supplied cargo-deny-native-zip9-current
report now have complete local source patches: seven GEMM0.19.0 packages plus
pulp0.22.3 and tokenizers0.22.2. The combined provenance/checksum inventory is
PASTEY-VENDOR-SOURCE.json; each tree retains its detailed UPSTREAM-SOURCE.json and
exact agentforge-pastey.patch. The original seven GEMM source trees and Candle
patch were not modified by this follow-up.

Both new packages were copied from their exact cached published crates, checked
against archive members and the corresponding cached crates.io index SHA256:

| Package | Archive SHA256 | License | Copied upstream files |
| --- | --- | --- | --- |
| pulp0.22.3 | 046aa45b989642ec2e4717c8e72d677b13edd831a4d3b6cf37d9a3e54912496a | MIT |37 |
| tokenizers0.22.2 | b238e22d44a15349529690fb07bd645cf58149a1b1e44d6cb5bd1641ff1a6223 | Apache-2.0 |98 |

Only the normalized Cargo.toml changes in each tree:

```toml
[dependencies.paste]
package = "pastey"
version = "=0.2.3"
```

This is an explicit alias to the real pastey0.2.3 package, using the exact same
migration documented in its published README. pastey0.2.3's archive was already
verified as SHA2562ee67f1008b1ba2321834326597b8e186293b049a023cdef258527550b9935b4.
There is no fake paste package/version, ignored advisory or hand-copied proc macro.

All135 new upstream files other than the two normalized manifests remain exactly
as published: APIs, SIMD/intrinsics kernels, scalar fallbacks, build.rs, algorithms,
normalizers/pre-tokenizers/decoders/post-processors, BPE/WordPiece/Unigram models,
serde/format support, native/WASM/regex/onig/optional HTTP features, fixtures,
benchmarks, examples, tests, original Cargo.toml.orig, READMEs and licenses. No
feature/model format or tokenizer was removed. Cargo.lock (unused library lock)
and .cargo-ok (local cache marker) are omitted as documented; root dependency
resolution remains exclusively controlled by the application workspace.

Primary-owned root wiring, appended alongside the seven GEMM/Candle patches:

```toml
[patch.crates-io]
pulp = { path = "vendor/pulp-0.22.3" }
tokenizers = { path = "vendor/tokenizers-0.22.2" }
```

If required by Cargo workspace auto-membership, add these actual third-party
packages to workspace.exclude and run their retained tests separately. Exclusion
from first-party workspace targets is not exclusion from dependency audit/SBOM.
Do not bypass first-party LOC checks or rewrite upstream implementation to shorten
files. Existing native Docker COPY already includes this standard vendor location.

Observed in this source task: package versions/index checksums/archive members,
unchanged source and license hashes, exact dependency-only diffs and all nine
reported direct paste-parent coverage were verified locally. No Cargo/Docker or
rustc build, new downloads, service startup or private-input reads occurred.
Primary still needs the single controlled lock refresh, actual cargo-deny rerun,
full native/BGE chain compilation and existing offline tokenizer/embedding/rerank
acceptance. Source preservation does not prove macro/numerical/model correctness.
Other dependency findings (RSA/OIDC, ttf-parser/egui etc.) remain separate blockers.
