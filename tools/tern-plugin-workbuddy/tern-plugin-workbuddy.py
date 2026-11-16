#!/usr/bin/env python3
"""Python reference sidecar for tern's external-plugin contract.

This is a mock-capable contract example, not a real WorkBuddy integration.
The actual WorkBuddy endpoint, login flow, and signing protocol are intentionally
not guessed. See README.md and docs/plugins-external-process-design-zh.md.

Run locally (POSIX):
    TERN_PLUGIN_TOKEN=0123456789abcdef0123456789abcdef python3 tern-plugin-workbuddy.py serve --port 18080 --mock

Tern strips the provider prefix before forwarding: client model
``workbuddy/hunyuan-a52b`` becomes plugin model ``hunyuan-a52b``.
"""

import argparse
import hmac
import json
import os
import sys
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PLUGIN_ID = "workbuddy"
MODELS = ["hunyuan-a52b"]  # Bare upstream IDs; never include the tern provider prefix.
MAX_REQUEST_BYTES = 8 * 1024 * 1024
SESSION_FILE = os.path.join(os.path.dirname(os.path.abspath(__file__)), "session.json")


def load_session_token():
    """Read this plugin's own session file without logging credential material."""
    try:
        with open(SESSION_FILE, encoding="utf-8") as session_file:
            data = json.load(session_file)
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as exc:
        print(f"[plugin] session.json unreadable ({type(exc).__name__})", file=sys.stderr, flush=True)
        return None
    if not isinstance(data, dict):
        return None
    token = data.get("token") or data.get("access_token")
    return token if isinstance(token, str) and token else None


def mock_chat_reply(body):
    """Canned OpenAI-compatible reply; usage lets tern exercise its parser."""
    model = body.get("model", MODELS[0])
    text = "Hello from the tern WorkBuddy reference plugin (mock). Real WorkBuddy forwarding is not implemented."
    if body.get("stream") is True:
        now = int(time.time())
        chunks = [
            {
                "id": "chatcmpl-mock",
                "object": "chat.completion.chunk",
                "created": now,
                "model": model,
                "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": None}],
            },
            {
                "id": "chatcmpl-mock",
                "object": "chat.completion.chunk",
                "created": now,
                "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 8, "completion_tokens": 24, "total_tokens": 32},
            },
        ]
        return chunks
    return {
        "id": "chatcmpl-mock",
        "object": "chat.completion",
        "created": int(time.time()),
        "model": model,
        "choices": [{"index": 0, "message": {"role": "assistant", "content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 8, "completion_tokens": 24, "total_tokens": 32},
    }


def chat_completions_url(upstream):
    """Accept an OpenAI API root, /v1 root, or full chat-completions URL."""
    base = upstream.rstrip("/")
    if base.endswith("/v1/chat/completions"):
        return base
    if base.endswith("/v1"):
        return base + "/chat/completions"
    return base + "/v1/chat/completions"


def forward_chat(body, upstream, session_token):
    """Open the upstream response and return it unconsumed for genuine streaming.

    The caller owns and closes the returned response. No request credentials or
    X-Tern-Plugin-Token are forwarded; only the plugin's own session token is
    used for the real upstream.
    """
    if not upstream:
        raise RuntimeError("no upstream configured; use --mock for local contract tests")
    request = urllib.request.Request(
        chat_completions_url(upstream),
        data=json.dumps(body).encode("utf-8"),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    if session_token:
        request.add_header("Authorization", f"Bearer {session_token}")
    try:
        response = urllib.request.urlopen(request, timeout=60)
    except urllib.error.HTTPError as error_response:
        # HTTPError is also a readable response; preserve upstream status/body.
        response = error_response
    return response.status, response.headers, response


class Handler(BaseHTTPRequestHandler):
    # Close-delimited HTTP/1.0 responses let us relay upstream SSE incrementally
    # without buffering or inventing a Content-Length.
    protocol_version = "HTTP/1.0"
    token = None
    mock = True
    upstream = ""
    session_token = None

    def log_message(self, fmt, *args):
        # Never include query strings (which can contain credentials) in logs.
        route = self.path.partition("?")[0]
        sys.stderr.write(f"[plugin] {self.address_string()} {self.command} {route}\n")
        sys.stderr.flush()

    def _authorized(self):
        expected = self.token
        supplied = self.headers.get("X-Tern-Plugin-Token", "")
        return isinstance(expected, str) and bool(expected) and hmac.compare_digest(supplied, expected)

    def _require_auth(self):
        if self._authorized():
            return True
        self._send_json(401, {"error": {"message": "unauthorized", "type": "authentication_error"}})
        return False

    def _send_json(self, status, obj):
        data = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _read_json(self):
        raw_length = self.headers.get("Content-Length")
        try:
            length = int(raw_length or "0")
        except ValueError as exc:
            raise ValueError("invalid Content-Length") from exc
        if length <= 0:
            raise ValueError("request body is required")
        if length > MAX_REQUEST_BYTES:
            raise OverflowError("request body too large")
        try:
            body = json.loads(self.rfile.read(length).decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise ValueError("request body must be valid JSON") from exc
        if not isinstance(body, dict):
            raise ValueError("request body must be a JSON object")
        return body

    def do_GET(self):
        if not self._require_auth():
            return
        route = self.path.partition("?")[0]
        if route == "/health":
            self._send_json(200, {"ok": True, "models": MODELS})
        elif route == "/v1/models":
            self._send_json(200, {
                "object": "list",
                "data": [
                    {"id": model, "object": "model", "created": int(time.time()), "owned_by": PLUGIN_ID}
                    for model in MODELS
                ],
            })
        else:
            self._send_json(404, {"error": {"message": "not found", "type": "not_found"}})

    def do_POST(self):
        if not self._require_auth():
            return
        if self.path.partition("?")[0] != "/v1/chat/completions":
            self._send_json(404, {"error": {"message": "not found", "type": "not_found"}})
            return
        try:
            body = self._read_json()
        except OverflowError:
            self._send_json(413, {"error": {"message": "request body too large", "type": "invalid_request_error"}})
            return
        except ValueError as error:
            self._send_json(400, {"error": {"message": str(error), "type": "invalid_request_error"}})
            return
        if not isinstance(body.get("model"), str) or not body["model"].strip():
            self._send_json(400, {"error": {"message": "model is required", "type": "invalid_request_error"}})
            return

        if self.mock:
            payload = mock_chat_reply(body)
            if body.get("stream") is True:
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream; charset=utf-8")
                self.send_header("Cache-Control", "no-cache")
                self.end_headers()
                for chunk in payload:
                    self.wfile.write(f"data: {json.dumps(chunk, ensure_ascii=False)}\n\n".encode("utf-8"))
                    self.wfile.flush()
                self.wfile.write(b"data: [DONE]\n\n")
                self.wfile.flush()
            else:
                self._send_json(200, payload)
            return

        try:
            status, headers, response = forward_chat(body, self.upstream, self.session_token)
        except Exception as exc:
            # Exception text may include a URL; do not reflect it to clients or logs.
            print(f"[plugin] upstream request failed ({type(exc).__name__})", file=sys.stderr, flush=True)
            self._send_json(502, {"error": {"message": "upstream request failed", "type": "upstream_error"}})
            return

        try:
            self.send_response(status)
            content_type = headers.get("Content-Type", "application/json")
            self.send_header("Content-Type", content_type)
            cache_control = headers.get("Cache-Control")
            if cache_control:
                self.send_header("Cache-Control", cache_control)
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True

            if body.get("stream") is True and 200 <= status < 300:
                while True:
                    # read1 returns currently available bytes instead of waiting
                    # to fill the full buffer; small SSE events reach the client
                    # without waiting for the upstream stream to finish.
                    part = response.read1(64 * 1024)
                    if not part:
                        break
                    self.wfile.write(part)
                    self.wfile.flush()
            else:
                data = response.read()
                self.wfile.write(data)
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            response.close()


def main(argv=None):
    parser = argparse.ArgumentParser(prog="tern-plugin-workbuddy")
    commands = parser.add_subparsers(dest="cmd", required=True)
    serve = commands.add_parser("serve")
    serve.add_argument("--port", type=int, default=None, help="port to bind (default: $TERN_PLUGIN_PORT)")
    serve.add_argument("--mock", action="store_true", help="return canned replies; never calls WorkBuddy")
    serve.add_argument("--upstream", default="", help="OpenAI-compatible API root (default: $TERN_PLUGIN_UPSTREAM_URL)")
    args = parser.parse_args(argv)

    try:
        port = args.port if args.port is not None else int(os.environ.get("TERN_PLUGIN_PORT", "0"))
    except ValueError:
        parser.error("TERN_PLUGIN_PORT must be an integer")
    if not 1 <= port <= 65535:
        parser.error("--port or TERN_PLUGIN_PORT must be in the range 1..65535")

    token = os.environ.get("TERN_PLUGIN_TOKEN", "")
    if len(token) < 32:
        parser.error("TERN_PLUGIN_TOKEN must be a high-entropy token of at least 32 characters")

    Handler.token = token
    Handler.upstream = args.upstream or os.environ.get("TERN_PLUGIN_UPSTREAM_URL", "")
    Handler.mock = args.mock
    if not Handler.mock and not Handler.upstream:
        parser.error("real forwarding is not implemented; use --mock for the reference contract server")
    Handler.session_token = None if Handler.mock else load_session_token()
    if not Handler.mock and not Handler.session_token:
        parser.error("real mode requires a plugin-owned session token; real WorkBuddy forwarding is still a stub")
    if Handler.mock:
        print("[plugin] mock mode: canned response only; no upstream request", file=sys.stderr, flush=True)

    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    print(f"[plugin] {PLUGIN_ID} listening on 127.0.0.1:{port}", file=sys.stderr, flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
