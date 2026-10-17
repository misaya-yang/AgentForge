# pulp0.22.3: explicit pastey package alias

Complete upstream source from the exact published pulp0.22.3 crate is retained.
Only normalized Cargo.toml changes paste dependency to real package pastey=0.2.3
under its documented local paste import alias. No Rust source, feature, algorithm,
format, test, fixture, benchmark, example, version or license is changed.

UPSTREAM-SOURCE.json records original archive/index SHA256, full original file
inventory, license and VCS receipt; agentforge-pastey.patch is the exact diff.
See ../PASTEY-VENDOR.md and ../PASTEY-VENDOR-SOURCE.json for combined provenance
and primary-owned root Cargo/lock wiring. No advisory exemptions or fake versions.

Only Cargo's local .cargo-ok and unused published library Cargo.lock are omitted.
No Cargo/Docker/rustc build, download or actual BGE/tokenizer acceptance was run.
