from __future__ import annotations

from ai_gateway_contracts.capability_proof import sign_capability_proof


def test_python_proof_matches_rust_fixture() -> None:
    header = sign_capability_proof(
        "capability-proof-secret-012345678901234567890123",
        method="post",
        path=" /internal/v2/capabilities/knowledge/docs/retrieve ",
        body={"query": "退款", "top_k": 5, "threshold": 0.0},
        tenant_id="tenant-a",
        user_id="user-a",
        session_id="session-a",
        execution_id="exec_01",
        run_id="run_01",
        nonce="nonce-fixed-0123456789",
        now=1_700_000_000,
    )
    assert header == (
        "v1.eyJib2R5X3NoYTI1NiI6ImU2NTk3MGI3YzM2NWE4NmI2YzZkZGVjZDNhODllOGRl"
        "MzZkMzlhNzE5YTlhYjFmNGMwZGQ0YWI0MmMwNjYxMmEiLCJleGVjdXRpb25faWQiOiJleGVj"
        "XzAxIiwiZXhwaXJlc19hdCI6MTcwMDAwMDAzMCwibWV0aG9kIjoiUE9TVCIsIm5vbmNlIjoi"
        "bm9uY2UtZml4ZWQtMDEyMzQ1Njc4OSIsInBhdGgiOiIvaW50ZXJuYWwvdjIvY2FwYWJpbGl0"
        "aWVzL2tub3dsZWRnZS9kb2NzL3JldHJpZXZlIiwicnVuX2lkIjoicnVuXzAxIiwic2NoZW1h"
        "X3ZlcnNpb24iOiJhaS1wbGF0Zm9ybS1jYXBhYmlsaXR5LXByb29mL3YxIiwic2Vzc2lvbl9p"
        "ZCI6InNlc3Npb24tYSIsInNpZ25hdHVyZSI6IjUyNjBhNjExOTQyNTAxMzg1ODY2YmI0MTc1"
        "OGRlNjQ5OGY4NjE1MjdlYjNiNTExZTdkNWRiYmE5M2I4NjlhZmEiLCJ0ZW5hbnRfaWQiOiJ0"
        "ZW5hbnQtYSIsInVzZXJfaWQiOiJ1c2VyLWEifQ"
    )
