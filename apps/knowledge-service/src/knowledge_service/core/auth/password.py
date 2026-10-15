"""Shim: password verification for delete confirmation."""
import hmac

try:
    import bcrypt
except ImportError:  # pragma: no cover - stripped-down deployments keep plaintext fallback.
    bcrypt = None


def verify_password(plain_password: str, hashed_password: str) -> bool:
    """Verify delete-confirmation passwords against stored account hashes."""
    if not plain_password or not hashed_password:
        return False
    if hashed_password.startswith(("$2a$", "$2b$", "$2y$")):
        if bcrypt is None:
            return False
        try:
            return bool(bcrypt.checkpw(plain_password.encode("utf-8"), hashed_password.encode("utf-8")))
        except Exception:
            return False
    # Legacy local rows may still contain plaintext in development DBs.
    return hmac.compare_digest(plain_password, hashed_password)
