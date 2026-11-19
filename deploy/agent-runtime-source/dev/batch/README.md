# Controlled development source batch v2

This producer preserves the original source-only v1 producer and its strict
24-command admission. A v2 source unit cannot pass that old admission. The new
`platform-tools source-batch verify` consumes v2 source units and v3 export
receipts. It grants no production or generated-release identity.

The new producer executes eight actual Cargo workflow commands, all locked and
offline. Separate `cargo --version` and `rustc --version` probes record toolchain
identity and are not workflow coverage receipts:

| Invocation | Coverage retained or expanded |
| --- | --- |
| metadata | Complete resolved workspace graph and lib/bin target metadata |
| check | Contract, Worker, Runtime, quiz executor, Python compat, Office; all targets; keep going |
| list | All lib/bin test executables for the same six packages, unfiltered |
| tests | Those exact lib/bin executables and complete names; no fail fast |
| docs | All six packages, including the original Contract doc test scope |
| bins | All targets of the same six packages; eleven required normal ELF artifacts |
| final metadata | Final source-unit, lock, source inventory binding |
| schema example | Original controlled schema exporter; identical source binding |

The same explicit quiz executor feature applies throughout check/list/tests/docs/
bins. The previous producer compiled several overlapping feature closures in
separate invocations. Batching preserves coverage and lets Cargo unify the graph.
No per-package legacy command receipt is generated or inferred. Compiled target
artifacts, Cargo's actual `Running unittests` paths, and complete raw test lists
and outcomes establish target coverage, including zero-test helper binaries.
Ignored integration tests remain explicitly ignored and are not acceptance.

The admission rejects missing or duplicate targets, disabled quiz executor,
escaping test paths, filtered/failed/partial outcomes, changed command arguments,
missing original Contract tests, changed mandatory service test policy, missing
normal ELF artifacts, source changes, export/schema changes, or failed commands.
All source-unit payload/mode/preimage/upstream checks are shared with v1; canonical
plugin and workload/data source-sync verification is required. The original
composed-export validation also checks exact complete source snapshots, metadata
against the resolved lock and source manifest paths, strict schema JSON, toolchain
versions, clean official upstream HEAD/tree/origin, license/NOTICE and both SBOMs.
A separate v3 export path reuses those checks and writes new external source
evidence; old v1/v2 public entrypoints do not accept the new protocol. Existing 39 upstream
patch paths are preserved byte for byte; this workflow adds no upstream changes.

`collector.rs` is std-only and compiled in the pinned Rust 1.95 controlled image.
It calls an actually compiled Linux native `platform-tools` ELF only to validate
the batched inventory. That validator is exported from the existing native cache,
with SHA/ELF provenance in the frozen public control context. No server graph is
compiled a second time. Final source admission runs the same native validator
against read-only public source mounts. Old failed/partial runs remain untouched.

Preparation and actual execution live in an immutable external attempt directory.
The launcher enforces the existing builder, exact Rust base digest, existing three
cache IDs, one Cargo job, debug/incremental off, offline/no-pull, 4 GiB/4 CPUs,
10 GiB free-space floor and bounded warm growth. A prepared plan is not a pass.
Final dev image packaging requires a successful run, v2 admission and eleven real
source-bound ELF hashes. Original release/history/production gates are unchanged.
