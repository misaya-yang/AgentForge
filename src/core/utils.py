"""
核心工具函数
"""

from __future__ import annotations


def estimate_tokens(text: str | None) -> int:
    """
    估算文本的 token 数量。

    使用简单的启发式算法：
    - CJK 字符（中日韩）：约 2 个字符 = 1 个 token
    - 其他字符：约 4 个字符 = 1 个 token

    Args:
        text: 输入文本，可为 None

    Returns:
        估算的 token 数量，至少返回 1（非空文本）
    """
    if not text:
        return 0

    # CJK 字符范围：\u4e00-\u9fff
    cjk_count = sum(1 for c in text if "\u4e00" <= c <= "\u9fff")
    non_cjk_count = max(len(text) - cjk_count, 0)

    # CJK: 2 字符/Token，其他: 4 字符/Token
    return max(1, cjk_count // 2 + non_cjk_count // 4)


def truncate_text(text: str, max_tokens: int) -> str:
    """
    按估算 token 数截断文本。

    Args:
        text: 输入文本
        max_tokens: 最大 token 数

    Returns:
        截断后的文本
    """
    if not text or estimate_tokens(text) <= max_tokens:
        return text

    # 二分查找截断点
    left, right = 0, len(text)
    while left < right:
        mid = (left + right) // 2
        if estimate_tokens(text[:mid]) <= max_tokens:
            left = mid + 1
        else:
            right = mid

    return text[: left - 1] if left > 0 else ""
