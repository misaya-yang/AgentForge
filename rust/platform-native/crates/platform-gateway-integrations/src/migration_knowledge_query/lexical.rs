//! Direct lexical_v1 token presence, candidate BM25, fusion and MMR algorithms.
use super::types::{QueryError, Result};
use std::collections::{BTreeMap, BTreeSet};
include!("stopwords.rs");
pub fn is_cjk(c: char) -> bool {
    ('\u{4e00}'..='\u{9fff}').contains(&c)
}
fn is_arabic(c: char) -> bool {
    matches!(c as u32,0x0600..=0x06ff|0x0750..=0x077f|0x08a0..=0x08ff|0xfb50..=0xfdff|0xfe70..=0xfeff)
}
fn latin(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}
fn runs(text: &str, predicate: fn(char) -> bool) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        if predicate(c) {
            word.push(c)
        } else if !word.is_empty() {
            out.push(std::mem::take(&mut word))
        }
    }
    if !word.is_empty() {
        out.push(word)
    }
    out
}
pub fn detect_language(text: &str) -> &'static str {
    let chars: Vec<_> = text.chars().take(2000).collect();
    let n = chars.len().max(1) as f64;
    let a = chars.iter().filter(|c| is_arabic(**c)).count() as f64;
    let z = chars.iter().filter(|c| is_cjk(**c)).count() as f64;
    let l = chars.iter().filter(|c| latin(**c)).count() as f64;
    if a / n > 0.2 {
        if l > a * 0.3 { "mixed" } else { "ar" }
    } else if z / n > 0.1 {
        "zh"
    } else {
        "en"
    }
}
pub fn normalize_arabic(text: &str) -> String {
    text.chars()
        .filter(|c| !matches!(*c as u32, 0x064b..=0x0652 | 0x0670))
        .map(|c| match c {
            'أ' | 'إ' | 'آ' | 'ٱ' => 'ا',
            'ة' => 'ه',
            'ى' => 'ي',
            _ => c,
        })
        .collect()
}
pub fn tokenize(
    text: &str,
    keep_original: bool,
    remove_stopwords: bool,
    deduplicate: bool,
) -> Vec<String> {
    let t = text.trim();
    if t.is_empty() {
        return vec![];
    }
    let mut tokens = Vec::new();
    if keep_original && t.chars().count() > 1 {
        tokens.push(t.to_lowercase())
    }
    let chars: Vec<char> = t.chars().collect();
    let mut clean = String::new();
    let mut i = 0;
    // Source regex accepts either quote as the closing delimiter.
    while i < chars.len() {
        if chars[i] == '\'' || chars[i] == '"' {
            if let Some(j) = ((i + 1)..chars.len()).find(|&j| chars[j] == '\'' || chars[j] == '"') {
                if j > i + 1 {
                    let p: String = chars[i + 1..j].iter().collect();
                    if !p.trim().is_empty() {
                        tokens.push(p.trim().to_lowercase())
                    }
                    clean.push(' ');
                    i = j + 1;
                    continue;
                }
            }
        }
        clean.push(chars[i]);
        i += 1
    }
    let clean = clean.to_lowercase();
    for w in runs(&clean, latin) {
        if !remove_stopwords || !ENGLISH_STOPWORDS.contains(&w.as_str()) {
            tokens.push(w)
        }
    }
    for r in runs(&clean, is_cjk) {
        let c: Vec<char> = r.chars().collect();
        if c.len() >= 2 {
            tokens.push(r);
            for pair in c.windows(2) {
                tokens.push(pair.iter().collect())
            }
        }
        tokens.extend(c.into_iter().map(|x| x.to_string()))
    }
    for w in runs(&normalize_arabic(&clean), is_arabic) {
        if w.chars().count() < 2 || (remove_stopwords && ARABIC_STOPWORDS.contains(&w.as_str())) {
            continue;
        }
        tokens.push(w.clone());
        if w.chars().count() > 3 {
            for p in ["و", "ف", "ب", "ل", "ك", "ال", "وال", "فال", "بال", "لل"] {
                if let Some(stem) = w.strip_prefix(p) {
                    if stem.chars().count() >= 2 && !ARABIC_STOPWORDS.contains(&stem) {
                        tokens.push(stem.into())
                    }
                    break;
                }
            }
        }
    }
    if deduplicate {
        let mut seen = BTreeSet::new();
        tokens.retain(|t| !t.is_empty() && seen.insert(t.clone()))
    }
    tokens
}
pub fn bm25_scores(query: &[String], docs: &[Vec<String>], k1: f64, b: f64) -> Vec<f64> {
    if docs.is_empty() {
        return vec![];
    }
    let n = docs.len() as f64;
    let avg = docs
        .iter()
        .map(|d| d.iter().filter(|t| !t.is_empty()).count())
        .sum::<usize>() as f64
        / n;
    let mut df = BTreeMap::<&str, usize>::new();
    for doc in docs {
        for t in doc
            .iter()
            .filter(|t| !t.is_empty())
            .map(String::as_str)
            .collect::<BTreeSet<_>>()
        {
            *df.entry(t).or_default() += 1
        }
    }
    let q: BTreeSet<_> = query
        .iter()
        .filter(|t| !t.is_empty())
        .map(String::as_str)
        .collect();
    docs.iter()
        .map(|doc| {
            let mut tf = BTreeMap::<&str, usize>::new();
            for t in doc.iter().filter(|t| !t.is_empty()) {
                *tf.entry(t.as_str()).or_default() += 1
            }
            q.iter()
                .map(|term| {
                    let f = *tf.get(term).unwrap_or(&0) as f64;
                    if f == 0.0 {
                        return 0.0;
                    }
                    let d = *df.get(term).unwrap_or(&0) as f64;
                    let idf = (1.0 + (n - d + 0.5) / (d + 0.5)).ln();
                    idf * (f * (k1 + 1.0))
                        / (f + k1 * (1.0 - b + b * (doc.len() as f64 / avg.max(1e-9)))).max(1e-9)
                })
                .sum()
        })
        .collect()
}
pub fn token_hash(token: &str) -> u32 {
    token
        .bytes()
        .fold(0x811c9dc5, |h, b| (h ^ b as u32).wrapping_mul(0x01000193))
}
pub fn sparse_vector(text: &str, query: bool) -> (Vec<u32>, Vec<f64>) {
    let mut tf = BTreeMap::<u32, f64>::new();
    for t in tokenize(text, false, !query, true) {
        let x = tf.entry(token_hash(&t)).or_default();
        if query { *x = 1.0 } else { *x += 1.0 }
    }
    tf.into_iter().unzip()
}
pub fn reciprocal_rank_fusion(
    lists: &[(String, Vec<String>)],
    k: usize,
    weights: &BTreeMap<String, f64>,
    qdrant_weighted: bool,
) -> Result<BTreeMap<String, f64>> {
    if k < 1 {
        return Err(QueryError::Validation("RRF k must be positive".into()));
    }
    let mut fused = BTreeMap::new();
    for (source, ids) in lists {
        let w = weights.get(source).copied().unwrap_or(1.0);
        if !w.is_finite() || w < 0.0 {
            return Err(QueryError::Validation(
                "RRF weights must be finite and non-negative".into(),
            ));
        }
        for (rank, id) in ids.iter().enumerate() {
            if id.is_empty() {
                continue;
            }
            let contribution = if qdrant_weighted {
                if w == 0.0 {
                    0.0
                } else {
                    1.0 / ((rank + 1) as f64 / w + k as f64 - 1.0)
                }
            } else {
                w / (k + rank + 1) as f64
            };
            *fused.entry(id.clone()).or_insert(0.0) += contribution
        }
    }
    Ok(fused)
}
pub fn normalize(scores: &BTreeMap<String, f64>, method: &str) -> BTreeMap<String, f64> {
    if scores.is_empty() {
        return BTreeMap::new();
    }
    let mut values: Vec<_> = scores.values().copied().collect();
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if method == "sigmoid" {
        return scores
            .iter()
            .map(|(k, v)| (k.clone(), 1.0 / (1.0 + (-(v - 0.5) * 2.0).exp())))
            .collect();
    }
    if method == "l2" {
        let norm = values.iter().map(|v| v * v).sum::<f64>().sqrt();
        return scores
            .iter()
            .map(|(k, v)| (k.clone(), if norm < 1e-9 { 0.0 } else { v / norm }))
            .collect();
    }
    if method == "percentile" && n > 1 {
        let mut ordered: Vec<_> = scores.iter().collect();
        ordered.sort_by(|a, b| a.1.total_cmp(b.1));
        return ordered
            .into_iter()
            .enumerate()
            .map(|(i, (k, _))| (k.clone(), (i + 1) as f64 / n as f64))
            .collect();
    }
    let (low, high) = if method == "minmax" {
        (values[0], values[n - 1])
    } else {
        (
            values[(n as f64 * 0.05) as usize],
            values[((n as f64 * 0.95) as usize).min(n - 1)],
        )
    };
    let range = high - low;
    scores
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                if range < 1e-9 {
                    0.5
                } else {
                    ((v - low) / range).clamp(0.0, 1.0)
                },
            )
        })
        .collect()
}
pub fn language_weights(query: &str, dense: f64, bm25: f64) -> (f64, f64) {
    match detect_language(query) {
        "ar" => (0.55, 0.45),
        "zh" => (0.75, 0.25),
        "mixed" => (0.65, 0.35),
        _ => (dense, bm25),
    }
}
pub fn cosine(a: &[f64], b: &[f64]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let d = a.iter().zip(b).map(|(x, y)| x * y).sum::<f64>();
    let n = (a.iter().map(|x| x * x).sum::<f64>() * b.iter().map(|x| x * x).sum::<f64>()).sqrt();
    if n > 0.0 { d / n } else { 0.0 }
}
#[derive(Clone, Debug)]
pub struct MmrPick {
    pub mmr_score: f64,
    pub relevance: f64,
    pub max_sim_to_selected: f64,
}
pub fn mmr_select(
    candidates: &[String],
    relevance: &BTreeMap<String, f64>,
    vectors: &BTreeMap<String, Vec<f64>>,
    top_k: usize,
    lambda: f64,
    threshold: Option<f64>,
    fill_policy: &str,
) -> Result<(Vec<String>, BTreeMap<String, MmrPick>)> {
    if !["strict", "fill"].contains(&fill_policy) {
        return Err(QueryError::Validation("unknown MMR fill_policy".into()));
    }
    let mut seen = BTreeSet::new();
    let ids: Vec<_> = candidates
        .iter()
        .filter(|s| !s.is_empty() && seen.insert((*s).clone()))
        .cloned()
        .collect();
    let mut alive = vec![true; ids.len()];
    let mut max_sim = vec![0.0f64; ids.len()];
    let mut selected = Vec::new();
    let mut picks = BTreeMap::new();
    let lam = lambda.clamp(0.0, 1.0);
    for _ in 0..top_k.min(ids.len()) {
        let mut best = None;
        let mut score = -1e30;
        for (i, id) in ids.iter().enumerate() {
            if !selected.is_empty() && threshold.is_some_and(|t| max_sim[i] >= t) {
                alive[i] = false
            }
            if !alive[i] {
                continue;
            }
            let mmr = lam * relevance.get(id).copied().unwrap_or(0.0) - (1.0 - lam) * max_sim[i];
            if mmr > score {
                best = Some(i);
                score = mmr
            }
        }
        let Some(i) = best else { break };
        alive[i] = false;
        selected.push(ids[i].clone());
        picks.insert(
            ids[i].clone(),
            MmrPick {
                mmr_score: score,
                relevance: relevance.get(&ids[i]).copied().unwrap_or(0.0),
                max_sim_to_selected: max_sim[i],
            },
        );
        if let Some(v) = vectors.get(&ids[i]) {
            for (j, id) in ids.iter().enumerate() {
                if alive[j] {
                    if let Some(u) = vectors.get(id) {
                        max_sim[j] = max_sim[j].max(cosine(u, v))
                    }
                }
            }
        }
    }
    if fill_policy == "fill" {
        for id in ids {
            if selected.len() >= top_k {
                break;
            }
            if !selected.contains(&id) {
                selected.push(id)
            }
        }
    }
    Ok((selected, picks))
}
pub fn text_match_score(query: &str, text: &str) -> (f64, serde_json::Value) {
    use serde_json::json;
    if query.is_empty() || text.is_empty() {
        return (
            0.0,
            json!({"exact_match":false,"term_matches":0,"term_ratio":0.0}),
        );
    }
    let q = query.trim().to_lowercase();
    let t = text.to_lowercase();
    let exact = t.contains(&q);
    let words: Vec<_> = q.split_whitespace().collect();
    let partial = !words.is_empty() && words.iter().all(|w| t.contains(w));
    let mut terms: Vec<_> = q
        .split(|c: char| c.is_whitespace() || "\"'-()[]，。：；！？".contains(c))
        .map(str::trim)
        .filter(|s| s.chars().count() > 1)
        .collect();
    if terms.is_empty() && q.chars().count() > 1 {
        terms.push(&q)
    }
    let matches = terms.iter().filter(|s| t.contains(**s)).count();
    let ratio = matches as f64 / terms.len().max(1) as f64;
    let score = if exact {
        1.0
    } else if partial {
        0.85
    } else if ratio >= 0.8 {
        0.7 + ratio * 0.15
    } else if ratio >= 0.5 {
        0.4 + ratio * 0.3
    } else {
        ratio * 0.4
    };
    (
        score,
        json!({"exact_match":exact,"partial_exact":partial,"term_matches":matches,"total_terms":terms.len(),"term_ratio":ratio}),
    )
}
