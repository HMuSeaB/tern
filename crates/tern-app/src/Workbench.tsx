import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { formatMoneyHero, formatTokens } from "./format";
import type { ClaudeDesktopStatus, WireStatus } from "./Wire";
import type { ConfigSummary, Panel, ServerStatus } from "./types";

/**
 * 网关工作台：把「网关运行」「当前供应商」「客户端接入」「今日流量」放在同一条工作流里。
 * 深入管理仍留在各自页面；日常检查和切换不必再在几个互不相干的页面之间来回找。
 */
function endpointLabel(url: string): string {
  try {
    const parsed = new URL(url.includes("://") ? url : `http://${url}`);
    return parsed.host + (parsed.pathname !== "/" ? parsed.pathname.replace(/\/$/, "") : "");
  } catch {
    return url;
  }
}

function apiFormatLabel(format: string): string {
  const labels: Record<string, string> = {
    anthropic: "Anthropic API",
    openai_chat: "OpenAI Chat",
    openai_responses: "OpenAI Responses",
    gemini_native: "Gemini API",
  };
  return labels[format] ?? format;
}

function keyStateLabel(state: string): string {
  const labels: Record<string, string> = {
    real: "凭据已配置",
    placeholder: "需填写凭据",
    empty: "凭据为空",
    subscription: "订阅授权",
  };
  return labels[state] ?? state;
}

export function Workbench({
  running,
  busy,
  server,
  config,
  panel,
  onStart,
  onStop,
  onRefresh,
  onOpenConnections,
  onOpenProviders,
}: {
  running: boolean;
  busy: boolean;
  server: ServerStatus | null;
  config: ConfigSummary | null;
  panel: Panel | null;
  onStart: () => void;
  onStop: () => void;
  onRefresh: () => void;
  onOpenConnections: () => void;
  onOpenProviders: () => void;
}) {
  const [wire, setWire] = useState<WireStatus | null>(null);
  const [desktop, setDesktop] = useState<ClaudeDesktopStatus | null>(null);
  const [wireBusy, setWireBusy] = useState(false);
  const [selectingProvider, setSelectingProvider] = useState(false);
  const [providerPickerOpen, setProviderPickerOpen] = useState(false);
  const [providerQuery, setProviderQuery] = useState("");
  const providerPickerRef = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);
  const providers = config?.providers ?? [];
  const active = providers.find((provider) => provider.active) ?? null;
  const filteredProviders = useMemo(() => {
    const query = providerQuery.trim().toLocaleLowerCase();
    if (!query) return providers;
    return providers.filter((provider) =>
      [provider.name, provider.id, provider.base_url, provider.api_format]
        .some((value) => value.toLocaleLowerCase().includes(query)),
    );
  }, [providerQuery, providers]);

  const refreshWire = useCallback(async () => {
    const [cliResult, desktopResult] = await Promise.allSettled([
      invoke<WireStatus>("wire_status"),
      invoke<ClaudeDesktopStatus>("claude_desktop_status"),
    ]);
    if (cliResult.status === "fulfilled") setWire(cliResult.value);
    if (desktopResult.status === "fulfilled") setDesktop(desktopResult.value);
    if (cliResult.status === "rejected" && desktopResult.status === "rejected") {
      setError(String(cliResult.reason));
    } else {
      setError(null);
    }
  }, []);

  useEffect(() => {
    void refreshWire();
  }, [refreshWire]);

  useEffect(() => {
    if (!providerPickerOpen) return;
    const closeOnOutsideClick = (event: PointerEvent) => {
      if (!providerPickerRef.current?.contains(event.target as Node)) {
        setProviderPickerOpen(false);
      }
    };
    document.addEventListener("pointerdown", closeOnOutsideClick);
    return () => document.removeEventListener("pointerdown", closeOnOutsideClick);
  }, [providerPickerOpen]);

  const selectProvider = async (id: string) => {
    if (id === active?.id) {
      setProviderPickerOpen(false);
      return;
    }
    setSelectingProvider(true);
    setError(null);
    try {
      await invoke<ConfigSummary>("select_provider", { id });
      setProviderPickerOpen(false);
      setProviderQuery("");
      onRefresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setSelectingProvider(false);
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

        <div className="flowline" aria-label="Claude Code CLI 或 Claude Desktop 经过 tern 网关，按路由规则转发到上游">
          <div className="flow-clients">
            <div className={`flow-client ${wire?.wired ? "is-connected" : ""}`}>
              <span className="flow-node-icon">⌘</span>
              <span><b>Claude Code CLI</b><small>{wire?.wired ? "已接入" : "未接入"}</small></span>
            </div>
            <div className={`flow-client ${desktop?.connected ? "is-connected" : ""}`}>
              <span className="flow-node-icon">▣</span>
              <span><b>Claude Desktop</b><small>{desktop?.connected ? "已接入 · Chat / Cowork / Code" : "未接入"}</small></span>
            </div>
          </div>
          <span className={`flow-link ${(wire?.wired || desktop?.connected) && running ? "is-active" : ""}`} aria-hidden />
          <div className={`flow-node flow-gateway ${running ? "is-connected" : ""}`}>
            <span className="flow-node-icon">t</span>
            <span><b>tern 网关</b><small>{server?.listen ?? "127.0.0.1:15800"}</small></span>
          </div>
          <span className={`flow-link ${running ? "is-active" : ""}`} aria-hidden />
          <div className={`flow-node ${active ? "is-connected" : ""}`}>
            <span className="flow-node-icon">↗</span>
            <span><b>{active?.name ?? "选择供应商"}</b><small>{active ? `默认兜底 · ${apiFormatLabel(active.api_format)}` : "尚未设置兜底"}</small></span>
          </div>
        </div>

        <div className="workbench-controls">
          <div className="workbench-provider">
            <span className="workbench-provider-label">默认兜底供应商</span>
            <div className="provider-picker" ref={providerPickerRef}>
              <button
                type="button"
                className="provider-picker-trigger"
                aria-haspopup="listbox"
                aria-expanded={providerPickerOpen}
                disabled={!providers.length || selectingProvider}
                onClick={() => {
                  setProviderPickerOpen((open) => !open);
                  setProviderQuery("");
                }}
              >
                <span className={`live-dot ${active ? "on" : "off"}`} aria-hidden />
                <span className="provider-picker-current">
                  <b>{selectingProvider ? "正在切换…" : active?.name ?? "尚未设置"}</b>
                  <small>{active ? `${endpointLabel(active.base_url)} · ${apiFormatLabel(active.api_format)}` : "选择一家作为未指定请求的去向"}</small>
                </span>
                <span className="provider-picker-chevron" aria-hidden>⌄</span>
              </button>
              {providerPickerOpen && (
                <div className="provider-picker-popover">
                  <input
                    autoFocus
                    type="search"
                    role="combobox"
                    aria-label="搜索供应商"
                    aria-expanded="true"
                    aria-controls="fallback-provider-options"
                    placeholder="搜索名称、地址或协议…"
                    value={providerQuery}
                    onChange={(event) => setProviderQuery(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Escape") setProviderPickerOpen(false);
                    }}
                  />
                  <div id="fallback-provider-options" className="provider-picker-options" role="listbox" aria-label="可选供应商">
                    {filteredProviders.map((provider) => (
                      <button
                        key={provider.id}
                        type="button"
                        role="option"
                        aria-selected={provider.id === active?.id}
                        className={`provider-picker-option ${provider.id === active?.id ? "is-selected" : ""}`}
                        onClick={() => void selectProvider(provider.id)}
                        disabled={selectingProvider}
                      >
                        <span className="provider-option-main">
                          <b>{provider.name}</b>
                          <small>{endpointLabel(provider.base_url)}</small>
                        </span>
                        <span className="provider-option-meta">
                          <span className={`provider-key-state key-${provider.key_state}`}>{keyStateLabel(provider.key_state)}</span>
                          <span className="provider-api-format">{apiFormatLabel(provider.api_format)}</span>
                        </span>
                      </button>
                    ))}
                    {filteredProviders.length === 0 && (
                      <p className="provider-picker-empty">没有找到匹配的供应商</p>
                    )}
                  </div>
                </div>
              )}
            </div>
            <div className="route-explainer">
              <span>只处理没有写供应商前缀的请求；模型由客户端请求决定。</span>
              <span>写成 <code>deepseek/模型名</code> 就固定走 DeepSeek，不受这里影响。<button className="route-inline-link" onClick={onOpenProviders}>了解供应商与路由</button></span>
            </div>
          </div>
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
              title="将 Claude Code CLI 的请求接入 tern"
            >
              {wireBusy ? "处理中…" : wire?.wired ? "断开 CLI" : "接入 CLI"}
            </button>
            <button className="btn ghost" onClick={onOpenConnections}>管理客户端接入</button>
          </div>
        </div>

        {!running && server?.last_error && <p className="workbench-warning">{server.last_error}</p>}
        {(wire?.wired || desktop?.connected) && !running && (
          <p className="workbench-warning">客户端已经接入，但网关已停止。启动网关后请求才能正常转发。</p>
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
        <span>{wire?.current_base_url ? `CLI 当前地址：${wire.current_base_url}` : "接入 CLI 或 Claude Desktop 后，客户端请求会经过上方这条链路。"}</span>
        <button className="btn ghost" onClick={onRefresh}>刷新状态</button>
      </div>
    </div>
  );
}
