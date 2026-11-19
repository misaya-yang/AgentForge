//! Offline BM25 tokenizer comparison; Qdrant's server tokenizer is not replaced.
use super::{
    Args, Result,
    bakeoff::{Case, dcg},
    error, fail,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
fn cjk(c: char) -> bool {
    matches!(c,'\u{3040}'..='\u{30ff}'|'\u{3400}'..='\u{4dbf}'|'\u{4e00}'..='\u{9fff}'|'\u{f900}'..='\u{faff}'|'\u{ac00}'..='\u{d7af}')
}
pub fn tokenize(text: &str, mode: &str, jieba: &jieba_rs::Jieba) -> Vec<String> {
    let mut runs: Vec<(bool, String)> = vec![];
    for c in text.chars() {
        let kind = cjk(c);
        if runs.last().is_none_or(|r| r.0 != kind) {
            runs.push((kind, String::new()));
        }
        runs.last_mut().unwrap().1.push(c);
    }
    let mut tokens = vec![];
    for (is_cjk, run) in runs {
        if !is_cjk {
            let lower = run.to_lowercase();
            tokens.extend(
                lower
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned),
            );
        } else {
            match mode {
                "pg_simple_emulation" => tokens.push(run),
                "jieba_words" => tokens.extend(
                    jieba
                        .tokenize(&run, jieba_rs::TokenizeMode::Default, true)
                        .into_iter()
                        .filter(|t| !t.word.trim().is_empty())
                        .map(|t| t.word.to_owned()),
                ),
                _ => {
                    let chars: Vec<_> = run.chars().collect();
                    if chars.len() == 1 {
                        tokens.push(run);
                    } else {
                        tokens.extend(chars.windows(2).map(|pair| pair.iter().collect()));
                    }
                }
            }
        }
    }
    tokens
}
pub struct Bm25 {
    tf: Vec<BTreeMap<String, usize>>,
    lengths: Vec<usize>,
    average: f64,
    idf: BTreeMap<String, f64>,
}
impl Bm25 {
    pub fn new(docs: &[Vec<String>]) -> Self {
        let mut tf = vec![];
        let mut df: BTreeMap<String, usize> = BTreeMap::new();
        let mut lengths = vec![];
        for doc in docs {
            lengths.push(doc.len());
            let mut counts = BTreeMap::new();
            for token in doc {
                *counts.entry(token.clone()).or_insert(0) += 1;
            }
            for token in counts.keys() {
                *df.entry(token.clone()).or_insert(0) += 1;
            }
            tf.push(counts);
        }
        let n = docs.len() as f64;
        let average = lengths.iter().sum::<usize>() as f64 / n.max(1.);
        let idf = df
            .into_iter()
            .map(|(token, d)| (token, (1. + (n - d as f64 + 0.5) / (d as f64 + 0.5)).ln()))
            .collect();
        Self {
            tf,
            lengths,
            average,
            idf,
        }
    }
    pub fn scores(&self, query: &[String]) -> Vec<f64> {
        let mut scores = vec![0.; self.tf.len()];
        for token in query.iter().collect::<BTreeSet<_>>() {
            let Some(idf) = self.idf.get(token) else {
                continue;
            };
            for (i, counts) in self.tf.iter().enumerate() {
                let f = *counts.get(token).unwrap_or(&0) as f64;
                if f > 0. {
                    let denominator = f + 1.2
                        * (1. - 0.75
                            + 0.75 * self.lengths[i] as f64
                                / if self.average > 0. { self.average } else { 1. });
                    scores[i] += idf * f * 2.2 / denominator;
                }
            }
        }
        scores
    }
}
fn run_leg(
    name: &str,
    cases: &[Case],
    corpus: &BTreeMap<String, String>,
    k: usize,
    recall_cut: usize,
    jieba: &jieba_rs::Jieba,
) -> Value {
    let ids: Vec<_> = corpus.keys().collect();
    let docs: Vec<_> = corpus.values().map(|s| tokenize(s, name, jieba)).collect();
    let index = Bm25::new(&docs);
    let mut per_case = vec![];
    for case in cases {
        let tokens = tokenize(&case.query, name, jieba);
        let scores = index.scores(&tokens);
        let mut order: Vec<_> = (0..ids.len()).collect();
        order.sort_by(|a, b| scores[*b].total_cmp(&scores[*a]).then(ids[*a].cmp(ids[*b])));
        let ranked: Vec<_> = order.iter().map(|i| ids[*i]).collect();
        let grades: Vec<_> = ranked
            .iter()
            .map(|id| *case.relevance.get(*id).unwrap_or(&0))
            .collect();
        let mut ideal: Vec<_> = case.relevance.values().copied().collect();
        ideal.sort_by(|a, b| b.cmp(a));
        let idcg = dcg(&ideal, k);
        let top: Vec<_> = ranked.iter().take(recall_cut).copied().collect();
        let hits = top
            .iter()
            .filter(|id| case.relevance.get(**id).is_some_and(|g| *g > 0))
            .count();
        let mrr = grades
            .iter()
            .position(|g| *g > 0)
            .map(|i| 1. / (i + 1) as f64)
            .unwrap_or(0.);
        per_case.push(json!({"case_id":case.id,"language":case.language,"recall_at_k":if case.relevance.is_empty(){0.}else{hits as f64/case.relevance.len()as f64},"mrr":mrr,"ndcg_at_k":if idcg>0.{dcg(&grades,k)/idcg}else{0.},"top_ids":top.into_iter().take(5).collect::<Vec<_>>(),"query_token_count":tokens.len()}));
    }
    let slice = |language: Option<&str>| {
        let rows: Vec<_> = per_case
            .iter()
            .filter(|r| language.is_none_or(|l| r["language"] == l))
            .collect();
        if rows.is_empty() {
            return json!({"cases":0});
        }
        let average = |key: &str| {
            rows.iter().map(|r| r[key].as_f64().unwrap()).sum::<f64>() / rows.len() as f64
        };
        json!({"cases":rows.len(),"recall_at_k":average("recall_at_k"),"mrr":average("mrr"),"ndcg_at_k":average("ndcg_at_k")})
    };
    json!({"leg":name,"eligible":true,"slices":{"overall":slice(None),"language:zh":slice(Some("zh")),"language:en":slice(Some("en"))},"per_case":per_case})
}
pub fn build(cases: &[Case], k: usize, recall_cut: usize) -> Result<Value> {
    let mut corpus = BTreeMap::new();
    for case in cases {
        for (id, text) in &case.candidates {
            if corpus.get(id).is_some_and(|old| old != text) {
                return fail("corpus conflict on segment id");
            }
            corpus.insert(id.clone(), text.clone());
        }
    }
    let jieba = jieba_rs::Jieba::new();
    let mut legs: Vec<_> = ["pg_simple_emulation", "cjk_bigram_proxy", "jieba_words"]
        .into_iter()
        .map(|name| run_leg(name, cases, &corpus, k, recall_cut, &jieba))
        .collect();
    legs.push(json!({"leg":"bge_m3_learned_sparse","eligible":false,"skip_reason":"learned sparse requires the inference container; not runnable offline"}));
    Ok(
        json!({"schema":"chinese-lexical-shadow-v1","generated_at":chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs,false),"params":{"k":k,"recall_cut":recall_cut,"corpus_size":corpus.len(),"cases":cases.len()},"legs":legs}),
    )
}
pub fn run(args: &[String]) -> Result<()> {
    let args = Args::parse(args, &[])?;
    args.validate(&["cases", "out", "top-k", "recall-cut"], 0)?;
    let cases = super::bakeoff::load(Path::new(args.get(
        "cases",
        "tests/fixtures/eval/rag/bakeoff/rerank_bakeoff_v1.jsonl",
    )))?;
    let k = args.get("top-k", "10").parse()?;
    let cut = args.get("recall-cut", "5").parse()?;
    let out = args
        .options
        .get("out")
        .ok_or_else(|| error("--out required"))?;
    let report = build(&cases, k, cut)?;
    super::bakeoff::write_reports(
        Path::new(out),
        &report,
        "Chinese lexical shadow eval (PRD T2-1)",
    )?;
    for leg in report["legs"].as_array().unwrap() {
        println!(
            "{}: {}",
            leg["leg"].as_str().unwrap(),
            if leg["eligible"] == true {
                format!(
                    "zh recall={:.3}",
                    leg["slices"]["language:zh"]["recall_at_k"]
                        .as_f64()
                        .unwrap_or(0.)
                )
            } else {
                "skip".into()
            }
        );
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cjk_runs_and_latin_preserve_source_behavior() {
        let j = jieba_rs::Jieba::new();
        assert_eq!(
            tokenize("中文 ABC12", "pg_simple_emulation", &j),
            vec!["中文", "abc12"]
        );
        assert_eq!(
            tokenize("中文词 A", "cjk_bigram_proxy", &j),
            vec!["中文", "文词", "a"]
        );
        assert_eq!(tokenize("字", "cjk_bigram_proxy", &j), vec!["字"]);
    }
    #[test]
    fn bm25_repeated_query_tokens_do_not_double_score() {
        let index = Bm25::new(&[vec!["a".into(), "a".into()], vec!["b".into()]]);
        assert_eq!(
            index.scores(&["a".into()]),
            index.scores(&["a".into(), "a".into()])
        );
        assert!(index.scores(&["a".into()])[0] > 0.);
    }
}
