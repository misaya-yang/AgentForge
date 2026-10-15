# Provider-scoped Assistant model selection

This is the MO-01 identity and MO-04 disable/restore connection in the
[models and governance plan](../plans/product-feature-completion-2026-09-23/05-models-governance-operations.md).
It does not implement capability probes or claim that MO-02/MO-03/MO-05 or
Playground acceptance is complete.

## Identity from management to a new turn

Management already identifies models by tenant, provider and model ID. The
Assistant catalog returns both `id` and `provider`, so equal model IDs from two
providers must remain distinct choices. The selector uses an internal JSON tuple
key and displays the selected provider. It resolves capabilities, vision and
reasoning options from that exact catalog row. This key is never sent as
`model_id` to an API or image generation.

Session configuration retains the original `selected_model` string and adds
optional `selected_provider`. The last explicit browser choice is provider-bound
and remains scoped to the authenticated user. Old model-only records continue
to work when exactly one eligible provider exposes that ID. An ambiguous old
record is not silently assigned to another provider. A removed/disabled saved
provider stays unavailable even if another provider has the same model name;
the existing unavailable-model notice and new-conversation action preserve the
saved history and current draft. The current enabled/provider catalog is still
required before sending.

## Explicit additive API contract

`POST /api/v2/agent/threads` gains optional nullable `provider_id`, a nonempty
string of at most 255 characters. This matches the existing V2 turn request.
Python's `ThreadCreateRequest`, native request validation, checked-in OpenAPI
and the Web request projection share this change. Existing callers can omit it;
model-only lookup remains fail-closed when ambiguous. It is a lookup hint within
the authenticated tenant, never authentication or a grant of model access.
A fixed Agent Version rejects either a model or provider override and obtains
its provider from its authorized snapshot.

The first thread's capability catalog initialization reads the selected
provider/model row. New turns send the same pair to the existing model-control
plane, which rechecks model/provider enabled state and model access before
issuing its immutable lease. Vision admission uses this provider for both new
uploads and existing session-owned image artifacts. Attachment ownership,
format/size checks and later source-access checks remain in place.

The Web uses V2. This change does not add a provider field to the legacy Python
V1 Assistant chat schema. Native and Python V2 behavior are covered together;
legacy model-only clients retain their current support boundary.

## Lifetime and recovery

Changing the selector changes only the next submitted request. It cannot rewrite
an issued turn snapshot or lease; the UI disables model selection while a run is
active. Existing Agent Version pins remain authoritative. Session restoration
reconstitutes the provider/model pair, including when the catalog finishes
loading after history. Request payloads contain scalar copies, so a later UI
choice cannot mutate a constructed request. No routing fallback to another
provider is introduced.

## Verification boundary

Production-function regressions cover the real Web stream adapter's first-thread
and turn request bodies, pair selection/capability lookup, legacy and unavailable
restoration, Python V2 validation/thread creation, thread capability revision
lookup, and provider-scoped upload/existing-image admission. Native route tests
exercise optional-provider validation and fixed-version override rejection.
Full service, authenticated-browser and live-provider acceptance require a
separate supported runtime; fixture transports do not establish those results.
