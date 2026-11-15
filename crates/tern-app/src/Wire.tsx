import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ConfigSummary } from "./types";

/**
 * 把 Claude Code 接到 tern 的流量上。
 *
 * 为什么非有不可：用户的 `~/.claude/settings.json` 里 `env.ANTHROPIC_BASE_URL`
 * 指向 cc-switch 的本地代理（旁边那个 `PROXY_MANAGED` 就是它接管的证据），
 * tern 监听另一个端口。不起这个开关，网关起得再好也没有一个请求流经过它——
 * 用量面板永远是空的。这是"装了不会用"的真正原因。
 *
 * 读-改-写只动 `env.ANTHROPIC_BASE_URL` 和 `env.ANTHROPIC_AUTH_TOKEN` 两个键，
 * cc-switch 的模型别名和用户自己的字段原样带回（Rust 侧有测试守住）。
 */
export function Wire({
  running,
  onStart,
  startBusy = false,
  onChanged,
}: {
  /** 网关未启动时不允许接入，避免客户端指向一个没有服务的地址 */
  running: boolean;
  startBusy?: boolean;
  onStart: () => void;
  onChanged?: () => void;
}) {
  const [status, setStatus] = useState<WireStatus | null>(null);
  const [busy, setBusy] = useState(false);
  const [probing, setProbing] = useState(false);
  const [probe, setProbe] = useState<ProbeResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setStatus(await invoke<WireStatus>("wire_status"));
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const toggle = useCallback(async () => {
    setBusy(true);
    setProbe(null);
    try {
      setStatus(
        await invoke<WireStatus>(status?.wired ? "wire_disable" : "wire_enable"),
      );
      setError(null);
      onChanged?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [status?.wired, onChanged]);

  const runProbe = useCallback(async () => {
    setProbing(true);
    setProbe(null);
    try {
      setProbe(await invoke<ProbeResult>("wire_probe"));
      setError(null);
      onChanged?.();
    } catch (e) {
      setError(String(e));
    } finally {
      setProbing(false);
    }
  }, [onChanged]);

  if (!status) {
    return (
      <div className="card">
        <div className="head"><span className="title">Claude Code CLI</span></div>
        <p className="empty-text">正在读取接入状态…</p>
      </div>
    );
  }

  return (
    <div className="card">
      <div className="head">
        <span className="title">Claude Code CLI</span>
        <span className={`wire-status ${status.wired && running ? "is-ready" : ""}`}>
          {status.wired ? (running ? "已接入 tern" : "已接入 · 网关未运行") : "尚未接入"}
        </span>
      </div>

      <div className="wire-steps" aria-label="接入步骤">
        <div className={`wire-step ${running ? "is-done" : "is-current"}`}>
          <span className="wire-step-number">{running ? "✓" : "1"}</span>
          <span><b>启动网关</b><small>{running ? "网关正在运行" : "先启动，Claude Code 才能连接"}</small></span>
          {!running && <button className="btn primary" onClick={onStart} disabled={startBusy}>{startBusy ? "正在启动…" : "启动 tern"}</button>}
        </div>
        <div className={`wire-step ${status.wired ? "is-done" : running ? "is-current" : ""}`}>
          <span className="wire-step-number">{status.wired ? "✓" : "2"}</span>
          <span><b>连接 Claude Code</b><small>{status.wired ? (running ? "请求会经过 tern 网关" : "地址已设好，启动网关后即可使用") : "只修改连接地址，可随时恢复"}</small></span>
          <button
            className={`btn ${status.wired ? "" : "primary"}`}
            onClick={() => void toggle()}
            disabled={busy || (!running && !status.wired)}
          >
            {busy ? "处理中…" : status.wired ? "断开并恢复原设置" : "接入 Claude Code"}
          </button>
        </div>
      </div>

      <details className="wire-details">
        <summary>查看当前连接地址和设置说明</summary>
        <div className="wire-now">
          <span className="side-label">Claude Code 当前地址</span>
          <code className="wire-url">{status.current_base_url ?? "尚未配置"}</code>
          {status.wired && <span className="wire-vs is-ok">当前指向 tern</span>}
          {!status.wired && status.current_base_url && <span className="wire-vs">当前指向其它服务，不经过 tern</span>}
        </div>
        <p className="perm-foot">接入会修改 <code>~/.claude/settings.json</code> 中的网关地址和认证令牌，其它设置不动；断开时恢复接入前的值。</p>
      </details>

      <div className="wire-probe">
        <div><b>确认连接是否正常</b><small>发送一条真实请求，会产生约 1 次调用费用。</small></div>
        <button className="btn" onClick={() => void runProbe()} disabled={probing || !running || !status.wired}>
          {probing ? "正在测试…" : "发送测试请求"}
        </button>
      </div>

      <details className="wire-details">
        <summary>进阶：如何指定请求走哪家供应商？</summary>
        <div className="wire-route">
          <div className="wire-route-head">模型名前加「供应商 ID/」即可指定</div>
          <p>例如 <code>deepseek/deepseek-v4-pro</code> 会固定走 deepseek；不加前缀则使用工作台选定的默认供应商。</p>
          <p>供应商 ID 和模型名可在「供应商」页查看；「最近请求」会显示实际走向。</p>
        </div>
      </details>

      {!status.token_configured && (
        <p className="wire-security-hint">
          提醒：尚未设置 accessToken，本机其它程序也能使用网关。个人单机使用可继续；需要隔离访问时，请先设置 accessToken。
        </p>
      )}

      {probe && (
        <div className={`notice ${probe.reachable ? "ok" : "err"}`} style={{ marginTop: 14 }}>
          <span className="notice-ic">{probe.reachable ? "✓" : "!"}</span>
          <span className="notice-body">
            <b>{probe.message}</b>
            <span className="notice-cmd">
              {probe.model} · HTTP {probe.http_status ?? "—"}
            </span>
          </span>
        </div>
      )}

      {error && (
        <div className="notice err" style={{ marginTop: 14 }}>
          <span className="notice-ic">!</span>
          <span className="notice-body">{error}</span>
          <button className="btn ghost" onClick={() => void reload()}>重试</button>
        </div>
      )}

      <details className="wire-details wire-caution">
        <summary>接入后没生效怎么办？</summary>
        <p className="perm-foot">
          cc-switch 切换供应商时可能覆盖 Claude Code 的连接设置。如果这里显示「尚未接入」，重新点一次「接入 Claude Code」即可。
          接入前会保存原设置，断开时可恢复。
        </p>
      </details>
    </div>
  );
}

/** Claude Desktop 是独立客户端：使用 3P configLibrary，不触碰 Claude Code 的 settings.json。 */
export function ClaudeDesktopWire({
  config,
  running,
}: {
  config: ConfigSummary | null;
  running: boolean;
}) {
  const [status, setStatus] = useState<ClaudeDesktopStatus | null>(null);
  const [providerId, setProviderId] = useState("");
  const [modelsText, setModelsText] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      const next = await invoke<ClaudeDesktopStatus>("claude_desktop_status");
      setStatus(next);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    if (!providerId && config?.providers.length) setProviderId(config.providers[0].id);
  }, [config, providerId]);

  useEffect(() => {
    if (!status?.connected || status.models.length === 0) return;
    const firstRoute = status.models[0].split("/", 1)[0];
    if (config?.providers.some((provider) => provider.id === firstRoute)) setProviderId(firstRoute);
    setModelsText(status.models.map((model) => {
      const prefix = `${firstRoute}/`;
      return model.startsWith(prefix) ? model.slice(prefix.length) : model;
    }).join("\n"));
  }, [status, config]);

  const connect = async () => {
    const models = modelsText.split(/[\n,，]+/).map((model) => model.trim()).filter(Boolean);
    if (!providerId || !models.length) {
      setError("请选择供应商，并填写至少一个上游模型名。");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      setStatus(await invoke<ClaudeDesktopStatus>("claude_desktop_connect", { providerId, models }));
      setNotice("Claude Desktop 已接入。请完全退出并重新打开 Claude Desktop 后生效。");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const disconnect = async () => {
    setBusy(true);
    setError(null);
    try {
      setStatus(await invoke<ClaudeDesktopStatus>("claude_desktop_disconnect"));
      setNotice("已恢复接入前的 Claude Desktop 配置。完全退出并重开应用后生效。");
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const providers = config?.providers ?? [];
  const selectedProvider = providers.find((provider) => provider.id === providerId);

  return (
    <section className="card desktop-wire">
      <div className="head">
        <span className="title">Claude Desktop</span>
        <span className="act">{status?.connected ? "已接入 tern" : status?.active ? "有活动配置" : "可选"}</span>
      </div>
      <details className="wire-details desktop-wire-details">
        <summary>{status?.connected ? "查看或修改 Desktop 设置（Chat / Cowork / Code）" : "可选设置：接入 Claude Desktop（Chat / Cowork / Code）"}</summary>
        <p className="perm-hint">
          这是独立于上方 Claude Code CLI 的可选配置。接入后需完全退出并重开 Claude Desktop。
        </p>
        {status?.connected && (
          <div className="wire-now">
            <span className="side-label">当前 Desktop 网关</span>
            <code className="wire-url">{status.baseUrl}</code>
            <span className="wire-vs">模型：{status.models.join("、")}</span>
          </div>
        )}

      <div className="desktop-wire-form">
        <label className="desktop-wire-field">
          <span>供应商</span>
          <select value={providerId} onChange={(event) => setProviderId(event.target.value)} disabled={!providers.length}>
            {providers.map((provider) => <option key={provider.id} value={provider.id}>{provider.name} · {provider.id}</option>)}
          </select>
        </label>
        <label className="desktop-wire-field">
          <span>模型名 <small>填写该上游的真实模型 ID，可多行或逗号分隔</small></span>
          <textarea
            value={modelsText}
            onChange={(event) => setModelsText(event.target.value)}
            placeholder={selectedProvider ? "例如：\nclaude-sonnet-4-6" : "先选择供应商"}
            rows={3}
          />
        </label>
      </div>

      <div className="wire-row">
        <button className="btn primary" onClick={() => void connect()} disabled={busy || !running || !providers.length}>
          {busy ? "处理中…" : status?.connected ? "更新 Desktop 接入" : "接入 Claude Desktop"}
        </button>
        {status?.connected && (
          <button className="btn" onClick={() => void disconnect()} disabled={busy}>
            断开并恢复原配置
          </button>
        )}
        <button className="btn ghost" onClick={() => void reload()} disabled={busy}>刷新状态</button>
      </div>

      {!running && <p className="workbench-warning">先启动 tern 网关，再接入 Claude Desktop。</p>}
      {status && !status.tokenConfigured && (
        <div className="notice warn" style={{ marginTop: 12 }}>
          <span className="notice-ic">!</span>
          <span className="notice-body">
            tern 尚未设置 accessToken。Desktop 仍可接入，但本机其他进程也能访问网关并使用已配置的供应商凭据。
            建议先在 tern.json 中设置 accessToken。
          </span>
        </div>
      )}
      {notice && <div className="notice ok" style={{ marginTop: 12 }}><span className="notice-ic">✓</span><span className="notice-body">{notice}</span></div>}
      {error && <div className="notice err" style={{ marginTop: 12 }}><span className="notice-ic">!</span><span className="notice-body">{error}</span></div>}
      <p className="perm-foot">
        设置保存在 <code>{status?.profilePath ?? "%LOCALAPPDATA%/Claude-3p/configLibrary"}</code> 的 tern 专属 profile 中；断开时恢复原配置。
      </p>
      </details>
    </section>
  );
}

export interface ClaudeDesktopStatus {
  connected: boolean;
  active: boolean;
  tokenConfigured: boolean;
  baseUrl: string | null;
  models: string[];
  profilePath: string;
}

export interface WireStatus {
  wired: boolean;
  current_base_url: string | null;
  tern_base_url: string;
  token_written: boolean;
  token_configured: boolean;
  replaced: [string, string][];
  running: boolean;
  settings_path: string;
}

export interface ProbeResult {
  reachable: boolean;
  http_status: number | null;
  message: string;
  model: string;
}
