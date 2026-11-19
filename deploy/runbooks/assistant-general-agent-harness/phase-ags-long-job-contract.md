# AGS — Long-job contract

A chat run is one budgeted attempt. It is not a two-day worker.

- Wall clock, model turns, and tool calls stay on `ASSISTANT_RUN_*` limits.
- Progress that must survive the next user message lives in working memory, session KB, and artifacts.
- Finished tasks are archived. Unfinished goals stay visible after compaction.
- Background continuation, if added later, consumes that same state. It must not stretch the HTTP/SSE stream.
