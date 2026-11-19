# Native adapter streaming framing

Source contract reviewed 2026-10-12; runtime and release evidence remain separate.

The shared `migration_adapters::transport::Lines` path frames upstream HTTP
bytes for OpenAI-compatible, Dify Chat and official SDK adapters. It remains a
single-consumer pull stream. Adapter construction retains its HTTP connection
pool; this change creates no new client, task, channel, retry or configuration
cache. Authentication, tenant admission, quota/accounting, signed authority,
trace propagation and stream terminal interpretation are outside this change.

## Bounded optimization

Previously each completed line drained the front of a `Vec`, moving every
remaining byte in a coalesced HTTP chunk. Each partial HTTP fragment also caused
an unfinished line to be scanned again from its beginning. With many short lines
in a single chunk, the former produces quadratic suffix movement. With a long
line in many small fragments, the latter produces quadratic delimiter scanning.

The line buffer now keeps separate consume and scan cursors. It allocates the
same owned output string per completed line, advances the consume cursor, and
compacts only when more staged bytes are appended or an unterminated EOF tail
is returned. Delimiter search resumes at the previous scan cursor. A CR at the
end of a nonfinal chunk is revisited so that a following LF remains part of the
same delimiter. No speculative reads or background producer are introduced.

Existing behavior is preserved for LF, CR, CRLF split between chunks, empty
lines, split multibyte UTF-8, invalid UTF-8 and an unterminated final line.
`close()` and Drop release the owned response and connection permit; explicit
close now also frees the staging allocation and retained raw HTTP chunk. EOF
releases the response and permit before a remaining buffered tail is returned.
Cancelling a pending `next()` future retains framing state for the next poll.
Adapter-specific terminal and usage handling remain in their existing owners.

## Line budgets and terminal errors

An unfinished line is bounded before UTF-8/JSON decoding or adapter event assembly.
The budget counts raw UTF-8 bytes, including an SSE field prefix, but excludes LF,
CR or CRLF delimiters. A line exactly at the budget is accepted, including a final
unterminated line. The limit is per line, not per HTTP chunk or total stream.

- Official SDK: 4 MiB per line, derived from the existing 4 MiB framed-stream
  aggregate limit. The aggregate limit remains enforced separately. A fragmented
  unfinished line can now fail before the worker supplies a delimiter or EOF.
- Dify Chat: 1 MiB + 6 bytes per line, derived from the existing 1 MiB event budget
  plus the longest supported `data: ` prefix. Event assembly counts the inserted
  newline between nonempty data fragments before appending, closing the previous
  one-byte overrun at the 1 MiB boundary. The existing 4 MiB agent-answer limit
  remains separate.
- OpenAI-compatible and the public `Lines::new` default: a new fixed 4 MiB per-line
  ceiling. Previously unbounded provider lines above this ceiling are rejected.
  This is an explicit compatibility change, not a claim that OpenAI specifies it.

These are protocol-owned constants, with no new public configuration or request
parameter. Long comments and ignored SSE fields are also bounded; the Dify prefix
allowance does not raise its assembled event budget. Many valid short lines in
one large coalesced chunk are accepted without a whole-chunk copy into the buffer.

The framer stages at most `line budget + 2` bytes, keeping the existing consume and
scan cursors. It retains an unconsumed `Bytes` slice of the current HTTP chunk and
copies only the available staging budget. Geometric reserve requests are capped
at that same bound rather than doubling past it; allocator overhead is separate.
Two extra bytes permit CRLF or one-byte look-ahead after CR. The current HTTP chunk
is already allocated by reqwest and remains outside this staging bound. This is
not a whole-process/RSS bound: raw HTTP buffers, a returned line string, JSON/event
assembly and caller-retained output are separate allocations. No HTTP read-ahead,
new producer, retry or provider re-invocation is introduced.

An over-budget line returns the redacted validation code
`provider_stream_line_too_large` exactly once. The framer closes first, dropping
its response, connection permit, raw chunk and staging allocation; subsequent
`next()` calls return EOF. Invalid UTF-8 keeps its existing validation code and is
now similarly terminal at the shared framer. No truncated line or success/final
chunk is synthesized. OpenAI and Dify mark their producer finished. SDK also uses
its existing failure path to fail the usage audit and capability lease; this may
now report the shared line error before its existing `sdk_stream_too_large`
aggregate error. Earlier valid deltas remain delivered. Local cancellation does
not assert a remote provider stop, refund, or successful SDK execution.

The native managed private model plane, transparent proxy and LangGraph use
other framing paths and are unchanged. A source-level bound here does not
establish their throughput, memory bounds or an enterprise release SLO.

## Verification

`migration_adapters::transport::tests` covers the framing cases above, exhaustive
partitions of mixed line delimiters, cursor invariants, no read-ahead, EOF permit
release, explicit close, Drop and cancellation of a pending read. The in-memory
reqwest bodies use the actual `Lines` implementation without services or DBs.
Additional source tests cover exact UTF-8 byte limits, all split positions with
LF/CR/CRLF/EOF, one-byte overflow before EOF, oversized coalesced input, many small
coalesced lines, no whole-chunk copy, immediate release, and cancellation/resume at
the limit. Adapter tests cover OpenAI and Dify failure without success/final chunks,
Dify's exact single-line/multiline event budget and one-byte multiline overflow,
and both SDK frameworks failing their trusted lease.
Existing official SDK and Dify stream suites remain separate regression gates.

The external verification packet also provides an exact-source std-only fixture
and deterministic before/after operation counters for 4,096 short lines in one
HTTP chunk and a 65,536-byte unfinished line delivered in 1,024 fragments. These
are algorithmic-work counters, not elapsed-time benchmarks or live-route results.
Tests supplied in source must not be called passed until actually executed.
Measure authenticated request/stream workloads against identified binaries
before making a latency, throughput, memory or end-to-end speedup claim.
