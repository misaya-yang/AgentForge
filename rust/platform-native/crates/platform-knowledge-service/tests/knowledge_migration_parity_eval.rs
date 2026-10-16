use ai_platform_knowledge_service::{
    migration_knowledge_ingest::parser::{Block, DocIR},
    parsing_eval::*,
};
use serde_json::json;
fn block(id: &str, text: &str, order: usize) -> Block {
    serde_json::from_value(json!({"block_id":id,"text":text,"order":order})).unwrap()
}
#[test]
fn levenshtein_known_values() {
    assert_eq!(levenshtein("kitten", "sitting"), 3);
    assert_eq!(levenshtein("", "abc"), 3);
    assert_eq!(levenshtein("same", "same"), 0);
    assert_eq!(edit_distance_similarity("中文段落", "中文段落"), 1.);
    assert_eq!(edit_distance_similarity("", ""), 1.);
    assert_eq!(edit_distance_similarity("abc", ""), 0.);
    let s = edit_distance_similarity("扫描件识别文字", "扫描件识别的文字");
    assert!(s > 0. && s < 1.);
}
#[test]
fn table_similarity_identical_and_degraded() {
    let t = "| a | b |\n|---|---|\n| 1 | 2 |";
    assert_eq!(table_similarity(t, t), 1.);
    assert_eq!(table_similarity("", ""), 1.);
    let s = table_similarity(t, "| a | b |\n|---|---|\n| 1 |  |");
    assert!(s > 0. && s < 1.);
    assert_eq!(table_similarity(t, "| x | y |\n|---|---|\n| 9 | 8 |"), 0.);
}
#[test]
fn normalized_latex_and_similarity() {
    assert_eq!(normalize_latex("$\\left( x \\right)$"), "(x)");
    assert_eq!(formula_similarity("\\frac{1}{2}", "\\frac{1}{2}"), 1.);
    assert_eq!(formula_similarity("$E=mc^2$", "E = mc^2"), 1.);
    assert!(formula_similarity("\\alpha", "\\beta") < 1.);
}
#[test]
fn matched_reading_order() {
    let blocks = [
        block("a", "first para", 0),
        block("b", "second para", 1),
        block("c", "third para", 2),
    ];
    let expected = blocks.iter().collect::<Vec<_>>();
    assert_eq!(reading_order_score(&expected, &expected), 1.);
    assert_eq!(
        reading_order_score(
            &expected,
            &expected.iter().rev().copied().collect::<Vec<_>>()
        ),
        0.
    );
    assert_eq!(reading_order_score(&expected, &expected[..1]), 1.);
    let s = reading_order_score(&expected, &[&blocks[1], &blocks[0], &blocks[2]]);
    assert!(s > 0.5 && s < 1.);
}
#[test]
fn identical_document_is_perfect() {
    let d:DocIR=serde_json::from_value(json!({"doc_id":"d","pages":[{"page_number":1,"blocks":[block("x","内容甲",0),block("y","内容乙",1)]}]})).unwrap();
    let r = score_document(&d, &d);
    assert_eq!(r.pages, 1);
    assert_eq!(r.text_similarity, 1.);
    assert_eq!(r.table_similarity, 1.);
    assert_eq!(r.formula_similarity, 1.);
    assert_eq!(r.reading_order, 1.);
    assert_eq!(r.to_dict()["reading_order"], 1.);
}
#[test]
fn bad_parse_is_penalized() {
    let e:DocIR=serde_json::from_value(json!({"doc_id":"e","pages":[{"page_number":1,"blocks":[block("a","第一页文本",0),block("b","第二页文本",1)]}]})).unwrap();
    let mut a = e.clone();
    a.pages[0].blocks[0].text = "完全错误的内容".into();
    let r = score_document(&e, &a);
    assert!(r.text_similarity > 0. && r.text_similarity < 1.);
}
