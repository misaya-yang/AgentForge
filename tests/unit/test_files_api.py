"""
Unit tests for File Upload API

Tests:
- File upload validation (size, type, user_id)
- File listing
- File deletion
- Admin endpoint access control
- Path traversal prevention
"""

import pytest

# ============ Test Validation Functions ============


class TestValidationFunctions:
    """Tests for validation helper functions."""

    def test_validate_user_id_valid(self):
        """Test valid user_id formats."""
        from src.api.v1.files import validate_user_id

        # Valid formats
        assert validate_user_id("user123") == "user123"
        assert validate_user_id("user-123") == "user-123"
        assert validate_user_id("user_123") == "user_123"
        assert validate_user_id("ABC123") == "ABC123"

    def test_validate_user_id_invalid(self):
        """Test invalid user_id formats are rejected."""
        from fastapi import HTTPException

        from src.api.v1.files import validate_user_id

        # Invalid formats (path traversal attempts)
        invalid_ids = [
            "../etc/passwd",
            "user/../../root",
            "user..id",
            "",
            "a" * 65,  # Too long
            "user<script>",
            "user;rm -rf",
        ]

        for invalid_id in invalid_ids:
            with pytest.raises(HTTPException) as exc_info:
                validate_user_id(invalid_id)
            assert exc_info.value.status_code == 400

    def test_validate_file_id_valid(self):
        """Test valid file_id formats."""
        from src.api.v1.files import validate_file_id

        # Valid 8-char hex IDs
        assert validate_file_id("a1b2c3d4") == "a1b2c3d4"
        assert validate_file_id("12345678") == "12345678"
        assert validate_file_id("abcdef12") == "abcdef12"

    def test_validate_file_id_invalid(self):
        """Test invalid file_id formats are rejected."""
        from fastapi import HTTPException

        from src.api.v1.files import validate_file_id

        # Invalid formats
        invalid_ids = [
            "abc",  # Too short
            "a1b2c3d4e5",  # Too long
            "ABCD1234",  # Uppercase not allowed
            "a1b2c3g4",  # 'g' not hex
            "",
        ]

        for invalid_id in invalid_ids:
            with pytest.raises(HTTPException) as exc_info:
                validate_file_id(invalid_id)
            assert exc_info.value.status_code == 400

    def test_validate_file_extension(self):
        """Test file extension validation."""
        from fastapi import HTTPException

        from src.api.v1.files import validate_file_extension

        # Valid extensions
        assert validate_file_extension("doc.pdf") == ".pdf"
        assert validate_file_extension("image.PNG") == ".png"
        assert validate_file_extension("data.CSV") == ".csv"

        # Invalid extensions
        with pytest.raises(HTTPException) as exc_info:
            validate_file_extension("script.exe")
        assert exc_info.value.status_code == 400


class TestAdminAccess:
    """Tests for admin access control."""

    def test_require_admin_with_admin_user(self):
        """Test admin user passes check."""
        from src.api.v1.files import ADMIN_USER_IDS, require_admin
        from src.core.auth.user_resolver import UserContext

        # Add test admin
        original_admins = ADMIN_USER_IDS.copy()
        ADMIN_USER_IDS.add("test-admin-user")

        try:
            user = UserContext(user_id="test-admin-user")
            # Should not raise
            require_admin(user)
        finally:
            ADMIN_USER_IDS.clear()
            ADMIN_USER_IDS.update(original_admins)

    def test_require_admin_with_admin_role(self):
        """Test user with admin role passes check."""
        from src.api.v1.files import require_admin
        from src.core.auth.user_resolver import UserContext

        user = UserContext(user_id="regular-user")
        user.roles = ["admin"]

        # Should not raise
        require_admin(user)

    def test_require_admin_with_file_admin_role(self):
        """Test user with file_admin role passes check."""
        from src.api.v1.files import require_admin
        from src.core.auth.user_resolver import UserContext

        user = UserContext(user_id="regular-user")
        user.roles = ["file_admin"]

        # Should not raise
        require_admin(user)

    def test_require_admin_without_admin(self):
        """Test non-admin user is rejected."""
        from fastapi import HTTPException

        from src.api.v1.files import require_admin
        from src.core.auth.user_resolver import UserContext

        user = UserContext(user_id="regular-user")

        with pytest.raises(HTTPException) as exc_info:
            require_admin(user)
        assert exc_info.value.status_code == 403


class TestFileIdGeneration:
    """Tests for file ID generation."""

    def test_generate_file_id_format(self):
        """Test file ID is 8 hex characters."""
        from src.api.v1.files import FILE_ID_PATTERN, generate_file_id

        for _ in range(10):
            file_id = generate_file_id()
            assert len(file_id) == 8
            assert FILE_ID_PATTERN.match(file_id)

    def test_generate_file_id_uniqueness(self):
        """Test file IDs are unique."""
        from src.api.v1.files import generate_file_id

        ids = [generate_file_id() for _ in range(100)]
        assert len(ids) == len(set(ids))  # All unique


class TestMimeTypes:
    """Tests for MIME type detection."""

    def test_get_mime_type_documents(self):
        """Test MIME types for documents."""
        from src.api.v1.files import get_mime_type

        assert get_mime_type(".pdf") == "application/pdf"
        assert get_mime_type(".txt") == "text/plain"
        assert get_mime_type(".csv") == "text/csv"
        assert get_mime_type(".md") == "text/markdown"

    def test_get_mime_type_images(self):
        """Test MIME types for images."""
        from src.api.v1.files import get_mime_type

        assert get_mime_type(".png") == "image/png"
        assert get_mime_type(".jpg") == "image/jpeg"
        assert get_mime_type(".jpeg") == "image/jpeg"
        assert get_mime_type(".gif") == "image/gif"
        assert get_mime_type(".webp") == "image/webp"

    def test_get_mime_type_unknown(self):
        """Test unknown extension returns octet-stream."""
        from src.api.v1.files import get_mime_type

        assert get_mime_type(".xyz") == "application/octet-stream"
        assert get_mime_type(".unknown") == "application/octet-stream"


class TestPathSecurity:
    """Tests for path security measures."""

    def test_user_uploads_path_validated(self):
        """Test that get_user_uploads_path validates user_id."""
        from fastapi import HTTPException

        from src.api.v1.files import get_user_uploads_path

        # Path traversal attempts should be blocked
        with pytest.raises(HTTPException):
            get_user_uploads_path("../etc")

        with pytest.raises(HTTPException):
            get_user_uploads_path("user/../../../root")

    def test_user_uploads_path_normal(self):
        """Test normal user_id creates proper path."""
        from src.api.v1.files import FILE_STORAGE_PATH, get_user_uploads_path

        path = get_user_uploads_path("test_user_123")

        # Should be under FILE_STORAGE_PATH
        assert str(FILE_STORAGE_PATH) in str(path)
        assert "test_user_123" in str(path)
        # Should not contain path traversal
        assert ".." not in str(path)
