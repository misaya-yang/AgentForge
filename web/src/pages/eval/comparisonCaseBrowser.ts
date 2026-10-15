export const COMPARISON_CASE_PAGE_SIZE = 25;
export interface ComparisonCaseRow { item: Record<string, unknown>; position: number }
export function comparisonCaseStatus(item: Record<string, unknown>): string {
  return typeof item.status === "string" && item.status ? item.status
    : typeof item.classification === "string" && item.classification ? item.classification : "unknown";
}
export function comparisonCaseRows(items: Array<Record<string, unknown>>): ComparisonCaseRow[] {
  // Stable original position preserves duplicate/missing IDs and does not merge attempts.
  return items.map((item, position) => ({ item, position })).sort((left, right) => {
    const rank = (row: ComparisonCaseRow) => /regress|fail/i.test(comparisonCaseStatus(row.item)) ? 0 : 1;
    return rank(left) - rank(right);
  });
}
export function comparisonCaseWindow(rows: ComparisonCaseRow[], search: string, status: string | undefined, offset: number) {
  const query = search.trim().toLowerCase();
  const matches = rows.filter(({ item }) => (!status || comparisonCaseStatus(item) === status)
    && (!query || (typeof item.case_id === "string" && item.case_id.toLowerCase().includes(query))));
  const last = Math.max(0, Math.floor((matches.length - 1) / COMPARISON_CASE_PAGE_SIZE) * COMPARISON_CASE_PAGE_SIZE);
  const current = Number.isSafeInteger(offset) && offset >= 0 ? Math.min(Math.floor(offset / COMPARISON_CASE_PAGE_SIZE) * COMPARISON_CASE_PAGE_SIZE, last) : 0;
  return { rows: matches.slice(current, current + COMPARISON_CASE_PAGE_SIZE), offset: current,
    start: matches.length ? current + 1 : 0, end: Math.min(current + COMPARISON_CASE_PAGE_SIZE, matches.length),
    filtered: matches.length, hasNext: current + COMPARISON_CASE_PAGE_SIZE < matches.length };
}
