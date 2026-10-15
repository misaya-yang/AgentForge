"""
Password utilities - hashing, verification, validation

Provides secure password handling for the account management system.
"""

from __future__ import annotations

import os
import re
import uuid
from datetime import datetime, timedelta, timezone
from typing import Any

try:
    import bcrypt

    HAS_BCRYPT = True
except ImportError:
    HAS_BCRYPT = False
    bcrypt = None

try:
    import jwt

    HAS_JWT = True
except ImportError:
    HAS_JWT = False
    jwt = None

from ai_gateway_core.exceptions import AuthError

# ============================================================
# Configuration Constants
# ============================================================
ALLOWED_EMAIL_DOMAIN = os.environ.get("AUTH_ALLOWED_EMAIL_DOMAIN", "example.com").strip().lower()
DEFAULT_PASSWORD = os.environ.get("DEFAULT_USER_PASSWORD", "")
MIN_PASSWORD_LENGTH = 8
BCRYPT_COST_FACTOR = 12
MAX_LOGIN_ATTEMPTS = 5


# ============================================================
# Password Hashing
# ============================================================


def hash_password(password: str) -> str:
    """
    Hash password using bcrypt with a secure cost factor.

    Args:
        password: Plain text password to hash

    Returns:
        bcrypt hash string

    Raises:
        RuntimeError: If bcrypt is not installed
    """
    if not HAS_BCRYPT:
        raise RuntimeError("bcrypt is not installed. Run: pip install bcrypt")

    salt = bcrypt.gensalt(rounds=BCRYPT_COST_FACTOR)
    return bcrypt.hashpw(password.encode("utf-8"), salt).decode("utf-8")


def verify_password(password: str, hashed: str) -> bool:
    """
    Verify a password against a bcrypt hash.

    Args:
        password: Plain text password to verify
        hashed: bcrypt hash to verify against

    Returns:
        True if password matches, False otherwise
    """
    if not HAS_BCRYPT:
        return False

    if not hashed:
        return False

    try:
        return bcrypt.checkpw(password.encode("utf-8"), hashed.encode("utf-8"))
    except Exception:
        return False


# ============================================================
# Password Validation
# ============================================================


def validate_password_strength(password: str) -> list[str]:
    """
    Validate password strength requirements.

    Requirements:
    - Minimum 8 characters
    - At least one letter (a-z or A-Z)
    - At least one number (0-9)
    - At least one special character (!@#$%^&*(),.?":{}|<>)

    Args:
        password: Password to validate

    Returns:
        List of error messages (empty if valid)
    """
    errors = []

    if len(password) < MIN_PASSWORD_LENGTH:
        errors.append(f"Password must be at least {MIN_PASSWORD_LENGTH} characters")

    if not re.search(r"[a-zA-Z]", password):
        errors.append("Password must contain at least one letter")

    if not re.search(r"\d", password):
        errors.append("Password must contain at least one number")

    if not re.search(r'[!@#$%^&*(),.?":{}|<>\-_=+\[\]\\;\'`~]', password):
        errors.append("Password must contain at least one special character")

    return errors


# ============================================================
# Email Validation
# ============================================================


def is_valid_email_domain(email: str) -> bool:
    """
    Check if email belongs to allowed domain.

    Args:
        email: Email address to validate

    Returns:
        True if email domain is allowed, False otherwise
    """
    if not email:
        return False
    return email.lower().endswith(f"@{ALLOWED_EMAIL_DOMAIN}")


def validate_email(email: str) -> list[str]:
    """
    Validate email format and domain.

    Args:
        email: Email address to validate

    Returns:
        List of error messages (empty if valid)
    """
    errors = []

    if not email:
        errors.append("Email is required")
        return errors

    # Basic email format validation
    email_pattern = r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$"
    if not re.match(email_pattern, email):
        errors.append("Invalid email format")
        return errors

    # Domain validation
    if not is_valid_email_domain(email):
        errors.append(f"Only @{ALLOWED_EMAIL_DOMAIN} email addresses are allowed")

    return errors


def extract_username_from_email(email: str) -> str:
    """
    Extract username from email for use as user_id.

    Args:
        email: Email address

    Returns:
        Username portion of email with dots replaced by underscores
    """
    if not email or "@" not in email:
        return email or ""

    username = email.split("@")[0]
    # Replace dots with underscores for user_id
    return username.replace(".", "_").lower()


# ============================================================
# JWT Token Creation
# ============================================================


def create_access_token(
    data: dict[str, Any],
    secret: str,
    expires_delta: timedelta | None = None,
    algorithm: str = "HS256",
) -> tuple[str, str]:
    """
    Create a JWT access token with a unique token ID (jti).

    Args:
        data: Payload data to encode in token
        secret: Secret key for signing
        expires_delta: Token expiration time (default: 3 hours)
        algorithm: JWT algorithm (default: HS256)

    Returns:
        Tuple of (encoded JWT token string, token ID/jti)

    Raises:
        RuntimeError: If PyJWT is not installed
        AuthError: If token creation fails
    """
    if not HAS_JWT:
        raise RuntimeError("PyJWT is not installed. Run: pip install pyjwt")

    to_encode = data.copy()

    # Generate unique token ID for Redis storage
    token_id = str(uuid.uuid4())

    if expires_delta:
        expire = datetime.utcnow() + expires_delta
    else:
        expire = datetime.utcnow() + timedelta(hours=3)  # Default 3 hours

    to_encode.update(
        {
            "exp": expire,
            "iat": datetime.utcnow(),
            "jti": token_id,  # JWT ID for token identification
        }
    )

    try:
        encoded_jwt = jwt.encode(to_encode, secret, algorithm=algorithm)
        return encoded_jwt, token_id
    except Exception as exc:
        raise AuthError("Failed to create access token") from exc


def decode_token(
    token: str,
    secret: str,
    algorithm: str = "HS256",
) -> dict[str, Any] | None:
    """
    Decode and verify a JWT token.

    Args:
        token: JWT token string
        secret: Secret key for verification
        algorithm: JWT algorithm (default: HS256)

    Returns:
        Decoded payload or None if invalid
    """
    if not HAS_JWT:
        return None

    try:
        payload = jwt.decode(token, secret, algorithms=[algorithm])
        return payload
    except jwt.ExpiredSignatureError:
        return None
    except jwt.InvalidTokenError:
        return None


def get_token_id(token: str, secret: str, algorithm: str = "HS256") -> str | None:
    """
    Extract token ID (jti) from a JWT token.

    Args:
        token: JWT token string
        secret: Secret key for verification
        algorithm: JWT algorithm (default: HS256)

    Returns:
        Token ID or None if invalid
    """
    payload = decode_token(token, secret, algorithm)
    if payload:
        return payload.get("jti")
    return None


# ============================================================
# Account Lockout Utilities
# ============================================================


def is_account_locked(locked_until: datetime | str | None) -> bool:
    """
    Check if account is currently locked.

    Args:
        locked_until: Lock expiration timestamp. Accepts both ``datetime`` and
            ISO-8601 ``str`` because the value may round-trip through a Redis
            JSON cache which serialises datetimes as strings. Without this
            defensive parse, a stuck lock row would crash every login with a
            ``TypeError: '>' not supported between instances of 'str' and
            'datetime.datetime'`` — taking down auth for all users, not just
            the locked account.

    Returns:
        True if account is locked, False otherwise
    """
    if not locked_until:
        return False
    if isinstance(locked_until, str):
        try:
            locked_until = datetime.fromisoformat(locked_until.replace("Z", "+00:00"))
        except ValueError:
            return False
    # Normalise to naive UTC so the comparison never blows up on tz mismatch
    if locked_until.tzinfo is not None:
        locked_until = locked_until.astimezone(timezone.utc).replace(tzinfo=None)
    return locked_until > datetime.utcnow()


def should_lock_account(login_attempts: int) -> bool:
    """
    Check if account should be locked based on failed attempts.

    Args:
        login_attempts: Current number of failed attempts

    Returns:
        True if account should be locked
    """
    return login_attempts >= MAX_LOGIN_ATTEMPTS
