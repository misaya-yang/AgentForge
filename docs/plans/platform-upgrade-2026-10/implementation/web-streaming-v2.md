# U-WEB-05 streaming client v2 implementation

The console uses one byte reader in `web/src/lib/sse.ts` for data-only SSE,
named SSE and AG-UI. This is implementation evidence, not completed product acceptance.

## Activity and visibility

- The default budget is 45 seconds without received bytes in a visible tab.
  Headers, partial UTF-8/data frames and comment keep-alives refresh the budget.
  `idleTimeoutMs` overrides it; `timeoutMs` remains a compatible alias with idle
  semantics, and zero disables the budget. There is no total turn deadline.
- Hidden tabs continue reading; their idle timer is paused so browser scheduling
  cannot falsely terminate the run. Visibility restores a fresh idle budget.
  Reconnect attempts wait for visibility, and caller abort releases the wait.
- Silence raises `SSEIdleTimeoutError`. Network interruption raises
  `SSEInterruptedError`. Both communicate an unknown server outcome, rather than
  claiming that a capability or inference failed before execution.

## Replay boundaries

- Assistant thread/turn creation occurs once. Its existing read-only event GET
  resumes with `after_sequence`; duplicate/non-safe-integer sequences are
  rejected before projection. Terminal events stop reading. Failed reads use
  capped exponential backoff with jitter. Explicit Stop retains its existing
  interrupt behavior; view detach retains its existing transport-only behavior.
- Knowledge document progress GET uses opt-in `Last-Event-ID` replay. The shared
  transport deduplicates event IDs, fences reconnects when credentials change,
  limits retries, and refuses replay after an event without a safe cursor.
  Document terminals are per-document; they do not terminate the dataset feed.
- Playground and Knowledge QA POST requests are **never automatically retried**.
  Their existing reducers retain partial content and distinguish a missing
  terminal from completion. The Playground displays an explicit unknown-outcome
  message for an interrupted or idle-expired request.
- `onConnectionState` is additive. Assistant shows reconnecting, background and
  unknown-outcome statuses; durable terminal observation clears transient status.
  Client connection state is separate from server event vocabulary and does not
  produce or repeat capability calls.

## Verification and remaining acceptance

Targeted transport/parser/runtime tests cover a virtual hour with byte heartbeats,
foreground idle expiry, background pause/resume, Last-Event-ID replay, token/tool
result/terminal deduplication, POST non-replay and cursor-based runtime recovery.
The virtual-hour test advances fake timers; it is **not a real one-hour run**.

The repository browser specification is the `U-WEB-05 network flap` case in
`web/e2e/chat-experience.spec.ts`. It delivers partial text, aborts the next network
read, observes reconnecting UI, then replays the backlog and terminal. It checks
one turn start, unchanged cursor, unique text and no interrupt. A managed browser
run must execute this specification; adding its source is not browser acceptance.

Still required: run an actual provider-backed turn for at least one wall-clock
hour under the managed environment, with recorded keep-alives/activity and a final
durable terminal; exercise a real network interruption and tab visibility change;
verify exactly one turn/capability effect and retained partial output. A faster
fixture or virtual-clock run must not replace that evidence.
