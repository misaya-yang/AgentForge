"""Stream one immutable Agent Publication with a scoped Runtime API token."""

from __future__ import annotations

import asyncio
import json
import os
from pathlib import Path

import httpx


async def main() -> None:
    base_url = os.environ.get("AI_GATEWAY_BASE_URL", "http://localhost:8080").rstrip("/")
    publication_id = os.environ["AGENT_PUBLICATION_ID"]
    runtime_token = os.environ["AGENT_RUNTIME_TOKEN"]
    headers = {
        "Authorization": f"Bearer {runtime_token}",
        "Idempotency-Key": "sdk-example-turn-1",
    }
    async with httpx.AsyncClient(base_url=base_url, timeout=120) as client:
        attachments: list[dict[str, str]] = []
        attachment_path = os.environ.get("AGENT_ATTACHMENT_PATH", "").strip()
        if attachment_path:
            path = Path(attachment_path).expanduser()
            with path.open("rb") as handle:
                upload_response = await client.post(
                    f"/api/v1/agent-runtime/{publication_id}/attachments",
                    headers={"Authorization": headers["Authorization"]},
                    files={"file": (path.name, handle)},
                )
            upload_response.raise_for_status()
            uploaded = upload_response.json()
            attachments.append({
                "artifact_id": uploaded["artifact_id"],
                "filename": uploaded["filename"],
                "mime_type": uploaded["mime_type"],
            })
        session_response = await client.post(
            f"/api/v1/agent-runtime/{publication_id}/sessions",
            headers={"Authorization": headers["Authorization"]},
        )
        session_response.raise_for_status()
        session_id = session_response.json()["session_id"]
        async with client.stream(
            "POST",
            f"/api/v1/agent-runtime/{publication_id}/chat/stream",
            headers=headers,
            json={
                "message": "Summarize the approved policy.",
                "session_id": session_id,
                "attachments": attachments,
            },
        ) as response:
            response.raise_for_status()
            async for line in response.aiter_lines():
                if line.startswith("data:") and line[5:].strip() != "[DONE]":
                    print(json.loads(line[5:]))


if __name__ == "__main__":
    asyncio.run(main())
