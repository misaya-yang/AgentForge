import asyncio
import os

from src.core.auth.password import hash_password
from src.persistence.database import db

async def main():
    if not os.getenv("DATABASE_URL"):
        raise RuntimeError("DATABASE_URL must be set")
    admin_email = os.getenv("ADMIN_EMAIL")
    admin_password = os.getenv("ADMIN_PASSWORD")
    if not admin_email or not admin_password:
        raise RuntimeError("ADMIN_EMAIL and ADMIN_PASSWORD must be set")

    await db.connect()
    pwd_hash = hash_password(admin_password)
    await db.execute(
        "INSERT INTO users (user_id, email, password_hash, status, roles) VALUES ($1, $2, $3, $4, $5)",
        "admin", admin_email, pwd_hash, "active", ["admin"]
    )
    print("Admin created")
    await db.disconnect()

if __name__ == "__main__":
    asyncio.run(main())
