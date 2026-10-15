"""Trusted-local, one-use approval verification.

The verifier is deliberately injected from a trusted native composition root.
It is separate from the platform action-signature verifier.  The macOS module
contains an explicit prompt/Keychain signer adapter, but the standalone CLI
does not select it and other platforms remain unavailable without injection.
"""

from __future__ import annotations

import hashlib
import sqlite3
import threading
import time
from pathlib import Path
from typing import Callable

from .models import TrustedLocalApprovalVerifier


class OneUseTrustedLocalApprovalVerifier(TrustedLocalApprovalVerifier):
    """Wrap a trusted signature callback with durable nonce consumption.

    ``verify_signature`` must be backed by the packaged native companion's
    trusted-local key verification. It is never sourced from Web or an action
    envelope. The nonce is persisted only after signature verification.
    """

    def __init__(
        self,
        *,
        device_id: str,
        state_path: Path,
        verify_signature: Callable[[bytes, str], bool],
    ) -> None:
        if not device_id:
            raise ValueError("trusted local approval device id is required")
        state_path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        self._device_id = device_id
        self._verify_signature = verify_signature
        self._lock = threading.RLock()
        self._db = sqlite3.connect(state_path, check_same_thread=False, isolation_level=None)
        state_path.chmod(0o600)
        self._db.execute("PRAGMA journal_mode=WAL")
        self._db.execute("PRAGMA synchronous=FULL")
        self._db.execute(
            "CREATE TABLE IF NOT EXISTS consumed_local_approvals ("
            "nonce TEXT PRIMARY KEY, payload_digest TEXT NOT NULL, consumed_at REAL NOT NULL)"
        )

    @property
    def device_id(self) -> str:
        return self._device_id

    def verify_and_consume(
        self,
        *,
        payload: bytes,
        signature: str,
        nonce: str,
    ) -> bool:
        if not nonce or len(nonce) > 512 or not signature or len(signature) > 8192:
            return False
        try:
            verified = self._verify_signature(payload, signature)
        except Exception:
            return False
        if verified is not True:
            return False
        digest = hashlib.sha256(payload).hexdigest()
        with self._lock:
            self._db.execute("BEGIN IMMEDIATE")
            try:
                existing = self._db.execute(
                    "SELECT payload_digest FROM consumed_local_approvals WHERE nonce=?",
                    (nonce,),
                ).fetchone()
                if existing is not None:
                    self._db.execute("ROLLBACK")
                    return False
                self._db.execute(
                    "INSERT INTO consumed_local_approvals VALUES(?,?,?)",
                    (nonce, digest, time.time()),
                )
                self._db.execute("COMMIT")
                return True
            except Exception:
                self._db.execute("ROLLBACK")
                raise

    def close(self) -> None:
        self._db.close()
