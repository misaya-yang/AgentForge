# AI Gateway Assistant SDK

Python SDK for AI Gateway — chat, streaming, knowledge base, image generation, and more.

## Install

```bash
pip install ai-gateway-sdk
```

## Quick Start

```python
from ai_assistant import AssistantClient

async with AssistantClient(api_key="your-key", base_url="http://localhost:8080") as client:
    # Simple chat
    response = await client.chat.send("Summarize the onboarding checklist")
    print(response.content)

    # Streaming
    async for event in client.chat.stream("Explain our refund policy"):
        if event.event_type == "text_delta":
            print(event.data, end="", flush=True)

    # Knowledge base search + chat
    response = await client.knowledge.ask(
        "Compare the free and enterprise plan limits",
        dataset_ids=["product-docs"]
    )

    # Image generation
    result = await client.images.generate("a clean dashboard hero image")
```

## Features

- **Streaming SSE** — `async for event in client.chat.stream()` with 73 typed event types
- **Knowledge Base** — RAG-powered Q&A over your datasets
- **Sub-Agents** — Parallel explore/task/plan agents
- **Image Generation** — Gemini + DashScope with auto-routing
- **Artifacts** — File upload/download with S3 storage
- **Sessions** — Multi-turn conversation management
- **Tools** — 15+ built-in tools + MCP integration

## Requirements

- Python 3.11+
- `httpx` (auto-installed)

## Recover an existing SDK or managed run

`client.runs.get(run_id)` calls the owner-authenticated
`GET /api/v1/assistant/runs/{run_id}` endpoint. Use the canonical lowercase
`assistant_run_id` supplied in the SDK service's early StreamChunk metadata
(`sdk_framework` identifies `openai_agents` or `claude_agent_sdk`), or its persisted
`usage.sdk_usage.assistant_run_id`. Use the actual `run_id` for a managed run.
Do not substitute a worker session, request ID, Codex thread, or turn identity.
This method is recovery for an already admitted run; it does not add SDK service
submission or reproduce the original response text.

```python
from ai_assistant import AssistantClient, UnknownOutcomeError

async with AssistantClient("owner-api-key", base_url="https://gateway.example") as client:
    # Restore only an ID actually returned by the Gateway for this owner's run.
    run_id = saved_assistant_run_id
    snapshot = await client.runs.get(run_id)  # GET only, safe after a disconnect
    print(snapshot["run"]["engine"], snapshot["run"]["status"])

    # Call only when the user explicitly requests cancellation.
    try:
        acknowledgement = await client.chat.cancel(run_id)
        print(acknowledgement["cancelled"])
    except UnknownOutcomeError:
        snapshot = await client.runs.get(run_id)  # Inspect; do not repeat the POST.
```

The Gateway authenticates each read/cancel and enforces current tenant/owner
access. `tenant_id` and `user_id` configuration do not grant access to another
owner. A revoked credential or denied/not-found run raises an error and never
returns a previously cached snapshot. Status refresh does not create a session,
submit a turn, approve a tool, or restart a paid SDK loop.

Run responses use the same allowlisted projection as the native client and
`platformctl runs get`: engine/run identity, lifecycle fields and observed usage.
Malformed identity/engine responses are rejected. SDK `completed` and managed
`succeeded` remain distinct; legacy managed `completed` stays uninterpreted.
Unknown status strings are preserved without inferring success. Empty output is
not a failure, and absent usage is not zero. Partial tokens and provider cost are
observations, while `accounting_status` remains `not_asserted`. Run terminal state
does not imply final usage, settled billing, or that external effects stopped.
A later explicit GET can observe late usage without repeating the original work.

Cancellation reuses `client.chat.cancel`, with the existing response shape. It
validates the exact task ID and boolean acknowledgement. A timeout, connection
failure, server error, redirect, malformed JSON or mismatched acknowledgement
raises `UnknownOutcomeError` (an `AssistantError` subclass). Read the original
run before deciding whether another explicit cancellation is appropriate. A
`cancelled: false` acknowledgement is preserved; it is not converted to success.

### Transport compatibility

`max_retries` now applies only to GET/HEAD requests. POST/PUT/PATCH/DELETE and SSE
submissions are single-attempt, including after 429/5xx or a lost response; a
caller-supplied idempotency header does not grant automatic replay permission.
Transient reads keep the configured retry count and finite backoff (server
Retry-After capped at 30 seconds). Stream disconnection never re-submits a chat
or approval. Managed V2 cursor reads remain separate from SDK status recovery.

Authenticated redirects are refused, including same-origin redirects that could
rewrite a POST as GET. Configure the final Gateway URL directly. JSON and SSE
request URLs must remain on that origin; off-origin server-supplied event URLs
are rejected before sending credentials. Per-request headers cannot replace
Authorization, X-API-Key, tenant/user scope, Host, Cookie or Proxy-Authorization.
Existing API-key initialization is unchanged. No automatic JWT login, credential
fallback, client-side authorization, or account-switch cache is added.

These surfaces require the corresponding Gateway routes. Source and offline mock
checks are not live owner-isolation, paired-release or published-wheel acceptance.

Artifact downloads use the same authenticated Gateway origin and receive bytes
only after current owner/source-access checks. `artifacts.download_url` constructs
that protected route; it does not produce an anonymous presigned grant. Older
Gateways that redirect to object storage are intentionally unsupported by this
authenticated transport. Redirect refusal also applies to downloads; no credentials
are forwarded and no source-revocation bypass is added for legacy behavior.


## Close a stream when stopping early

Use `contextlib.aclosing` when leaving a stream before exhausting it:

```python
from contextlib import aclosing

async with AssistantClient("owner-api-key", base_url="https://gateway.example") as client:
    async with aclosing(client.chat.stream("Explain our refund policy")) as events:
        async for event in events:
            if event.is_text():
                print(event.text, end="", flush=True)
                break
```

Explicit `await events.aclose()` also works. Closing a started stream awaits its
underlying HTTP response cleanup through `client.stream`, `client.chat.stream`,
`client.knowledge.ask_stream`, and `client.threads.events`. Closing an unstarted
iterator sends no request. A bare `break` does not itself close an async iterator;
keep the `aclosing` context around the loop. Cancellation while awaiting the next
event propagates and attempts the same cleanup. An already observed read failure
or cancellation takes precedence if response cleanup also fails; ordinary closing
without a preceding read failure can still raise a cleanup error. A non-cooperative cleanup operation can delay closing; there is no hard
cleanup timeout or guarantee against repeated interruption during cleanup.

Closing local observation does not cancel the remote task or undo side effects.
Use the explicit cancellation and run-readback workflow above when appropriate;
stream closure never submits a cancellation or replays the original request.


## Submit and read an asynchronous image task

```python
async with AssistantClient("owner-api-key", base_url="https://gateway.example") as client:
    receipt = await client.images.generate_async(
        "A clean dashboard hero image", model_id="your-registered-image-model"
    )
    task_id = receipt["task_id"]  # Keep the exact returned image task identity.
    snapshot = await client.images.get_task(task_id)
    print(snapshot["status"])
    if snapshot["status"] == "completed":
        print(snapshot.get("images", []))
```

Asynchronous submission requires an explicit registered `model_id` and uses
`POST /api/v1/assistant/generate-image-async`. Each `get_task` call performs a
read of `GET /api/v1/assistant/image-task/{task_id}`. Repeat that explicit read
as needed; it does not generate another image. Synchronous `images.generate`
continues to use its existing endpoint and response model.

The SDK validates task identity and a nonempty status, preserving the remaining
response fields. It returns producer statuses unchanged, including `pending`,
`running`, `completed`, `failed`, `unknown`, and future statuses. A successful
HTTP read is not proof of image-generation success. `unknown` can mean provider
or persistence effects cannot be established; do not treat it as a safe retry.
The backend remains authoritative for model availability, ownership and limits.

Submission is never automatically replayed. An HTTP 4xx rejection except 408
keeps its normal SDK error. Timeouts, transport/server errors, redirects, and an
invalid success receipt raise `UnknownOutcomeError`; a task may already exist.
If you retained a trustworthy task ID, read that ID. Without one, the SDK cannot
recover the task from this submission; do not blindly send it again. Cancelling
the local request also does not prove that remote generation stopped.

`images.cancel(task_id)` raises `NotImplementedError` without network access.
Neither supported Gateway implementation exposes image-task cancellation through
this SDK contract; the managed Agent task cancellation endpoint is a different
workflow. Controlled HTTP fixtures verify request/response wiring, not live
provider generation, storage durability or installed-package acceptance.

## Discover and test instruction skills

Runtime capabilities and instruction artifacts have separate catalogs:
`client.tools.list_tools()` remains the runtime capability catalog. Use the
explicit skill API for tenant instruction artifacts:

```python
catalog = await client.skills.list(enabled_only=False)
for row in catalog["skills"]:
    detail = await client.skills.get(row["name"])
    print(detail.get("instructions"), detail.get("status"))
# An explicit instruction test, only when wanted:
result = await client.skills.test("skill-create", input="Draft a refund guide")
print(result["success"], result.get("result"), result.get("error"))
```

`list` preserves the full server envelope, including `total`; `get` preserves
content, version and lifecycle metadata. The Gateway owns current authorization
and filtering, including its platform skill entry. Names follow its kebab-case
3–50 character contract. A malformed or mismatched response raises
`AssistantError`, never a fabricated empty catalog or success. A `success: false`
test response is returned unchanged. Returned instructions are data; the SDK does
not execute them, activate a skill, invoke a runtime tool, or dispatch a provider.

Legacy `tools.get`, `tools.enable` and `tools.disable` continue addressing skills
with their existing return shapes. They do not manage tools returned by
`tools.list_tools`. Prefer `skills.get` when full instruction metadata is needed.
Legacy `tools.test(name, input_data=...)` now sends the required text input:
strings pass through, dictionaries become JSON text (`allow_nan=False`), and
omitted/None input becomes an empty string. Non-serializable or non-finite values
fail locally. This serialization is text for the skill, not structured tool
arguments. The new `skills.test(name, input=...)` accepts strings only.

A test is an explicit single-attempt POST. Transport errors, server errors and
invalid responses do not trigger replay; an inconclusive response is not proof
that nothing happened. Read operations use existing bounded GET retry behavior.
Offline wire checks do not establish live Gateway, installed-wheel, provider,
owner-isolation or complete product acceptance.


### Open a citation's exact source

```python
answer = await client.knowledge.ask("What does the source say?", [dataset_id])
context = answer.contexts[0]
identity = context["chunks"][0]["metadata"]
source = await client.knowledge.get_source(
    context["dataset_id"], identity["document_id"],
    source_version=identity["source_version"], source_hash=identity["source_hash"],
)
print(source["content"])
```

Check that a citation has both source_version and source_hash before opening it;
legacy unversioned citations cannot use this method. The authenticated GET
rechecks current access and returns document_id, version_number, source_hash,
title and content only. The SDK verifies the exact identity and SHA256 of the full
UTF-8 content, which may be longer than the displayed excerpt. Invalid identity
or content raises AssistantError; denied/unavailable sources retain the existing
HTTP error behavior. No current-source fallback or restore is performed. Ordinary
transport GET retries remain available; this method sends no mutation.

Source reads cap the complete JSON response at 2 MiB, matching the native Gateway
limit. The SDK requests identity encoding and refuses compressed responses before
decoding. Oversized declared lengths and accumulated stream bytes fail without
retrying the download. Normal network failures retain GET retry policy. This caps
the SDK's accumulated response; a custom transport's prior buffers or individual
chunks are outside that allocation guarantee. No download fallback is attempted.
