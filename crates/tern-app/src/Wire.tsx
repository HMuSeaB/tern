import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";

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
