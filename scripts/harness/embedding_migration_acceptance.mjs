#!/usr/bin/env node
// Public-API, synthetic embedding migration acceptance. Dry-run by default.
// Never reads database credentials or reports document text/provider response bodies.
import fs from 'node:fs/promises';
import crypto from 'node:crypto';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';

const SECRETS = join(homedir(), '.cache/agentforge/secrets');
const origin = (process.env.NATIVE_GATEWAY_ORIGIN ?? 'http://127.0.0.1:8080').replace(/\/$/, '');
const credentialsPath = process.env.NATIVE_ADMIN_CREDENTIALS ?? join(SECRETS, 'local-test-admin.json');
const cachedLoginPath = process.env.NATIVE_LOGIN_CACHE ?? join(SECRETS, 'native-current-login.json');
const resumePath = process.env.NATIVE_EMBEDDING_MIGRATION_RESUME_REPORT;
const reportPath = resolve(process.env.NATIVE_EMBEDDING_MIGRATION_REPORT ?? resumePath ?? 'reports/native-current-acceptance-20261016/embedding-migration-real.json');
const templateDataset = process.env.NATIVE_EMBEDDING_TEMPLATE_DATASET ?? 'native-knowledge-acceptance-20261016';
const controlProvider = process.env.NATIVE_EMBEDDING_CONTROL_PROVIDER ?? 'dashscope';
const controlModel = process.env.NATIVE_EMBEDDING_CONTROL_MODEL ?? 'text-embedding-v4';
const controlDimension = Number(process.env.NATIVE_EMBEDDING_CONTROL_DIMENSION ?? '1024');
const targetProvider = process.env.NATIVE_EMBEDDING_TARGET_PROVIDER ?? 'dashscope';
const targetModel = process.env.NATIVE_EMBEDDING_TARGET_MODEL ?? 'text-embedding-v4';
const targetDimension = Number(process.env.NATIVE_EMBEDDING_TARGET_DIMENSION ?? '768');
const REQUEST_TIMEOUT_MS = 90_000;
const OPERATION_TIMEOUT_MS = 10 * 60_000;
const sleep = ms => new Promise(resolveSleep => setTimeout(resolveSleep, ms));
const sha256 = value => crypto.createHash('sha256').update(value, 'utf8').digest('hex');

class RunnerFailure extends Error {
  constructor(details) { super('embedding migration acceptance failed'); this.name = 'RunnerFailure'; this.details = details; }
}
const assert = (ok, code, extra = {}) => { if (!ok) throw new RunnerFailure({ kind: 'assertion', code, ...extra }); };
const safeStatus = value => String(value ?? '').toLowerCase().replace(/[^a-z0-9_-]/g, '').slice(0, 40) || null;
function safeCode(body) {
  const code = body?.detail?.code ?? body?.code ?? body?.error?.code;
  return typeof code === 'string' && /^[A-Za-z0-9_.-]{1,100}$/.test(code) ? code : null;
}
function safeFailure(error) {
  return error instanceof RunnerFailure
    ? error.details
    : { kind: 'runner', code: String(error?.safeCode ?? error?.name ?? 'unexpected_error').replace(/[^A-Za-z0-9_.-]/g, '_').slice(0, 80), http_status: Number.isInteger(error?.status) ? error.status : null };
}
function originUrl(value) {
  let parsed;
  try { parsed = new URL(value); } catch { throw new RunnerFailure({ kind: 'usage', code: 'invalid_gateway_origin' }); }
  const isLoopback = ['localhost', '127.0.0.1', '::1'].includes(parsed.hostname);
  assert(parsed.protocol === 'https:' || (isLoopback && parsed.protocol === 'http:'), 'gateway_origin_must_be_https_or_loopback');
  assert(!parsed.username && !parsed.password && !parsed.search && !parsed.hash, 'gateway_origin_must_not_include_credentials_or_query');
  return `${parsed.protocol}//${parsed.host}`;
}
const base = originUrl(origin);
let token = null;
let authRefreshes = 0;
let stage = 'preflight';
let resumedReport = null;
if (resumePath) resumedReport = JSON.parse(await fs.readFile(resumePath, 'utf8'));
const runId = resumedReport?.dataset_id?.split('-').at(-1) ?? crypto.randomUUID().replaceAll('-', '').slice(0, 10);
const dataset = resumedReport?.dataset_id ?? `native-embedding-migration-${new Date().toISOString().slice(0, 10).replaceAll('-', '')}-${runId}`;
const marker = resumedReport?.markers?.primary ?? `EMBED_MIGRATION_NATIVE_${runId.toUpperCase()}`;
const cancelMarker = resumedReport?.markers?.cancellation ?? `EMBED_MIGRATION_CANCEL_${runId.toUpperCase()}`;
const firstContent = `${marker}. The amber kite migration control record is owned by Cedar Wharf team. The release review is scheduled for November 18, 2026. Reference code E5-Control-${runId}.`;
const cancelContent = Array.from({ length: 180 }, (_, index) => `${cancelMarker} section ${index + 1}: the silver otter recovery record remains owned by Juniper Gate team; its public index is retained until the replacement generation passes checks. This independent synthetic paragraph verifies cancellation fencing and continued service of the original collection.`).join('\n\n');
const report = resumedReport ?? {
  schema: 'native-public-embedding-migration-acceptance/v1',
  dataset_id: dataset,
  markers: { primary: marker, cancellation: cancelMarker },
  control: { provider: controlProvider, model: controlModel, dimension: controlDimension },
  target: { provider: targetProvider, model: targetModel, dimension: targetDimension },
  started_at: new Date().toISOString(),
  stages: [],
  auth_refreshes: 0,
};
if (!Array.isArray(report.retrievals)) report.retrievals = [];
if (resumedReport) {
  delete report.failure;
  delete report.overall;
  delete report.finished_at;
  report.resumed_at = new Date().toISOString();
}
async function writeReport() {
  report.auth_refreshes = authRefreshes;
  await fs.mkdir(resolve(reportPath, '..'), { recursive: true });
  await fs.writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o644 });
  await fs.chmod(reportPath, 0o644);
}
async function login() {
  const c = JSON.parse(await fs.readFile(credentialsPath, 'utf8'));
  const response = await fetch(`${base}/api/v1/auth/login`, {
    method: 'POST', headers: { 'content-type': 'application/json', accept: 'application/json' },
    body: JSON.stringify({ email: c.email, password: c.password }), signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
  });
  const body = await response.json().catch(() => null);
  if (!response.ok || typeof body?.access_token !== 'string') {
    throw Object.assign(new Error('login_failed'), { status: response.status, safeCode: safeCode(body), name: 'LoginError' });
  }
  token = body.access_token;
  authRefreshes += 1;
}
async function loadCachedToken() {
  try {
    const cached = JSON.parse(await fs.readFile(cachedLoginPath, 'utf8'));
    if (typeof cached?.body?.access_token === 'string') token = cached.body.access_token;
  } catch { /* Login on the first 401 or if the cache is absent. */ }
}
async function api(method, route, payload, retryAuth = true) {
  const headers = { accept: 'application/json' };
  if (token) headers.authorization = `Bearer ${token}`;
  if (payload !== undefined) headers['content-type'] = 'application/json';
  let response;
  try {
    response = await fetch(new URL(route, base), {
      method, headers, body: payload === undefined ? undefined : JSON.stringify(payload),
      signal: AbortSignal.timeout(REQUEST_TIMEOUT_MS),
    });
  } catch (error) {
    throw Object.assign(new Error('api_transport'), { name: error?.name ?? 'FetchError' });
  }
  const body = await response.json().catch(() => null);
  if (response.status === 401 && retryAuth) {
    await login();
    return api(method, route, payload, false);
  }
  if (!response.ok) {
    throw Object.assign(new Error('api_failed'), { status: response.status, safeCode: safeCode(body), name: 'ApiError' });
  }
  return { status: response.status, body };
}
const dsPath = id => `/api/v1/knowledge/datasets/${encodeURIComponent(id)}`;
const docPath = (ds, id) => `/api/v1/knowledge/${encodeURIComponent(ds)}/documents/${encodeURIComponent(id)}`;
const migrationPath = ds => `${dsPath(ds)}/embedding-migration`;
function bindingSummary(binding) {
  if (!binding || typeof binding !== 'object') return null;
  return {
    binding_id: binding.binding_id ?? null,
    state: safeStatus(binding.state),
    collection_name: binding.collection_name ?? null,
    provider: binding.embedding_provider ?? null,
    model: binding.embedding_model ?? null,
    model_version: binding.embedding_model_version ?? null,
    dimension: Number(binding.embedding_dimension ?? 0) || null,
  };
}
function migrationSummary(value) {
  const migration = value?.live_migration ?? value?.latest_migration ?? value?.migration ?? null;
  return migration ? {
    migration_id: migration.migration_id ?? null,
    state: safeStatus(migration.state),
    error_present: typeof migration.error === 'string' && migration.error.length > 0,
    pending_chunks: Number(value?.pending_chunks ?? migration?.totals?.remaining ?? 0),
  } : null;
}
function describeSummary(value) {
  return {
    serving_binding: bindingSummary(value?.serving_binding),
    source_binding: bindingSummary(value?.source_binding),
    target_binding: bindingSummary(value?.target_binding),
    migration: migrationSummary(value),
    pending_chunks: Number(value?.pending_chunks ?? 0),
    enabled_chunks: Number(value?.enabled_chunks ?? 0),
    collection_health: safeStatus(value?.collection_health?.status),
    recent_job_states: Array.isArray(value?.recent_action_jobs) ? value.recent_action_jobs.slice(0, 4).map(j => ({ action: safeStatus(j.action), state: safeStatus(j.state) })) : [],
  };
}
async function describe() {
  return (await api('GET', migrationPath(dataset))).body;
}
async function waitForDocument(id, timeoutMs = OPERATION_TIMEOUT_MS) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const row = (await api('GET', docPath(dataset, id))).body;
    const state = safeStatus(row?.status ?? row?.display_status);
    if (state === 'completed') return row;
    if (state === 'error') throw new RunnerFailure({ kind: 'processing', code: 'document_ingestion_failed', stage });
    await sleep(750);
  }
  throw new RunnerFailure({ kind: 'timeout', code: 'document_ingestion_timeout', stage });
}
async function createText(title, content) {
  const response = await api('POST', `/api/v1/knowledge/${encodeURIComponent(dataset)}/documents/text`, { title, content, metadata: {} });
  const id = response.body?.document_id;
  assert(typeof id === 'string' && id.length > 0, 'create_document_missing_id', { stage });
  await waitForDocument(id);
  return id;
}
async function currentSource(id) {
  const prefix = docPath(dataset, id);
  const [doc, source, versions] = await Promise.all([
    api('GET', prefix), api('GET', `${prefix}/source`), api('GET', `${prefix}/versions?limit=100&offset=0`),
  ]);
  const version = Number(versions.body?.current_version ?? doc.body?.current_version ?? 0);
  assert(version > 0 && typeof source.body?.content === 'string', 'active_source_missing', { stage });
  const contentHash = sha256(source.body.content);
  const versionRow = (await api('GET', `${prefix}/versions/${version}`)).body;
  assert(String(versionRow?.content_hash ?? '').toLowerCase() === contentHash, 'active_version_hash_mismatch', { stage });
  return { prefix, document: doc.body, source: source.body, version, contentHash, versionRow };
}
async function historicalSource(id, version, hash) {
  const query = new URLSearchParams({ hash });
  return (await api('GET', `${docPath(dataset, id)}/versions/${version}/source?${query}`)).body;
}
async function retrieveAndCheck(label, id, contentMarker, expectedVersion, expectedHash) {
  const query = `${contentMarker} who owns this migration record ${crypto.randomUUID()}`;
  const response = await api('POST', `/api/v1/knowledge/${encodeURIComponent(dataset)}/retrieve`, {
    query, top_k: 5, mode: 'dense', rerank: false, hierarchical: false, include_context: true, document_id: id,
  });
  const rows = Array.isArray(response.body?.results) ? response.body.results : [];
  const hits = rows.filter(row => String(row?.document_id ?? row?.metadata?.document_id ?? '') === id);
  const bindings = hits.map(row => ({
    version: Number(row?.source_version ?? row?.metadata?.source_version ?? 0) || null,
    hash: String(row?.source_hash ?? row?.metadata?.source_hash ?? '').toLowerCase() || null,
    marker_present: JSON.stringify(row).includes(contentMarker),
  }));
  const hit = bindings.find(value => value.version === expectedVersion && value.hash === expectedHash);
  assert(hit, `${label}_dense_retrieval_source_binding_mismatch`, { stage, results: rows.length, document_hits: hits.length });
  assert(hit.marker_present, `${label}_dense_retrieval_marker_missing`, { stage });
  const exact = await historicalSource(id, expectedVersion, expectedHash);
  assert(typeof exact?.content === 'string' && sha256(exact.content) === expectedHash && exact.source_hash === expectedHash, `${label}_historical_source_mismatch`, { stage });
  const item = {
    label, http_status: response.status, mode: safeStatus(response.body?.metadata?.mode),
    result_count: rows.length, document_hit_count: hits.length,
    source_version: expectedVersion, source_hash: expectedHash, marker_present: hit.marker_present,
    exact_public_source_http_status: 200, exact_public_source_hash_match: true,
    active_version: Number(response.body?.metadata?.current_version ?? 0) || null,
  };
  report.retrievals.push(item);
  await writeReport();
  return item;
}
async function waitForDocumentExecution(id, timeoutMs = OPERATION_TIMEOUT_MS) {
  return waitForDocument(id, timeoutMs);
}
async function enqueueAction(migrationId, action, payload = {}) {
  const response = await api('POST', `${migrationPath(dataset)}/${encodeURIComponent(migrationId)}/${action}`, payload);
  const job = response.body;
  assert(response.status === 202 && typeof job?.execution_id === 'string' && typeof job?.job_url === 'string', `${action}_enqueue_contract_mismatch`, { stage, http_status: response.status });
  return { response, job };
}
async function pollJob(jobUrl, timeoutMs = OPERATION_TIMEOUT_MS) {
  const started = Date.now();
  let last = null;
  while (Date.now() - started < timeoutMs) {
    last = (await api('GET', jobUrl)).body;
    const state = safeStatus(last?.state);
    if (state === 'succeeded' || state === 'failed') return last;
    await sleep(500);
  }
  throw new RunnerFailure({ kind: 'timeout', code: 'migration_job_timeout', stage, job_state: safeStatus(last?.state) });
}
function jobSummary(job) {
  const result = job?.result && typeof job.result === 'object' ? job.result : {};
  return {
    job_id: job?.job_id ?? null,
    action: safeStatus(job?.action),
    state: safeStatus(job?.state),
    attempt_count: Number(job?.attempt_count ?? 0),
    error_present: typeof job?.error === 'string' && job.error.length > 0,
    operator_cancelled: job?.error === 'cancelled by operator',
    result_migration_state: safeStatus(result?.migration?.state),
    passed: typeof result?.passed === 'boolean' ? result.passed : typeof result?.verdict?.passed === 'boolean' ? result.verdict.passed : null,
    remaining: Number(result?.remaining ?? result?.migration?.totals?.remaining ?? 0),
  };
}
async function main() {
  const args = new Set(process.argv.slice(2));
  if (args.has('--help')) {
    process.stdout.write([
      'Usage: node scripts/harness/embedding_migration_acceptance.mjs [--execute]',
      'Dry-run by default. --execute creates a fresh private synthetic dataset,',
      'or resumes NATIVE_EMBEDDING_MIGRATION_RESUME_REPORT if supplied.',
      'migrates DashScope text-embedding-v4 1024→768, retrieves, rolls back,',
      'then exercises action cancellation and abort against a new synthetic item.',
      'Optional environment: NATIVE_GATEWAY_ORIGIN, NATIVE_ADMIN_CREDENTIALS,',
      'NATIVE_LOGIN_CACHE, NATIVE_EMBEDDING_MIGRATION_REPORT,',
      'NATIVE_EMBEDDING_TEMPLATE_DATASET, NATIVE_EMBEDDING_CONTROL_*, NATIVE_EMBEDDING_TARGET_*.',
    ].join('\n') + '\n');
    return;
  }
  assert([...args].every(arg => arg === '--execute'), 'unknown_argument', { kind: 'usage' });
  if (!args.has('--execute')) {
    process.stdout.write(JSON.stringify({
      status: 'planned', gateway_origin: base, target: report.target,
      checks: ['isolated synthetic dataset and document', 'public describe/start/backfill/verify/gate/cutover', 'dense retrieval with exact version/hash source', 'rollback and serving-binding check', 'separate action cancel plus migration abort', 'old serving retrieval remains available'],
    }) + '\n');
    return;
  }
  assert(controlProvider === 'dashscope' && targetProvider === 'dashscope', 'acceptance_requires_dashscope_control_and_target');
  assert(controlModel === 'text-embedding-v4' && targetModel === controlModel, 'acceptance_requires_same_verified_model');
  assert(controlDimension === 1024 && targetDimension === 768, 'acceptance_dimension_pair_must_be_1024_to_768');
  await loadCachedToken();
  await writeReport();

  let primaryId = report.primary_document_id;
  let primary;
  if (!primaryId) {
    stage = 'create_dataset';
    const created = await api('POST', '/api/v1/knowledge/datasets', {
      dataset_id: dataset,
      name: `Embedding migration acceptance ${runId}`,
      description: 'Synthetic public-API migration acceptance. No production content.',
      visibility: 'private',
      embedding_provider: controlProvider,
      embedding_model: controlModel,
      embedding_dimension: controlDimension,
      embedding_config: {},
      index_config: {},
    });
    report.dataset = {
      http_status: created.status,
      dataset_id: created.body?.dataset_id ?? dataset,
      source_collection: created.body?.collection_name ?? null,
      source_identity: { provider: created.body?.embedding_provider ?? controlProvider, model: created.body?.embedding_model ?? controlModel, dimension: Number(created.body?.embedding_dimension ?? controlDimension) },
    };
    report.stages.push({ name: 'create_private_synthetic_dataset', status: 'passed' });
    await writeReport();

    stage = 'create_control_document';
    primaryId = await createText(`Embedding control ${runId}`, firstContent);
    report.primary_document_id = primaryId;
    primary = await currentSource(primaryId);
    report.control_document = { document_id: primaryId, current_version: primary.version, content_sha256: primary.contentHash, content_bytes: Buffer.byteLength(primary.source.content, 'utf8') };
    report.stages.push({ name: 'publish_control_v4_1024', status: 'passed' });
    await writeReport();
  } else {
    stage = 'resume_control';
    primary = await currentSource(primaryId);
    assert(primary.contentHash === report.control_document?.content_sha256, 'resumed_control_source_changed');
  }
  stage = 'retrieve_control';
  const initialDescription = await describe();
  const initialBinding = bindingSummary(initialDescription?.serving_binding);
  assert(initialBinding?.provider === controlProvider && initialBinding?.model === controlModel && initialBinding?.dimension === controlDimension, 'control_serving_binding_mismatch');
  report.initial_description = describeSummary(initialDescription);
  const controlRetrieval = await retrieveAndCheck('control_before_migration', primaryId, marker, primary.version, primary.contentHash);
  report.stages.push({ name: 'control_dense_retrieval', status: 'passed', mode: controlRetrieval.mode });
  await writeReport();

  stage = 'start_migration';
  const started = await api('POST', `${migrationPath(dataset)}/start`, {
    embedding_provider: targetProvider,
    embedding_model: targetModel,
    embedding_model_version: '',
    embedding_dimension: targetDimension,
  });
  const migrationId = started.body?.migration?.migration_id ?? started.body?.migration_id;
  assert(typeof migrationId === 'string' && migrationId.length > 0, 'start_missing_migration_id');
  report.migration_id = migrationId;
  report.start = { http_status: started.status, migration_id: migrationId, migration_state: safeStatus(started.body?.migration?.state), target_binding: bindingSummary(started.body?.target_binding) };
  report.stages.push({ name: 'describe_and_start_target_v4_768', status: 'passed' });
  await writeReport();

  stage = 'backfill';
  const backfill = await enqueueAction(migrationId, 'backfill');
  const backfillJob = await pollJob(backfill.job.job_url);
  report.backfill_job = jobSummary(backfillJob);
  assert(backfillJob.state === 'succeeded', 'backfill_job_failed', { stage });
  const afterBackfill = await describe();
  report.after_backfill = describeSummary(afterBackfill);
  assert(report.after_backfill.pending_chunks === 0, 'backfill_left_pending_chunks', { stage, pending_chunks: report.after_backfill.pending_chunks });
  report.stages.push({ name: 'backfill_job_and_progress', status: 'passed' });
  await writeReport();

  stage = 'verify';
  const verify = await enqueueAction(migrationId, 'verify');
  const verifyJob = await pollJob(verify.job.job_url);
  report.verify_job = jobSummary(verifyJob);
  assert(verifyJob.state === 'succeeded', 'verify_job_failed', { stage });
  const afterVerify = await describe();
  report.after_verify = describeSummary(afterVerify);
  assert(report.after_verify.migration?.state === 'verified', 'verify_did_not_set_verified', { stage });
  report.stages.push({ name: 'verify_job', status: 'passed' });
  await writeReport();

  stage = 'gate';
  const gate = await enqueueAction(migrationId, 'gate', { sample_size: 1, top_k: 1, tolerance: 0.1, floor: 0.8 });
  const gateJob = await pollJob(gate.job.job_url);
  report.gate_job = jobSummary(gateJob);
  assert(gateJob.state === 'succeeded' && report.gate_job.passed === true, 'gate_did_not_pass', { stage });
  const afterGate = await describe();
  report.after_gate = describeSummary(afterGate);
  assert(report.after_gate.migration?.state === 'ready', 'gate_did_not_set_ready', { stage });
  report.stages.push({ name: 'evaluation_gate', status: 'passed' });
  await writeReport();

  stage = 'cutover';
  const cutover = await api('POST', `${migrationPath(dataset)}/${encodeURIComponent(migrationId)}/cutover`, { retention_seconds: 86_400 });
  report.cutover = { http_status: cutover.status, migration_state: safeStatus(cutover.body?.migration?.state) };
  const afterCutover = await describe();
  report.after_cutover = describeSummary(afterCutover);
  assert(report.after_cutover.serving_binding?.dimension === targetDimension && report.after_cutover.serving_binding?.model === targetModel, 'target_not_serving_after_cutover', { stage });
  report.stages.push({ name: 'cutover_target_768', status: 'passed' });
  await writeReport();

  stage = 'retrieve_cutover';
  const targetRetrieval = await retrieveAndCheck('target_v4_768_after_cutover', primaryId, marker, primary.version, primary.contentHash);
  report.stages.push({ name: 'target_dense_retrieval_and_exact_source', status: 'passed', mode: targetRetrieval.mode });
  await writeReport();

  stage = 'rollback';
  const rolled = await api('POST', `${migrationPath(dataset)}/${encodeURIComponent(migrationId)}/rollback`, { keep_shadow: true });
  report.rollback = { http_status: rolled.status, migration_state: safeStatus(rolled.body?.migration?.state) };
  const afterRollback = await describe();
  report.after_rollback = describeSummary(afterRollback);
  assert(report.after_rollback.migration?.state === 'rolled_back' && report.after_rollback.serving_binding?.dimension === controlDimension && report.after_rollback.serving_binding?.model === controlModel, 'source_not_restored_after_rollback', { stage });
  report.stages.push({ name: 'rollback_to_v4_1024', status: 'passed' });
  await writeReport();

  stage = 'retrieve_rollback';
  const rollbackRetrieval = await retrieveAndCheck('control_after_rollback', primaryId, marker, primary.version, primary.contentHash);
  report.stages.push({ name: 'rollback_dense_retrieval_and_exact_source', status: 'passed', mode: rollbackRetrieval.mode });
  await writeReport();

  stage = 'create_abort_control_document';
  const cancelId = await createText(`Embedding cancellation control ${runId}`, cancelContent);
  const cancelSource = await currentSource(cancelId);
  report.cancel_control_document = { document_id: cancelId, current_version: cancelSource.version, content_sha256: cancelSource.contentHash, content_bytes: Buffer.byteLength(cancelSource.source.content, 'utf8') };
  const preCancelDescribe = await describe();
  assert(bindingSummary(preCancelDescribe?.serving_binding)?.dimension === controlDimension, 'source_serving_changed_before_cancel_test');
  const beforeCancelRetrieval = await retrieveAndCheck('control_before_cancel_abort', cancelId, cancelMarker, cancelSource.version, cancelSource.contentHash);
  report.stages.push({ name: 'publish_second_synthetic_item_on_original_binding', status: 'passed', mode: beforeCancelRetrieval.mode });
  await writeReport();

  stage = 'reopen_for_cancel_abort';
  const reopened = await api('POST', `${migrationPath(dataset)}/start`, {
    embedding_provider: targetProvider, embedding_model: targetModel,
    embedding_model_version: '', embedding_dimension: targetDimension,
  });
  assert(reopened.body?.migration?.migration_id === migrationId && reopened.body?.reopened === true, 'rolled_back_migration_not_reopened');
  report.reopen = { migration_id: migrationId, reopened: reopened.body?.reopened === true, migration_state: safeStatus(reopened.body?.migration?.state) };
  await writeReport();

  stage = 'cancel_job';
  const cancelBackfill = await enqueueAction(migrationId, 'backfill');
  let cancelledAction = 'backfill';
  let cancelJobUrl = cancelBackfill.job.job_url;
  let cancelResponse;
  try {
    cancelResponse = await api('DELETE', cancelJobUrl);
  } catch (error) {
    if (error?.status !== 409) throw error;
    const latest = (await api('GET', cancelJobUrl)).body;
    report.cancel_race = { initial_action: 'backfill', terminal_state_before_cancel: safeStatus(latest?.state) };
    if (latest?.state === 'succeeded') {
      const verifyCancel = await enqueueAction(migrationId, 'verify');
      cancelledAction = 'verify';
      cancelJobUrl = verifyCancel.job.job_url;
      cancelResponse = await api('DELETE', cancelJobUrl);
    } else {
      throw error;
    }
  }
  const cancelledJob = await pollJob(cancelJobUrl);
  report.cancel_job = { action: cancelledAction, http_status: cancelResponse.status, ...jobSummary(cancelledJob) };
  assert(cancelledJob.state === 'failed' && cancelledJob.error === 'cancelled by operator', 'action_cancel_not_terminal', { stage, action: cancelledAction, state: safeStatus(cancelledJob.state) });
  report.stages.push({ name: 'cancel_action_terminal', status: 'passed', action: cancelledAction });
  await writeReport();

  stage = 'abort_migration';
  const aborted = await api('POST', `${migrationPath(dataset)}/${encodeURIComponent(migrationId)}/abort`, { reason: 'synthetic public-API cancellation and abort acceptance', purge_shadow: true });
  report.abort = { http_status: aborted.status, migration_state: safeStatus(aborted.body?.migration?.state) };
  const afterAbort = await describe();
  report.after_abort = describeSummary(afterAbort);
  assert(report.abort.migration_state === 'abandoned' && report.after_abort.serving_binding?.dimension === controlDimension && report.after_abort.serving_binding?.model === controlModel, 'abort_did_not_leave_original_binding_serving', { stage });
  report.stages.push({ name: 'abort_shadow_and_keep_original_serving', status: 'passed' });
  await writeReport();

  stage = 'retrieve_after_abort';
  const afterAbortRetrieval = await retrieveAndCheck('original_binding_after_abort', cancelId, cancelMarker, cancelSource.version, cancelSource.contentHash);
  report.stages.push({ name: 'original_dense_retrieval_after_abort', status: 'passed', mode: afterAbortRetrieval.mode });
  report.finished_at = new Date().toISOString();
  report.overall = 'passed';
  await writeReport();
  process.stdout.write(JSON.stringify({ overall: report.overall, report_path: reportPath, dataset_id: dataset, primary_document_id: primaryId, cancellation_document_id: cancelId, migration_id: migrationId, target: report.target, cutover_state: report.cutover.migration_state, rollback_state: report.rollback.migration_state, cancel_job_state: report.cancel_job.state, abort_state: report.abort.migration_state, retrievals: report.retrievals.map(x => ({ label: x.label, mode: x.mode, version: x.source_version, sha256: x.source_hash, source_ok: x.exact_public_source_hash_match })) }) + '\n');
}

try {
  if (process.argv.includes('--help')) {
    process.stdout.write('Use --execute for one fresh, isolated public-API embedding migration acceptance.\n');
  } else if (process.argv.includes('--execute')) {
    await main();
  } else {
    process.stdout.write(JSON.stringify({ status: 'planned', gateway_origin: base, target: report.target, requires_execute_flag: true }) + '\n');
  }
} catch (error) {
  report.failure = { stage, ...safeFailure(error) };
  report.finished_at = new Date().toISOString();
  report.overall = 'failed';
  await writeReport();
  process.stdout.write(JSON.stringify({ overall: report.overall, report_path: reportPath, dataset_id: dataset, failure: report.failure }) + '\n');
  process.exitCode = 1;
}
