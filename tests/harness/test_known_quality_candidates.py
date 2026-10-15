from scripts.evidence.known_candidates import validate


def test_prd_confirmed_dead_and_self_proving_candidates_are_closed() -> None:
    result = validate()

    assert result["result"] == "pass"
    assert result["pending_scoped_removal"] == []
    assert result["removed"] == [
        "file-storage-barrel-only-export",
        "unused-get-langgraph-proxy",
    ]
    assert result["repaired"] == [
        "streaming-timeout-placeholder",
        "streaming-connection-placeholder",
    ]
