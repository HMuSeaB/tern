import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatMoneyHero, formatTokens } from "./format";
import type { WireStatus } from "./Wire";
import type { ConfigSummary, Panel, ServerStatus } from "./types";

/**
 * 网关工作台：把「网关运行」「当前供应商」「客户端接入」「今日流量」放在同一条工作流里。
 * 深入管理仍留在各自页面；日常检查和切换不必再在几个互不相干的页面之间来回找。
 */
export function Workbench({
  running,
  busy,
  server,
  config,
  panel,
  onStart,
  onStop,
  onRefresh,
}: {
  running: boolean;
  busy: boolean;
  server: ServerStatus | null;
  config: ConfigSummary | null;
  panel: Panel | null;
  onStart: () => void;
  onStop: () => void;
  onRefresh: () => void;
}) {
  const [wire, setWire] = useState<WireStatus | null>(null);
  const [wireBusy, setWireBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const active = config?.providers.find((provider) => provider.active) ?? null;

  const refreshWire = useCallback(async () => {
    try {
      setWire(await invoke<WireStatus>("wire_status"));
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refreshWire();
  }, [refreshWire]);

  const selectProvider = async (id: string) => {
    try {
      await invoke<ConfigSummary>("select_provider", { id });
      onRefresh();
    } catch (e) {
      setError(String(e));
    }
  };

  const toggleWire = async () => {
    setWireBusy(true);
    try {
      setWire(await invoke<WireStatus>(wire?.wired ? "wire_disable" : "wire_enable"));
      setError(null);
      onRefresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setWireBusy(false);
    }
  };

  const today = panel?.today;
  const totalTokens = today
    ? today.fresh_input + today.output + today.cache_read + today.cache_write
    : 0;

  return (
    <div className="workbench">
      <section className="card workbench-hero">
        <div className="workbench-heading">
          <div>
            <div className="eyebrow">TERN GATEWAY · 工作台</div>
            <h1>流量，正在这里汇合。</h1>
            <p>运行状态、上游供应商和客户端接入，在同一处完成检查与切换。</p>
          </div>
          <span className={`workbench-status ${running ? "is-live" : ""}`}>
            <span className={`live-dot ${running ? "on" : "off"}`} />
            {running ? "网关运行中" : "网关已停止"}
          </span>
        </div>

        <div className="flowline" aria-label="客户端经过 tern 网关转发到供应商">
          <div className={`flow-node ${wire?.wired ? "is-connected" : ""}`}>
            <span className="flow-node-icon">⌘</span>
            <span><b>Claude Code</b><small>{wire?.wired ? "已接入 tern" : "客户端"}</small></span>
          </div>
          <span className={`flow-link ${wire?.wired && running ? "is-active" : ""}`} aria-hidden />
          <div className={`flow-node flow-gateway ${running ? "is-connected" : ""}`}>
            <span className="flow-node-icon">t</span>
            <span><b>tern 网关</b><small>{server?.listen ?? "127.0.0.1:15800"}</small></span>
          </div>
          <span className={`flow-link ${running ? "is-active" : ""}`} aria-hidden />
          <div className={`flow-node ${active ? "is-connected" : ""}`}>
            <span className="flow-node-icon">↗</span>
            <span><b>{active?.name ?? "选择供应商"}</b><small>{active?.api_format ?? "上游供应商"}</small></span>
          </div>
        </div>

        <div className="workbench-controls">
          <label className="workbench-provider">
            <span>默认供应商</span>
            <select
              value={active?.id ?? ""}
              disabled={!config || config.providers.length === 0}
              onChange={(event) => void selectProvider(event.target.value)}
            >
              {!active && <option value="">尚未选择</option>}
              {config?.providers.map((provider) => (
                <option key={provider.id} value={provider.id}>{provider.name}</option>
              ))}
            </select>
          </label>
          <div className="workbench-actions">
            <button
              className={`btn ${running ? "" : "primary"}`}
              onClick={running ? onStop : onStart}
              disabled={busy}
            >
              {busy ? "请稍候…" : running ? "停止网关" : "启动网关"}
            </button>
            <button
              className={`btn ${wire?.wired ? "ghost" : "primary"}`}
              onClick={() => void toggleWire()}
              disabled={wireBusy || !wire}
              title="将 Claude Code 的请求接入 tern"
            >
              {wireBusy ? "处理中…" : wire?.wired ? "断开 Claude Code" : "接入 Claude Code"}
            </button>
          </div>
        </div>

        {!running && server?.last_error && <p className="workbench-warning">{server.last_error}</p>}
        {wire?.wired && !running && (
          <p className="workbench-warning">Claude Code 已接入，但网关已停止。启动网关后请求才能正常转发。</p>
        )}
        {!active && config?.providers.length === 0 && (
          <p className="workbench-warning">还没有供应商配置。先到「供应商」页新增或导入一家。</p>
        )}
        {error && <p className="workbench-error">{error}<button className="btn ghost" onClick={() => void refreshWire()}>重试</button></p>}
      </section>

      <section className="workbench-stats" aria-label="今日网关用量">
        <div className="card workbench-stat">
          <span>今日花费</span>
          <b>{today ? formatMoneyHero(today.cost) : "—"}</b>
          <small>{today ? `${today.requests} 次请求` : "启动网关后开始记录"}</small>
        </div>
        <div className="card workbench-stat">
          <span>今日 Token</span>
          <b>{today ? formatTokens(totalTokens) : "—"}</b>
          <small>{today ? `缓存省下 ${formatMoneyHero(today.cache_savings)}` : "流量会汇总在这里"}</small>
        </div>
        <div className="card workbench-stat">
          <span>需要留意</span>
          <b className={today && (today.failures > 0 || today.unpriced > 0) ? "has-alert" : ""}>
            {today ? today.failures + today.unpriced : "—"}
          </b>
          <small>{today ? `${today.failures} 次失败 · ${today.unpriced} 次未定价` : "检查接入和供应商状态"}</small>
        </div>
      </section>

      <div className="workbench-footer">
        <span>{wire?.current_base_url ? `客户端当前地址：${wire.current_base_url}` : "接入 Claude Code 后，流量会经过上方这条链路。"}</span>
        <button className="btn ghost" onClick={onRefresh}>刷新状态</button>
      </div>
    </div>
  );
}
