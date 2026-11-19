/** One reviewed operation per session. Transport errors never authorize replay. */
export interface CatalogChange {
  model_id: string; kind: "add" | "change" | "unchanged";
  changes: { field: string; before: unknown; after: unknown }[];
  existing: Record<string, unknown> | null;
  proposed: Record<string, unknown>; preserved_fields: string[];
  new_model_policy: Record<string, unknown> | null;
}
export interface CatalogPreview {
  review_id: string; provider_id: string; state: "ready";
  changes: CatalogChange[]; retained_models: Record<string, unknown>[];
  affected_references: { agent_id: string; name: string; model_id: string; kind: string; match: string }[];
  reference_coverage: string; discovery_warnings: string[];
  skipped_models: {model_id: string; reason: string}[]; notice: string;
}
export interface CatalogReadback {
  review_id: string; provider_id: string; state: string;
  result?: { models?: Record<string, unknown>[]; message?: string };
  current?: {models: Record<string, unknown>[]};
}
export interface CatalogTransport {
  preview(provider: string): Promise<CatalogPreview>;
  apply(provider: string, body: {review_id: string; model_ids: string[]; replace_existing_metadata: boolean}): Promise<CatalogReadback>;
  read(provider: string, review: string): Promise<CatalogReadback>;
}
export class CatalogReviewSession {
  private live = true;
  private generation = 0;
  busy = false;
  consumed = false;
  preview: CatalogPreview | null = null;
  result: CatalogReadback | null = null;
  error = "";
  readonly provider: string;
  private api: CatalogTransport;
  private changed: () => void;
  constructor(provider: string, api: CatalogTransport, changed: () => void) { this.provider = provider; this.api = api; this.changed = changed; }
  mount() { this.live = true; }
  dispose() { this.live = false; this.generation++; this.busy = false; }
  async load() {
    if (!this.live || this.busy || this.consumed) return;
    const generation = this.generation;
    this.busy = true; this.error = ""; this.changed();
    try { const value = await this.api.preview(this.provider); if (this.live && generation === this.generation) { if (value.provider_id !== this.provider || !value.review_id || value.state !== "ready") throw new Error("Invalid review response"); this.preview = value; } }
    catch { if (this.live && generation === this.generation) this.error = "preview_failed"; }
    finally { if (this.live && generation === this.generation) { this.busy = false; this.changed(); } }
  }
  async apply(ids: string[], replace: boolean) {
    if (!this.live || this.busy || this.consumed || !this.preview || !ids.length) return;
    if (new Set(ids).size !== ids.length || ids.some(id => !this.preview!.changes.some(c => c.model_id === id))) return;
    if (!replace && ids.some(id => this.preview!.changes.some(c => c.model_id === id && c.existing))) return;
    const generation = this.generation;
    this.consumed = true; this.busy = true; this.error = ""; this.changed();
    try { const result = await this.api.apply(this.provider, {review_id: this.preview.review_id, model_ids: ids, replace_existing_metadata: replace}); if (this.live && generation === this.generation) { if (result.provider_id !== this.provider || result.review_id !== this.preview.review_id) throw new Error("Invalid apply response"); this.result = result; } }
    catch { if (this.live && generation === this.generation) this.error = "apply_unknown"; }
    finally { if (this.live && generation === this.generation) { this.busy = false; this.changed(); } }
  }
  async read() {
    if (!this.live || this.busy || !this.preview) return;
    const generation = this.generation;
    this.busy = true; this.changed();
    try { const value = await this.api.read(this.provider, this.preview.review_id); if (this.live && generation === this.generation) { if (value.provider_id !== this.provider || value.review_id !== this.preview.review_id) throw new Error("Invalid readback"); this.result = value; this.error = ""; if (value.state !== "ready") this.consumed = true; } }
    catch { if (this.live && generation === this.generation) this.error = "read_failed"; }
    finally { if (this.live && generation === this.generation) { this.busy = false; this.changed(); } }
  }
}
