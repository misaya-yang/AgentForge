from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


def test_real_local_file_process_watch_and_ledger_acceptance():
    script = Path(__file__).resolve().parents[1] / "scripts" / "local_live_acceptance.py"
    completed = subprocess.run(
        (sys.executable, str(script)),
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )
    assert completed.returncode == 0, completed.stdout or completed.stderr
    receipt = json.loads(completed.stdout)
    assert receipt["overall_status"] == "passed"
    assert receipt["evidence"]["exact_file_list_read_search_hash"]["status"] == "passed"
    assert receipt["evidence"]["watch"]["metadata_only"] is True
    assert receipt["evidence"]["atomic_write_stale_rollback_replay"]["stale_target_denied"] is True
    assert receipt["evidence"]["structured_process"]["provider_key_absent"] is True
    assert receipt["evidence"]["structured_process"]["cancel_status"] == "cancelled"
    assert receipt["evidence"]["runtime_disconnect"]["unresolved_terminal"] == "unknown"
    assert receipt["evidence"]["ledger"]["tamper_detected"] is True
