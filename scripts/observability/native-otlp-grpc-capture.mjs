#!/usr/bin/env node
// Small dependency-free OTLP/gRPC receiver for local acceptance evidence.
// Run with: node scripts/observability/native-otlp-grpc-capture.mjs
//   --host 127.0.0.1 --port 43175 --out ~/.cache/agentforge/runtime-work/native-otel-capture.json
// Point Gateway and Knowledge at http://127.0.0.1:43175 and optionally set
// OTEL_METRIC_EXPORT_INTERVAL=2000 (milliseconds) before their normal restart.
// It records only service/trace/span IDs, hashed request IDs, safe metric names,
// finite diagnostic attributes, and aggregate values. It never saves bodies,
// authorization data, baggage values, URLs, provider headers, or credentials.
import { createHash } from "node:crypto";
import { createServer } from "node:http2";
import { writeFile } from "node:fs/promises";

const TRACE_PATH = "/opentelemetry.proto.collector.trace.v1.TraceService/Export";
const METRIC_PATH = "/opentelemetry.proto.collector.metrics.v1.MetricsService/Export";
const SAFE_SPAN_NAMES = new Set([
  "gen_ai.provider.request",
  "knowledge.http.request",
  "knowledge.rpc.retrieve",
]);
const SAFE_ATTRIBUTES = new Set([
  "http.request.method",
  "http.response.status_code",
  "http.status_code",
  "http.method",
  "gen_ai.token.type",
  "error.type",
]);

function options(argv) {
  const result = { host: "127.0.0.1", port: 43175, output: "" };
  for (let index = 0; index < argv.length; index += 1) {
    const value = argv[index + 1];
    if (argv[index] === "--host" && value) result.host = value, index += 1;
    else if (argv[index] === "--port" && value) result.port = Number(value), index += 1;
    else if (argv[index] === "--out" && value) result.output = value, index += 1;
  }
  if (!Number.isInteger(result.port) || result.port < 1 || result.port > 65535) {
    throw new Error("invalid --port");
  }
  return result;
}

function parseFields(bytes) {
  const fields = [];
  let offset = 0;
  const readVarint = () => {
    let result = 0n;
    let shift = 0n;
    for (;;) {
      if (offset >= bytes.length || shift > 63n) throw new Error("invalid protobuf varint");
      const byte = bytes[offset++];
      result |= BigInt(byte & 0x7f) << shift;
      if ((byte & 0x80) === 0) return result;
      shift += 7n;
    }
  };
  while (offset < bytes.length) {
    const tag = Number(readVarint());
    const number = tag >>> 3;
    const wire = tag & 7;
    if (number === 0) throw new Error("invalid protobuf field number");
    if (wire === 0) fields.push({ number, wire, value: readVarint() });
    else if (wire === 1) {
      if (offset + 8 > bytes.length) throw new Error("truncated fixed64");
      fields.push({ number, wire, value: bytes.subarray(offset, offset + 8) });
      offset += 8;
    } else if (wire === 2) {
      const length = Number(readVarint());
      if (!Number.isSafeInteger(length) || length < 0 || offset + length > bytes.length) {
        throw new Error("truncated length-delimited field");
      }
      fields.push({ number, wire, value: bytes.subarray(offset, offset + length) });
      offset += length;
    } else if (wire === 5) {
      if (offset + 4 > bytes.length) throw new Error("truncated fixed32");
      fields.push({ number, wire, value: bytes.subarray(offset, offset + 4) });
      offset += 4;
    } else {
      throw new Error("unsupported protobuf wire type");
    }
  }
  return fields;
}

function all(fields, number, wire) {
  return fields.filter((field) => field.number === number && (wire === undefined || field.wire === wire));
}
function first(fields, number, wire) {
  return all(fields, number, wire)[0]?.value;
}
function string(value) {
  return value ? Buffer.from(value).toString("utf8") : "";
}
function number(value) {
  if (typeof value === "bigint") return Number(value);
  if (Buffer.isBuffer(value) && value.length === 8) return value.readDoubleLE(0);
  return undefined;
}
function anyValue(bytes) {
  const fields = parseFields(bytes);
  if (first(fields, 1, 2)) return string(first(fields, 1, 2));
  if (first(fields, 2, 0) !== undefined) return first(fields, 2, 0) !== 0n;
  if (first(fields, 3, 0) !== undefined) return number(first(fields, 3, 0));
  if (first(fields, 4, 1)) return number(first(fields, 4, 1));
  return undefined;
}
function keyValue(bytes) {
  const fields = parseFields(bytes);
  const key = string(first(fields, 1, 2));
  const encoded = first(fields, 2, 2);
  return [key, encoded ? anyValue(encoded) : undefined];
}
function attributes(fields, repeatedField) {
  const result = {};
  for (const item of all(fields, repeatedField, 2)) {
    const [key, value] = keyValue(item.value);
    if (key === "http.request.method" && typeof value === "string") {
      result[key] = /^[A-Z]{3,12}$/.test(value) ? value : "OTHER";
    } else if (key === "gen_ai.token.type" && typeof value === "string" && ["input", "output", "reasoning", "cached_input"].includes(value)) {
      result[key] = value;
    } else if (key === "error.type" && typeof value === "string" && /^[A-Za-z0-9_.-]{1,64}$/.test(value)) {
      result[key] = value;
    } else if (key === "http.response.status_code" || key === "http.status_code") {
      if (typeof value === "number" && Number.isFinite(value)) result[key] = value;
    } else if (SAFE_ATTRIBUTES.has(key) && ["number", "boolean"].includes(typeof value)) {
      result[key] = value;
    } else if (key === "request.id" && typeof value === "string") {
      result.request_id_sha256 = createHash("sha256").update(value).digest("hex").slice(0, 16);
    }
  }
  return result;
}
function resourceAttributes(bytes) {
  const resource = parseFields(bytes);
  const result = {};
  for (const item of all(resource, 1, 2)) {
    const [key, value] = keyValue(item.value);
    if (key === "service.name" || key === "service.version" || key === "deployment.environment.name") {
      if (typeof value === "string") result[key] = value;
    }
  }
  return result;
}
function safeSpanName(value) {
  if (SAFE_SPAN_NAMES.has(value)) return value;
  if (/^[A-Z]{3,7} \/(health(?:\/ready|\/live)?|metrics)$/.test(value)) return value;
  return "http.server.request";
}
function parseSpan(bytes, resource) {
  const fields = parseFields(bytes);
  const traceId = first(fields, 1, 2);
  const spanId = first(fields, 2, 2);
  const parentId = first(fields, 4, 2);
  const name = string(first(fields, 5, 2));
  if (!traceId || !spanId) return undefined;
  const statusFields = first(fields, 13, 2) ? parseFields(first(fields, 13, 2)) : [];
  return {
    service_name: resource["service.name"] || "unknown",
    trace_id: Buffer.from(traceId).toString("hex"),
    span_id: Buffer.from(spanId).toString("hex"),
    parent_span_id: parentId?.length ? Buffer.from(parentId).toString("hex") : null,
    name: safeSpanName(name),
    status_code: Number(first(statusFields, 3, 0) || 0n),
    attributes: attributes(fields, 9),
  };
}
function parseTraceRequest(bytes) {
  const request = parseFields(bytes);
  const spans = [];
  for (const resourceSpan of all(request, 1, 2)) {
    const fields = parseFields(resourceSpan.value);
    const resource = first(fields, 1, 2) ? resourceAttributes(first(fields, 1, 2)) : {};
    for (const scopeSpans of all(fields, 2, 2)) {
      const scope = parseFields(scopeSpans.value);
      for (const span of all(scope, 2, 2)) {
        const parsed = parseSpan(span.value, resource);
        if (parsed) spans.push(parsed);
      }
    }
  }
  return spans;
}

function parseMetricPoint(bytes, kind) {
  const fields = parseFields(bytes);
  return {
    attributes: attributes(fields, kind === "histogram" ? 9 : 7),
    count: kind === "histogram" ? number(first(fields, 4, 0)) : undefined,
    sum: kind === "histogram" ? number(first(fields, 5, 1)) : undefined,
    value: kind === "histogram" ? undefined : (number(first(fields, 6, 0)) ?? number(first(fields, 4, 1))),
  };
}
function parseMetric(bytes, resource) {
  const fields = parseFields(bytes);
  const name = string(first(fields, 1, 2));
  const description = string(first(fields, 2, 2));
  const unit = string(first(fields, 3, 2));
  for (const [fieldNumber, kind] of [[5, "gauge"], [7, "sum"], [9, "histogram"]]) {
    const encoded = first(fields, fieldNumber, 2);
    if (!encoded) continue;
    const data = parseFields(encoded);
    return {
      service_name: resource["service.name"] || "unknown",
      name,
      unit,
      kind,
      description: SAFE_SPAN_NAMES.has(description) ? description : undefined,
      points: all(data, 1, 2).map((point) => parseMetricPoint(point.value, kind)),
    };
  }
  return undefined;
}
function parseMetricRequest(bytes) {
  const request = parseFields(bytes);
  const metrics = [];
  for (const resourceMetric of all(request, 1, 2)) {
    const fields = parseFields(resourceMetric.value);
    const resource = first(fields, 1, 2) ? resourceAttributes(first(fields, 1, 2)) : {};
    for (const scopeMetrics of all(fields, 2, 2)) {
      const scope = parseFields(scopeMetrics.value);
      for (const metric of all(scope, 2, 2)) {
        const parsed = parseMetric(metric.value, resource);
        if (parsed) metrics.push(parsed);
      }
    }
  }
  return metrics;
}

function grpcMessages(chunks) {
  const buffer = Buffer.concat(chunks);
  const messages = [];
  let offset = 0;
  while (offset < buffer.length) {
    if (offset + 5 > buffer.length) throw new Error("incomplete gRPC frame header");
    const compressed = buffer[offset];
    const length = buffer.readUInt32BE(offset + 1);
    offset += 5;
    if (compressed !== 0 || offset + length > buffer.length) throw new Error("invalid OTLP gRPC frame");
    messages.push(buffer.subarray(offset, offset + length));
    offset += length;
  }
  return messages;
}
function grpcFrame(bytes) {
  const header = Buffer.alloc(5);
  header.writeUInt32BE(bytes.length, 1);
  return Buffer.concat([header, bytes]);
}

const config = options(process.argv.slice(2));
const evidence = {
  schema_version: "native-otlp-capture/v1",
  started_at: new Date().toISOString(),
  protocol: "otlp-grpc-h2c",
  receiver: { host: config.host, port: config.port },
  received: { trace_exports: 0, metric_exports: 0 },
  spans: [],
  metrics: [],
  errors: [],
};
async function persist() {
  if (!config.output) return;
  const spanIndex = new Map(evidence.spans.map((span) => [`${span.trace_id}:${span.span_id}`, span]));
  evidence.parent_edges = evidence.spans
    .filter((span) => span.parent_span_id)
    .map((span) => {
      const parent = spanIndex.get(`${span.trace_id}:${span.parent_span_id}`);
      return {
        trace_id: span.trace_id,
        parent_span_id: span.parent_span_id,
        parent_service: parent?.service_name || "not_exported_here",
        parent_name: parent?.name || undefined,
        child_span_id: span.span_id,
        child_service: span.service_name,
        child_name: span.name,
      };
    });
  evidence.services = [...new Set(evidence.spans.map((span) => span.service_name))].sort();
  await writeFile(config.output, `${JSON.stringify(evidence, null, 2)}\n`, { mode: 0o600 });
}

const server = createServer();
server.on("stream", (stream, headers) => {
  const path = headers[":path"];
  const chunks = [];
  stream.on("data", (chunk) => chunks.push(chunk));
  stream.on("error", () => {});
  stream.on("end", async () => {
    let response = Buffer.alloc(0);
    let grpcStatus = "0";
    try {
      const messages = grpcMessages(chunks);
      for (const message of messages) {
        if (path === TRACE_PATH) {
          evidence.spans.push(...parseTraceRequest(message));
          evidence.received.trace_exports += 1;
        } else if (path === METRIC_PATH) {
          evidence.metrics.push(...parseMetricRequest(message));
          evidence.received.metric_exports += 1;
        } else {
          grpcStatus = "12";
          evidence.errors.push({ path_class: "unsupported_grpc_method" });
        }
      }
    } catch {
      grpcStatus = "13";
      evidence.errors.push({ path_class: "invalid_otlp_payload" });
    }
    await persist().catch(() => {});
    stream.respond(
      { ":status": 200, "content-type": "application/grpc", "grpc-encoding": "identity" },
      { waitForTrailers: true },
    );
    stream.on("wantTrailers", () => {
      stream.sendTrailers({ "grpc-status": grpcStatus });
    });
    stream.end(grpcFrame(response));
    process.stdout.write(`${path === TRACE_PATH ? "trace" : path === METRIC_PATH ? "metric" : "other"} export captured; traces=${evidence.spans.length} metrics=${evidence.metrics.length}\n`);
  });
});
server.listen(config.port, config.host, () => {
  process.stdout.write(`OTLP gRPC capture listening at ${config.host}:${config.port}\n`);
  persist().catch(() => {});
});
async function stop() {
  evidence.stopped_at = new Date().toISOString();
  await persist();
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 1000).unref();
}
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
