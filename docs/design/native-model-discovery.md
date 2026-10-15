# Native model discovery and diagnostics

The native provider sync endpoint retains its existing `created_models`,
`updated_models`, `skipped_models` and `discovery_warnings` response fields.
This implements part of MO-01, MO-03 and MO-05 in the
[models and operations plan](../plans/product-feature-completion-2026-09-23/05-models-governance-operations.md).

## Discovery is not capability verification

Google/Vertex templates retain trusted catalog admission and capability override
conflict handling. An unknown model ID is never automatically enabled. Custom
`openai` and `openai-compatible` providers now use the existing custom template
for discovery, instead of silently declining sync because their provider ID is
not a built-in template. Their discovered IDs appear in `skipped_models` with
`discovered_model_not_in_trusted_catalog`; the administrator must add models
explicitly and verify capabilities. Discovery never infers tool use, vision,
context limits, prices, or successful generation from model-list membership.
An empty custom catalog does not claim that trusted defaults were installed.

## Wire and diagnostic boundary

Compatible discovery follows the repository's existing provider connection-test
contract: GET `/v1/models`, using a Bearer header. A custom base already ending in
`/v1` receives only `/models`. No custom default host is guessed. Embedded URL
credentials, query parameters and fragments are rejected with a safe warning.
Google and Vertex retain their current request contracts. Redirects and retries
remain disabled by the existing transport. No live provider call was made while
implementing this change.

Discovery warnings distinguish missing/unsupported credentials, invalid base
URL, timeout, transport failure, denied access, unsupported endpoint, rate limit,
provider outage, invalid JSON and incompatible response shape. Fixed diagnostic
text never includes upstream bodies, error strings, credentials or request URLs.
Internal credential-service errors continue to propagate rather than becoming a
false successful discovery result. Trusted catalogs remain usable when upstream
discovery fails; a warning makes that fallback explicit.

Compatible parsing accepts only bounded, nonempty model IDs, deduplicates them,
and extracts no additional upstream metadata. Sync caps discovery results at
1000 and reports truncation. A provider pagination marker (`nextPageToken` or
`has_more`) produces an incompleteness warning; this implementation deliberately
does not claim to fetch subsequent pages. Sync does not delete models absent
from discovery, overwrite manual capability policies, or imply a connection or
capability probe succeeded.

## Administrator access and pricing

Catalog access levels and input/output prices initialize newly created rows.
Once a provider-scoped tenant model exists, ordinary and startup catalog sync
leave its `access_level`, `input_price_per_1k`, `output_price_per_1k`, disabled
state and capability overrides untouched. Administrators retain the existing
model-edit API for changing access and prices. Sync still refreshes catalog
metadata and validates capability changes against the saved overrides.

There is no persisted marker distinguishing an old catalog price from an
administrator edit. Existing prices therefore remain authoritative until an
administrator changes them. The update omits these policy columns entirely;
it does not read and write back an earlier value over a concurrent edit. This
implements the manual-override protection in MO-03 without adding a schema,
reset policy, or broadening the scope of catalog discovery.

## Verification status

Staged Rust formatting and patch checks have passed. Consolidated native
compilation and fixture regression are still required, including compatible
root/versioned base URLs, secret-safe errors, custom model review, trusted
catalog fallback, duplicate IDs and pagination/truncation warnings. Live provider
acceptance is not performed or claimed.
