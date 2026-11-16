"""Contract and streaming regression tests; Python stdlib only."""

import http.server
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import urllib.error
import urllib.request

SCRIPT = Path(__file__).resolve().parents[1] / "tern-plugin-workbuddy.py"
PLUGIN_TOKEN = "plugin-test-token-0123456789abcdef"
SESSION_TOKEN = "upstream-session-secret"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


class DelayedSseHandler(http.server.BaseHTTPRequestHandler):
    first_chunk_sent = threading.Event()
    seen_request = {}

    def log_message(self, *_args):
        pass

    def do_POST(self):
        size = int(self.headers.get("Content-Length", "0"))
        type(self).seen_request = {
            "path": self.path,
            "authorization": self.headers.get("Authorization"),
            "plugin_token": self.headers.get("X-Tern-Plugin-Token"),
            "body": json.loads(self.rfile.read(size)),
        }
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.wfile.write(b"data: first\n\n")
        self.wfile.flush()
        type(self).first_chunk_sent.set()
        time.sleep(0.8)
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()


class PluginContractTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tern-plugin-test-")
        self.plugin_dir = Path(self.temp.name)
        self.script = self.plugin_dir / "tern-plugin-workbuddy.py"
        shutil.copyfile(SCRIPT, self.script)
        (self.plugin_dir / "session.json").write_text(
            json.dumps({"token": SESSION_TOKEN}), encoding="utf-8"
        )
        try:
            os.chmod(self.plugin_dir / "session.json", 0o600)
        except OSError:
            pass  # Windows ACLs are inherited from the private temporary directory.
        self.port = free_port()
        self.process = None

    def tearDown(self):
        if self.process is not None:
            self.process.terminate()
            try:
                self.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=3)
        self.temp.cleanup()

    def start_plugin(self, *args, env=None):
        process_env = os.environ.copy()
        process_env["TERN_PLUGIN_PORT"] = str(self.port)
        process_env["TERN_PLUGIN_TOKEN"] = PLUGIN_TOKEN
        if env:
            process_env.update(env)
        self.process = subprocess.Popen(
            [sys.executable, str(self.script), "serve", *args],
            cwd=self.plugin_dir,
            env=process_env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        base = f"http://127.0.0.1:{self.port}"
        headers = {"X-Tern-Plugin-Token": PLUGIN_TOKEN}
        for _ in range(100):
            if self.process.poll() is not None:
                self.fail("plugin exited before becoming ready")
            try:
                request = urllib.request.Request(base + "/health", headers=headers)
                with urllib.request.urlopen(request, timeout=0.2) as response:
                    if response.status == 200:
                        return base, headers
            except (OSError, urllib.error.URLError):
                time.sleep(0.03)
        self.fail("plugin did not become ready")

    def test_auth_required_for_health_and_models(self):
        base, headers = self.start_plugin("--mock")
        for path in ("/health", "/v1/models", "/v1/chat/completions"):
            with self.subTest(path=path):
                data = json.dumps({"model": "hunyuan-a52b"}).encode() if path.startswith("/v1/chat") else None
                request = urllib.request.Request(base + path, data=data)
                with self.assertRaises(urllib.error.HTTPError) as raised:
                    urllib.request.urlopen(request, timeout=2)
                self.assertEqual(raised.exception.code, 401)

                request = urllib.request.Request(base + path, data=data, headers=headers)
                with urllib.request.urlopen(request, timeout=2) as response:
                    self.assertEqual(response.status, 200)
                    payload = json.load(response)
                if path == "/v1/models":
                    self.assertEqual([item["id"] for item in payload["data"]], ["hunyuan-a52b"])

    def test_mock_response_uses_bare_upstream_model_and_usage(self):
        base, headers = self.start_plugin("--mock")
        body = {"model": "hunyuan-a52b", "messages": [{"role": "user", "content": "hi"}]}
        request = urllib.request.Request(
            base + "/v1/chat/completions",
            data=json.dumps(body).encode(),
            headers={**headers, "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=2) as response:
            payload = json.load(response)
        self.assertEqual(payload["model"], "hunyuan-a52b")
        self.assertEqual(payload["usage"]["total_tokens"], 32)

    def test_mock_sse_includes_usage_and_done(self):
        base, headers = self.start_plugin("--mock")
        body = {"model": "hunyuan-a52b", "stream": True, "messages": [{"role": "user", "content": "hi"}]}
        request = urllib.request.Request(
            base + "/v1/chat/completions",
            data=json.dumps(body).encode(),
            headers={**headers, "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=2) as response:
            stream = response.read().decode()
        self.assertIn('"total_tokens": 32', stream)
        self.assertIn("data: [DONE]", stream)

    def test_real_upstream_sse_is_forwarded_incrementally_and_strips_plugin_token(self):
        DelayedSseHandler.first_chunk_sent.clear()
        upstream = http.server.ThreadingHTTPServer(("127.0.0.1", 0), DelayedSseHandler)
        upstream_thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        upstream_thread.start()

        def stop_upstream():
            upstream.shutdown()
            upstream_thread.join(timeout=2)
            upstream.server_close()

        self.addCleanup(stop_upstream)

        base, headers = self.start_plugin(
            "--upstream", f"http://127.0.0.1:{upstream.server_port}/v1"
        )
        body = {"model": "hunyuan-a52b", "stream": True, "messages": [{"role": "user", "content": "hi"}]}
        request = urllib.request.Request(
            base + "/v1/chat/completions",
            data=json.dumps(body).encode(),
            headers={**headers, "Content-Type": "application/json"},
        )
        with urllib.request.urlopen(request, timeout=2) as response:
            started = time.monotonic()
            first = response.readline()
            elapsed = time.monotonic() - started
            self.assertEqual(first, b"data: first\n")
            self.assertLess(elapsed, 0.5, "first SSE event was buffered until the upstream completed")
            self.assertIn(b"[DONE]", response.read())

        self.assertTrue(DelayedSseHandler.first_chunk_sent.is_set())
        seen = DelayedSseHandler.seen_request
        self.assertEqual(seen["path"], "/v1/chat/completions")
        self.assertEqual(seen["authorization"], f"Bearer {SESSION_TOKEN}")
        self.assertIsNone(seen["plugin_token"], "local tern→plugin token leaked to the real upstream")
        self.assertEqual(seen["body"]["model"], "hunyuan-a52b")


if __name__ == "__main__":
    unittest.main()
