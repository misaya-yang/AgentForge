"""
Tests for path traversal attack prevention in file uploads.

Ensures that user_id and file_id validation prevents directory escape attacks.
"""

import pytest
from fastapi import HTTPException


class TestPathTraversalPrevention:
    """Test path traversal attack prevention."""

    def test_dotdot_in_user_id_rejected(self):
        """User ID containing .. should be rejected."""
        from src.api.v1.files import validate_user_id

        with pytest.raises(HTTPException) as exc_info:
            validate_user_id("../../../etc/passwd")

        assert exc_info.value.status_code == 400

    def test_slash_in_user_id_rejected(self):
        """User ID containing / should be rejected."""
        from src.api.v1.files import validate_user_id

        with pytest.raises(HTTPException) as exc_info:
            validate_user_id("user/subdir")

        assert exc_info.value.status_code == 400

    def test_backslash_in_user_id_rejected(self):
        """User ID containing \\ should be rejected."""
        from src.api.v1.files import validate_user_id

        with pytest.raises(HTTPException) as exc_info:
            validate_user_id("user\\subdir")

        assert exc_info.value.status_code == 400

    def test_dot_in_user_id_rejected(self):
        """User ID containing . should be rejected."""
        from src.api.v1.files import validate_user_id

        with pytest.raises(HTTPException) as exc_info:
            validate_user_id("user.name")

        assert exc_info.value.status_code == 400

    def test_empty_user_id_rejected(self):
        """Empty user ID should be rejected."""
        from src.api.v1.files import validate_user_id

        with pytest.raises(HTTPException) as exc_info:
            validate_user_id("")

        assert exc_info.value.status_code == 400

    def test_null_bytes_in_user_id_rejected(self):
        """User ID containing null bytes should be rejected."""
        from src.api.v1.files import validate_user_id

        with pytest.raises(HTTPException) as exc_info:
            validate_user_id("user\x00admin")

        assert exc_info.value.status_code == 400

    def test_valid_user_ids_accepted(self):
        """Valid user IDs should pass validation."""
        from src.api.v1.files import validate_user_id

        # These should all pass
        assert validate_user_id("user123") == "user123"
        assert validate_user_id("user_name") == "user_name"
        assert validate_user_id("user-name-123") == "user-name-123"
        assert validate_user_id("A" * 64) == "A" * 64  # Max length
        assert validate_user_id("UserName") == "UserName"  # Mixed case
        assert validate_user_id("123") == "123"  # Numeric only

    def test_too_long_user_id_rejected(self):
        """User ID exceeding 64 chars should be rejected."""
        from src.api.v1.files import validate_user_id

        with pytest.raises(HTTPException) as exc_info:
            validate_user_id("A" * 65)

        assert exc_info.value.status_code == 400


class TestFileIdValidation:
    """Test file ID validation."""

    def test_valid_file_id_accepted(self):
        """Valid hex file IDs should pass."""
        from src.api.v1.files import validate_file_id

        assert validate_file_id("a1b2c3d4") == "a1b2c3d4"
        assert validate_file_id("00000000") == "00000000"
        assert validate_file_id("ffffffff") == "ffffffff"

    def test_dotdot_in_file_id_rejected(self):
        """File ID containing .. should be rejected."""
        from src.api.v1.files import validate_file_id

        with pytest.raises(HTTPException) as exc_info:
            validate_file_id("../../../")

        assert exc_info.value.status_code == 400

    def test_slash_in_file_id_rejected(self):
        """File ID containing / should be rejected."""
        from src.api.v1.files import validate_file_id

        with pytest.raises(HTTPException) as exc_info:
            validate_file_id("abc/defg")

        assert exc_info.value.status_code == 400

    def test_too_short_file_id_rejected(self):
        """File ID shorter than 8 chars should be rejected."""
        from src.api.v1.files import validate_file_id

        with pytest.raises(HTTPException) as exc_info:
            validate_file_id("abc")

        assert exc_info.value.status_code == 400

    def test_too_long_file_id_rejected(self):
        """File ID longer than 8 chars should be rejected."""
        from src.api.v1.files import validate_file_id

        with pytest.raises(HTTPException) as exc_info:
            validate_file_id("a1b2c3d4e5")

        assert exc_info.value.status_code == 400

    def test_uppercase_hex_rejected(self):
        """Uppercase hex in file ID should be rejected (lowercase only)."""
        from src.api.v1.files import validate_file_id

        with pytest.raises(HTTPException) as exc_info:
            validate_file_id("A1B2C3D4")

        assert exc_info.value.status_code == 400

    def test_non_hex_chars_rejected(self):
        """Non-hex characters in file ID should be rejected."""
        from src.api.v1.files import validate_file_id

        with pytest.raises(HTTPException) as exc_info:
            validate_file_id("ghijklmn")

        assert exc_info.value.status_code == 400


class TestPathConstruction:
    """Test safe path construction."""

    def test_user_path_within_uploads_dir(self):
        """User path should always be within uploads directory."""
        from src.api.v1.files import get_uploads_path, get_user_uploads_path

        base = get_uploads_path()
        user_path = get_user_uploads_path("testuser123")

        # Verify path is under base
        assert base in user_path.parents or user_path.parent == base

        # Verify no path escape using resolve
        try:
            user_path.resolve().relative_to(base.resolve())
        except ValueError:
            pytest.fail("User path escaped uploads directory!")

    def test_special_chars_dont_escape(self):
        """Special characters that might cause escape should be rejected."""
        from src.api.v1.files import validate_user_id

        dangerous_inputs = [
            "..",
            "../",
            "..\\",
            "foo/../bar",
            "foo/./bar",
            "%2e%2e",  # URL encoded ..
            "....//",
            "..;",
        ]

        for dangerous in dangerous_inputs:
            with pytest.raises(HTTPException):
                validate_user_id(dangerous)
