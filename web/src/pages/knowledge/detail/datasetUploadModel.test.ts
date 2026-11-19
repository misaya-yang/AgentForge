import assert from "node:assert/strict";
import test from "node:test";

import {
  buildDatasetUploadConfigPatch,
  uploadDatasetFiles,
} from "./datasetUploadModel.ts";
import { isDefiniteUploadRejection } from "../create/datasetCreateModel.ts";

interface TestFile {
  name: string;
}

test("existing documents keep immutable ingestion identity during upload", () => {
  assert.deepEqual(
    buildDatasetUploadConfigPatch({
      documentCount: 3,
      chunkingConfig: { mode: "automatic" },
      rerank: { enabled: true, model: "gte-rerank" },
      embedding: {
        changed: true,
        provider: "dashscope",
        model: "text-embedding-v4",
        dimension: 1024,
      },
    }),
    { retrieval_config: { rerank: { enabled: true, model: "gte-rerank" } } }
  );
});

test("empty datasets may establish chunking and embedding identity", () => {
  assert.deepEqual(
    buildDatasetUploadConfigPatch({
      documentCount: 0,
      chunkingConfig: { mode: "heading" },
      rerank: { enabled: false, model: "gte-rerank" },
      embedding: {
        changed: true,
        provider: "dashscope",
        model: "text-embedding-v4",
        dimension: 1024,
      },
    }),
    {
      chunking_config: { mode: "heading" },
      retrieval_config: { rerank: { enabled: false, model: "gte-rerank" } },
      embedding_provider: "dashscope",
      embedding_model: "text-embedding-v4",
      embedding_dimension: 1024,
    }
  );
});

test("single uploads retain only failed files for retry", async () => {
  const files: TestFile[] = [{ name: "accepted.pdf" }, { name: "retry.pdf" }];
  const outcome = await uploadDatasetFiles(files, {
    uploadBatch: async () => ({ accepted: 0, errors: [] }),
    uploadOne: async (file) => {
      if (file.name === "retry.pdf") throw new Error("unsupported format");
    },
    describeError: (error) => (error as Error).message,
    isRetrySafe: () => true,
  });

  assert.equal(outcome.accepted, 1);
  assert.deepEqual(outcome.failures, [
    { file: files[1], error: "unsupported format" },
  ]);
});

test("single upload with unknown outcome is not offered for direct retry", async () => {
  const file: TestFile = { name: "maybe-accepted.txt" };
  const outcome = await uploadDatasetFiles([file], {
    uploadBatch: async () => ({ accepted: 0, errors: [] }),
    uploadOne: async () => { throw new Error("connection lost"); },
    describeError: () => "upload result unknown",
    isRetrySafe: isDefiniteUploadRejection,
  });

  assert.deepEqual(outcome.failures, [{
    file,
    error: "upload result unknown",
    retrySafe: false,
  }]);
  assert.equal(isDefiniteUploadRejection({ response: { status: 413 } }), true);
  assert.equal(isDefiniteUploadRejection({ response: { status: 400 } }), false);
  assert.equal(isDefiniteUploadRejection({ response: { status: 503 } }), false);
  assert.equal(isDefiniteUploadRejection(new Error("timeout")), false);
});

test("batch uploads map server rejections back to retryable files", async () => {
  const files: TestFile[] = [
    { name: "one.pdf" },
    { name: "two.html" },
    { name: "three.docx" },
  ];
  const outcome = await uploadDatasetFiles(files, {
    uploadBatch: async () => ({
      accepted: 2,
      errors: [{ filename: "two.html", error: "parser limit", retry_safe: true }],
    }),
    uploadOne: async () => undefined,
    describeError: () => "unused",
    unknownOutcomeMessage: "Check the document list",
  });

  assert.equal(outcome.accepted, 2);
  assert.deepEqual(outcome.failures, [
    { file: files[1], error: "parser limit" },
  ]);
});

test("batch failures use file position when names repeat", async () => {
  const files: TestFile[] = [
    { name: "same.txt" },
    { name: "same.txt" },
    { name: "other.txt" },
  ];
  const outcome = await uploadDatasetFiles(files, {
    uploadBatch: async () => ({
      accepted: 2,
      errors: [{ file_index: 1, filename: "same.txt", error: "empty file", retry_safe: true }],
    }),
    uploadOne: async () => undefined,
    describeError: () => "unused",
  });

  assert.equal(outcome.accepted, 2);
  assert.deepEqual(outcome.failures, [{ file: files[1], error: "empty file" }]);
});

test("created document with rejected enqueue is not offered for file reupload", async () => {
  const files: TestFile[] = [
    { name: "one.txt" },
    { name: "two.txt" },
    { name: "three.txt" },
  ];
  const outcome = await uploadDatasetFiles(files, {
    uploadBatch: async () => ({
      accepted: 2,
      errors: [{
        file_index: 1,
        filename: "two.txt",
        error: "queue unavailable",
        document_id: "created-document",
        retry_safe: false,
      }],
    }),
    uploadOne: async () => undefined,
    describeError: () => "unused",
    unknownOutcomeMessage: "Check the document list",
  });

  assert.deepEqual(outcome.failures, [{
    file: files[1],
    error: "Check the document list",
    documentId: "created-document",
    retrySafe: false,
  }]);
});

test("batch failure without an explicit safe retry receipt stays uncertain", async () => {
  const files: TestFile[] = [
    { name: "one.txt" },
    { name: "maybe-created.txt" },
    { name: "three.txt" },
  ];
  const outcome = await uploadDatasetFiles(files, {
    uploadBatch: async () => ({
      accepted: 2,
      errors: [{ file_index: 1, filename: "maybe-created.txt", error: "connection lost" }],
    }),
    uploadOne: async () => undefined,
    describeError: () => "unused",
    unknownOutcomeMessage: "Check the document list",
  });
  assert.deepEqual(outcome.failures, [{
    file: files[1],
    error: "Check the document list",
    retrySafe: false,
  }]);
});

test("batch rejects a mismatched file position even when a name exists", async () => {
  await assert.rejects(
    uploadDatasetFiles(
      [{ name: "same.txt" }, { name: "same.txt" }, { name: "other.txt" }],
      {
        uploadBatch: async () => ({
          accepted: 2,
          errors: [{ file_index: 2, filename: "same.txt", error: "invalid" }],
        }),
        uploadOne: async () => undefined,
        describeError: () => "unused",
      }
    ),
    /do not match/
  );
});

test("batch receipt mismatch fails closed instead of losing a source", async () => {
  await assert.rejects(
    uploadDatasetFiles(
      [{ name: "one.pdf" }, { name: "two.pdf" }, { name: "three.pdf" }],
      {
        uploadBatch: async () => ({
          accepted: 2,
          errors: [{ filename: "not-submitted.pdf", error: "rejected" }],
        }),
        uploadOne: async () => undefined,
        describeError: () => "unused",
      }
    ),
    /do not match/
  );
});

test("batch receipt count mismatch also fails closed", async () => {
  await assert.rejects(
    uploadDatasetFiles(
      [{ name: "one.pdf" }, { name: "two.pdf" }, { name: "three.pdf" }],
      {
        uploadBatch: async () => ({ accepted: 1, errors: [] }),
        uploadOne: async () => undefined,
        describeError: () => "unused",
      }
    ),
    /do not match/
  );
});
