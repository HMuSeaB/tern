#!/usr/bin/env python3
"""tern-plugin-zcode —— ZCode（zcode.z.ai）赠送套餐的外挂插件（v0.3，Anthropic 直通）。

让 Claude Code / Codex 通过 tern 用上 ZCode 账号的赠送额度
（Start Plan / 周末档这类 gift plan，如 ZCODE WEEKEND BUILD 的
GLM-5.3-Flash 500M token）。

v0.3 的关键简化：manifest 声明 api_format = "anthropic"，tern 对
Anthropic 客户端（Claude Code）是原生直通，对 Codex 做 OpenAI Responses
→ Anthropic 翻译——所以本插件收到的**就是 Anthropic Messages 请求**，
不再需要任何协议翻译层（v0.2 的 ~250 行 OpenAI 转换代码已删）。
插件只做 ZCode 特有的三件事：

  1. 读登录态：~/.zcode/v2/credentials.json（enc:v2:os：AES-256-GCM，
     密钥在 Windows 凭据管理器；enc:v1 旧格式 sha256 种子回退）
  2. dress：请求体按 ZCode 客户端整形（三段系统块逐字节 3.17.2 +
     日期前缀 + metadata.user_id + cache_control 形状 + ToolSearch 别名）
  3. startHeaders：全套 ZCode 头（HTTP-Referer / ai-sdk UA /
     X-Release-Channel / X-Client-Language / X-Client-Timezone /
     X-ZCode-Agent / X-Platform / X-Os-Category / X-Os-Version /
     x-request-id / x-zcode-session-type / x-zcode-trace-id /
     x-zcode-query-source / x-query-id / x-session-id），
     Anthropic SSE 原样透传（仅 ToolSearch 别名改回）

指纹与 magpie 社区插件 @magpie-community/opencode-zcode-auth 对齐
（其 prompt.mjs / startHeaders / queryOf / sessionOf 是字节级依据）。
提示词外置在 prompt.json（带 sha256 自检），ZCode 改版时只换数据文件。

契约（docs/plugins-external-process-design-zh.md §4，api_format=anthropic）：
  GET  /health       -> {"ok": true, "models": [...], "plans": [...]}
  GET  /v1/models    -> OpenAI 兼容列表
  POST /v1/messages  -> Anthropic Messages（流式 SSE / 非流式 JSON）
  每个请求校验 X-Tern-Plugin-Token（恒定时间比较），转发前剥离。

风险自担：赠送档额度、风控 405/3012（账号被限制只能找 ZCode 客服）、
单并发 3008、边缘阻断页（本机地址保持 3 分钟）。只自用，别拿主力号。

依赖：仅 Python 标准库（Windows：ctypes 调凭据管理器与 BCrypt）。
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import hmac
import json
import os
import re
import sys
import threading
import time
import urllib.error
import urllib.request
import urllib.parse
import uuid
from datetime import date
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))

# ---------------------------------------------------------------- 常量

SERVICE = "ZCode Credential Encryption"
ZCODE_DIR = os.path.join(os.path.expanduser("~"), ".zcode", "v2")
CREDENTIALS = os.path.join(ZCODE_DIR, "credentials.json")
SETTING = os.path.join(ZCODE_DIR, "setting.json")
ZCODE = "https://zcode.z.ai"
START_BASE = ZCODE + "/api/v1/zcode-plan/anthropic"
APP_VERSION = "3.17.2"
UA = "ZCode/" + APP_VERSION
UA_START = UA + " ai-sdk/anthropic/3.0.81"
ANTHROPIC_VERSION = "2023-06-01"
MAX_REQUEST_BYTES = 200 * 1024 * 1024
GIFT_SUFFIX = "-trial"  # 消费端后缀，统一小写比较（如 glm-5.3-flash-trial）

# enc:v1 的种子（ZCode 3.14–3.17 旧格式回退用）
V1_SEED = "zcode-credential-fallback:{platform}:{homedir}:{username}"

# 3012 / 边缘阻断（与社区插件 START_RESTRICTED / START_EDGE 同源）
SEARCH = "ToolSearch"
SEARCH_ALIAS = "ToolSearchTool"
PLACEHOLDER = "DeferredToolPlaceholder"


START_REFUSED = re.compile(r"unusual activity|request has been blocked|\"code\"\s*:\s*\"?3012\b", re.I)
EDGE_PAGE = re.compile(r"errors\.aliyun\.com|<html", re.I)
UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", re.I)


class _NoRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Never forward the ZCode JWT or request body to a redirect target."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def _open_no_redirect(req, timeout: int):
    opener = urllib.request.build_opener(_NoRedirectHandler)
    return opener.open(req, timeout=timeout)


def log(msg: str) -> None:
    """诊断只进 stderr；任何密钥都不打印（只打前 4 后 4）。"""
    sys.stderr.write(f"[tern-plugin-zcode] {msg}\n")
    sys.stderr.flush()


def mask(s: str) -> str:
    if len(s) <= 8:
        return "…"
    return s[:4] + "…" + s[-4:]


def uuid_of(seed: str) -> str:
    """确定性 UUID（同社区插件 uuidOf：sha256 → v4 形状）。"""
    h = hashlib.sha256(seed.encode("utf-8")).hexdigest()
    variant = (int(h[16], 16) & 3) | 8
    return f"{h[0:8]}-{h[8:12]}-4{h[13:16]}-{variant:x}{h[17:20]}-{h[20:32]}"


# ---------------------------------------------------------------- 提示词（外置 prompt.json + sha256 自检）

PROMPT_FILE = os.path.join(HERE, "prompt.json")

# 3.17.2 原文的 sha256 前 16 位（同社区插件 start.test.mjs 的校验表）。
# prompt.json 被改动（哪怕是误编辑）时启动即报错，不带着错的样子去撞风控。
PROMPT_SHA256 = {
    "cliPrefix": "7779642ea44a4696",
    "stableSections[0]": "aac4ba4cc7b555b3",
    "stableSections[1]": "86e0396775d5ed0e",
    "dynamicBehavior": "7d843753f3adae93",
    # 注意：社区插件 README 写的 406a0480 是陈旧值，实测通过风控的是
    # prompt.mjs 现行文本 = 16eb9b7c（本机 e2e 200 OK 验证过）
    "contextManagement": "16eb9b7cc031fc96",
}


def load_prompt() -> dict:
    with open(PROMPT_FILE, encoding="utf-8") as f:
        data = json.load(f)
    for path, expected in PROMPT_SHA256.items():
        node = data
        for part in path.replace("]", "").split("["):
            node = node[int(part)] if part.isdigit() else node[part]
        actual = hashlib.sha256(node.encode("utf-8")).hexdigest()[:16]
        if actual != expected:
            raise RuntimeError(f"prompt.json 的 {path} 与 3.17.2 原文不符（sha256 {actual} ≠ {expected}）")
    return data


PROMPT = load_prompt()


# ---------------------------------------------------------------- AES-256-GCM（Windows CNG，ctypes）

try:
    import ctypes
    from ctypes import wintypes as wt

    PUCHAR = ctypes.POINTER(ctypes.c_ubyte)
    bcd = ctypes.WinDLL("bcrypt", use_last_error=True)

    class _AUTH_INFO(ctypes.Structure):
        # 对齐 bcrypt.h 的 BCRYPT_AUTHENTICATED_CIPHER_MODE_INFO（x64 = 88 字节）
        _fields_ = [
            ("cbSize", wt.ULONG),
            ("dwInfoVersion", wt.ULONG),  # 必须 = 1
            ("pbNonce", ctypes.c_void_p),
            ("cbNonce", wt.ULONG),
            ("pbAuthData", ctypes.c_void_p),
            ("cbAuthData", wt.ULONG),
            ("pbTag", ctypes.c_void_p),
            ("cbTag", wt.ULONG),
            ("pbMacContext", ctypes.c_void_p),
            ("cbMacContext", wt.ULONG),
            ("cbAAD", wt.ULONG),
            ("cbData", ctypes.c_ulonglong),
            ("dwFlags", wt.ULONG),
        ]

    assert ctypes.sizeof(_AUTH_INFO) == 88

    bcd.BCryptOpenAlgorithmProvider.argtypes = [ctypes.POINTER(ctypes.c_void_p), wt.LPCWSTR, wt.LPCWSTR, wt.ULONG]
    bcd.BCryptOpenAlgorithmProvider.restype = wt.LONG
    bcd.BCryptSetProperty.argtypes = [ctypes.c_void_p, wt.LPCWSTR, ctypes.c_void_p, wt.ULONG, wt.ULONG]
    bcd.BCryptSetProperty.restype = wt.LONG
    bcd.BCryptGenerateSymmetricKey.argtypes = [
        ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p), ctypes.c_void_p,
        wt.ULONG, ctypes.c_void_p, wt.ULONG, wt.ULONG,
    ]
    bcd.BCryptGenerateSymmetricKey.restype = wt.LONG
    bcd.BCryptDecrypt.argtypes = [
        ctypes.c_void_p, ctypes.c_void_p, wt.ULONG, ctypes.POINTER(_AUTH_INFO),
        ctypes.c_void_p, wt.ULONG, ctypes.c_void_p, wt.ULONG,
        ctypes.POINTER(wt.ULONG), wt.ULONG,
    ]
    bcd.BCryptDecrypt.restype = wt.LONG
    bcd.BCryptDestroyKey.argtypes = [ctypes.c_void_p]
    bcd.BCryptDestroyKey.restype = wt.LONG
    bcd.BCryptCloseAlgorithmProvider.argtypes = [ctypes.c_void_p, wt.ULONG]
    bcd.BCryptCloseAlgorithmProvider.restype = wt.LONG

    _alg = ctypes.c_void_p()
    _mode = ctypes.create_unicode_buffer("ChainingModeGCM")
    if bcd.BCryptOpenAlgorithmProvider(ctypes.byref(_alg), "AES", None, 0) != 0:
        raise OSError("BCryptOpenAlgorithmProvider")
    if bcd.BCryptSetProperty(_alg, "ChainingMode", ctypes.cast(_mode, ctypes.c_void_p), ctypes.sizeof(_mode), 0) != 0:
        raise OSError("BCryptSetProperty")
    _key_handles: dict = {}

    def _hkey(key: bytes) -> ctypes.c_void_p:
        h = _key_handles.get(key)
        if h is None:
            kb = ctypes.create_string_buffer(key, len(key))
            h = ctypes.c_void_p()
            if bcd.BCryptGenerateSymmetricKey(_alg, ctypes.byref(h), None, 0, ctypes.cast(kb, PUCHAR), len(key), 0) != 0:
                raise OSError("BCryptGenerateSymmetricKey")
            _key_handles[key] = h
        return h

    def aes_gcm_decrypt(key: bytes, iv: bytes, tag: bytes, data: bytes) -> bytes:
        ivb = ctypes.create_string_buffer(iv, len(iv))
        tagb = ctypes.create_string_buffer(tag, len(tag))
        ai = _AUTH_INFO(
            ctypes.sizeof(_AUTH_INFO), 1,
            ctypes.cast(ivb, ctypes.c_void_p), len(iv),
            None, 0,
            ctypes.cast(tagb, ctypes.c_void_p), len(tag),
            None, 0, 0, 0, 0,
        )
        db = ctypes.create_string_buffer(data, max(len(data), 1))
        out = ctypes.create_string_buffer(max(len(data), 1))
        n = wt.ULONG(0)
        st = bcd.BCryptDecrypt(
            _hkey(key), ctypes.cast(db, ctypes.c_void_p), len(data), ctypes.byref(ai),
            None, 0, ctypes.cast(out, ctypes.c_void_p), len(data), ctypes.byref(n), 0,
        )
        if st != 0:
            raise OSError(f"BCryptDecrypt 0x{st & 0xFFFFFFFF:08x}")
        return out.raw[: n.value]

    HAVE_CNG = True
except Exception as e:  # noqa: BLE001  非 Windows 或 CNG 不可用
    HAVE_CNG = False
    _cng_err = e

    def aes_gcm_decrypt(key: bytes, iv: bytes, tag: bytes, data: bytes) -> bytes:  # type: ignore[misc]
        raise OSError(f"本机没有可用的 CNG（{_cng_err}）；仅支持 Windows")


# ---------------------------------------------------------------- 凭据读取

try:
    import ctypes as _ct

    _adv = _ct.WinDLL("advapi32", use_last_error=True)

    class _CRED(_ct.Structure):
        _fields_ = [
            ("Flags", wt.DWORD), ("Type", wt.DWORD), ("TargetName", wt.LPWSTR),
            ("Comment", wt.LPWSTR), ("LastWritten", wt.FILETIME),
            ("CredentialBlobSize", wt.DWORD), ("CredentialBlob", ctypes.c_void_p),
            ("Persist", wt.DWORD), ("AttributeCount", wt.DWORD),
            ("Attributes", ctypes.c_void_p), ("TargetAlias", wt.LPWSTR), ("UserName", wt.LPWSTR),
        ]

    _adv.CredReadW.argtypes = [wt.LPCWSTR, wt.DWORD, wt.DWORD, ctypes.POINTER(ctypes.POINTER(_CRED))]
    _adv.CredReadW.restype = wt.BOOL
    _adv.CredFree.argtypes = [ctypes.c_void_p]
    HAVE_CREDMAN = True
except Exception:  # noqa: BLE001
    HAVE_CREDMAN = False


def credman_read(target: str) -> bytes:
    if not HAVE_CREDMAN:
        raise OSError("无凭据管理器（非 Windows）")
    cp = ctypes.POINTER(_CRED)()
    if not _adv.CredReadW(target, 1, 0, ctypes.byref(cp)):
        raise OSError(f"CredRead 失败 gle={ctypes.get_last_error()}")
    c = cp[0]
    blob = ctypes.string_at(c.CredentialBlob, c.CredentialBlobSize) if c.CredentialBlobSize else b""
    _adv.CredFree(cp)
    return blob


def b64u(s: str) -> bytes:
    return base64.urlsafe_b64decode(s + "=" * (-len(s) % 4))


def v1_key() -> bytes:
    name = os.environ.get("USERNAME") or os.environ.get("USER") or ""
    seed = V1_SEED.format(platform=sys.platform, homedir=os.path.expanduser("~"), username=name)
    return hashlib.sha256(seed.encode()).digest()


def os_key(cred_path: str) -> bytes:
    """enc:v2:os 的 AES key：凭据管理器 TargetName = <sha256(路径反斜杠).hex>.<service>，
    blob 是 UTF-16LE 的 base64url 密钥字符串。"""
    account = hashlib.sha256(os.path.abspath(cred_path).encode("utf-8")).hexdigest()
    blob = credman_read(f"{account}.{SERVICE}")
    key_str = None
    if len(blob) % 2 == 0 and blob[1::2] == b"\x00" * (len(blob) // 2):
        key_str = blob.decode("utf-16-le")
    if key_str is None:
        key_str = blob.decode("utf-8")
    key = b64u(key_str)
    if len(key) != 32:
        raise OSError(f"凭据管理器里的密钥长度异常：{len(key)}")
    return key


def decrypt_value(key: bytes, v: str) -> str | None:
    """解 enc:v1 / enc:v2:os 单个值；明文原样返回；key 不匹配返回 None。"""
    if not v.startswith("enc:"):
        return v
    if v.startswith("enc:v2:"):
        mode, vid, payload = v[len("enc:v2:") :].split(":", 2)
        if hashlib.sha256(key).hexdigest()[:24] != vid:
            return None
        iv_b, tag_b, data_b = payload.split(".")
        return aes_gcm_decrypt(key, b64u(iv_b), b64u(tag_b), b64u(data_b)).decode("utf-8", "replace")
    if v.startswith("enc:v1:"):
        parts = v[len("enc:v1:") :].split(".")
        if len(parts) != 3:
            return None
        iv_b, tag_b, data_b = (b64u(p) for p in parts)
        return aes_gcm_decrypt(key, iv_b, tag_b, data_b).decode("utf-8", "replace")
    return None


def _credentials_path() -> str:
    path = CREDENTIALS
    # setting.json 的 dataBaseDir 支持数据目录搬移（ZCode 3.14+）
    try:
        with open(SETTING, encoding="utf-8") as f:
            set_ = json.load(f)
        moved = set_.get("dataBaseDir")
        if isinstance(moved, str) and os.path.isabs(moved.strip()):
            path = os.path.join(moved.strip(), ".zcode", "v2", "credentials.json")
    except Exception:  # noqa: BLE001
        pass
    return path


def load_credentials() -> dict:
    path = _credentials_path()
    with open(path, encoding="utf-8") as f:
        store = json.load(f)
    keys = []
    if os.name == "nt" and HAVE_CREDMAN:
        try:
            keys.append(os_key(path))
        except Exception as e:  # noqa: BLE001
            log(f"凭据管理器读 key 失败：{e}")
    keys.append(v1_key())
    out = {}
    for k, v in store.items():
        if not isinstance(v, str):
            continue
        for key in keys:
            try:
                plain = decrypt_value(key, v)
            except Exception:  # noqa: BLE001
                plain = None
            if plain is not None:
                out[k] = plain
                break
    return out


# ---------------------------------------------------------------- JWT

def jwt_expiry(jwt: str) -> int:
    parts = str(jwt or "").split(".")
    if len(parts) != 3:
        return 0
    try:
        claims = json.loads(b64u(parts[1]).decode("utf-8", "replace"))
        exp = float(claims.get("exp") or 0)
        return int(exp * 1000) if exp > 0 else 0
    except Exception:  # noqa: BLE001
        return 0


def jwt_expired(jwt: str) -> bool:
    t = jwt_expiry(jwt)
    return t > 0 and int(time.time() * 1000) > t


# ---------------------------------------------------------------- ZCode 客户端


class ConcurrencyBusy(RuntimeError):
    """赠送档并发上限（HTTP 429 code 3008）。"""


class UpstreamRefused(RuntimeError):
    """405 / 3012 / 阻断页：风控拒绝。"""


class SessionExpired(RuntimeError):
    """JWT 过期且重读后仍过期。"""


class ZCodeClient:
    """会话 JWT + 余额 + 伪装转发。单并发串行（赠送档 3008）。"""

    def __init__(self) -> None:
        self.jwt: str | None = None
        self.device: str | None = None
        self.gift_models: list = []
        self.plans: list = []
        self._lock = threading.Lock()
        self._balance_at = 0.0
        self._rest_until: dict = {}

    def ensure_session(self, allow_reread: bool = True) -> tuple:
        if self.jwt and self.device and not jwt_expired(self.jwt):
            return self.jwt, self.device
        creds = load_credentials()
        jwt = creds.get("zcodejwttoken", "")
        if not jwt:
            raise RuntimeError(
                "ZCode 会话 JWT 未找到：请先在 ZCode 应用里登录"
                "（~/.zcode/v2/credentials.json 的 zcodejwttoken）"
            )
        if jwt_expired(jwt):
            if allow_reread:
                time.sleep(0.5)
                creds = load_credentials()
                jwt = creds.get("zcodejwttoken", jwt)
            if jwt_expired(jwt):
                raise SessionExpired("ZCode 登录已过期：请打开 ZCode 应用重新登录（会自动刷新本插件用的会话）")
        log(f"会话 JWT 就绪：{mask(jwt)}")
        # device id：从 JWT 稳定派生（同社区插件 keyDevice 的做法，重启不变）
        self.device = uuid_of("zcode-device\0" + jwt)
        self.jwt = jwt
        return jwt, self.device

    def refresh_session(self) -> None:
        self.jwt = None
        self.device = None
        self.ensure_session(allow_reread=False)

    def balance(self, force: bool = False) -> dict:
        jwt, device = self.ensure_session()
        now = time.time()
        if not force and now - self._balance_at < 300 and self.gift_models:
            return {"models": list(self.gift_models), "plans": list(self.plans)}
        url = f"{ZCODE}/api/v1/zcode-plan/billing/balance?app_version={APP_VERSION}"
        req = urllib.request.Request(url, headers=self._api_headers(jwt, device), method="GET")
        with _open_no_redirect(req, timeout=30) as resp:
            v = json.loads(resp.read().decode("utf-8"))
        data = v.get("data") or {}
        models: list = []
        now_sec = int(data.get("server_time") or time.time())
        plan_names: list = []
        for p in data.get("plans") or []:
            plan_names.append(p.get("name"))
            if str(p.get("status", "")).lower() != "active":
                continue
            ends = int(p.get("ends_at") or 0)
            if ends and ends <= now_sec:
                continue
            for ent in p.get("entitlements") or []:
                for cap in ent.get("capabilities") or []:
                    if cap.startswith("model:"):
                        models.append(cap[len("model:") :])
        self.gift_models = sorted(set(models))
        self.plans = [p for p in plan_names if p]
        self._balance_at = now
        return {"models": list(self.gift_models), "plans": list(self.plans)}

    def _model_ready(self, wire_model: str) -> bool:
        until = self._rest_until.get(wire_model)
        return until is None or time.time() >= until

    def _rest_model(self, wire_model: str, seconds: int = 60) -> None:
        self._rest_until[wire_model] = time.time() + seconds

    @staticmethod
    def _client_language() -> str:
        """同 ZCode：zh-CN 或 en-US（locale 以 zh 开头算中文）。"""
        try:
            k32 = ctypes.WinDLL("kernel32", use_last_error=True)
            buf = ctypes.create_unicode_buffer(128)
            if hasattr(k32, "GetUserDefaultLocaleName"):
                k32.GetUserDefaultLocaleName.argtypes = [wt.LPWSTR, wt.INT]
                if k32.GetUserDefaultLocaleName(buf, 128) > 0 and buf.value:
                    return "zh-CN" if buf.value.lower().startswith("zh") else "en-US"
        except Exception:  # noqa: BLE001
            pass
        return "en-US"

    @staticmethod
    def _client_timezone() -> str:
        """IANA 时区（Windows 常见名映射，兜底按 UTC+8 猜 / unknown）。"""
        mapping = {
            "China Standard Time": "Asia/Shanghai",
            "UTC": "UTC",
            "GMT Standard Time": "Europe/London",
            "W. Europe Standard Time": "Europe/Berlin",
            "Tokyo Standard Time": "Asia/Tokyo",
            "Korea Standard Time": "Asia/Seoul",
            "Singapore Standard Time": "Asia/Singapore",
            "Taipei Standard Time": "Asia/Taipei",
            "Hong Kong S.A.R. Standard Time": "Asia/Hong_Kong",
            "US Eastern Standard Time": "America/New_York",
            "Eastern Standard Time": "America/New_York",
            "Pacific Standard Time": "America/Los_Angeles",
        }
        try:
            k32 = ctypes.WinDLL("kernel32", use_last_error=True)

            class TZI(ctypes.Structure):
                _fields_ = [("Bias", wt.LONG), ("StandardName", wt.WCHAR * 32), ("StandardDate", wt.SYSTEMTIME),
                            ("StandardBias", wt.LONG), ("DaylightName", wt.WCHAR * 32), ("DaylightDate", wt.SYSTEMTIME),
                            ("DaylightBias", wt.LONG)]

            tzi = TZI()
            if k32.GetTimeZoneInformation(ctypes.byref(tzi)) != -1:
                if tzi.StandardName in mapping:
                    return mapping[tzi.StandardName]
                if -(tzi.Bias + tzi.DaylightBias) == 480:
                    return "Asia/Shanghai"
        except Exception:  # noqa: BLE001
            pass
        return "unknown"

    @staticmethod
    def _marketing_headers() -> dict:
        arch = os.environ.get("PROCESSOR_ARCHITECTURE", "x64").lower().replace("amd64", "x64")
        return {
            "X-ZCode-App-Version": APP_VERSION,
            "X-Platform": f"{sys.platform}-{arch}",
            "X-Client-Language": ZCodeClient._client_language(),
        }

    def _api_headers(self, jwt: str, device: str) -> dict:
        """zcode.z.ai 的管理类调用（余额等）：社区插件 call() 的形状。"""
        h = {
            "Content-Type": "application/json",
            "Accept": "application/json",
            "User-Agent": UA,
            "Authorization": f"Bearer {jwt}",
            "X-Device-Mid": device,
        }
        h.update(self._marketing_headers())
        return h

    @staticmethod
    def _start_headers(jwt: str) -> dict:
        """Start Plan messages 请求的头部（同社区插件 startHeaders，逐项对齐）。"""
        arch = os.environ.get("PROCESSOR_ARCHITECTURE", "AMD64").lower().replace("amd64", "x64")
        return {
            "content-type": "application/json",
            "Authorization": f"Bearer {jwt}",
            "anthropic-version": ANTHROPIC_VERSION,
            "HTTP-Referer": ZCODE,
            "User-Agent": UA_START,
            "X-ZCode-App-Version": APP_VERSION,
            "X-Title": "Z Code@electron",
            "X-Release-Channel": "production",
            "X-Client-Language": ZCodeClient._client_language(),
            "X-Client-Timezone": ZCodeClient._client_timezone(),
            "X-ZCode-Agent": "glm",
            "X-Platform": f"{sys.platform}-{arch}",
            "X-Os-Category": "windows",
            "X-Os-Version": os_release(),
            "x-request-id": str(uuid.uuid4()),
            "x-zcode-session-type": "main",
            "x-zcode-trace-id": str(uuid.uuid4()),
            "x-zcode-query-source": "main_turn",
        }

    # ---- Anthropic Messages → ZCode 伪装 → SSE 透传 ----

    def chat(self, body: dict, stream: bool):
        def request_locked():
            jwt, device = self.ensure_session()
            attempts = 0
            while True:
                attempts += 1
                try:
                    return self._chat_once(body, stream, jwt, device, allow_refresh=attempts == 1)
                except ConcurrencyBusy:
                    if attempts >= 5:
                        raise
                    time.sleep(1.5 * attempts)

        if not stream:
            with self._lock:
                return request_locked()

        # Open the upstream request before returning so connection errors are still
        # handled before the HTTP handler commits a 200 response.
        self._lock.acquire()
        try:
            result = request_locked()
        except BaseException:
            self._lock.release()
            raise

        def stream_locked():
            # Keep the single-concurrency lock until the upstream SSE response is consumed.
            try:
                yield from result
            finally:
                try:
                    close = getattr(result, "close", None)
                    if close:
                        close()
                finally:
                    self._lock.release()

        return stream_locked()

    def _chat_once(self, body: dict, stream: bool, jwt: str, device: str, allow_refresh: bool = True):
        upstream_model = strip_gift_suffix(body.get("model", ""))
        if not self._model_ready(upstream_model):
            raise RuntimeError(f"模型 {upstream_model} 在赠送档 429 后的 60 秒冷却期内，稍后再试")
        body["model"] = upstream_model
        session = session_of(None, device)
        dressed = dress(body, device, session)

        url = START_BASE + "/v1/messages"
        payload = json.dumps(dressed, ensure_ascii=False).encode("utf-8")
        headers = self._start_headers(jwt)
        headers["x-session-id"] = session
        q = query_of(dressed, session)
        if q:
            headers["x-query-id"] = q
        req = urllib.request.Request(url, data=payload, headers=headers, method="POST")
        try:
            resp = _open_no_redirect(req, timeout=600)
        except urllib.error.HTTPError as e:
            try:
                text = e.read().decode("utf-8", "replace")
            finally:
                e.close()
            if e.code == 401 and allow_refresh:
                try:
                    self.refresh_session()
                    jwt, device = self.ensure_session()
                    return self._chat_once(body, stream, jwt, device, allow_refresh=False)
                except Exception as e2:  # noqa: BLE001
                    raise RuntimeError(f"上游 401，重读登录态后仍失败 ({type(e2).__name__})")  # noqa: B904
            if e.code == 429 and "3008" in text:
                raise ConcurrencyBusy(text)  # noqa: B904
            if e.code == 400 and "3006" in text:
                self._rest_model(upstream_model)
                raise RuntimeError("上游 3006 model not allowed：模型不在赠送档服务列表，或请求被风控评分拒绝")  # noqa: B904
            if e.code == 405 or START_REFUSED.search(text) or EDGE_PAGE.search(text):
                raise UpstreamRefused(
                    "ZCode 风控拒绝（405/3012 或阻断页）：账号被限制或网络被标记；"
                    "等几分钟或换网络，仍不行只能找 ZCode 客服"
                )  # noqa: B904
            raise RuntimeError(f"ZCode 上游返回 HTTP {e.code}")  # noqa: B904
        return anthropic_passthrough(resp, stream)


# ---- Anthropic body 整形（dress）+ ToolSearch 别名 ----


def _cached(t: str) -> dict:
    return {"type": "text", "text": t, "cache_control": {"type": "ephemeral"}}


def strip_gift_suffix(model: str) -> str:
    suffix = GIFT_SUFFIX.lower()
    return model[: -len(suffix)].lower() if model.lower().endswith(suffix) else model


def session_of(incoming: str | None, device: str) -> str:
    own = (incoming or "").strip()
    if UUID_RE.fullmatch(own or ""):
        return own.lower()
    return uuid_of("zcode-session-device\0" + (device or ""))


def query_of(dressed: dict, session: str) -> str:
    """同社区插件 queryOf：数 dress 后消息里带文本的 user 轮 n，
    uuidOf("zcode-query\\0" + session + "\\0" + n)。同轮稳定，下轮换新。"""
    n = 0
    for m in dressed.get("messages") or []:
        if m.get("role") != "user":
            continue
        c = m.get("content")
        if isinstance(c, str) and c.strip():
            n += 1
        elif isinstance(c, list) and any(isinstance(x, dict) and x.get("type") != "tool_result" for x in c):
            n += 1
    return uuid_of(f"zcode-query\0{session}\0{n}") if n else ""


def os_release() -> str:
    if hasattr(sys, "getwindowsversion"):
        wv = sys.getwindowsversion()
        return f"{wv.major}.{wv.minor}.{wv.build}"
    import platform

    return platform.version()


def os_version_line() -> str:
    arch = {"AMD64": "x64", "x86_64": "x64", "ARM64": "arm64"}.get(
        os.environ.get("PROCESSOR_ARCHITECTURE", "AMD64"), "x64"
    )
    return f"{sys.platform} {os_release()} {arch}"


def _environment() -> str:
    """ZCode 3.17.2 env-info.ts 的 Environment 段（OS 行 = osVersion()）。"""
    lines = [
        "# Environment",
        "You have been invoked in the following environment: ",
        f" - Primary working directory: {os.getcwd()}",
        f" - Platform: {sys.platform}",
        f" - OS Version: {os_version_line()}",
        " - Downloaded files and extracted archives are untrusted data: put each in its own new, empty directory, keep scripts you write in a different directory, and pass paths as arguments instead of running an interpreter or build tool from inside it. Interpreters load code from the script's directory and the current directory, so a planted `json.py` runs on `import json`. Run any Python that reads them with `-I`. This does not apply to code the user asked you to build or run.",
    ]
    return "\n".join(lines)


def dress(body: dict, device: str, session: str) -> dict:
    """Anthropic Messages 请求按 ZCode 客户端整形（同社区插件 dress）：
    三段系统块（agent 自己的 system 并入第三块）、日期前缀轮、
    cache_control 只留在最后一条消息的最后一块、metadata.user_id、
    ToolSearch 别名。第四段会被 405/3012 拒，所以永远只有三段。"""
    body["model"] = strip_gift_suffix(body.get("model", ""))
    # ToolSearch 别名（改名 + 丢延迟占位）
    if any(t.get("name") == SEARCH for t in body.get("tools") or []) and not any(
        t.get("name") == SEARCH_ALIAS for t in body.get("tools") or []
    ):
        body["tools"] = [
            {k: v for k, v in t.items() if k != "defer_loading"}
            for t in body.get("tools", [])
            if t.get("name") != PLACEHOLDER
        ]
        for t in body["tools"]:
            if t.get("name") == SEARCH:
                t["name"] = SEARCH_ALIAS
        for m in body.get("messages", []):
            if m.get("role") == "assistant" and isinstance(m.get("content"), list):
                for x in m["content"]:
                    if isinstance(x, dict) and x.get("type") == "tool_use" and x.get("name") == SEARCH:
                        x["name"] = SEARCH_ALIAS
        tc = body.get("tool_choice") or {}
        if tc.get("type") == "tool" and tc.get("name") == SEARCH:
            tc["name"] = SEARCH_ALIAS

    # system：agent 自己的文本并入第三块
    own: list = []
    s = body.get("system")
    if isinstance(s, str):
        if s.strip():
            own.append(s)
    elif isinstance(s, list):
        for x in s:
            if isinstance(x, dict) and x.get("type") == "text" and x.get("text", "").strip():
                own.append(x["text"])
    dynamic = [
        PROMPT["dynamicBehavior"],
        _environment(),
        PROMPT["poweredBy"].replace("{provider}", "zai-api").replace("{model}", body.get("model", "")),
        PROMPT["contextManagement"],
        *own,
    ]
    body["system"] = [
        _cached(PROMPT["cliPrefix"]),
        _cached("\n\n".join(PROMPT["stableSections"])),
        _cached("\n\n" + "\n\n".join(dynamic)),
    ]

    # 日期前缀轮
    today = date.today().isoformat()
    prefix = {
        "role": "user",
        "content": [{"type": "text", "text": "<system-reminder>" + PROMPT["contextDate"].replace("{date}", today) + "</system-reminder>"}],
    }
    body["messages"] = [prefix, *(body.get("messages") or [])]

    # cache_control 形状：消息块上的全删，最后一条消息的最后一块放 ephemeral
    last = None
    for m in body["messages"]:
        if m.get("role") == "system":
            continue
        last = m
        if isinstance(m.get("content"), list):
            for x in m["content"]:
                if isinstance(x, dict):
                    x.pop("cache_control", None)
    if last:
        if isinstance(last.get("content"), str):
            last["content"] = [_cached(last["content"])]
        elif isinstance(last.get("content"), list) and last["content"]:
            x = last["content"][-1]
            if isinstance(x, dict) and "cache_control" not in x:
                x["cache_control"] = {"type": "ephemeral"}

    meta = body.get("metadata") if isinstance(body.get("metadata"), dict) else {}
    body["metadata"] = {
        **meta,
        "user_id": json.dumps({"device_id": device, "account_uuid": "", "session_id": session}),
    }
    return body


# ---- SSE 透传（ToolSearch 别名改回） ----

SEARCH_LINE = re.compile(r'"name"\s*:\s*"' + re.escape(SEARCH_ALIAS) + '"')


def _rewrite_sse_event(lines: list[str]) -> str:
    """Rewrite ToolSearch aliases inside one SSE event without changing its framing."""
    data_indexes = [i for i, line in enumerate(lines) if line.startswith("data:")]
    if not data_indexes:
        return "".join(lines)

    payload = "\n".join(
        lines[i][5:].rstrip("\r\n").removeprefix(" ") for i in data_indexes
    )
    try:
        event = json.loads(payload)
    except json.JSONDecodeError:
        return "".join(lines)
    if not isinstance(event, dict):
        return "".join(lines)
    block = event.get("content_block")
    if isinstance(block, dict) and block.get("name") == SEARCH_ALIAS:
        block["name"] = SEARCH
    else:
        return "".join(lines)

    first = data_indexes[0]
    original = lines[first]
    ending = "\r\n" if original.endswith("\r\n") else "\n" if original.endswith("\n") else ""
    prefix = "data: " if original.startswith("data: ") else "data:"
    rewritten = prefix + json.dumps(event, ensure_ascii=False, separators=(",", ":")) + ending
    later = set(data_indexes[1:])
    return "".join(rewritten if i == first else "" if i in later else line for i, line in enumerate(lines))


def anthropic_passthrough(resp, stream: bool):
    """透传 Anthropic 响应；流式按 SSE 事件转发，保持 event/data 归属与边界。"""
    if not stream:
        try:
            data = resp.read()
            try:
                v = json.loads(data.decode("utf-8", "replace"))
                for blk in v.get("content") or []:
                    if isinstance(blk, dict) and blk.get("type") == "tool_use" and blk.get("name") == SEARCH_ALIAS:
                        blk["name"] = SEARCH
                return json.dumps(v, ensure_ascii=False)
            except json.JSONDecodeError:
                return data.decode("utf-8", "replace")
        finally:
            close = getattr(resp, "close", None)
            if close:
                close()

    def gen():
        event_lines: list[str] = []
        try:
            for raw in resp:
                line = raw.decode("utf-8", "replace")
                if line in ("\n", "\r\n"):
                    yield _rewrite_sse_event(event_lines)
                    yield line
                    event_lines.clear()
                else:
                    event_lines.append(line)
            if event_lines:
                yield _rewrite_sse_event(event_lines)
        finally:
            close = getattr(resp, "close", None)
            if close:
                close()

    return gen()


# ---------------------------------------------------------------- HTTP 服务

class Handler(BaseHTTPRequestHandler):
    server_version = "tern-plugin-zcode/0.1.1"
    client: ZCodeClient = None  # type: ignore[assignment]
    plugin_token: str = ""
    mock: bool = False
    allow_unauthenticated_local: bool = False

    def log_message(self, fmt, *args):  # noqa: A003
        log(f"{self.address_string()} {fmt % args}")

    def _auth(self) -> bool:
        """恒定时间比较本地令牌；无令牌时仅显式 opt-in 才放行。"""
        if not self.plugin_token:
            return self.allow_unauthenticated_local
        got = self.headers.get("X-Tern-Plugin-Token", "")
        return hmac.compare_digest(got, self.plugin_token)

    def _json(self, code: int, obj):
        data = json.dumps(obj, ensure_ascii=False).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):  # noqa: N802
        if not self._auth():
            return self._json(401, {"error": {"message": "未授权（X-Tern-Plugin-Token 不匹配）"}})
        path = self.path.split("?")[0]
        if path == "/health":
            try:
                b = self.client.balance()
                models = [m + GIFT_SUFFIX for m in b.get("models", [])]
                plans = b.get("plans", [])
            except Exception as e:  # noqa: BLE001
                log(f"health 读余额失败 ({type(e).__name__})")
                models = [m + GIFT_SUFFIX for m in self.client.gift_models]
                plans = list(self.client.plans)
            out = {"ok": True, "models": models}
            if plans:
                out["plans"] = plans
            return self._json(200, out)
        if path == "/v1/models":
            try:
                models = list(self.client.balance().get("models", []))
            except Exception as e:  # noqa: BLE001
                log(f"models 读余额失败 ({type(e).__name__})")
                models = list(self.client.gift_models)
            data = [{"id": m + GIFT_SUFFIX, "object": "model", "owned_by": "zcode"} for m in models]
            return self._json(200, {"object": "list", "data": data})
        return self._json(404, {"error": {"message": f"未知路径 {path}"}})

    def do_POST(self):  # noqa: N802
        if not self._auth():
            return self._json(401, {"error": {"message": "未授权（X-Tern-Plugin-Token 不匹配）"}})
        path = self.path.split("?")[0]
        if path != "/v1/messages":
            return self._json(404, {"error": {"message": f"未知路径 {path}（api_format=anthropic 只认 /v1/messages）"}})
        try:
            length = int(self.headers.get("Content-Length") or 0)
        except ValueError:
            return self._json(400, {"error": {"message": "Content-Length 无效"}})
        if length <= 0:
            return self._json(400, {"error": {"message": "请求体不能为空"}})
        if length > MAX_REQUEST_BYTES:
            return self._json(413, {"error": {"message": "请求体超过 200 MiB 限制"}})
        try:
            body = json.loads(self.rfile.read(length))
        except (json.JSONDecodeError, UnicodeDecodeError):
            return self._json(400, {"error": {"message": "请求体不是有效 JSON"}})
        if not isinstance(body, dict):
            return self._json(400, {"error": {"message": "请求体必须是 JSON 对象"}})
        if not isinstance(body.get("model"), str) or not body["model"].strip():
            return self._json(400, {"error": {"message": "model 必须是非空字符串"}})

        stream = body.get("stream") is True
        if self.mock:
            return self._mock(body, stream)
        try:
            result = self.client.chat(body, stream)
        except ConcurrencyBusy:
            return self._json(429, {"error": {"message": "ZCode 赠送档并发上限（3008），已重试仍忙"}})
        except UpstreamRefused as e:
            return self._json(403, {"error": {"message": str(e), "type": "zcode_restricted"}})
        except SessionExpired as e:
            return self._json(401, {"error": {"message": str(e)}})
        except Exception as e:  # noqa: BLE001
            # urllib/network exception text can contain URLs or upstream response data.
            log(f"chat 失败 ({type(e).__name__})")
            return self._json(502, {"error": {"message": "ZCode 上游请求失败"}})

        if stream:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                for line in result:
                    self.wfile.write(line.encode("utf-8"))
                    self.wfile.flush()
            except BrokenPipeError:
                pass
            except Exception as e:  # noqa: BLE001
                log(f"流式响应失败 ({type(e).__name__})")
            finally:
                try:
                    close = getattr(result, "close", None)
                    if close:
                        close()
                except Exception as e:  # noqa: BLE001
                    log(f"流式响应关闭失败 ({type(e).__name__})")
                self.close_connection = True
            return
        data = result.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def _mock(self, body: dict, stream: bool):
        """--mock 模式：离线验证契约（Anthropic 形状 + usage）。"""
        model = body.get("model", "mock")
        created = int(time.time())
        content = [{"type": "text", "text": "[mock] 收到", "cache_control": {"type": "ephemeral"}}]
        usage = {"input_tokens": 12, "output_tokens": 6}
        if stream:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Cache-Control", "no-cache")
            self.end_headers()
            events = [
                {"type": "message_start", "message": {"id": "msg_mock", "model": model, "usage": {"input_tokens": 12}}},
                {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}},
                {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "[mock] 收到"}},
                {"type": "content_block_stop", "index": 0},
                {"type": "message_delta", "delta": {"stop_reason": "end_turn"}, "usage": {"output_tokens": 6}},
                {"type": "message_stop"},
            ]
            for ev in events:
                self.wfile.write((f"event: {ev['type']}\ndata: " + json.dumps(ev, ensure_ascii=False) + "\n\n").encode())
                self.wfile.flush()
            return
        self._json(
            200,
            {
                "id": "msg_mock",
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": content,
                "stop_reason": "end_turn",
                "usage": usage,
            },
        )


def serve(port: int, token: str, mock: bool) -> int:
    Handler.client = ZCodeClient()
    Handler.plugin_token = token
    Handler.mock = mock
    if not mock:
        try:
            b = Handler.client.balance(force=True)
            log(f"余额读取成功：{b.get('plans')} → 模型 {b.get('models')}")
        except Exception as e:  # noqa: BLE001
            log(f"启动时余额读取失败（请求时会重试，{type(e).__name__}）")
    httpd = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    log(f"监听 127.0.0.1:{port}（mock={mock}，token={'已配置' if token else '未配置（手动模式）'}）")
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        httpd.server_close()
    return 0


def main() -> int:
    ap = argparse.ArgumentParser(description="tern ZCode 插件（外挂进程，Anthropic 直通）")
    ap.add_argument("cmd", nargs="?", default="serve", choices=["serve", "health"], help="serve=启动服务；health=打印一次 /health JSON 后退出")
    ap.add_argument("--port", type=int, default=int(os.environ.get("TERN_PLUGIN_PORT") or 0) or 18085)
    ap.add_argument("--mock", action="store_true", help="离线 mock 模式（契约自检用）")
    ap.add_argument(
        "--allow-unauthenticated-local",
        action="store_true",
        help="无 TERN_PLUGIN_TOKEN 时允许 loopback 未认证访问（仅用于手动接入；同机进程均可调用）",
    )
    ap.add_argument("--models", action="store_true", help="打印当前赠送档模型列表后退出")
    args = ap.parse_args()

    token = os.environ.get("TERN_PLUGIN_TOKEN", "")
    if args.cmd == "health" or args.models:
        c = ZCodeClient()
        try:
            b = c.balance(force=True)
            print(json.dumps(
                {"ok": True, "models": [m + GIFT_SUFFIX for m in b.get("models", [])], "plans": b.get("plans", [])},
                ensure_ascii=False,
            ))
            return 0
        except Exception as e:  # noqa: BLE001
            print(json.dumps({"ok": False, "error": type(e).__name__}, ensure_ascii=False))
            return 1

    if not token and not args.allow_unauthenticated_local:
        ap.error("服务模式必须设置 TERN_PLUGIN_TOKEN；手动 loopback 接入可显式加 --allow-unauthenticated-local")
    if os.name != "nt" and not args.mock:
        log("警告：非 Windows 环境读不了凭据管理器，只有 mock 模式可用")
    Handler.allow_unauthenticated_local = args.allow_unauthenticated_local
    return serve(args.port, token, args.mock)


if __name__ == "__main__":
    sys.exit(main())
