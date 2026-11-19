import { CompatibilityError } from "./chat_request_adapter.js";

export async function* readSsePayloads(
  body: ReadableStream<Uint8Array>,
  idleTimeoutMs: number,
  signal: AbortSignal,
): AsyncGenerator<string> {
  const reader = body.getReader();
  const decoder = new TextDecoder();
  let buffer = "";
  let wireBytes = 0;
  let framesSeen = 0;
  const started = Date.now();
  const maxFrameBytes = 1024 * 1024;
  const maxWireBytes = 64 * 1024 * 1024;
  const maxDurationMs = 300_000;
  try {
    while (true) {
      const remaining = maxDurationMs - (Date.now() - started);
      if (remaining <= 0) throw new CompatibilityError("provider_stream_deadline");
      const result = await readWithTimeout(reader, Math.min(idleTimeoutMs, remaining), signal);
      if (result.done) break;
      if (!result.value) continue;
      wireBytes += result.value.byteLength;
      if (wireBytes > maxWireBytes) throw new CompatibilityError("provider_stream_bytes_limit");
      // Fetch chunks are transport boundaries, not SSE frame boundaries. Decode
      // bounded slices so coalesced valid frames do not become one huge string.
      for (let offset = 0; offset < result.value.byteLength; offset += 64 * 1024) {
        buffer += decoder.decode(result.value.subarray(offset, offset + 64 * 1024), { stream: true });
        const frames = buffer.split(/\r?\n\r?\n/);
        buffer = frames.pop() ?? "";
        if (Buffer.byteLength(buffer) > maxFrameBytes) throw new CompatibilityError("provider_frame_limit");
        for (const frame of frames) {
          if (Buffer.byteLength(frame) > maxFrameBytes) throw new CompatibilityError("provider_frame_limit");
          if (++framesSeen > 100_000) throw new CompatibilityError("provider_event_limit");
          const data = frame.split(/\r?\n/)
            .filter((line) => line.startsWith("data:"))
            .map((line) => line.slice(5).trimStart())
            .join("\n");
          if (data) yield data;
        }
      }
    }
    buffer += decoder.decode();
    if (buffer.trim()) {
      const data = buffer.split(/\r?\n/)
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.slice(5).trimStart())
        .join("\n");
      if (data) yield data;
    }
  } finally {
    await reader.cancel(signal.reason).catch(() => undefined);
    reader.releaseLock();
  }
}

async function readWithTimeout(
  reader: ReadableStreamDefaultReader<Uint8Array>,
  timeoutMs: number,
  signal: AbortSignal,
): Promise<{ done: boolean; value?: Uint8Array }> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let abort: (() => void) | undefined;
  try {
    return await Promise.race([
      reader.read(),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new CompatibilityError("provider_stream_idle_timeout")), timeoutMs);
      }),
      new Promise<never>((_, reject) => {
        abort = () => reject(signal.reason ?? new Error("provider stream aborted"));
        if (signal.aborted) abort();
        else signal.addEventListener("abort", abort, { once: true });
      }),
    ]);
  } finally {
    if (timer) clearTimeout(timer);
    if (abort) signal.removeEventListener("abort", abort);
  }
}
