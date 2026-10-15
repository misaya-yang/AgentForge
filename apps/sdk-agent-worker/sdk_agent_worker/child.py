"""One isolated process per admitted run. Input is private stdin; stdout is bounded NDJSON."""
import asyncio
import json
import os
from pathlib import Path
import sys
from .contract import Launch
from .launch_frame import read_launch_frame
from .knowledge import ensure_access, ensure_profile
from .engines import openai_run, claude_run

async def main():
    async def emit(value):
        line = json.dumps(value, separators=(",", ":"), allow_nan=False)
        if len(line.encode()) > 1048576:
            raise RuntimeError("sdk_event_too_large")
        print(line, flush=True)
    try:
        body = read_launch_frame(sys.stdin.buffer)
        launch = Launch.parse(body["request"], body["provider_key"])
        state = Path(os.environ["HOME"])
        ensure_profile(state,launch)
        await ensure_access(launch)
        await (openai_run if launch.framework == "openai_agents" else claude_run)(launch, state, emit)
    except Exception:
        await emit({"event": "error", "code": "sdk_execution_failed"})
        return 1
    return 0

if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
