import json
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import agent


class Handler(BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length)
        if self.path.startswith("/mcp"):
            body = json.loads(raw or b"{}")
            method = body.get("method")
            if method == "notifications/initialized":
                self.send_response(202)
                self.end_headers()
                return
            result = {
                "initialize": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "platform", "version": "1.0.0"},
                },
                "tools/list": {
                    "tools": [
                        {
                            "name": "space_read_page",
                            "description": "Read one page in the caller's space.",
                            "inputSchema": {
                                "type": "object",
                                "properties": {"item_id": {"type": "string"}},
                            },
                        }
                    ]
                },
                "tools/call": {"content": [{"type": "text", "text": "cited item-7"}]},
            }.get(method, {})
            payload = json.dumps({"jsonrpc": "2.0", "id": body.get("id"), "result": result}).encode()
        else:
            payload = json.dumps(
                {
                    "id": "chatcmpl-mock",
                    "object": "chat.completion",
                    "created": 0,
                    "model": "gpt-4.1-mini",
                    "choices": [
                        {
                            "index": 0,
                            "message": {"role": "assistant", "content": "cited item-7"},
                            "finish_reason": "stop",
                        }
                    ],
                }
            ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def log_message(self, fmt, *args):
        return


def test_create_agent_reads_the_mock_platform():
    if os.environ.get("LIVE") == "1":
        agent.assert_live_answer(agent.answer(agent.smoke_prompt()))
        return
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    port = server.server_address[1]
    os.environ["OPENAI_BASE_URL"] = os.environ["BASE_URL"] = f"http://127.0.0.1:{port}/v1"
    os.environ["OPENAI_API_KEY"] = os.environ["API_KEY"] = "test-key"
    os.environ["MCP_URL"] = f"http://127.0.0.1:{port}/mcp"
    try:
        assert "cited item-7" in agent.answer("Read the cited page.")
    finally:
        server.shutdown()
