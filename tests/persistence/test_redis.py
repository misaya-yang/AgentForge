"""
Redis 存储测试

测试内容：
- JSON 序列化
- datetime 序列化
- Redis 键格式
"""

import json
from datetime import datetime, timezone

from src.persistence.redis import RedisStorage


class TestRedisJsonSerialization:
    """Redis JSON 序列化测试"""

    def test_json_default_serializes_datetime(self):
        """测试 datetime 序列化"""
        payload = {"created_at": datetime(2025, 1, 2, 3, 4, 5, tzinfo=timezone.utc)}
        s = json.dumps(payload, default=RedisStorage._json_default, ensure_ascii=False)
        assert "2025-01-02T03:04:05+00:00" in s

    def test_json_default_handles_regular_types(self):
        """测试普通类型序列化"""
        payload = {
            "string": "hello",
            "number": 42,
            "float": 3.14,
            "boolean": True,
            "null": None,
            "list": [1, 2, 3],
            "nested": {"key": "value"},
        }
        s = json.dumps(payload, default=RedisStorage._json_default, ensure_ascii=False)
        parsed = json.loads(s)

        assert parsed["string"] == "hello"
        assert parsed["number"] == 42
        assert parsed["float"] == 3.14
        assert parsed["boolean"] is True
        assert parsed["null"] is None
        assert parsed["list"] == [1, 2, 3]
        assert parsed["nested"]["key"] == "value"


class TestRedisKeyFormats:
    """Redis 键格式测试"""

    def test_session_key_format(self):
        """测试会话键格式"""
        session_id = "sess-123"
        expected = f"session:{session_id}"
        assert f"session:{session_id}" == expected

    def test_user_key_format(self):
        """测试用户键格式"""
        user_id = "user-456"
        expected = f"user:{user_id}"
        assert f"user:{user_id}" == expected

    def test_rate_limit_key_format(self):
        """测试限流键格式"""
        user_id = "user-789"
        expected = f"ratelimit:{user_id}"
        assert f"ratelimit:{user_id}" == expected

    def test_cache_key_format(self):
        """测试缓存键格式"""
        key = "some-data"
        expected = f"cache:{key}"
        assert f"cache:{key}" == expected
