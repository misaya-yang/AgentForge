# Candle-core0.11.0 downstream zip9 compatibility patch

This is the complete published candle-core0.11.0 source, not a reduced inference
implementation. Upstream package identity/version, MIT OR Apache-2.0 declaration,
LICENSE, Cargo.toml.orig, README, CPU/CUDA/Metal implementations, npy/pickle APIs,
all upstream tests/examples/benchmarks and their binary fixtures are retained.
No old ZIP crate is renamed/re-versioned and no advisory is ignored.

Source: crates.io candle-core0.11.0; upstream VCS commit
31f35b147389700ed2a178ee66a91c3cc25cc80d, candle-core subdirectory.
The local .crate SHA256 was checked against its local crates.io index checksum:
5ecb245093b0f791b89d3420c3df9c6d49c60ab63ba54db896bf8a3baf486706.
UPSTREAM-SOURCE.json records every copied upstream file's original SHA256.
Cargo's .cargo-ok cache marker and unused packaged library Cargo.lock are omitted;
the application workspace exclusively owns the actual dependency lock.

The only dependency change is zip8.6.0 -> exactly9.0.0, default-features=false as
upstream. zip9 is already present in the local registry. Its safe release addresses
RUSTSEC-2026-0367, RUSTSEC-2026-0368 and RUSTSEC-2026-0369 as stated in the current cargo-deny diagnostic.
A real dependency update is required: lack of filesystem extraction in Candle is
not an advisory exemption, and a fake zip8 version alias would not be an upgrade.

zip9 returns ZipResult<Cow<str>> from ZipFile::name()/ZipArchive::file_names().
The minimal source adaptation propagates ZipError and owns validated names in
src/npy.rs (two sites) and src/pickle.rs (one site). It never drops invalid names,
uses lossy decoding, omits tensors or disables pickle/NPZ. zip9.patch contains the
complete three-upstream-file diff. The extra downstream test target is identified
explicitly in Cargo.toml; its five tests cover NPZ eager/named/lazy roundtrip,
UTF8 and CP437 member names, malformed-directory error propagation and the retained
upstream pickle fixture. ZIP9 intentionally falls back to CP437 for non-UTF8 names;
invalid UTF8 alone is therefore not a malformed ZIP.

Root workspace wiring, owned by the primary writer:

```toml
[patch.crates-io]
candle-core = { path = "vendor/candle-core-0.11.0" }
```

If Cargo automatically adds the in-workspace path dependency as a member, add
`vendor/candle-core-0.11.0` to root workspace.exclude and run its retained tests
explicitly. This keeps third-party test targets out of the first-party workspace
without excluding the transitive dependency from audit/SBOM.

Keep existing candle-core/nn/transformers0.11 dependencies and BGE safetensors
embedding/rerank code intact. Existing native Docker source COPY includes this
standard vendor directory; do not add a separate download/build path. Update the
root lock through the controlled dependency workflow and verify only zip9 remains.

Required subsequent checks (not run by this bounded source task): cargo-deny,
Candle-core agentforge_zip9_tests + upstream serialization/pth tests, native
Knowledge all-target compilation and existing BGE embedding/rerank tests. Real
model execution still needs the already approved local model fixtures; do not
substitute toy tensor roundtrips for BGE model acceptance. No Cargo/Docker build,
registry download, model download or service startup was performed here.

Other Candle/tokenizer/gemm advisories in cargo-deny are independent blockers.
This patch does not claim the full dependency policy is green. First-party test
and metadata files remain first-party; original upstream large files should be
classified as actual third-party vendor source, not rewritten to evade LOC rules.
