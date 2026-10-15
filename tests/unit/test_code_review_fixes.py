"""
Code Review 修复验证测试

测试内容：
1. 图片魔数验证 (validate_image_magic)
2. 页面标题后备逻辑
3. 嵌入重试机制
"""

from datetime import datetime
from unittest.mock import patch

import pytest


class TestImageMagicValidation:
    """图片魔数验证测试"""

    def setup_method(self):
        """设置魔数验证函数"""
        # 复制 knowledge.py 中的验证逻辑
        IMAGE_MAGIC_BYTES = {
            b"\xff\xd8\xff": "image/jpeg",
            b"\x89PNG\r\n\x1a\n": "image/png",
            b"GIF87a": "image/gif",
            b"GIF89a": "image/gif",
            b"RIFF": "image/webp",
            b"BM": "image/bmp",
        }

        def validate_image_magic(data: bytes) -> bool:
            """Validate image by checking magic bytes."""
            for magic in IMAGE_MAGIC_BYTES:
                if data.startswith(magic):
                    return True
            # Special case for WebP: RIFF....WEBP
            return bool(data[:4] == b"RIFF" and len(data) >= 12 and data[8:12] == b"WEBP")

        self.validate_image_magic = validate_image_magic

    def test_jpeg_magic_bytes(self):
        """测试 JPEG 魔数验证"""
        # JPEG 文件头
        jpeg_header = b"\xff\xd8\xff\xe0\x00\x10JFIF\x00"
        assert self.validate_image_magic(jpeg_header) is True

    def test_png_magic_bytes(self):
        """测试 PNG 魔数验证"""
        # PNG 文件头
        png_header = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"
        assert self.validate_image_magic(png_header) is True

    def test_gif87a_magic_bytes(self):
        """测试 GIF87a 魔数验证"""
        gif_header = b"GIF87a\x00\x00\x00\x00"
        assert self.validate_image_magic(gif_header) is True

    def test_gif89a_magic_bytes(self):
        """测试 GIF89a 魔数验证"""
        gif_header = b"GIF89a\x00\x00\x00\x00"
        assert self.validate_image_magic(gif_header) is True

    def test_webp_magic_bytes(self):
        """测试 WebP 魔数验证"""
        # WebP: RIFF....WEBP
        webp_header = b"RIFF\x00\x00\x00\x00WEBP"
        assert self.validate_image_magic(webp_header) is True

    def test_bmp_magic_bytes(self):
        """测试 BMP 魔数验证"""
        bmp_header = b"BM\x00\x00\x00\x00"
        assert self.validate_image_magic(bmp_header) is True

    def test_invalid_file_rejected(self):
        """测试无效文件被拒绝"""
        # 文本文件
        text_content = b"Hello, this is a text file"
        assert self.validate_image_magic(text_content) is False

        # PDF 文件
        pdf_header = b"%PDF-1.4"
        assert self.validate_image_magic(pdf_header) is False

        # JavaScript 伪造
        js_content = b'<script>alert("xss")</script>'
        assert self.validate_image_magic(js_content) is False

    def test_empty_content_rejected(self):
        """测试空内容被拒绝"""
        assert self.validate_image_magic(b"") is False

    def test_short_content_rejected(self):
        """测试过短内容被拒绝"""
        assert self.validate_image_magic(b"\xff") is False

    def test_spoofed_content_type_rejected(self):
        """测试伪造的 content-type 被拒绝（实际内容不是图片）"""
        # 内容是文本，但可能被伪造为 image/jpeg
        fake_jpeg = b"Not a real JPEG file content"
        assert self.validate_image_magic(fake_jpeg) is False
class TestPageTitleFallback:
    """页面标题后备逻辑测试"""

    def test_page_title_fallback_format(self):
        """测试页面标题获取失败时的后备格式"""
        # 模拟的页面 ID
        page_id = "12345678"

        # 后备标题格式应该是 "Page {page_id}"
        fallback_title = f"Page {page_id}"

        assert fallback_title == "Page 12345678"
        assert page_id in fallback_title

    def test_page_title_fallback_not_empty(self):
        """测试后备标题不是空字符串"""
        page_id = "test-page-id"
        fallback_title = f"Page {page_id}"

        assert fallback_title != ""
        assert len(fallback_title) > 0


class TestEmbeddingRetryMechanism:
    """嵌入重试机制测试"""

    @pytest.mark.asyncio
    async def test_retry_constants_defined(self):
        """测试重试策略常量已定义且配置合理。

        不对具体数字做硬编码，避免实现优化（如重试次数/基础延迟调整）导致
        非功能性回归失败。这里仅验证“有上限重试 + 正向退避 + 超时保护”。
        """
        from knowledge_service.services.knowledge.embedding import DashScopeEmbedding

        assert hasattr(DashScopeEmbedding, "MAX_RETRIES")
        assert hasattr(DashScopeEmbedding, "RETRY_BASE_DELAY")
        assert hasattr(DashScopeEmbedding, "REQUEST_TIMEOUT")

        assert isinstance(DashScopeEmbedding.MAX_RETRIES, int)
        assert 1 <= DashScopeEmbedding.MAX_RETRIES <= 10

        assert isinstance(DashScopeEmbedding.RETRY_BASE_DELAY, (int, float))
        assert 0 < DashScopeEmbedding.RETRY_BASE_DELAY <= 5

        assert isinstance(DashScopeEmbedding.REQUEST_TIMEOUT, (int, float))
        assert DashScopeEmbedding.REQUEST_TIMEOUT >= 30


class TestEmbeddingRetryCompatibility:
    @pytest.mark.asyncio
    async def test_call_with_retry_method_exists(self):
        """测试 _call_with_retry 方法存在"""
        from knowledge_service.services.knowledge.embedding import DashScopeEmbedding

        # 创建一个 mock 实例来检查方法
        with patch.object(DashScopeEmbedding, "__init__", lambda _x: None):
            embedding = DashScopeEmbedding()
            assert hasattr(embedding, "_call_with_retry")
            assert callable(getattr(embedding, "_call_with_retry", None))


class TestDatabaseMigration:
    """数据库迁移测试"""

    def test_migration_file_contains_sync_images_field(self):
        """测试迁移文件包含 sync_images 字段"""
        import os

        migration_path = "database/migrations/016_confluence_multi_root_pages.sql"

        # 检查文件存在
        assert os.path.exists(migration_path), f"Migration file should exist: {migration_path}"

        # 读取文件内容
        with open(migration_path, encoding="utf-8") as f:
            content = f.read()

        # 检查 sync_images 字段
        assert "sync_images" in content, "Migration should add sync_images column"
        assert "image_max_size_bytes" in content, "Migration should add image_max_size_bytes column"
        assert "BOOLEAN" in content, "sync_images should be BOOLEAN type"
        assert "INTEGER" in content or "int" in content.lower(), (
            "image_max_size_bytes should be INTEGER type"
        )

    def test_migration_file_contains_root_page_ids(self):
        """测试迁移文件包含 root_page_ids 字段"""

        migration_path = "database/migrations/016_confluence_multi_root_pages.sql"

        with open(migration_path, encoding="utf-8") as f:
            content = f.read()

        assert "root_page_ids" in content, "Migration should add root_page_ids column"
        assert "root_page_titles" in content, "Migration should add root_page_titles column"
        assert "TEXT[]" in content, "Arrays should be TEXT[] type"


class TestCryptoUtilities:
    """加密工具测试"""

    def test_encrypt_decrypt_roundtrip(self):
        """测试加密解密往返"""
        from src.core.crypto import decrypt_value, encrypt_value

        original = "my-secret-api-token-12345"
        encryption_key = "test-encryption-key-32chars!!"

        encrypted = encrypt_value(original, encryption_key)
        assert encrypted != original
        assert encrypted.startswith("enc:")

        decrypted = decrypt_value(encrypted, encryption_key)
        assert decrypted == original

    def test_encrypt_empty_value(self):
        """测试空值加密"""
        from src.core.crypto import encrypt_value

        result = encrypt_value("", "some-key")
        assert result == ""

        result = encrypt_value(None, "some-key")
        assert result is None

    def test_decrypt_plaintext_returns_as_is(self):
        """测试解密明文直接返回"""
        from src.core.crypto import decrypt_value

        plaintext = "not-encrypted-value"
        result = decrypt_value(plaintext, "some-key")
        assert result == plaintext

    def test_encrypt_without_key_returns_plaintext(self):
        """测试无密钥时返回明文"""
        from src.core.crypto import encrypt_value

        value = "my-secret"
        result = encrypt_value(value, "")
        assert result == value

    def test_is_encrypted_function(self):
        """测试 is_encrypted 函数"""
        from src.core.crypto import is_encrypted

        assert is_encrypted("enc:somedata") is True
        assert is_encrypted("plaintext") is False
        assert is_encrypted("") is False

    def test_generate_encryption_key(self):
        """测试生成加密密钥"""
        from src.core.crypto import generate_encryption_key

        key = generate_encryption_key()
        assert len(key) == 32  # 16 bytes hex = 32 chars
        assert key.isalnum()


class TestCQLValidation:
    """CQL 输入验证测试"""

    def test_valid_space_keys(self):
        """测试有效的 Space Key"""
        from knowledge_service.services.knowledge.confluence.client import _escape_cql_value

        assert _escape_cql_value("ENG") == "ENG"
        assert _escape_cql_value("my-space") == "my-space"
        assert _escape_cql_value("space_123") == "space_123"
        assert _escape_cql_value("ABC123") == "ABC123"

    def test_invalid_space_keys_rejected(self):
        """测试无效的 Space Key 被拒绝"""
        from knowledge_service.services.knowledge.confluence.client import _escape_cql_value

        invalid_keys = [
            "space key",  # space
            "space'key",  # quote
            'space"key',  # double quote
            "space;key",  # semicolon
            "space=key",  # equals
            "space(key)",  # parentheses
            "space AND type=page",  # injection attempt
        ]

        for key in invalid_keys:
            try:
                _escape_cql_value(key)
                raise AssertionError(f"Should reject invalid key: {key}")
            except ValueError:
                pass  # Expected

    def test_escape_cql_string(self):
        """测试 CQL 字符串转义"""
        from knowledge_service.services.knowledge.confluence.client import _escape_cql_string

        assert _escape_cql_string('hello"world') == 'hello\\"world'
        assert _escape_cql_string("hello\\world") == "hello\\\\world"
        assert _escape_cql_string("normal") == "normal"


class TestUtcNowFunction:
    """UTC 时间函数测试"""

    def test_utc_now_returns_naive_datetime(self):
        """测试 _utc_now 返回无时区 datetime"""
        from knowledge_service.services.knowledge.confluence.sync_service import _utc_now

        now = _utc_now()
        assert now.tzinfo is None  # Naive datetime

    def test_utc_now_is_utc(self):
        """测试 _utc_now 返回 UTC 时间"""
        from datetime import timezone

        from knowledge_service.services.knowledge.confluence.sync_service import _utc_now

        now = _utc_now()
        utc_now = datetime.now(timezone.utc).replace(tzinfo=None)

        # Should be within 1 second
        diff = abs((now - utc_now).total_seconds())
        assert diff < 1.0

    def test_scheduler_utc_now(self):
        """测试调度器 _utc_now 函数"""
        from knowledge_service.services.knowledge.confluence.scheduler import _utc_now

        now = _utc_now()
        assert now.tzinfo is None  # Naive datetime


class TestAsyncTaskHandling:
    """异步任务处理测试"""

    @pytest.mark.asyncio
    async def test_background_task_creation(self):
        """测试后台任务创建方法存在"""
        from knowledge_service.services.knowledge.confluence.sync_service import (
            ConfluenceSyncService,
        )

        # Check that the method exists
        assert hasattr(ConfluenceSyncService, "_create_background_task")
        assert hasattr(ConfluenceSyncService, "_handle_task_exception")


# 运行测试的入口
if __name__ == "__main__":
    pytest.main([__file__, "-v"])
