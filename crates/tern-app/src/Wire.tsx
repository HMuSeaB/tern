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
  onChanged,
}: {
  /** 网关是否在跑。没跑时接线成功也没流量，UI 要催启动 */
  running: boolean;
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
        <div className="head"><span className="title">接入 Claude Code</span></div>
        <p className="empty-text">正在读取…</p>
      </div>
    );
  }

  return (
    <div className="card">
      <div className="head">
        <span className="title">接入 Claude Code</span>
        <span className="act">{status.wired ? "已接入" : "未接入"}</span>
      </div>

      {/* 说清楚现在流量去哪了——这是用户最想知道、也最容易被绿灯骗的一件事 */}
      <div className="wire-now">
        <span className="side-label">当前生效的地址</span>
        <code className="wire-url">{status.current_base_url ?? "（未配置）"}</code>
        {!status.wired && (
          <span className="wire-vs">不是 tern，流量还绕着我们走</span>
        )}
      </div>

      <div className="wire-row">
        <button
          className={`btn ${status.wired ? "" : "primary"}`}
          onClick={() => void toggle()}
          disabled={busy}
        >
          {status.wired ? "断开" : "接入"}
        </button>
        <button className="btn" onClick={() => void runProbe()} disabled={probing}>
          {probing ? "正在发一条测试请求…" : "发一条测试请求"}
        </button>
        <span className="wire-cost">会消耗约 1 次调用的费用</span>
      </div>

      {/* 接线只解决"流量进不进得来"，走哪一家是另一件事——而用户正是在这一刻
          决定 model 怎么写。不说清楚的话，接完线模型名还是从 cc-switch 带来的
          裸名，请求全部落到默认供应商，显式路由等于没做。
          不写成"必须"：默认供应商那条路是用户自己选的，图省事一直用它没问题 */}
      <div className="wire-route">
        <div className="wire-route-head">
          想指定走哪家，把 model 写成 <code>供应商/模型名</code>
        </div>
        <p>
          比如 <code>deepseek/deepseek-v4-pro</code> 就固定走 deepseek 那家，
          和当前默认供应商是谁无关。不写前缀则落到默认供应商——切换供应商时
          自动配好的那几档模型名就是这种写法，图省事可以一直用它。
        </p>
        <p>
          模型名去「供应商」页点「获取模型列表」，每行还有个「复制前缀」按钮；
          面板的「最近请求」里会标出每条走了显式还是兜底。
        </p>
      </div>

      {status.wired && !running && (
        <div className="notice warn" style={{ marginTop: 14 }}>
          <span className="notice-ic">!</span>
          <span className="notice-body">
            <b>接线成功了，但网关没在跑。</b>
            Claude Code 现在会连一个没有服务的端口。点右上角「启动」。
          </span>
        </div>
      )}

      {!status.token_configured && (
        <div className="notice warn" style={{ marginTop: 14 }}>
          <span className="notice-ic">!</span>
          <span className="notice-body">
            <b>还没设 accessToken。</b>
            接入后本机任何进程都能连上 tern 借用你的供应商 key。建议先去
            <code> tern.json</code> 里填一个。
          </span>
        </div>
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

      <p className="perm-foot">
        写入 <code>~/.claude/settings.json</code> 的 <code>env</code> 段，只改
        <code> ANTHROPIC_BASE_URL</code> 和 <code> ANTHROPIC_AUTH_TOKEN</code>
        ，其余字段不动。原值存在 tern 自己那儿，断开时还原。
        注意这个文件被 cc-switch 托管，切一次供应商接线就会被擦掉——
        这里会显示成未接入，再点一次即可。测试请求会真花一次调用的钱。
      </p>
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
        <span className="title">Claude Desktop · Chat / Cowork / Code</span>
        <span className="act">{status?.connected ? "已接入 tern" : status?.active ? "有活动配置" : "未接入"}</span>
      </div>
      <p className="perm-hint">
        Claude Desktop 的 Chat、Cowork 和 Code 共用这条网关配置。它和 Claude Code CLI 是两套设置：此处写入 Desktop 的 3P profile，不会改动 <code>~/.claude/settings.json</code>。
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
        写入 <code>{status?.profilePath ?? "%LOCALAPPDATA%/Claude-3p/configLibrary"}</code> 中 tern 专属的 3P profile，并保留其它 profile。
        Claude Desktop 的模型列表不负责自动发现 Tern 的路由名，因此这里按 <code>供应商/模型</code> 配置。
        接入后完全退出并重开 Claude Desktop；断开会恢复原活动 profile 和原启动模式。
      </p>
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
