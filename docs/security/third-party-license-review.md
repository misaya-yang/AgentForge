# Unresolved original license material

This is a source investigation record, not a passing current inventory or policy
exception. The final post-upgrade Cargo metadata and generated `SOURCE_REQUESTS.json`
remain authoritative for which exact components occur in the current graph.
Historical inventory rows are not deleted to reduce a missing-text count.

The following real published packages need original legal material or a dependency
remediation before a complete notice bundle can pass. Declared terms are preserved;
no copyright-holder name, date or license text has been invented.

| Component | Observed declaration | Actual source observation |
| --- | --- | --- |
| `dispatch 0.2.0` | MIT | Published Cargo package and official `SSheldon/rust-dispatch` commit `82d6c7a5b75dc0c71c3f46f87bb6c16a476f7748` contain no complete MIT license; the exact-commit license endpoint returns 404. |
| `unicode-casefold 0.2.0` | `MIT/Apache-2.0` | Published package has no complete license file or VCS revision. Keep the original legacy declaration; an inferred SPDX/copyright rewrite is not a source artifact. |
| `html-parse-stringify 3.0.1` | MIT | npm release gitHead `ce46022f537ef9b050fac592f9fcc30bf838e5ba` has only a short README declaration, no full original license; the exact-commit license endpoint returns 404. |
| `keyv 4.5.4` | MIT | Published package has a short README attribution and no complete license. Exact release registry metadata has no gitHead; a current repository-head license is insufficient to bind the release. |
| `natural-compare 1.4.0` | MIT | npm gitHead `eec83eee67cfac84d6db30cdd65363f155673770` and package retain the original Lauri Rooden copyright and a link to the author's MIT page, but no complete version-bound text. |
| `stackback 0.0.2` | MIT | Exact official tag resolves to commit `2963095372abf7b75ba55f01cef08ab1e62c2ff4`; published package and commit have no complete MIT license. The exact-commit license endpoint returns 404. |

The historical `fxhash 0.2.1` and `mac 0.1.1` missing-text observations are also
retained in the original report. They were absent from the inspected later native
metadata; this is a graph difference, not a fabricated license fix. Likewise the
old npm inventory is not interchangeable with the current installed dependency
roots. Regenerate against the final locks and actual sources before assigning a
current count or claiming any closure.

Full embedded MIT/BSD README grants are now collected for packages such as
`agent-base`, `https-proxy-agent`, `imurmurhash`, `is-mobile` and `esrecurse`.
Monorepo omissions have exact package/VCS/manifest-bound upstream files in the
source index. MPL-2.0 and CC0-1.0 standard terms have separate bindings to their
original explicit source grants; this grants no policy exception and does not
invent an MIT copyright template for the unresolved packages above.

Native, JavaScript, font, certificate, source-delivery and binary/OS SBOM policy
checks remain independent. Do not remove a component, weaken `--require-zero`,
substitute an unrelated project's copyright/license, or mark this investigation
as distribution acceptance to make the collection succeed.
