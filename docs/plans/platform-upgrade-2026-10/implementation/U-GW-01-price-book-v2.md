# U-GW-01 price book v2 candidate

This implements P-ADR-5 price selection and receipt compatibility. Activation and
acceptance remain pending. Existing billing APIs, catalog fallback and historical
v1 snapshots keep their behavior until the host explicitly installs the v2 resolver.

`price_book::PriceBook` validates immutable publications with exact book version,
provider, model, model revision, component, currency and half-open millisecond
ranges. A tenant override wins over the global entry; an exact tenant/key discount
then applies to the selected price. Overlapping terms in the same scope fail.
Different providers, tenants, currencies and publications cannot collide. New
prices can use a new `book_version` without modifying an open-ended old entry or
changing a model revision. The host must activate that publication explicitly.

All errors are typed `PriceBookError`: misses, ambiguity, invalid references,
storage/decode failures, precision failures and arithmetic overflow do not turn
into free usage. Rates are nonnegative integer nanocurrency per an explicit
number of units. Discounts use basis points and fail when they would require
finer rate precision. `quote_nanos` rounds component totals half-even to
nanocurrency. It does not settle into existing USD accounting columns.

Frozen `PriceReceipt` terms carry content-addressed entry and discount references.
The receipt itself has a hash over the complete selection. Integrity hashes are
not authorization. Only a trusted host may select or append prices. Deserialized
receipts must pass validation before use.

For the existing ledger, `token_snapshot` only accepts matching USD input/output
per-1K-token prices. It derives the unchanged v1 snapshot and wraps both v2
references. `usage_recorder::prepare_with_snapshot` validates this envelope before
mutation, then uses the existing `TokenRates` arithmetic and component rounding.
It retains the full envelope as the recorder's first immutable pricing snapshot.
Cumulative observations therefore reuse original terms rather than current prices.
Other components/currencies remain typed quotes; mapping them to a future ledger
requires an explicit accounting contract and is not silently approximated as tokens.

The optional PostgreSQL adapter has typed failure paths, validates stored content
references, loads only global plus the requested tenant terms, and appends reviewed
publications transactionally. The unregistered SQL proposal enforces append-only
terms and excludes overlapping ranges with `btree_gist`. It also defines append-only
migration audit records. It is not a migration authority registration or schema gate.

Legacy migration requires an explicit reviewed source-row/provider/model-version/
book-version attribution list. The planner never guesses from a model prefix.
Missing/multiple/conflicting attribution, malformed prices and overlapping results
are quarantined with source IDs. Inactive rows are recorded separately. The planner
returns serializable entries, attributed references and quarantine evidence.
`persist_legacy_import` atomically inserts those entries and audit dispositions;
legacy tables and old receipts remain untouched. A duplicate import returns an
error instead of overwriting evidence.

Integration owner must export `pub mod price_book;` from foundation `lib.rs`,
register/review a successor migration without taking reserved 023, run native
checks/tests, and perform migration/rollback/live accounting gates. The host can
then explicitly construct `PriceBookTokenResolver { book, book_version,
model_version }` from admitted pricing/admission state and pass it to
`UsageRecorder::record_with_resolver`. This resolver deliberately performs no
provider or tenant inference; identity and authorization must already be established.
Do not put it behind the legacy `ScopedUsagePricing::catalog_only` fallback.

Required acceptance: native compilation and new/existing receipt-recorder tests;
concurrent PostgreSQL overlap rejection; role privilege and immutable trigger
checks; reviewed legacy import audit including quarantine; cumulative and restart
receipt replay across a publication change; real tenant/provider charge isolation;
and confirmation that historical v1 receipt bytes and costs remain unchanged.
