"""
Tests for configuration security defaults.

Ensures default settings are secure and don't expose sensitive information.
"""


class TestDatabaseSettingsSecurity:
    """Test database configuration security."""

    def test_dsn_not_default_credentials(self):
        """DSN should not have default postgres:postgres credentials."""
        from src.config.settings import DatabaseSettings

        settings = DatabaseSettings()
        # DSN should be empty or not contain default credentials
        assert "postgres:postgres" not in settings.dsn, (
            "Default DSN should not contain default credentials"
        )

    def test_auto_init_disabled_by_default(self):
        """auto_init should be False by default for production safety."""
        from src.config.settings import DatabaseSettings

        settings = DatabaseSettings()
        # auto_init should be disabled by default to prevent accidental schema changes
        assert settings.auto_init is False, "auto_init should be disabled by default"


class TestJWTSettingsSecurity:
    """Test JWT configuration security."""

    def test_jwt_secret_is_empty_by_default(self):
        """JWT secret should be empty by default (require explicit configuration)."""
        from src.config.settings import AuthJWTSettings

        settings = AuthJWTSettings()
        # Secret should be empty, requiring explicit configuration
        assert settings.secret == "", "JWT secret should be empty by default"

    def test_jwt_disabled_by_default(self):
        """JWT should be disabled by default."""
        from src.config.settings import AuthJWTSettings

        settings = AuthJWTSettings()
        assert settings.enabled is False, "JWT should be disabled by default"


class TestRedisSettingsSecurity:
    """Test Redis configuration security."""

    def test_redis_url_not_production_default(self):
        """Redis URL should be a localhost default (not production)."""
        from src.config.settings import RedisSettings

        settings = RedisSettings()
        # Localhost is acceptable for default (dev-friendly)
        # But should not have production-looking URLs by default
        assert "localhost" in settings.url or "127.0.0.1" in settings.url, (
            "Default Redis URL should be localhost for development"
        )

    def test_redis_disabled_by_default(self):
        """Redis should be disabled by default."""
        from src.config.settings import RedisSettings

        settings = RedisSettings()
        assert settings.enabled is False, "Redis should be disabled by default"


class TestAPIKeySettingsSecurity:
    """Test API key configuration security."""

    def test_api_key_no_default_keys(self):
        """No default API keys should be configured."""
        from src.config.settings import AuthAPIKeySettings

        settings = AuthAPIKeySettings()
        # Should not have any default keys
        assert len(settings.keys) == 0, "No default API keys should be configured"

    def test_api_key_disabled_by_default(self):
        """API key auth should be disabled by default."""
        from src.config.settings import AuthAPIKeySettings

        settings = AuthAPIKeySettings()
        assert settings.enabled is False, "API key auth should be disabled by default"


class TestOverallSecurityDefaults:
    """Test overall security of default configuration."""

    def test_all_auth_disabled_by_default(self):
        """All authentication mechanisms should be disabled by default."""
        from src.config.settings import AuthenticationSettings

        settings = AuthenticationSettings()
        assert settings.jwt.enabled is False
        assert settings.api_key.enabled is False

    def test_database_disabled_by_default(self):
        """Database should be disabled by default."""
        from src.config.settings import DatabaseSettings

        settings = DatabaseSettings()
        assert settings.enabled is False
