# AgentForge tokenizers 0.22.2 patches

The complete upstream source is retained. The dependency-only change in `Cargo.toml` aliases `pastey=0.2.3` under the `paste` import name; `agentforge-pastey.patch` records that manifest diff.

## Existing ignored test metadata

Three pre-existing upstream `#[ignore]` attributes now carry reason strings; `agentforge-test-ignore-reasons.patch` records only those attribute changes. The two documentation training tests require Wikitext-103 files under `data/wikitext-103-raw`, which are not bundled. The ByteLevel pre-tokenized offset regression remains ignored; its source comment expects unchanged offsets, but the upstream reason for ignoring it is undocumented. Its ignore status, body, and assertions are preserved. No production Rust implementation, feature, algorithm, fixture, version, or license changed.

`UPSTREAM-SOURCE.json` retains the original archive/file hashes and records the locally modified test paths and metadata patch hash. No upstream file was removed.
