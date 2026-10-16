//! Source parsing/eval.py metric contract. TEDS-lite is positional cell equality,
//! not full tree-edit TEDS. No model, network, persistence or hidden corpus.
use crate::migration_knowledge_ingest::{
    parser::{Block, DocIR, PageIR},
    structured::parse_markdown_rows,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
pub fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<_> = b.chars().collect();
    let mut previous: Vec<_> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            current.push(
                (previous[j + 1] + 1)
                    .min(current[j] + 1)
                    .min(previous[j] + usize::from(ca != *cb)),
            );
        }
        previous = current;
    }
    previous[b.len()]
}
pub fn edit_distance_similarity(a: &str, b: &str) -> f64 {
    let length = a.chars().count().max(b.chars().count());
    if length == 0 {
        1.
    } else {
        1. - levenshtein(a, b) as f64 / length as f64
    }
}
pub fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}
pub fn normalize_latex(text: &str) -> String {
    text.trim_matches(['$', ' ', '\n', '\t'])
        .replace("\\left", "")
        .replace("\\right", "")
        .split_whitespace()
        .collect()
}
pub fn formula_similarity(a: &str, b: &str) -> f64 {
    edit_distance_similarity(&normalize_latex(a), &normalize_latex(b))
}
fn cells(markdown: &str) -> Vec<String> {
    let (header, data) = parse_markdown_rows(markdown);
    header
        .into_iter()
        .filter(|line| {
            !line
                .chars()
                .filter(|c| *c != ' ')
                .all(|c| "|-:".contains(c))
        })
        .chain(data)
        .flat_map(|row| row.trim().trim_matches('|').split('|').map(normalize_text))
        .collect()
}
pub fn table_similarity(expected: &str, actual: &str) -> f64 {
    let a = cells(expected);
    let b = cells(actual);
    let denominator = a.len().max(b.len());
    if denominator == 0 {
        1.
    } else {
        a.iter().zip(&b).filter(|(a, b)| a == b).count() as f64 / denominator as f64
    }
}
pub fn reading_order_score(expected: &[&Block], actual: &[&Block]) -> f64 {
    let mut used = BTreeSet::new();
    let mut indices = vec![];
    for expected in expected {
        let mut best = None;
        let mut best_score = 0.6;
        for (i, actual) in actual.iter().enumerate() {
            if used.contains(&i) {
                continue;
            }
            let score = edit_distance_similarity(
                &normalize_text(&expected.text),
                &normalize_text(&actual.text),
            );
            if score > best_score {
                best = Some(i);
                best_score = score;
            }
        }
        if let Some(i) = best {
            used.insert(i);
            indices.push(i);
        }
    }
    let mut concordant = 0;
    let mut discordant = 0;
    for i in 0..indices.len() {
        for j in i + 1..indices.len() {
            if indices[i] < indices[j] {
                concordant += 1;
            } else if indices[i] > indices[j] {
                discordant += 1;
            }
        }
    }
    if concordant + discordant == 0 {
        1.
    } else {
        concordant as f64 / (concordant + discordant) as f64
    }
}
#[derive(Clone, Debug, Default)]
pub struct EvalReport {
    pub text_similarity: f64,
    pub table_similarity: f64,
    pub formula_similarity: f64,
    pub reading_order: f64,
    pub pages: usize,
    pub details: BTreeMap<String, Value>,
}
impl EvalReport {
    pub fn to_dict(&self) -> Value {
        let round = |n: f64| (n * 10000.).round_ties_even() / 10000.;
        json!({"text_similarity":round(self.text_similarity),"table_similarity":round(self.table_similarity),"formula_similarity":round(self.formula_similarity),"reading_order":round(self.reading_order),"pages":self.pages,"details":self.details})
    }
}
fn mean(values: &[f64], empty: f64) -> f64 {
    if values.is_empty() {
        empty
    } else {
        values.iter().sum::<f64>() / values.len() as f64
    }
}
pub fn score_page(expected: &PageIR, actual: &PageIR) -> BTreeMap<String, f64> {
    let expected = expected.sorted_blocks();
    let actual = actual.sorted_blocks();
    let mut texts = vec![];
    let mut tables = vec![];
    let mut formulas = vec![];
    for (e, a) in expected.iter().zip(&actual) {
        texts.push(edit_distance_similarity(
            &normalize_text(&e.text),
            &normalize_text(&a.text),
        ));
        if e.kind == "table" || a.kind == "table" {
            tables.push(table_similarity(
                e.table
                    .as_ref()
                    .and_then(|v| v["markdown"].as_str())
                    .unwrap_or(""),
                a.table
                    .as_ref()
                    .and_then(|v| v["markdown"].as_str())
                    .unwrap_or(""),
            ));
        }
        if e.kind == "formula" || a.kind == "formula" {
            formulas.push(formula_similarity(
                e.formula
                    .as_ref()
                    .and_then(|v| v["latex"].as_str())
                    .unwrap_or(""),
                a.formula
                    .as_ref()
                    .and_then(|v| v["latex"].as_str())
                    .unwrap_or(""),
            ));
        }
    }
    BTreeMap::from([
        ("text".into(), mean(&texts, 0.)),
        ("table".into(), mean(&tables, 1.)),
        ("formula".into(), mean(&formulas, 1.)),
        (
            "reading_order".into(),
            reading_order_score(&expected, &actual),
        ),
    ])
}
pub fn score_corpus(expected: &[&PageIR], actual: &[&PageIR]) -> EvalReport {
    let scores: Vec<_> = expected
        .iter()
        .zip(actual)
        .map(|(e, a)| score_page(e, a))
        .collect();
    let avg = |key: &str| mean(&scores.iter().map(|s| s[key]).collect::<Vec<_>>(), 0.);
    EvalReport {
        text_similarity: avg("text"),
        table_similarity: avg("table"),
        formula_similarity: avg("formula"),
        reading_order: avg("reading_order"),
        pages: expected.len().min(actual.len()),
        details: BTreeMap::new(),
    }
}
pub fn score_document(expected: &DocIR, actual: &DocIR) -> EvalReport {
    let e: BTreeMap<_, _> = expected.pages.iter().map(|p| (p.page_number, p)).collect();
    let a: BTreeMap<_, _> = actual.pages.iter().map(|p| (p.page_number, p)).collect();
    let common: Vec<_> = e.keys().filter(|n| a.contains_key(n)).copied().collect();
    score_corpus(
        &common.iter().map(|n| e[n]).collect::<Vec<_>>(),
        &common.iter().map(|n| a[n]).collect::<Vec<_>>(),
    )
}
