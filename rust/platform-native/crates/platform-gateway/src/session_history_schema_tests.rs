use super::*;

#[test]
fn session_history_reads_runtime_tables_from_assistant_schema() {
    let queries = [
        RUNTIME_HISTORY_SQL,
        RUNTIME_SNAPSHOT_HISTORY_SQL,
        RUNTIME_CONTEXT_HISTORY_SQL,
        RUNTIME_THREAD_SESSION_SQL,
        UPLOADS_SQL,
        PARAMETERS_SQL,
        QUIZ_SQL,
    ];

    for query in queries {
        assert!(
            !query.contains("gateway.assistant_"),
            "runtime history query references the wrong schema: {query}"
        );
    }

    assert!(RUNTIME_HISTORY_SQL.contains("assistant.assistant_runs"));
    assert!(RUNTIME_HISTORY_SQL.contains("assistant.assistant_runtime_snapshots"));
    assert!(RUNTIME_HISTORY_SQL.contains("assistant.assistant_runtime_items"));
    assert!(RUNTIME_SNAPSHOT_HISTORY_SQL.contains("FROM assistant.assistant_runtime_snapshots"));
    assert!(RUNTIME_CONTEXT_HISTORY_SQL.contains("FROM assistant.assistant_runtime_items"));
    assert!(RUNTIME_THREAD_SESSION_SQL.contains("FROM assistant.assistant_runtime_threads"));
    assert!(UPLOADS_SQL.contains("JOIN assistant.assistant_runs"));
    assert!(PARAMETERS_SQL.contains("JOIN assistant.assistant_runs"));
    assert!(QUIZ_SQL.contains("FROM assistant.assistant_capability_executions"));
}
