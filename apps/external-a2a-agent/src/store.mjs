import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { mkdirSync, chmodSync, lstatSync, openSync, closeSync, unlinkSync, writeSync } from 'node:fs';
import { join } from 'node:path';
import { reject } from './contract.mjs';
export const TERMINAL = new Set(['TASK_STATE_COMPLETED', 'TASK_STATE_FAILED', 'TASK_STATE_REJECTED', 'TASK_STATE_CANCELED', 'TASK_STATE_UNSPECIFIED']);
export class Store {
  constructor(directory, { now = () => Date.now() } = {}) {
    mkdirSync(directory, { recursive: true, mode: 0o700 }); const d = lstatSync(directory); if (!d.isDirectory() || d.isSymbolicLink() || (d.mode & 0o077)) reject(503, 'STATE_DIRECTORY_NOT_PRIVATE');
    this.lease = join(directory, 'process-owner.lock'); try { const fd = openSync(this.lease, 'wx', 0o600); try { writeSync(fd, String(process.pid)); } finally { closeSync(fd); } } catch { reject(503, 'STATE_ALREADY_OWNED_OR_RECOVERY_REQUIRED'); }
    this.now = now; const file = join(directory, 'tasks.sqlite'); try { const m = lstatSync(file); if (!m.isFile() || m.isSymbolicLink() || (m.mode & 0o077)) reject(503, 'STATE_FILE_NOT_PRIVATE'); } catch (e) { if (e.code !== 'ENOENT') throw e; }
    this.db = new DatabaseSync(file); chmodSync(file, 0o600);
    this.db.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;
      CREATE TABLE IF NOT EXISTS contexts(id TEXT PRIMARY KEY,tenant TEXT NOT NULL,actor TEXT NOT NULL,agent TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY,context TEXT NOT NULL,tenant TEXT NOT NULL,actor TEXT NOT NULL,agent TEXT NOT NULL,message TEXT NOT NULL,request_hash TEXT NOT NULL,state TEXT NOT NULL,admission TEXT NOT NULL DEFAULT 'production',profile_sha TEXT,permit_sha TEXT,cancel_requested INTEGER NOT NULL DEFAULT 0,output TEXT,error_code TEXT,framework TEXT,framework_calls INTEGER,tool_calls INTEGER,created INTEGER NOT NULL,updated INTEGER NOT NULL,UNIQUE(tenant,actor,agent,message));
      CREATE TABLE IF NOT EXISTS nonces(id TEXT PRIMARY KEY,expires INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS capability_calls(task TEXT NOT NULL,request_id TEXT PRIMARY KEY,name TEXT NOT NULL,request_hash TEXT NOT NULL,effect TEXT NOT NULL,http_status INTEGER,FOREIGN KEY(task) REFERENCES tasks(id));
      CREATE TABLE IF NOT EXISTS model_calls(task TEXT NOT NULL,sequence INTEGER NOT NULL,request_id TEXT NOT NULL,request_hash TEXT NOT NULL,effect TEXT NOT NULL,gateway_request_id TEXT,response_id TEXT,input_tokens INTEGER,output_tokens INTEGER,http_status INTEGER,PRIMARY KEY(task,sequence),FOREIGN KEY(task) REFERENCES tasks(id));`);
    if (!this.db.prepare('PRAGMA table_info(tasks)').all().some(x => x.name === 'admission')) this.db.exec("ALTER TABLE tasks ADD COLUMN admission TEXT NOT NULL DEFAULT 'production'");
    if (!this.db.prepare('PRAGMA table_info(model_calls)').all().some(x => x.name === 'gateway_request_id')) this.db.exec('ALTER TABLE model_calls ADD COLUMN gateway_request_id TEXT');
    if (!this.db.prepare('PRAGMA table_info(tasks)').all().some(x => x.name === 'profile_sha')) this.db.exec('ALTER TABLE tasks ADD COLUMN profile_sha TEXT');
    if (!this.db.prepare('PRAGMA table_info(tasks)').all().some(x => x.name === 'permit_sha')) this.db.exec('ALTER TABLE tasks ADD COLUMN permit_sha TEXT');
    // A new process cannot prove what an interrupted framework/model already did.
    this.db.prepare("UPDATE tasks SET state='TASK_STATE_UNSPECIFIED',error_code='PROCESS_RESTART_EFFECT_UNKNOWN',updated=? WHERE state IN ('TASK_STATE_WORKING','TASK_STATE_SUBMITTED')").run(now());
  }
  close() { if (!this.closed) { this.db.close(); unlinkSync(this.lease); this.closed = true; } }
  transaction(fn) { this.db.exec('BEGIN IMMEDIATE'); try { const value = fn(); this.db.exec('COMMIT'); return value; } catch (e) { this.db.exec('ROLLBACK'); throw e; } }
  nonce(claims) { this.transaction(() => { this.db.prepare('DELETE FROM nonces WHERE expires<?').run(Math.floor(this.now() / 1000) - 300); if (this.db.prepare('SELECT 1 FROM nonces WHERE id=?').get(claims.jti)) reject(409, 'TASK_AUTHORITY_REPLAYED'); if (this.db.prepare('SELECT count(*) AS n FROM nonces').get().n >= 100000) reject(503, 'REPLAY_STORE_CAPACITY'); this.db.prepare('INSERT INTO nonces VALUES(?,?)').run(claims.jti, claims.exp); }); }
  row(owner, id) { const row = this.db.prepare('SELECT * FROM tasks WHERE id=? AND tenant=? AND actor=? AND agent=? AND message=?').get(id, owner.tenant_id, owner.user_id, owner.agent_name, owner.message_id); if (!row) reject(404, 'TASK_NOT_FOUND'); return row; }
  byMessage(owner) { const row = this.db.prepare('SELECT * FROM tasks WHERE tenant=? AND actor=? AND agent=? AND message=?').get(owner.tenant_id, owner.user_id, owner.agent_name, owner.message_id); if (!row) reject(404, 'TASK_NOT_FOUND'); return row; }
  admit(owner, request, hash, limits, admission = 'production', profileSha = null, permitSha = null) {
    return this.transaction(() => {
      const prior = this.db.prepare('SELECT * FROM tasks WHERE tenant=? AND actor=? AND agent=? AND message=?').get(owner.tenant_id, owner.user_id, owner.agent_name, request.messageId);
      if (prior) { if (prior.request_hash !== hash) reject(409, 'MESSAGE_ID_CONFLICT'); return { row: prior, fresh: false }; }
      const active = this.db.prepare("SELECT count(*) AS n FROM tasks WHERE state IN ('TASK_STATE_SUBMITTED','TASK_STATE_WORKING')").get().n; if (active >= limits.maxConcurrentTasks) reject(429, 'AGENT_CAPACITY_EXHAUSTED');
      const day = Math.floor(this.now() / 86400000) * 86400000; const count = this.db.prepare('SELECT count(*) AS n FROM tasks WHERE tenant=? AND actor=? AND created>=?').get(owner.tenant_id, owner.user_id, day).n; if (count >= limits.maxTasksPerActorPerDay) reject(429, 'AGENT_BUDGET_EXHAUSTED');
      const context = request.contextId ?? randomUUID(); const existing = this.db.prepare('SELECT * FROM contexts WHERE id=?').get(context);
      if (request.contextId && (!existing || existing.tenant !== owner.tenant_id || existing.actor !== owner.user_id || existing.agent !== owner.agent_name)) reject(404, 'CONTEXT_NOT_FOUND');
      if (!existing) this.db.prepare('INSERT INTO contexts VALUES(?,?,?,?)').run(context, owner.tenant_id, owner.user_id, owner.agent_name);
      if (this.db.prepare("SELECT 1 FROM tasks WHERE context=? AND state IN ('TASK_STATE_SUBMITTED','TASK_STATE_WORKING')").get(context)) reject(409, 'CONTEXT_BUSY');
      const id = randomUUID(); const time = this.now(); this.db.prepare("INSERT INTO tasks(id,context,tenant,actor,agent,message,request_hash,state,admission,profile_sha,permit_sha,created,updated) VALUES(?,?,?,?,?,?,?,'TASK_STATE_SUBMITTED',?,?,?,?,?)").run(id, context, owner.tenant_id, owner.user_id, owner.agent_name, request.messageId, hash, admission, profileSha, permitSha, time, time);
      return { row: this.row(owner, id), fresh: true };
    });
  }
  working(owner, id) { const changed = this.db.prepare("UPDATE tasks SET state='TASK_STATE_WORKING',updated=? WHERE id=? AND tenant=? AND actor=? AND agent=? AND message=? AND state='TASK_STATE_SUBMITTED' AND cancel_requested=0").run(this.now(), id, owner.tenant_id, owner.user_id, owner.agent_name, owner.message_id); return changed.changes === 1; }
  cancel(owner, id) { const row = this.row(owner, id); if (TERMINAL.has(row.state)) { if (row.state === 'TASK_STATE_CANCELED') return row; reject(409, 'TASK_NOT_CANCELABLE'); } this.db.prepare('UPDATE tasks SET cancel_requested=1,updated=? WHERE id=?').run(this.now(), id); return this.row(owner, id); }
  finish(owner, id, state, details = {}) {
    if (!TERMINAL.has(state)) reject(500, 'INVALID_TERMINAL_STATE');
    this.transaction(() => {
      const row = this.row(owner, id); if (TERMINAL.has(row.state)) return;
      if (row.cancel_requested) { state = 'TASK_STATE_CANCELED'; details.output = null; }
      if (state === 'TASK_STATE_COMPLETED' && this.unknownModelEffects(id)) { state = 'TASK_STATE_UNSPECIFIED'; details.output = null; details.error = 'MODEL_EFFECT_UNKNOWN'; }
      this.db.prepare('UPDATE tasks SET state=?,output=?,error_code=?,framework=?,framework_calls=?,tool_calls=?,updated=? WHERE id=?').run(state, details.output ?? null, details.error ?? null, details.framework ?? null, details.frameworkCalls ?? 0, details.toolCalls ?? 0, this.now(), id);
    });
  }
  dispatch(owner, task, hash, maximum) {
    return this.transaction(() => {
      const row = this.row(owner, task); if (row.state !== 'TASK_STATE_WORKING' || row.cancel_requested) reject(409, 'EXECUTION_STOPPED'); const sequence = this.db.prepare('SELECT count(*) AS n FROM model_calls WHERE task=?').get(task).n + 1; if (sequence > maximum) reject(429, 'MODEL_CALL_BUDGET_EXHAUSTED'); const id = randomUUID(); this.db.prepare("INSERT INTO model_calls(task,sequence,request_id,request_hash,effect) VALUES(?,?,?,?,'dispatched')").run(task, sequence, id, hash); return { sequence, id };
    });
  }
  capability(owner, task, name, hash, maximum) { const row = this.row(owner, task); if (row.state !== 'TASK_STATE_WORKING' || row.cancel_requested) reject(409, 'EXECUTION_STOPPED'); if (this.db.prepare('SELECT count(*) AS n FROM capability_calls WHERE task=?').get(task).n >= maximum) reject(429, 'CAPABILITY_CALL_BUDGET_EXHAUSTED'); const id = randomUUID(); this.db.prepare("INSERT INTO capability_calls VALUES(?,?,?,?,'dispatched',NULL)").run(task, id, name, hash); return id; }
  capabilityResponse(task, id, effect, status) { this.db.prepare("UPDATE capability_calls SET effect=?,http_status=? WHERE task=? AND request_id=? AND effect='dispatched'").run(effect, status, task, id); }
  successfulCapabilities(task) { return this.db.prepare("SELECT count(*) AS n FROM capability_calls WHERE task=? AND effect='confirmed'").get(task).n; }
  response(task, sequence, value) { this.db.prepare('UPDATE model_calls SET effect=?,gateway_request_id=?,response_id=?,input_tokens=?,output_tokens=?,http_status=? WHERE task=? AND sequence=? AND effect=\'dispatched\'').run(value.effect, value.gatewayRequestId ?? null, value.responseId ?? null, value.inputTokens ?? null, value.outputTokens ?? null, value.status, task, sequence); }
  unknownModelEffects(task) { if (this.db.prepare("SELECT 1 FROM capability_calls WHERE task=? AND effect IN ('dispatched','unknown')").get(task)) return true; return this.db.prepare("SELECT 1 FROM model_calls WHERE task=? AND effect IN ('dispatched','unknown')").get(task) != null; }
  describe(owner, id) {
    const row = this.row(owner, id); const calls = this.db.prepare('SELECT sequence,request_hash,request_id,gateway_request_id,response_id,effect,input_tokens,output_tokens,http_status FROM model_calls WHERE task=? ORDER BY sequence').all(id);
    const result = { id: row.id, contextId: row.context, status: { state: row.state }, metadata: { admission: row.admission, profileSha256: row.profile_sha, admissionPermitSha256: row.permit_sha, effectState: this.unknownModelEffects(id) || row.state === 'TASK_STATE_UNSPECIFIED' ? 'unknown' : 'confirmed', cancellationRequested: Boolean(row.cancel_requested), framework: row.framework, frameworkCalls: row.framework_calls, toolCalls: row.tool_calls, modelCalls: calls, capabilityCalls: this.db.prepare('SELECT request_id,name,effect,http_status FROM capability_calls WHERE task=? ORDER BY rowid').all(id), errorCode: row.error_code, governanceAcceptance: 'not_run' } };
    if (row.state === 'TASK_STATE_COMPLETED' && row.output) result.artifacts = [{ artifactId: `${row.id}-output`, parts: [{ text: row.output, mediaType: 'text/plain' }] }]; return result;
  }
}
