"""Container entrypoint and private probe; never log configuration or credentials."""
import argparse
import json
import os
from pathlib import Path
import stat
import urllib.request


def configuration():
    from .lifecycle import retention_seconds
    retention_seconds()
    token = os.environ.get("SDK_AGENT_WORKER_TOKEN", "")
    token_file = os.environ.get("SDK_AGENT_WORKER_TOKEN_FILE", "")
    if token_file:
        if token:
            raise ValueError("sdk_worker_ambiguous_token_source")
        token = Path(token_file).read_text().rstrip("\r\n")
    if len(token) < 32 or len(token) > 4096 or any(ord(c) < 33 or ord(c) > 126 for c in token):
        raise ValueError("sdk_worker_token_invalid")
    root = Path(os.environ.get("SDK_AGENT_STATE_ROOT", ""))
    if not root.is_absolute() or root.is_symlink() or not root.is_dir():
        raise ValueError("sdk_worker_state_invalid")
    info = root.stat()
    if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700 or not os.access(root, os.W_OK):
        raise ValueError("sdk_worker_state_permissions_invalid")
    return token


def probe():
    token = configuration()
    request = urllib.request.Request("http://127.0.0.1:8096/health/ready",
                                     headers={"Authorization": "Bearer " + token})
    # Loopback health must not leave the container through inherited HTTP proxies.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=3) as response:
        body = response.read(4097)
        if len(body) > 4096 or json.loads(body).get("ready") is not True:
            raise ValueError("sdk_worker_not_ready")


def main():
    parser = argparse.ArgumentParser(description="Private SDK worker container runtime")
    parser.add_argument("action", choices=("serve", "health"))
    args = parser.parse_args()
    try:
        if args.action == "health":
            probe()
            return 0
        token = configuration()
    except Exception:
        # Exceptions may include file contents or sensitive URLs; emit only a stable code.
        print("sdk_worker_not_ready", flush=True)
        return 1
    os.umask(0o077)
    os.environ["SDK_AGENT_WORKER_TOKEN"] = token
    os.environ.pop("SDK_AGENT_WORKER_TOKEN_FILE", None)
    import uvicorn
    uvicorn.run("sdk_agent_worker.server:app", host="127.0.0.1", port=8096,
                workers=1, access_log=False, proxy_headers=False, server_header=False,
                timeout_graceful_shutdown=10)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
