import assert from "node:assert/strict";
import test from "node:test";

import {
  MAX_FILE_SIZE,
  getSourceUploadError,
  listFailedSources,
  type PendingFile,
  type PendingUrl,
} from "./datasetCreateModel.ts";

test("knowledge create validates against the deployed 16 MiB default", () => {
  assert.equal(MAX_FILE_SIZE, 16 * 1024 * 1024);
});

test("upload errors turn an ingress 413 into an actionable reason", () => {
  assert.equal(
    getSourceUploadError(
      { response: { status: 413, data: "<html>Request Entity Too Large</html>" } },
      { fallback: "Upload failed", requestTooLarge: "Upload request is too large" }
    ),
    "Upload request is too large"
  );
});

test("upload errors preserve a structured service detail", () => {
  assert.equal(
    getSourceUploadError(
      { response: { status: 400, data: { detail: "PDF is encrypted" } } },
      { fallback: "Upload failed", requestTooLarge: "Upload request is too large" }
    ),
    "PDF is encrypted"
  );
});

test("failed source summaries include only failed filenames and reasons", () => {
  const files: PendingFile[] = [
    {
      id: "failed-pdf",
      file: {} as File,
      name: "LLM Reinforcement Learning Guide.pdf",
      size: 1_477_915,
      status: "error",
      error: "Upload request is too large",
    },
    {
      id: "accepted-pdf",
      file: {} as File,
      name: "Anthropic Agent.pdf",
      size: 964_279,
      status: "done",
    },
  ];
  const urls: PendingUrl[] = [
    {
      id: "failed-url",
      url: "https://example.com/paper",
      title: "Paper",
      status: "error",
      error: "Fetch timed out",
    },
  ];

  assert.deepEqual(listFailedSources(files, urls, "Upload failed"), [
    {
      key: "file:failed-pdf",
      name: "LLM Reinforcement Learning Guide.pdf",
      error: "Upload request is too large",
    },
    {
      key: "url:failed-url",
      name: "Paper",
      error: "Fetch timed out",
    },
  ]);
});
