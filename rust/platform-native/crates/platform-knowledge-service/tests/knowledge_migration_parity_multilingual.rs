//! Assertion-for-assertion replacement of tests/knowledge/test_knowledge_improvements.py.
use ai_platform_knowledge_service::{
    migration_knowledge_ingest::chunking::{Chunker, ChunkingConfig, TokenCounter},
    migration_knowledge_query::lexical,
};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn multilingual_token_counts_are_positive_and_repeatable() {
    let counter = TokenCounter::new().unwrap();
    assert_eq!(counter.count(""), 0);
    for text in [
        "Hello, how are you doing today?",
        "The deployment runbook defines the rollback procedure.",
        "توثق المنصة خطوات النشر والتحقق من الصحة",
        "يجب إيقاف الترحيل عند فشل التحقق",
        "تدعم قاعدة المعرفة البحث متعدد اللغات",
        "人工智能网关平台",
        "The runbook يقول: verify service health before reopening traffic.",
    ] {
        let count = counter.count(text);
        assert!(count > 0, "{text}");
        assert_eq!(counter.count(text), count);
    }
}

#[test]
fn arabic_normalization_removes_diacritics_and_stopwords() {
    for text in [
        "بِسْمِ اللَّهِ الرَّحْمَٰنِ الرَّحِيمِ",
        "بسم الله الرحمن الرحيم",
        "والصلاة والسلام على رسول الله",
        "الإسلام والمسلمين",
    ] {
        let normalized = lexical::normalize_arabic(text);
        assert!(!normalized.is_empty());
        assert!(
            !normalized
                .chars()
                .any(|c| ('\u{064b}'..='\u{0652}').contains(&c) || c == '\u{0670}')
        );
        // The production multilingual tokenizer dispatches these all-Arabic strings
        // through the same Arabic normalization/stemming path.
        let tokens = lexical::tokenize(text, false, true, true);
        assert!(!tokens.is_empty());
        assert!(tokens.iter().all(|t| *t == lexical::normalize_arabic(t)));
    }
    assert!(!lexical::tokenize("في الصلاة", false, true, true).contains(&"في".to_owned()));
}

#[test]
fn multilingual_tokenization_keeps_original_and_detects_language() {
    for (text, language) in [
        ("What is Salah in Islam?", "en"),
        ("ما هي الصلاة في الإسلام", "ar"),
        ("智能知识库系统", "zh"),
        ("What is الصلاة prayer?", "mixed"),
    ] {
        assert_eq!(lexical::detect_language(text), language);
        let tokens = lexical::tokenize(text, true, true, true);
        assert!(!tokens.is_empty());
        assert!(tokens.contains(&text.to_lowercase()));
    }
}

#[test]
fn language_specific_fusion_weights_preserve_source_values() {
    for (text, expected) in [
        ("What is prayer in Islam?", (0.7, 0.3)),
        ("ما هي الصلاة", (0.55, 0.45)),
        ("人工智能问答", (0.75, 0.25)),
        ("Prayer الصلاة 祈祷", (0.65, 0.35)),
    ] {
        let actual = lexical::language_weights(text, 0.7, 0.3);
        assert!((actual.0 - expected.0).abs() < 1e-12);
        assert!((actual.1 - expected.1).abs() < 1e-12);
        assert!((actual.0 + actual.1 - 1.0).abs() < 1e-12);
    }
}

#[test]
fn bm25_normalization_preserves_keys_extrema_and_percentiles() {
    let scores: BTreeMap<String, f64> = [
        ("doc1", 15.5),
        ("doc2", 8.3),
        ("doc3", 2.1),
        ("doc4", 25.0),
        ("doc5", 6.7),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v))
    .collect();
    let minmax = lexical::normalize(&scores, "minmax");
    let robust = lexical::normalize(&scores, "robust");
    let percentile = lexical::normalize(&scores, "percentile");
    for result in [&minmax, &robust, &percentile] {
        assert_eq!(
            result.keys().collect::<Vec<_>>(),
            scores.keys().collect::<Vec<_>>()
        );
    }
    assert_eq!(minmax["doc3"], 0.0);
    assert_eq!(minmax["doc4"], 1.0);
    assert!(robust.values().all(|v| (0.0..=1.0).contains(v)));
    assert!((percentile["doc3"] - 0.2).abs() < 1e-12);
    assert_eq!(percentile["doc4"], 1.0);
}

#[test]
fn token_chunks_preserve_english_and_arabic_at_three_limits() {
    let text = r#"
Deployment rollback procedure.

توثق هذه الفقرة خطوات الرجوع إلى الإصدار السابق عند فشل التحقق الصحي.

Before starting rollback, pause new deployment jobs and record the failing
health-check output. Notify the platform channel with the service name,
environment, image tag, and failing endpoint.

استعد صورة الحاوية السابقة ثم تحقق من قاعدة البيانات وقائمة الانتظار وخدمة التخزين.
إذا فشل الفحص مرة أخرى، أوقف التغيير وافتح تقرير حادثة جديداً.

After the service is healthy, reopen traffic gradually and compare error rate,
latency, and queue depth against the baseline.
    "#;
    let chunker = Chunker::new().unwrap();
    for limit in [100, 200, 300] {
        let config = ChunkingConfig::from_value(
            &json!({"mode":"fixed_size", "use_token_count":true, "token_limit":limit}),
        )
        .unwrap();
        let chunks = chunker
            .process_document(text, &config, "dataset", "document", "file")
            .unwrap();
        assert!(!chunks.is_empty());
        assert_eq!(
            chunks.iter().map(|c| c.position).collect::<Vec<_>>(),
            (0..chunks.len() as i32).collect::<Vec<_>>()
        );
        assert!(
            chunks
                .iter()
                .all(|c| !c.text.trim().is_empty() && c.token_count > 0)
        );
        assert!(
            chunks
                .iter()
                .any(|c| c.text.to_lowercase().contains("rollback"))
        );
        assert!(chunks.iter().any(|c| c.text.contains("التحقق")));
    }
}
