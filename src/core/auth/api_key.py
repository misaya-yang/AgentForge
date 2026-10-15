from __future__ import annotations

import hmac

from ai_gateway_core.exceptions import AuthError


def _constant_time_compare(a: str, b: str) -> bool:
    """常量时间字符串比较，防止时序攻击"""
    return hmac.compare_digest(a.encode("utf-8"), b.encode("utf-8"))


def verify_api_key(key: str, valid_keys: list[str]) -> None:
    """
    验证 API Key（使用常量时间比较防止时序攻击）

    Args:
        key: 待验证的 API key
        valid_keys: 有效的 API key 列表

    Raises:
        AuthError: 如果 key 无效
    """
    # 使用常量时间比较，即使 key 无效也遍历所有有效 key
    # 这样攻击者无法通过响应时间差异推断有效 key
    is_valid = False
    for valid_key in valid_keys:
        if _constant_time_compare(key, valid_key):
            is_valid = True
            # 不要 break，继续遍历以保持常量时间

    if not is_valid:
        raise AuthError("Invalid API key")
