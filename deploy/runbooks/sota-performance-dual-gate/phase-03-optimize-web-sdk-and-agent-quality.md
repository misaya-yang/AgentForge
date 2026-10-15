# Phase 03 - Optimize Web SDK and agent quality

- PHASE_ID: SPD-03
- FEATURE_ID: SPD-F004
- DEPENDS_ON: SPD-02

## Outcome

The console and SDKs meet their scale contracts and the three recorded AI Platform
quality failures improve through general harness behavior rather than benchmark routing.

## Scope

In:

- Route/locale/heavy-component loading, long-message rendering, session restore waterfall,
  SDK SSE normalization, evidence receipts, code-execution discovery, and eval normalization.

Out:

- Disabling thinking, prompt-regex routing, default semantic cache, or hiding failed tools.

## Done when

- [x] Public entry gzip is at most 330 KB and Assistant incremental gzip at most 220 KB.
- [x] 200-message restore and 20k-character streaming have no sustained task over 50 ms.
- [x] Python, Java, Dart, and CLI parse the same nested SSE fixture.
- [x] Finance normalization keeps adversarial negatives; engineering returns verified JSON-only code; research retains complete bounded evidence.
- [x] Focused and broad Web/SDK/eval gates pass with no tool or failure visibility regression.

## Verify

| Check | Command or observation | Proves |
| --- | --- | --- |
| Web | `pnpm -C web type-check && pnpm -C web lint && pnpm -C web build` plus configured Playwright | Bundle, rendering, and journey contracts pass. |
| SDK/eval | Shared SSE fixture tests and focused native-parity validators | Cross-client envelope and quality fixes are real and non-overfit. |

Use the smallest check that can falsify the outcome. Add runtime, browser, migration, or reviewer evidence only when the outcome requires it.

## Stop or confirm

- Stop when a dependency is incomplete, the same failure repeats without a new hypothesis, or the iteration cap is reached.
- Set `waiting_confirmation` only for a real authority boundary.
- Treat any evaluator relaxation as required review; it must retain explicit negative mutations.


## Attachment codec loading (2026-10-12)

`web/src/api/files.ts` loads `browser-image-compression` inside the existing
asynchronous compression branch, only for an image larger than the configured
size threshold (2 MiB by default). Utilities, non-image uploads, smaller images,
and uploads with compression disabled do not load the codec. Native module
caching shares the chunk across simultaneous uploads; each upload still runs
its own compression with the same options and original filename. Import or
compression failure retains the existing original-file upload fallback. No
route, React/provider boundary, translation loading, dependency version, or
bundle-budget threshold changes.

Same-path production builds using pinned Node 26.11.1 and pnpm 12.11.2 measure:

| Gzip closure | Before | After |
| --- | ---: | ---: |
| Public entry, larger locale | 328,645 B | 328,641 B |
| Assistant increment | 166,780 B | 147,180 B |
| Assistant restored conversation | 207,426 B | 187,827 B |
| Assistant with GFM | 219,248 B | 199,649 B |

The meaningful saving is approximately 19.6 KB gzip in the Assistant closures;
public entry remains close to its unchanged 330,000 B budget. Two repeated builds
of each variant are byte-identical within the comparison directory. Relocating
the verified source/dependencies changes Vite preload ordering, so these results
use the fresh same-path baseline rather than subtracting the earlier receipt.
The compression chunk remains available on demand and is outside the public,
Assistant, restored-conversation, and GFM static closures.

`web/src/api/files.test.ts` exercises the production upload module with HTTP and
codec doubles: lazy loading, size/type/opt-out bypasses, compression options,
filename/MIME/progress preservation, import/compression failures, concurrent
module caching, and independent uploads. Local type-check, lint, all Node tests,
production build/budget, and i18n checks are recorded in the external
`platform-verification/web-bundle-next/receipts/` evidence packet. Browser image
encoding, network chunk delivery, and authenticated upload acceptance are not
established by these local gates; no runtime latency or release claim is made.


## Status presentation loading (2026-10-12)

`SystemStatusPage` keeps its shared Button return action, authentication-based
return destination and compatibility exports eager. Only the status-specific
frame, translated heading/description, and 403/404 icons move to
`SystemStatusContent`, loaded through React lazy under the existing router
Suspense boundary. The rendered markup, classes, link semantics, locale keys,
permission priority and router/auth guard logic are unchanged. The first status
page visit can show the existing route loading fallback while its chunk loads.
No dependency versions, theme/provider boundaries or budget thresholds change.

Pinned same-path production builds measure a small net reduction:

| Gzip closure | Before | After |
| --- | ---: | ---: |
| Public entry, larger locale | 328,641 B | 327,783 B |
| Assistant increment | 147,180 B | 147,151 B |
| Assistant with GFM | 199,649 B | 199,616 B |
| Complete login, including larger locale | 393,554 B | 393,135 B |
| Complete Assistant, including layout and larger locale | 552,353 B | 551,898 B |

The 858 B entry reduction increases headroom to 2,217 B; it does not remove the
need to watch the unchanged 330,000 B budget. Complete login and Assistant
journeys also become slightly smaller. A broader status-page split was rejected:
it moved shared Button utilities out of the entry while increasing complete
journey gzip by 1.3–1.7 KB. This narrower boundary keeps those common utilities
in the entry and defers only status-specific presentation.

`web/src/router.test.ts` executes production TSX with real React lazy/Suspense,
MemoryRouter and server rendering. Session, translation hook and unrelated
route modules are doubled. It covers deferred status presentation, 403/404
landmarks and links, both locales, repeated status rendering, disabled Agent
Studio, and existing hydration/session/role/permission guards. Local check
receipts and complete-journey manifest comparisons are in the external
`platform-verification/public-entry-next/receipts/` packet. Browser navigation,
keyboard/focus behavior, network chunk delivery, visual screenshots and live
service acceptance remain unverified; these are byte and local-render results,
not measured browser latency or release acceptance.


## Knowledge locale loading (2026-10-12)

The awaited main locale no longer contains Knowledge detail/create/retrieval
resources. Each language moves 1,167 translation leaves into a deferred
`knowledge` namespace; the 65-leaf `knowledge.datasets` subtree remains eager
for AppLayout's title/subtitle and the list-only route. A canonical key/value
comparison proves that merging main and deferred resources exactly reconstructs
the prior English and Chinese resources, with no changed or removed translations.

Knowledge create/detail and Eval await their required resources before rendering.
Detail and Eval fetch Knowledge and Eval namespaces together inside the existing
serialized locale-operation queue, avoiding a new namespace download waterfall.
The single-namespace API remains available. Activity is committed only after a
complete batch succeeds; a failed or late import cannot activate an old-locale
namespace. Previously active namespaces, retry behavior, auth/permission checks,
route fallback and root error handling are preserved. List-only visits do not
request the deferred namespace. No dependency, lockfile, theme/provider boundary,
Vite setting, route contract or budget threshold changes.

Pinned Node 26.11.1 / pnpm 12.11.2 same-path production builds, with identical
read-only dependency closure and level-9 gzip accounting, measure:

| JS/CSS gzip closure, larger locale | Before | After |
| --- | ---: | ---: |
| Public entry | 329,037 B | 309,374 B |
| Initial login | 394,366 B | 374,740 B |
| Initial Assistant, with layout | 553,317 B | 533,723 B |
| Initial Knowledge list, with layout | 649,699 B | 630,142 B |
| Initial Knowledge create, with layout | 464,926 B | 466,686 B |
| Initial Knowledge detail, with layout and both namespaces | 657,879 B | 659,686 B |
| Initial Eval, with layout and both namespaces | 683,700 B | 685,465 B |
| Cumulative Knowledge list/create/detail + Eval, GFM/Math and toast | 966,493 B | 968,316 B |

Public-entry headroom increases from 963 B to
20,626 B under the unchanged 330,000 B limit.
This is a demand-loading tradeoff: detail/create/Eval visitors fetch one extra
locale chunk and pay roughly 1.8 KB additional gzip across the complete initial
route closure because resources are separately compressed. The English+Chinese
cumulative Knowledge/Eval/GFM/Math/toast union is
1,026,268 B before and
1,029,325 B after, with every shared
artifact counted once. Optional font binary fetches are not included in the
existing JS/CSS budget methodology. These totals do not estimate traffic mix or
claim a net saving for an all-features visit.

Full type-check, lint, all 737 Node tests (no failures/skips), i18n key check and
production build/budget pass against the final staged source. Focused regressions
execute the production locale loader with real i18next and the production router
with real React lazy/Suspense and server rendering. They cover both locales,
eager shell/list keys, batched parallel imports, caching, language-switch races,
failed imports/retries, late sibling completion, unchanged authorization gates,
and loading fallback. An AST guard audits external/eager Knowledge key consumers.
Evidence is in `platform-verification/public-entry-performance-next/`, including
`resource-equivalence.json`, `bundle-comparison.json` and `receipts/final-v4/`.
Browser route navigation, network chunk delivery, visual/focus behavior and
runtime latency remain unverified; this is artifact/local-regression evidence,
not a release or browser-performance acceptance claim.
