// Run the original shared fixtures against the production parser in its owning crate.
#[path = "../src/migration_knowledge_query/qa_stream.rs"]
mod qa_stream;
use qa_stream::QaProviderStream;
use serde_json::Value;

#[test]
fn shared_qa_provider_contract() {
    let cases: Value = serde_json::from_str(include_str!("../../../../../tests/fixtures/knowledge/qa-provider-stream.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let mut parser = QaProviderStream::default();
        let mut text = String::new(); let mut tokens = Value::Null; let mut error = None;
        let mut closed = false;
        for line in case["lines"].as_array().unwrap() {
            match parser.line(line.as_str().unwrap()) {
                Ok(Some(event)) => {
                    if let Some(content) = event.content { text.push_str(&content); }
                    if let Some(total) = event.tokens { tokens = total.into(); }
                    if event.closed { closed = true; break; }
                }
                Ok(None) => {},
                Err(reason) => { error = Some(reason); break; }
            }
        }
        if error.is_none() && !closed {
            match parser.finish() {
                Ok(Some(event)) => {
                    if let Some(content) = event.content { text.push_str(&content); }
                    if let Some(total) = event.tokens { tokens = total.into(); }
                }
                Ok(None) => {},
                Err(reason) => error = Some(reason),
            }
        }
        assert_eq!(error, case["error"].as_str(), "{}", case["name"]);
        assert_eq!(text, case["text"].as_str().unwrap(), "{}", case["name"]);
        assert_eq!(tokens, case["tokens"], "{}", case["name"]);
    }
}
#[test]
fn event_budget_covers_multiple_lines_and_utf8_bytes() {
    let mut parser = QaProviderStream::default();
    assert!(parser.line(&format!("data: {}", "x".repeat(3 * 1024 * 1024))).is_ok());
    assert_eq!(parser.line(&format!("data: {}", "x".repeat(2 * 1024 * 1024))).unwrap_err(), "qa_stream_event_too_large");
    let mut parser = QaProviderStream::default();
    assert_eq!(parser.line(&"中".repeat(2 * 1024 * 1024)).unwrap_err(), "qa_stream_event_too_large");
}
