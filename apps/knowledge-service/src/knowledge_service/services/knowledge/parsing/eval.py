"""Parsing-quality scoring harness (PRD T4 item 5, metric side).

The bilingual evaluation *corpus* (50–200 real pages of scanned / table-dense
/ formula / multi-column material) is a data-collection effort; this module is
the code side that scores a parser run against it, with the PRD's metric set:

* text quality — normalised edit-distance similarity;
* table quality — TEDS-style cell-grid similarity (see docstring caveat);
* formula quality — normalised LaTeX match;
* reading order — consistency of the matched-block order (Kendall tau scaled
  to [0, 1]).

All functions are pure stdlib and deterministic.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

from .ir import Block, BlockType, DocIR, PageIR
from .table_policy import parse_markdown_rows


def levenshtein(a: str, b: str) -> int:
    """Two-row dynamic-programming edit distance (unicode codepoints)."""
    if a == b:
        return 0
    if not a:
        return len(b)
    if not b:
        return len(a)
    prev = list(range(len(b) + 1))
    for i, ca in enumerate(a, start=1):
        cur = [i]
        for j, cb in enumerate(b, start=1):
            cur.append(min(prev[j] + 1, cur[j - 1] + 1, prev[j - 1] + (ca != cb)))
        prev = cur
    return prev[-1]


def edit_distance_similarity(a: str, b: str) -> float:
    """1 - distance / max-length, in [0, 1]. Empty-vs-empty is 1.0."""
    a, b = a or "", b or ""
    if not a and not b:
        return 1.0
    denom = max(len(a), len(b))
    return 1.0 - levenshtein(a, b) / denom


def normalize_text(text: str) -> str:
    """Collapse whitespace for robust text comparison."""
    return " ".join((text or "").split())


def normalize_latex(latex: str) -> str:
    """Cheap LaTeX normalisation for match scoring: strip whitespace,
    ``\\left``/``\\right``, and braces around single tokens."""
    out = (latex or "").strip("$ \n\t")
    out = out.replace("\\left", "").replace("\\right", "")
    out = "".join(out.split())
    return out


def formula_similarity(a: str, b: str) -> float:
    return edit_distance_similarity(normalize_latex(a), normalize_latex(b))


def _table_cells(markdown: str) -> list[list[str]]:
    header, data = parse_markdown_rows(markdown or "")
    rows = [ln for ln in header if not all(c in "|-: " for c in ln.replace(" ", ""))] + data
    return [[normalize_text(c) for c in r.strip().strip("|").split("|")] for r in rows]


def table_similarity(expected_md: str, actual_md: str) -> float:
    """TEDS-lite: cell-grid similarity.

    Full TEDS is tree edit distance over table structure; merged cells and
    nesting make it the expensive, licensing-encumbered part.  This proxy
    compares flattened cell rows positionally and reports
    ``matched_cells / max(total_cells_a, total_cells_b)`` after text
    normalisation — enough to rank parsers and catch the merged-cell markdown
    degradation PRD §3.2 warns about.  A real TEDS scorer can be swapped in
    behind this signature once the golden set exists.
    """
    a_cells = [c for row in _table_cells(expected_md) for c in row]
    b_cells = [c for row in _table_cells(actual_md) for c in row]
    if not a_cells and not b_cells:
        return 1.0
    denom = max(len(a_cells), len(b_cells))
    matched = sum(1 for x, y in zip(a_cells, b_cells, strict=False) if x == y)
    return matched / denom


def _match_blocks(expected: list[Block], actual: list[Block]) -> list[tuple[int, int]]:
    """Greedy match of expected→actual blocks by normalised-text similarity.

    Returns index pairs sorted by expected order; each expected block claims
    its best unused actual match above 0.6 similarity.
    """
    pairs: list[tuple[int, int]] = []
    used: set[int] = set()
    for ei, eb in enumerate(expected):
        best_i, best_score = -1, 0.6
        for ai, ab in enumerate(actual):
            if ai in used:
                continue
            score = edit_distance_similarity(normalize_text(eb.text), normalize_text(ab.text))
            if score > best_score:
                best_i, best_score = ai, score
        if best_i >= 0:
            used.add(best_i)
            pairs.append((ei, best_i))
    return pairs


def reading_order_score(expected: list[Block], actual: list[Block]) -> float:
    """Kendall-tau consistency of matched block order, scaled to [0, 1].

    1.0 = perfectly consistent; 0.5 = coin flip; 0.0 = fully reversed.
    Fewer than two matched blocks → 1.0 (nothing to order).
    """
    pairs = _match_blocks(expected, actual)
    idx = [a for _, a in pairs]
    n = len(idx)
    if n < 2:
        return 1.0
    concordant = discordant = 0
    for i in range(n):
        for j in range(i + 1, n):
            if idx[i] < idx[j]:
                concordant += 1
            elif idx[i] > idx[j]:
                discordant += 1
    total = concordant + discordant
    if total == 0:
        return 1.0
    tau = (concordant - discordant) / total
    return (tau + 1) / 2


@dataclass
class EvalReport:
    """Aggregate parse-quality numbers for one corpus run."""

    text_similarity: float = 0.0
    table_similarity: float = 0.0
    formula_similarity: float = 0.0
    reading_order: float = 0.0
    pages: int = 0
    details: dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> dict[str, Any]:
        return {
            "text_similarity": round(self.text_similarity, 4),
            "table_similarity": round(self.table_similarity, 4),
            "formula_similarity": round(self.formula_similarity, 4),
            "reading_order": round(self.reading_order, 4),
            "pages": self.pages,
            "details": self.details,
        }


def score_page(expected: PageIR, actual: PageIR) -> dict[str, float]:
    exp_blocks = expected.sorted_blocks()
    act_blocks = actual.sorted_blocks()
    texts_e = [normalize_text(b.text) for b in exp_blocks]
    texts_a = [normalize_text(b.text) for b in act_blocks]
    text_scores = [edit_distance_similarity(x, y) for x, y in zip(texts_e, texts_a, strict=False)]
    tables = [
        table_similarity(b.table.markdown if b.table else "", a.table.markdown if a.table else "")
        for b, a in zip(exp_blocks, act_blocks, strict=False)
        if b.type is BlockType.TABLE or a.type is BlockType.TABLE
    ]
    formulas = [
        formula_similarity(b.formula.latex if b.formula else "", a.formula.latex if a.formula else "")
        for b, a in zip(exp_blocks, act_blocks, strict=False)
        if b.type is BlockType.FORMULA or a.type is BlockType.FORMULA
    ]
    return {
        "text": sum(text_scores) / len(text_scores) if text_scores else 0.0,
        "table": sum(tables) / len(tables) if tables else 1.0,
        "formula": sum(formulas) / len(formulas) if formulas else 1.0,
        "reading_order": reading_order_score(exp_blocks, act_blocks),
    }


def score_corpus(expected_pages: list[PageIR], actual_pages: list[PageIR]) -> EvalReport:
    """Average per-page metric families across aligned page lists."""
    report = EvalReport(pages=min(len(expected_pages), len(actual_pages)))
    per_metric: dict[str, list[float]] = {"text": [], "table": [], "formula": [], "reading_order": []}
    for exp, act in zip(expected_pages, actual_pages, strict=False):
        scores = score_page(exp, act)
        for k, v in scores.items():
            per_metric[k].append(v)
    report.text_similarity = _mean(per_metric["text"])
    report.table_similarity = _mean(per_metric["table"])
    report.formula_similarity = _mean(per_metric["formula"])
    report.reading_order = _mean(per_metric["reading_order"])
    return report


def score_document(expected: DocIR, actual: DocIR) -> EvalReport:
    exp = {p.page_number: p for p in expected.pages}
    act = {p.page_number: p for p in actual.pages}
    common = sorted(set(exp) & set(act))
    return score_corpus([exp[n] for n in common], [act[n] for n in common])


def _mean(values: list[float]) -> float:
    return sum(values) / len(values) if values else 0.0
