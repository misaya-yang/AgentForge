from __future__ import annotations

import logging
from typing import Any

import jwt

from ai_gateway_core.exceptions import AuthError

logger = logging.getLogger(__name__)


def decode_jwt_token(
    token: str,
    secret: str,
    algorithms: list[str],
    audience: str | None = None,
    issuer: str | None = None,
) -> dict[str, Any]:
    """
    解码并验证 JWT token

    Args:
        token: JWT token 字符串
        secret: 签名密钥
        algorithms: 允许的算法列表
        audience: 预期的 audience（可选）
        issuer: 预期的 issuer（可选）

    Returns:
        解码后的 payload 字典

    Raises:
        AuthError: 包含具体错误信息的认证异常
    """
    try:
        options = {"verify_aud": audience is not None}
        return jwt.decode(
            token,
            secret,
            algorithms=algorithms,
            audience=audience,
            issuer=issuer,
            options=options,
        )
    except jwt.ExpiredSignatureError:
        logger.debug("JWT token has expired")
        raise AuthError("Token has expired")
    except jwt.InvalidAudienceError:
        logger.debug("JWT token has invalid audience")
        raise AuthError("Invalid token audience")
    except jwt.InvalidIssuerError:
        logger.debug("JWT token has invalid issuer")
        raise AuthError("Invalid token issuer")
    except jwt.InvalidSignatureError:
        logger.warning("JWT token has invalid signature")
        raise AuthError("Invalid token signature")
    except jwt.DecodeError as exc:
        logger.warning(f"JWT token decode error: {exc}")
        raise AuthError("Malformed token")
    except jwt.InvalidTokenError as exc:
        logger.warning(f"JWT token validation error: {exc}")
        raise AuthError("Invalid token")
    except Exception as exc:
        logger.error(f"Unexpected JWT error: {exc}")
        raise AuthError("Token validation failed") from exc
