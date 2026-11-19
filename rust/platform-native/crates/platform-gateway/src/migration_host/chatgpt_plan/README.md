# ChatGPT plan adapter

This is a separate explicit OAuth billing/authentication path for an OSS/local or
self-hosted application. It never imports Codex credentials, reads environment
variables, calls private ChatGPT endpoints, falls back to an API key, or assumes
that a model/plan has unlimited usage. Remotely hosted commercial distribution
needs the provider's separate integration process.

## Host integration

1. Register `pub mod chatgpt_plan` under the migration host. `ChatGptPlanClient::new`
   is inert. The host must enforce its existing user/tenant permissions before
   exposing any account, login, model or inference operation.
2. On an explicit sign-in action only, call `ProtectedStore::open` with a dedicated
   owner-only directory outside the repository. This explicit setup writes the
   stable host ID and takes an exclusive lifetime process lock. Share one Arc.
   It supports Unix; Windows requires a native secure-storage implementation.
3. `LoopbackLogin::bind(0)` binds only 127.0.0.1 and supplies the exact redirect URI.
   Create `AuthorizationAttempt::new` using that URI, the persisted host ID,
   consistent application name, and optionally the selected saved account.
4. Open `authorization_url().expose()` only through the user-authorized browser
   handoff. Never log it. `without_login_hints()` removes ID-token/email hints for
   a headless displayed URL while retaining expected returning identity checks.
   Start the browser on the same host as the loopback listener.
5. Wait for the callback, and retain `validated_callback_client_id` before
   consuming the attempt with `client.exchange`. If code exchange fails with
   invalid_grant, `resume_registration` can start fresh PKCE with that issued
   client ID. A pending client ID is not a signed-in account. Never activate it.
6. `exchange` validates callback state, one-time attempt ownership, signature,
   issuer, audience, expiration, nonce and returning subject before producing
   `Account`. Persist it with `account.persist(store)`. Identity is read-only;
   the issued client ID + verified subject isolate workspace registrations.
   A valid identity lacking the direct scope stays signed in with plan disabled.
7. `client.models(&account)` returns account-specific visible choices in server
   order. `preferred_luna()` is optional; if absent, ask the user to choose from
   that catalog. Refresh the catalog after account switches. Do not invent a
   Luna slug or silently select a differently billed provider.
8. `responses` handles text/developer/assistant history. `responses_with_tools`
   adds namespaced function definitions plus function_call/function_call_output
   input history with stable call IDs. It sends store=false and stream=true.
   `ResponseStream::next` exposes structured events including function argument
   deltas, output items, usage and completion. The caller executes approved tools
   through its existing tool/approval authority and returns the full history.
   This adapter does not execute tools or own an agent loop.
9. Success requires `response.completed`. An EOF, `[DONE]`, response.failed,
   response.incomplete or explicit error is not success. Preserve PlanError
   status, sanitized diagnostic shape/code and request ID when reporting errors;
   its Display/Debug excludes upstream content. Never automatically replay
   inference after partial output or switch billing on an error.
10. `sign_out` stops this account's future local requests and attempts discovered
    session revocation, then clears tokens while preserving registration and host
    mapping. A returned error means revocation and/or durable cleanup was not
    confirmed; tell the user and direct them to ChatGPT Settings. Already-running
    streams remain owned by the caller, which must cancel them on sign-out.

Refresh is serialized through a shared registration mutex, including across
multiple loads from one store. The store's exclusive process lock prevents
cross-process rotating-token races. Writes use private new files, fsync, atomic
rename and directory sync. Protected JSON is plaintext to its owner (not an OS
keychain); require encrypted host storage where the deployment demands it.
Do not copy this directory across hosts: host identity belongs to its lifecycle.

## Explicit limits

The current adapter supports HTTP/SSE, text history and client-executed function
tools grouped in namespaces. It does not implement image/file input, custom tools,
web search, WebSocket continuation, or hosted tools. The official plan route also
disallows embeddings, audio/video APIs, Files uploads, image generation, file
search, Code Interpreter, native computer use, hosted MCP/connectors and Responses
tool_search. Use a separate local/free embedding service; never route embeddings
through this OAuth adapter. No implicit paid provider or reranker is selected.

Responses request types deliberately cannot set unsupported sampling, storage,
background, previous_response_id, metadata, max token/tool-call fields or system
messages. Per-request transport timeout is ten minutes, idle read is sixty
seconds; JSON/event size and total stream limits bound memory. No automatic
retries are performed; the host may explicitly apply bounded backoff only to
safe temporary failures, without replaying a started inference.

## Verification and release

Offline contract tests and a proposed CLI are maintained outside the repository
under the task's verification directory. Do not claim live login, token refresh,
provider catalog, Luna availability, plan entitlement or completed inference until
user-authorized end-to-end verification actually runs. No credentials or live
OAuth grants were created to implement this module.

## Official references (reviewed 2026-10-11)

- https://developers.openai.com/siwc/token-sharing-open-source
- https://developers.openai.com/siwc/token-sharing-open-source/sign-in
- https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions
- https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference
- https://developers.openai.com/siwc/token-sharing-open-source/errors-and-recovery
- https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations
- https://developers.openai.com/siwc/website
