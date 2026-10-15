from pathlib import Path


def test_kb_maintenance_scripts_do_not_embed_production_pg_passwords():
    scripts = [
        Path("scripts/create_admin.py"),
    ]
    production_password = "Production" "SecretPlaceholder"

    for script in scripts:
        text = script.read_text()
        assert production_password not in text
        assert 'default="postgresql://' not in text
        assert 'os.getenv("PG_DSN", "postgresql://' not in text
