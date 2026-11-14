#!/usr/bin/env python3
"""tern-plugin-workbuddy — Python reference plugin for tern (Step 2 skeleton).

Contract: docs/plugins-external-process-design-zh.md §4.
- Independent process, localhost HTTP only. No pip dependencies, stdlib only.
- tern allocates the port and passes it via TERN_PLUGIN_PORT / --port.
- tern already translated the client request to openai_chat; this plugin
  only signs / forwards it to the real service and streams the answer back.
- Mock mode (--mock) returns a canned OpenAI-compatible reply with usage,
  so you can test tern routing + usage counting without a real WorkBuddy
  session. Real forwarding is a stub: the WorkBuddy endpoint / signing
  is the part that needs the service's real protocol — do NOT guess it.

Run:
    python3 tern-plugin-workbuddy.py serve --port 18080 --mock
    curl http://127.0.0.1:18080/health
    curl http://127.0.0.1:18080/v1/models
    curl -X POST http://127.0.0.1:18080/v1/chat/completions \\
      -H 'Content-Type: application/json' \\
      -d '{"model":"workbuddy/hunyuan-a52b","messages":[{"role":"user","content":"hi"}],"stream":false}'

Rust migration (later): same three endpoints, same JSON shapes, same
env var / --port handshake. Only the language changes — see README.md.
"""

import argparse
import json
import os
import sys
import time
import urllib.request
import urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PLUGIN_ID = "workbuddy"
MODELS = ["workbuddy/hunyuan-a52b"]
SESSION_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "session.json")


def mask_secret(value):
    """Same rule as tern ProviderAuth Debug: first 4 + last 4, else ***."""
    if not value:
        return "***"
    if len(value) < 8:
        return "***"
    return f"{value[:4]}...{value[-4:]}"


def load_session_token():
    """Read the plugin's own session file. Never log the full token."""
    try:
        with open(SESSION_FILE, encoding="utf-8") as f:
            data = json.load(f)
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as exc:
        print(f"[plugin] session.json unreadable: {exc}", file=sys.stderr, flush=True)
        return None
    token = data.get("token") or data.get("access_token")
    if token:
        print(f"[plugin] session loaded, token {mask_secret(token)}", file=sys.stderr, flush=True)
    return token


def mock_chat_reply(body):
    """Canned OpenAI-compatible reply. usage is what tern counts."""
    stream = bool(body.get("stream"))
    model = body.get("model", MODELS[0])
    text = "Hello from the tern WorkBuddy reference plugin (mock). Replace the upstream forwarding in forward_chat() with the real service call."
    if stream:
        # Minimal SSE: one content chunk, then usage + [DONE].
        # tern's streaming usage parser reads the usage chunk.
        chunks = [
            {"id": "chatcmpl-mock", "object": "chat.completion.chunk",
             "created": int(time.time()), "model": model,
             "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": None}]},
            {"id": "chatcmpl-mock", "object": "chat.completion.chunk",
             "created": int(time.time()), "model": model,
             "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
             "usage": {"prompt_tokens": 8, "completion_tokens": 24, "total_tokens": 32}},
        ]
        return "sse", chunks
    return "json", {
        "id": "chatcmpl-mock", "object": "chat.completion",
        "created": int(time.time()), "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text},
                     "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 8, "completion_tokens": 24, "total_tokens": 32},
    }


def forward_chat(body, upstream, token):
    """Forward to the real upstream. STUB — fill in the real endpoint + signing.

    Returns (status, headers_dict, body_bytes). Raises on connection error.
    Do not retry silently on 401: one refresh attempt is the max, then
    return the 401 so tern shows it instead of hammering the account.
    """
    if not upstream:
        raise RuntimeError(
            "no upstream configured: set TERN_PLUGIN_UPSTREAM_URL or --upstream, "
            "or run with --mock to test without a real service"
        )
    url = upstream.rstrip("/") + "/v1/chat/completions"
    req = urllib.request.Request(
        url,
        data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    if token:
        req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return resp.status, dict(resp.headers), resp.read()
    except urllib.error.HTTPError as exc:
        return exc.code, dict(exc.headers), exc.read()


class Handler(BaseHTTPRequestHandler):
    mock = True
    upstream = ""
    token = None

    # Silence default logging; we log one line per request ourselves.
    def log_message(self, fmt, *args):
        sys.stderr.write(f"[plugin] {self.address_string()} {fmt % args}\n")
        sys.stderr.flush()

    def _send_json(self, status, obj):
        data = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _read_json(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length <= 0:
            return {}
        raw = self.rfile.read(length)
        try:
            return json.loads(raw.decode("utf-8"))
        except ValueError:
            return {}

    def do_GET(self):
        if self.path == "/health":
            self._send_json(200, {"ok": True, "models": MODELS})
        elif self.path in ("/v1/models", "/models"):
            self._send_json(200, {
                "object": "list",
                "data": [{"id": m, "object": "model", "created": int(time.time()),
                          "owned_by": PLUGIN_ID} for m in MODELS],
            })
        elif self.path == "/":
            self._send_json(200, {"plugin": PLUGIN_ID, "models": MODELS,
                                  "mock": self.mock, "endpoints": ["/health", "/v1/models", "/v1/chat/completions"]})
        else:
            self._send_json(404, {"error": {"message": f"not found: {self.path}", "type": "not_found"}})

    def do_POST(self):
        if self.path in ("/v1/chat/completions", "/v1/chat/completions/"):
            body = self._read_json()
            if self.mock:
                kind, payload = mock_chat_reply(body)
                if kind == "sse":
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Cache-Control", "no-cache")
                    self.end_headers()
                    for chunk in payload:
                        line = f"data: {json.dumps(chunk, ensure_ascii=False)}\n\n".encode("utf-8")
                        self.wfile.write(line)
                        self.wfile.flush()
                    self.wfile.write(b"data: [DONE]\n\n")
                    self.wfile.flush()
                else:
                    self._send_json(200, payload)
                return
            try:
                status, headers, data = forward_chat(body, self.upstream, self.token)
            except Exception as exc:  # connection error, etc.
                self._send_json(502, {"error": {"message": f"upstream failed: {exc}", "type": "upstream_error"}})
                return
            self.send_response(status)
            ctype = headers.get("Content-Type", "application/json")
            self.send_header("Content-Type", ctype)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
            return
        if self.path == "/v1/messages":
            # Anthropic shape, mock only. tern translates to openai_chat for
            # this plugin (api_format=openai_chat), so this is a fallback.
            body = self._read_json()
            text = "Hello from the tern WorkBuddy reference plugin (mock, anthropic shape)."
            self._send_json(200, {
                "id": "msg-mock", "type": "message", "role": "assistant",
                "model": body.get("model", MODELS[0]),
                "content": [{"type": "text", "text": text}],
                "stop_reason": "end_turn", "stop_sequence": None,
                "usage": {"input_tokens": 8, "output_tokens": 24},
            })
            return
        self._send_json(404, {"error": {"message": f"not found: {self.path}", "type": "not_found"}})


def main(argv=None):
    parser = argparse.ArgumentParser(prog="tern-plugin-workbuddy")
    sub = parser.add_subparsers(dest="cmd", required=True)
    serve = sub.add_parser("serve")
    serve.add_argument("--port", type=int, default=None,
                       help="port to bind (default: $TERN_PLUGIN_PORT)")
    serve.add_argument("--mock", action="store_true",
                       help="return canned replies, no real upstream call")
    serve.add_argument("--upstream", default="",
                       help="real upstream base URL (default: $TERN_PLUGIN_UPSTREAM_URL)")
    args = parser.parse_args(argv)

    port = args.port or int(os.environ.get("TERN_PLUGIN_PORT", "0") or 0)
    if not port:
        parser.error("--port is required (or set TERN_PLUGIN_PORT)")
    Handler.mock = args.mock or not (args.upstream or os.environ.get("TERN_PLUGIN_UPSTREAM_URL"))
    Handler.upstream = args.upstream or os.environ.get("TERN_PLUGIN_UPSTREAM_URL", "")
    Handler.token = load_session_token()
    if Handler.mock:
        print("[plugin] mock mode: no real upstream call", file=sys.stderr, flush=True)
    elif Handler.upstream:
        print(f"[plugin] upstream: {Handler.upstream}", file=sys.stderr, flush=True)

    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"[plugin] {PLUGIN_ID} listening on http://127.0.0.1:{port}", file=sys.stderr, flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        server.shutdown()


if __name__ == "__main__":
    main()
