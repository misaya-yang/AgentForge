export interface RegionCitation {
  page: number; block: string; documentVersion: string; generation: string; sourceHash: string;
  coordinateSpace: "page_points" | "render_pixels" | "native_page_units" | "unknown";
  region: { x0: number; y0: number; x1: number; y1: number } | null;
}
/** Accept only server-enriched, exactly bound evidence. Never reconstruct a
 * region from a source URL, model text or an unverified vector reference. */
export function docirCitation(metadata: Record<string, unknown> | undefined, tenant: string | undefined, dataset: string | undefined, document: string | undefined): RegionCitation | null {
  const raw = metadata?.citation;
  if (!raw || typeof raw !== "object") return null;
  const c = raw as Record<string, unknown>, source = c.source as Record<string, unknown> | undefined;
  const text = (value: unknown): value is string => typeof value === "string" && value.length > 0 && value.length <= 512 && !/\p{Cc}/u.test(value);
  if (!tenant || !dataset || !document || !source || source.tenant_id !== tenant || source.dataset_id !== dataset || source.document_id !== document
    || !text(source.document_version) || !text(source.publication_generation) || !/^[1-9]\d*$/.test(source.publication_generation)
    || !text(source.source_sha256) || !/^[a-f0-9]{64}$/i.test(source.source_sha256) || !text(source.execution_id) || !text(source.parser_profile)
    || typeof c.page_number !== "number" || !Number.isSafeInteger(c.page_number) || c.page_number < 1 || !text(c.block_id)
    || !["page_points", "render_pixels", "native_page_units", "unknown"].includes(String(c.coordinate_space))) return null;
  let region: RegionCitation["region"] = null;
  if (c.region !== null) {
    if (!c.region || typeof c.region !== "object" || c.coordinate_space === "unknown") return null;
    const r = c.region as Record<string, unknown>;
    if (![r.x0, r.y0, r.x1, r.y1].every(n => typeof n === "number" && Number.isFinite(n) && n >= 0)
      || Number(r.x1) <= Number(r.x0) || Number(r.y1) <= Number(r.y0)) return null;
    region = { x0: Number(r.x0), y0: Number(r.y0), x1: Number(r.x1), y1: Number(r.y1) };
  }
  return { page: c.page_number, block: c.block_id, documentVersion: source.document_version,
    generation: source.publication_generation, sourceHash: source.source_sha256,
    coordinateSpace: c.coordinate_space as RegionCitation["coordinateSpace"], region };
}
