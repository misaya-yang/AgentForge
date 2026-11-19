# GEMM0.19.0 / pastey0.2.3 dependency patch

Seven complete published packages are retained here: gemm, gemm-common, gemm-c32,
gemm-c64, gemm-f16, gemm-f32 and gemm-f64, all at their real upstream0.19.0 version.
Every upstream source file, SIMD/kernel implementation, feature, test/benchmark,
README, original Cargo.toml.orig and MIT LICENSE is preserved byte-for-byte. No
GEMM/scalar/complex/half implementation is stubbed out or replaced.

The sole normalized-manifest change in each package is:

```toml
[dependencies.paste]
package = "pastey"
version = "=0.2.3"
```

This uses a real independently named pastey registry dependency under Rust's local
`paste` import alias. It does not pretend pastey is paste1.0, rewrite a registry
version, suppress an advisory or alter any Rust macro invocation. The official
cached pastey0.2.3 README explicitly documents this package-alias migration and
its proc-macro crate exports paste!. Its crates.io archive SHA256 was verified:
2ee67f1008b1ba2321834326597b8e186293b049a023cdef258527550b9935b4.
Its declared license remains MIT OR Apache-2.0; it is not vendored/renamed here.

For every GEMM package, UPSTREAM-SOURCE.json records the original crate archive
checksum, original per-file inventory/hashes, actual MIT license and upstream VCS
receipt. All63 copied upstream file bytes were verified against their respective
.crate tar members, and each archive digest against the cached crates.io index.
The package-specific agentforge-pastey.patch is the exact manifest diff. Cargo.lock
(unused library lock) and .cargo-ok (cache marker) are intentionally omitted; the
primary workspace controls the actual resolved dependency graph. The local cached
index's latest non-yanked GEMM record is0.19.0 and still depends on paste; no claim
of fresh online registry lookup is made.

Root wiring belongs to the primary; append these entries to the existing patch
section without replacing the already selected Candle patch:

```toml
[patch.crates-io]
gemm = { path = "vendor/gemm-0.19.0" }
gemm-common = { path = "vendor/gemm-common-0.19.0" }
gemm-c32 = { path = "vendor/gemm-c32-0.19.0" }
gemm-c64 = { path = "vendor/gemm-c64-0.19.0" }
gemm-f16 = { path = "vendor/gemm-f16-0.19.0" }
gemm-f32 = { path = "vendor/gemm-f32-0.19.0" }
gemm-f64 = { path = "vendor/gemm-f64-0.19.0" }
```

Classify these unchanged implementation files as genuine third-party vendor
source. Do not rewrite/split upstream code or exempt first-party code to pass LOC.
If path dependencies automatically become workspace members, explicitly exclude
these seven third-party packages from the first-party workspace and run their
retained upstream tests separately. They remain transitive audited dependencies.
The native Docker source COPY naturally includes the standard vendor directory.

After the controlled root lock refresh, run cargo-deny and affected real native
Knowledge/Candle/GEMM targets. CPU, complex and half outputs, architecture-specific
macro expansion and complete BGE embedding/rerank acceptance remain required.
No Cargo/Docker/rustc build, registry/model download, service startup or private
input access was performed in this bounded source task. Preservation/checksum
validation does not establish numerical or build acceptance.

The follow-up now adds complete pulp0.22.3 and tokenizers0.22.2 package-alias
patches; see PASTEY-VENDOR.md / PASTEY-VENDOR-SOURCE.json for the combined nine
reported direct paste parents. The original seven GEMM trees are unchanged.
Actual lock/audit/build/BGE acceptance still must run; RSA/OIDC and ttf-parser/egui
findings are separate work.

## Scraper0.27 compatibility findings

All five current first-party Cargo declarations of scraper should be pinned to
=0.27.0: platform-gateway-foundation, platform-gateway-integrations,
platform-knowledge-domain, platform-gateway and platform-knowledge-service.
Knowledge service's direct ego-tree dependency must also move0.10 -> =0.11.0.
Scraper0.27 uses ego-tree0.11.0, selectors0.38.0 and html5ever0.39.0. Keeping the old
Edge type imported from ego-tree0.10 would create incompatible traversal types.
The primary owns these existing Cargo/lock files; none was edited here.

No HTML business-source adapter is needed by the used public APIs, verified against
the local official0.27 source signatures and normalized0.23.1→0.27 source diff:

| Actual caller | Used API retained by0.27 |
| --- | --- |
| knowledge-domain migration_knowledge_ingest/parser.rs | Html::parse_document, Selector::parse, ElementRef::wrap, root_element, tree descendants, ancestors, children, text, Element::name/attr |
| knowledge-domain migration_knowledge_ingest/images.rs | Html/tree nodes, Node::as_text, ElementRef::wrap, select, element attributes |
| knowledge-domain migration_knowledge_ingest/parsing_v2/schema.rs | Html::parse_fragment, Selector::parse and select |
| knowledge-service confluence/parser.rs | Html::parse_fragment, tree traversal, Edge::Open/Close, Node::Text/Comment/Element |

No source/text sanitization, macro handling, image metadata or namespace behavior
was intentionally changed. html5ever/selectors parsing semantics still require real
existing HTML and Confluence regression tests after the dependency update; source
signature compatibility is not execution evidence. Existing Confluence tests include
source_text_and_markdown, source_macros, references_preserve_order_and_recent_context,
image_metadata_and_type_precedence and headings_and_reading_use_source_text_rules.
