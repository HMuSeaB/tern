"""tern-plugin-zcode v0.3 离线测试：契约（Anthropic 直通）+ dress 整形 + 加密还原。

跑法：python tests/test_plugin.py（无需 pytest；有 pytest 也能收集）。
全部离线：mock 模式起真实 HTTP 服务测契约；解密测试用合成密钥/已知向量，
不碰真实凭据。
"""
import base64
import hashlib
import json
import os
import sys
import threading
import time
import urllib.error
import urllib.request
import unittest
from http.server import ThreadingHTTPServer

HERE = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, HERE)

import tern_plugin_zcode as zc  # noqa: E402


# ---------------------------------------------------------------- 解密 / 提示词

class TestDecryptAndPrompt(unittest.TestCase):
    def test_plaintext_passthrough(self):
        self.assertEqual(zc.decrypt_value(b"k", "plain-key"), "plain-key")

    def test_v1_round_trip(self):
        """enc:v1：sha256 种子 → AES-GCM（cryptography 加密，插件 CNG 解密）。"""
        try:
            from cryptography.hazmat.primitives.ciphers.aead import AESGCM
        except ImportError:
            self.skipTest("cryptography 未安装")
        seed = zc.V1_SEED.format(platform=sys.platform, homedir=os.path.expanduser("~"), username=os.environ.get("USERNAME", ""))
        key = hashlib.sha256(seed.encode()).digest()
        iv = bytes(12)
        plain = "secret-value"
        sealed = AESGCM(key).encrypt(iv, plain.encode(), None)
        tag, ct = sealed[-16:], sealed[:-16]
        enc = "enc:v1:" + ".".join(base64.urlsafe_b64encode(x).decode().rstrip("=") for x in (iv, tag, ct))
        self.assertEqual(zc.decrypt_value(key, enc), plain)

    def test_prompt_json_sha256(self):
        """prompt.json 的三段系统块必须逐字节等于 3.17.2 校验表。"""
        self.assertEqual(
            hashlib.sha256(zc.PROMPT["cliPrefix"].encode()).hexdigest()[:16], zc.PROMPT_SHA256["cliPrefix"]
        )
        for path, expected in zc.PROMPT_SHA256.items():
            node = zc.PROMPT
            for part in path.replace("]", "").split("["):
                node = node[int(part)] if part.isdigit() else node[part]
            self.assertEqual(hashlib.sha256(node.encode()).hexdigest()[:16], expected, path)


# ---------------------------------------------------------------- dress 整形

class TestDress(unittest.TestCase):
    def _base_body(self):
        return {
            "model": "glm-5.3-flash-trial",
            "max_tokens": 100,
            "system": [{"type": "text", "text": "你是测试助手", "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "user", "content": [{"type": "text", "text": "你好"}]}],
        }

    def test_three_system_blocks(self):
        out = zc.dress(self._base_body(), "device-uuid", "sess-uuid")
        self.assertEqual(len(out["system"]), 3)  # 永远三段，第四段会被 405 拒
        self.assertEqual(out["system"][0]["text"], zc.PROMPT["cliPrefix"])
        self.assertIn("你是测试助手", out["system"][2]["text"])  # agent 的 system 并入第三块

    def test_date_prefix_and_cache_shape(self):
        out = zc.dress(self._base_body(), "d", "s")
        self.assertEqual(out["messages"][0]["role"], "user")
        self.assertIn("<system-reminder>Today's date is", out["messages"][0]["content"][0]["text"])
        marked = sum(
            1
            for m in out["messages"][1:]
            for x in (m["content"] if isinstance(m["content"], list) else [])
            if isinstance(x, dict) and "cache_control" in x
        )
        self.assertEqual(marked, 1)
        self.assertEqual(out["messages"][-1]["content"][-1]["cache_control"], {"type": "ephemeral"})

    def test_metadata_user_id(self):
        out = zc.dress(self._base_body(), "dev", "sess")
        uid = json.loads(out["metadata"]["user_id"])
        self.assertEqual(uid["device_id"], "dev")
        self.assertEqual(uid["account_uuid"], "")
        self.assertEqual(uid["session_id"], "sess")

    def test_model_suffix_stripped(self):
        out = zc.dress(self._base_body(), "d", "s")
        self.assertEqual(out["model"], "glm-5.3-flash")

    def test_tool_search_alias(self):
        body = self._base_body()
        body["tools"] = [
            {"name": "ToolSearch", "description": "s", "input_schema": {}},
            {"name": "DeferredToolPlaceholder", "input_schema": {}},
            {"name": "Bash", "description": "b", "input_schema": {}},
        ]
        body["messages"].append(
            {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "name": "ToolSearch", "input": {}}]}
        )
        body["messages"].append({"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "r"}]})
        out = zc.dress(body, "d", "s")
        names = [t["name"] for t in out.get("tools", [])]
        self.assertIn("ToolSearchTool", names)
        self.assertNotIn("ToolSearch", names)
        self.assertNotIn("DeferredToolPlaceholder", names)
        self.assertIn("Bash", names)
        renamed = [
            x["name"]
            for m in out["messages"]
            if isinstance(m.get("content"), list)
            for x in m["content"]
            if isinstance(x, dict) and x.get("type") == "tool_use"
        ]
        self.assertIn("ToolSearchTool", renamed)


# ---------------------------------------------------------------- 会话/查询派生

class TestSession(unittest.TestCase):
    def test_session_stable_per_device(self):
        self.assertEqual(zc.session_of(None, "dev-1"), zc.session_of(None, "dev-1"))
        self.assertNotEqual(zc.session_of(None, "dev-1"), zc.session_of(None, "dev-2"))

    def test_session_uuid_passthrough(self):
        u = "0123abcd-1111-4222-8333-444455556666"
        self.assertEqual(zc.session_of(u, "dev"), u)

    def test_query_of_stable_within_turn(self):
        body = {"messages": [{"role": "user", "content": [{"type": "text", "text": "第一句"}]}]}
        self.assertEqual(zc.query_of(body, "s"), zc.query_of(body, "s"))

    def test_query_of_changes_with_turn(self):
        b1 = {"messages": [{"role": "user", "content": [{"type": "text", "text": "第一句"}]}]}
        b2 = {
            "messages": [
                {"role": "user", "content": [{"type": "text", "text": "第一句"}]},
                {"role": "assistant", "content": [{"type": "text", "text": "答"}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t", "content": "x"}]},
                {"role": "user", "content": [{"type": "text", "text": "第二句"}]},
            ]
        }
        self.assertNotEqual(zc.query_of(b1, "s"), zc.query_of(b2, "s"))

    def test_query_of_empty(self):
        self.assertEqual(zc.query_of({"messages": []}, "s"), "")

    def test_strip_suffix_case_insensitive(self):
        self.assertEqual(zc.strip_gift_suffix("glm-5.3-flash-trial"), "glm-5.3-flash")
        self.assertEqual(zc.strip_gift_suffix("GLM-5.3-FLASH-TRIAL"), "glm-5.3-flash")
        self.assertEqual(zc.strip_gift_suffix("glm-5.3-flash"), "glm-5.3-flash")

    def test_uuid_of_deterministic(self):
        self.assertEqual(zc.uuid_of("a"), zc.uuid_of("a"))
        self.assertRegex(zc.uuid_of("a"), r"[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")

    def test_jwt_expiry(self):
        payload = base64.urlsafe_b64encode(json.dumps({"exp": 1}).encode()).decode().rstrip("=")
        self.assertTrue(zc.jwt_expired(f"aaa.{payload}.bbb"))
        import time as _t

        payload2 = base64.urlsafe_b64encode(json.dumps({"exp": int(_t.time()) + 3600}).encode()).decode().rstrip("=")
        self.assertFalse(zc.jwt_expired(f"aaa.{payload2}.bbb"))

    def test_start_headers_complete(self):
        h = zc.ZCodeClient._start_headers("jwt-x")
        for k in [
            "HTTP-Referer", "User-Agent", "X-ZCode-App-Version", "X-Title",
            "X-Release-Channel", "X-Client-Language", "X-Client-Timezone",
            "X-ZCode-Agent", "X-Platform", "X-Os-Category", "X-Os-Version",
            "x-request-id", "x-zcode-session-type", "x-zcode-trace-id",
            "x-zcode-query-source", "anthropic-version", "Authorization", "content-type",
        ]:
            self.assertIn(k, h, f"缺头 {k}")
        self.assertIn("ai-sdk/anthropic", h["User-Agent"])
        self.assertNotIn("X-Device-Mid", h)  # messages 路径不带（同社区插件）


# ---------------------------------------------------------------- SSE / JSON 透传

class TestPassthrough(unittest.TestCase):
    def test_nonstream_tool_use_name_restored(self):
        raw = json.dumps(
            {
                "type": "message",
                "content": [{"type": "tool_use", "id": "t1", "name": "ToolSearchTool", "input": {}}],
                "usage": {"input_tokens": 5, "output_tokens": 3},
            }
        ).encode()

        class FakeResp:
            def read(self):
                return raw

        out = zc.anthropic_passthrough(FakeResp(), stream=False)
        v = json.loads(out)
        self.assertEqual(v["content"][0]["name"], "ToolSearch")

    def test_stream_line_rewrite(self):
        sse = (
            b'event: content_block_start\n'
            b'data: {"type":"content_block_start","index":0,"content_block":{"type":"tool_use","name":"ToolSearchTool","id":"t1"}}\n\n'
            b'event: message_stop\n'
            b'data: {"type":"message_stop"}\n\n'
        )

        class FakeResp:
            def __iter__(self):
                yield from sse.splitlines(keepends=True)

        out = zc.anthropic_passthrough(FakeResp(), stream=True)
        text = "".join(out)
        self.assertIn('"name":"ToolSearch"', text)
        self.assertNotIn("ToolSearchTool", text)
        self.assertEqual(text.count("\n\n"), 2)
        self.assertEqual(text.count("event: content_block_start\n"), 1)
        self.assertEqual(text.count("event: message_stop\n"), 1)

    def test_stream_preserves_crlf_event_boundaries(self):
        sse = (
            b"event: content_block_delta\r\n"
            b'data: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}\r\n'
            b"\r\n"
            b"event: message_stop\r\n"
            b'data: {"type":"message_stop"}\r\n'
            b"\r\n"
        )

        class FakeResp:
            def __iter__(self):
                yield from sse.splitlines(keepends=True)

        text = "".join(zc.anthropic_passthrough(FakeResp(), stream=True))
        blocks = text.strip().split("\r\n\r\n")
        self.assertEqual(len(blocks), 2)
        self.assertTrue(blocks[0].startswith("event: content_block_delta\r\ndata: "))
        self.assertTrue(blocks[1].startswith("event: message_stop\r\ndata: "))


# ---------------------------------------------------------------- HTTP 契约（mock）

class TestContract(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        client = zc.ZCodeClient()
        client.balance = lambda force=False: {"models": ["glm-5.3-flash"], "plans": ["mock"]}
        cls.handler = type(
            "HandlerWithToken", (zc.Handler,), {"client": client, "plugin_token": "test-token", "mock": True}
        )
        cls.httpd = ThreadingHTTPServer(("127.0.0.1", 0), cls.handler)
        cls.port = cls.httpd.server_address[1]
        cls.base = f"http://127.0.0.1:{cls.port}"
        threading.Thread(target=cls.httpd.serve_forever, daemon=True).start()

    @classmethod
    def tearDownClass(cls):
        cls.httpd.shutdown()
        cls.httpd.server_close()

    def _anthropic_body(self, stream=False):
        return {
            "model": "glm-5.3-flash-trial",
            "max_tokens": 50,
            "stream": stream,
            "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
        }

    def _post(self, path, body, token="test-token"):
        headers = {"Content-Type": "application/json"}
        if token is not None:
            headers["X-Tern-Plugin-Token"] = token
        req = urllib.request.Request(
            self.base + path, data=json.dumps(body).encode(), headers=headers, method="POST"
        )
        try:
            with urllib.request.urlopen(req, timeout=10) as r:
                return r.status, r.read().decode()
        except urllib.error.HTTPError as e:
            return e.code, e.read().decode()

    def test_health_ok(self):
        with urllib.request.urlopen(
            urllib.request.Request(self.base + "/health", headers={"X-Tern-Plugin-Token": "test-token"}), timeout=5
        ) as r:
            v = json.loads(r.read())
        self.assertTrue(v["ok"])
        self.assertIsInstance(v["models"], list)

    def test_health_wrong_token_401(self):
        req = urllib.request.Request(self.base + "/health", headers={"X-Tern-Plugin-Token": "wrong"})
        try:
            urllib.request.urlopen(req, timeout=5)
            self.fail("应当 401")
        except urllib.error.HTTPError as e:
            self.assertEqual(e.code, 401)

    def test_no_token_is_rejected_by_default(self):
        handler = type(
            "HandlerNoTokenDefault",
            (zc.Handler,),
            {"client": zc.ZCodeClient(), "plugin_token": "", "mock": True},
        )
        httpd = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        try:
            req = urllib.request.Request(f"http://127.0.0.1:{httpd.server_port}/health")
            with self.assertRaises(urllib.error.HTTPError) as error:
                urllib.request.urlopen(req, timeout=5)
            self.assertEqual(error.exception.code, 401)
        finally:
            httpd.shutdown()
            httpd.server_close()

    def test_models_list(self):
        with urllib.request.urlopen(
            urllib.request.Request(self.base + "/v1/models", headers={"X-Tern-Plugin-Token": "test-token"}), timeout=5
        ) as r:
            v = json.loads(r.read())
        self.assertEqual(v["object"], "list")
        self.assertTrue(all("id" in m for m in v["data"]))

    def test_rejects_non_object_request(self):
        st, _ = self._post("/v1/messages", ["not", "an", "object"])
        self.assertEqual(st, 400)

    def test_rejects_request_without_model(self):
        st, _ = self._post("/v1/messages", {"messages": []})
        self.assertEqual(st, 400)

    def test_rejects_oversized_request_before_reading_body(self):
        req = urllib.request.Request(
            self.base + "/v1/messages",
            data=b"x",
            headers={
                "Content-Type": "application/json",
                "Content-Length": str(zc.MAX_REQUEST_BYTES + 1),
                "X-Tern-Plugin-Token": "test-token",
            },
            method="POST",
        )
        with self.assertRaises(urllib.error.HTTPError) as error:
            urllib.request.urlopen(req, timeout=5)
        self.assertEqual(error.exception.code, 413)

    def test_messages_nonstream_mock(self):
        st, text = self._post("/v1/messages", self._anthropic_body())
        self.assertEqual(st, 200)
        v = json.loads(text)
        self.assertEqual(v["type"], "message")
        self.assertEqual(v["content"][0]["text"], "[mock] 收到")
        self.assertEqual(v["usage"]["input_tokens"], 12)

    def test_messages_stream_mock(self):
        st, text = self._post("/v1/messages", self._anthropic_body(stream=True))
        self.assertEqual(st, 200)
        self.assertIn("event: message_start", text)
        self.assertIn("event: message_stop", text)
        self.assertIn("input_tokens", text)

    def test_openai_path_404(self):
        st, _ = self._post("/v1/chat/completions", self._anthropic_body())
        self.assertEqual(st, 404)  # api_format=anthropic 只认 /v1/messages

    def test_unknown_path_404(self):
        req = urllib.request.Request(self.base + "/nope", headers={"X-Tern-Plugin-Token": "test-token"})
        try:
            urllib.request.urlopen(req, timeout=5)
            self.fail("应当 404")
        except urllib.error.HTTPError as e:
            self.assertEqual(e.code, 404)

    def test_manual_mode_no_token(self):
        handler2 = type(
            "HandlerNoTokenOptIn",
            (zc.Handler,),
            {"client": zc.ZCodeClient(), "plugin_token": "", "mock": True, "allow_unauthenticated_local": True},
        )
        httpd2 = ThreadingHTTPServer(("127.0.0.1", 0), handler2)
        port2 = httpd2.server_address[1]
        t = threading.Thread(target=httpd2.serve_forever, daemon=True)
        t.start()
        try:
            req = urllib.request.Request(
                f"http://127.0.0.1:{port2}/v1/messages",
                data=json.dumps(self._anthropic_body()).encode(),
                headers={"Content-Type": "application/json"},
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=5) as r:
                self.assertEqual(r.status, 200)
        finally:
            httpd2.shutdown()
            httpd2.server_close()


class TestSingleConcurrency(unittest.TestCase):
    def test_stream_holds_lock_until_consumed_or_closed(self):
        client = zc.ZCodeClient()
        entered = threading.Event()
        release = threading.Event()

        def chat_once(body, stream, jwt, device, allow_refresh=True):
            if body["id"] == 1:
                entered.set()
                def response():
                    try:
                        yield "first chunk"
                        release.wait(timeout=5)
                        yield "last chunk"
                    finally:
                        body["closed"] = True
                return response()
            return ["second response"]

        client.ensure_session = lambda: ("test-jwt", "test-device")
        client._chat_once = chat_once
        first_result = []
        first = threading.Thread(target=lambda: first_result.extend(client.chat({"id": 1}, True)))
        first.start()
        self.assertTrue(entered.wait(timeout=2))

        second_result = []
        second = threading.Thread(target=lambda: second_result.extend(client.chat({"id": 2}, False)))
        second.start()
        time.sleep(0.1)
        self.assertTrue(second.is_alive(), "第二个请求应等待 SSE 流消费完成")
        release.set()
        first.join(timeout=2)
        second.join(timeout=2)
        self.assertFalse(first.is_alive())
        self.assertFalse(second.is_alive())
        self.assertEqual(first_result, ["first chunk", "last chunk"])
        self.assertEqual(second_result, ["second response"])

    def test_closing_partial_stream_releases_lock(self):
        client = zc.ZCodeClient()
        client.ensure_session = lambda: ("test-jwt", "test-device")
        client._chat_once = lambda *args, **kwargs: iter(["first chunk", "last chunk"])

        stream = client.chat({}, True)
        self.assertEqual(next(stream), "first chunk")
        stream.close()

        self.assertTrue(client._lock.acquire(blocking=False))
        client._lock.release()


class TestRedirectPolicy(unittest.TestCase):
    def test_upstream_redirect_does_not_forward_authorization(self):
        from http.server import BaseHTTPRequestHandler, HTTPServer

        observed = []

        class RedirectHandler(BaseHTTPRequestHandler):
            def do_POST(self):  # noqa: N802
                if self.path == "/redirect":
                    self.send_response(302)
                    self.send_header("Location", f"http://127.0.0.1:{self.server.server_port}/target")
                    self.end_headers()
                    return
                observed.append((self.path, self.headers.get("Authorization")))
                self.send_response(200)
                self.end_headers()

            def log_message(self, *args):
                pass

        httpd = HTTPServer(("127.0.0.1", 0), RedirectHandler)
        threading.Thread(target=httpd.serve_forever, daemon=True).start()
        try:
            req = urllib.request.Request(
                f"http://127.0.0.1:{httpd.server_port}/redirect",
                data=b"{}",
                headers={"Authorization": "Bearer sentinel-secret"},
                method="POST",
            )
            with self.assertRaises(urllib.error.HTTPError) as error:
                zc._open_no_redirect(req, timeout=5)
            self.assertEqual(error.exception.code, 302)
            self.assertEqual(observed, [])
        finally:
            httpd.shutdown()
            httpd.server_close()


if __name__ == "__main__":
    unittest.main(argv=[sys.argv[0], "-v"], exit=False)
