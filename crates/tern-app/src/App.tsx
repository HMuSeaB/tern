import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { PanelView } from "./PanelView";
import { Permissions } from "./Permissions";
import { usePanel } from "./usePanel";
import { useTern, type CcSwitchPreview } from "./useTern";
import { Welcome } from "./Welcome";

type Theme = "light" | "dark";
const THEME_KEY = "tern-theme";

export default function App() {
  const { boot, error, busy, refresh, start, stop, importFromCcSwitch, writeSample, openConfigDir } =
    useTern();
  const panel = usePanel();
  const [theme, setTheme] = useState<Theme>(initialTheme);
  const [preview, setPreview] = useState<CcSwitchPreview | null>(null);
  const [imported, setImported] = useState(false);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem(THEME_KEY, theme);
  }, [theme]);

  // 探测一次 cc-switch，供欢迎页展示"能导入什么"。
  // 纯只读的不落盘，确认后才调 import
  useEffect(() => {
    if (!boot?.firstRun) return;
    let alive = true;
    invoke<CcSwitchPreview>("import_preview")
      .then((result) => alive && setPreview(result))
      .catch(() => alive && setPreview(null));
    return () => {
      alive = false;
    };
  }, [boot?.firstRun]);

  const onImport = useCallback(async () => {
    const result = await importFromCcSwitch();
    if (result) {
      setImported(true);
      setPreview(null);
      // 导入后配置变了，网关若在跑要重启才吃得到新供应商
      panel.refresh();
    }
  }, [importFromCcSwitch, panel]);

  if (!boot) {
    return (
      <Shell theme={theme} setTheme={setTheme}>
        <div className="card empty">
          <div className="empty-title">正在启动…</div>
        </div>
      </Shell>
    );
  }

  return (
    <Shell theme={theme} setTheme={setTheme} server={boot.server} onStart={start} onStop={stop} busy={busy}>
      {(boot.firstRun && !imported) ? (
        <Welcome
          preview={preview}
          onImport={onImport}
          onStartFresh={writeSample}
          onOpenConfigDir={openConfigDir}
          busy={busy}
        />
      ) : (
        <>
          {error && <ErrorBar message={error} onRetry={refresh} />}
          {boot.server?.running && panel.state.kind === "ready" ? (
            <PanelView panel={panel.state.panel} onRefresh={panel.refresh} />
          ) : (
            <IdleState
              server={boot.server}
              warnings={boot.config?.warnings ?? []}
              providerCount={boot.config?.providers.length ?? 0}
            />
          )}
          {/* 权限与网关无关，任何时候都该能点——包括网关还没启动时 */}
          <Permissions />
        </>
      )}
    </Shell>
  );
}

function Shell({
  children,
  theme,
  setTheme,
  server,
  onStart,
  onStop,
  busy,
}: {
  children: React.ReactNode;
  theme: Theme;
  setTheme: (t: Theme) => void;
  server?: { running: boolean; listen: string | null } | null;
  onStart?: () => void;
  onStop?: () => void;
  busy?: boolean;
}) {
  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark">t</span>
          <span className="brand-name">tern</span>
        </div>
        {server && onStart && onStop && (
          <div className="server-toggle">
            <span className={`live-dot ${server.running ? "on" : "off"}`} />
            <span className="server-state">{server.running ? "网关运行中" : "网关已停止"}</span>
            {server.listen && <code className="server-addr">{server.listen}</code>}
            <button
              className={`btn ${server.running ? "" : "primary"}`}
              onClick={server.running ? onStop : onStart}
              disabled={busy}
            >
              {server.running ? "停止" : "启动"}
            </button>
          </div>
        )}
        <div className="spacer" />
        <button
          className="btn icon"
          onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
          title={theme === "dark" ? "切换到浅色" : "切换到深色"}
        >
          {theme === "dark" ? "☀" : "☾"}
        </button>
      </header>
      <main className="content">{children}</main>
    </div>
  );
}

/** 网关没跑时的占位。配了供应商就催启动，没配就引导去导入。 */
function IdleState({
  server,
  warnings,
  providerCount,
}: {
  server: { running: boolean; listen: string | null; last_error: string | null } | null;
  warnings: string[];
  providerCount: number;
}) {
  return (
    <div className="card empty">
      <div className="empty-title">网关未运行</div>
      {server?.last_error && <p className="empty-text warn-text">{server.last_error}</p>}
      <p className="empty-text">
        {providerCount > 0
          ? `已配置 ${providerCount} 个供应商。点右上角「启动」开始转发请求并记录用量。`
          : "还没有配置供应商。"}
      </p>
      {warnings.length > 0 && (
        <ul className="warn-list">
          {warnings.map((w, i) => (
            <li key={i}>{w}</li>
          ))}
        </ul>
      )}
      <p className="empty-text dim">
        启动后把 Claude Code 指过来即可：
        <code>ANTHROPIC_BASE_URL</code> 填 <code>{server?.listen ?? "127.0.0.1:15800"}</code>
      </p>
    </div>
  );
}

function ErrorBar({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <div className="notice err" style={{ marginBottom: 14 }}>
      <span className="notice-ic">!</span>
      <span className="notice-body">{message}</span>
      <button className="btn ghost" onClick={onRetry}>重试</button>
    </div>
  );
}

function initialTheme(): Theme {
  const saved = localStorage.getItem(THEME_KEY);
  if (saved === "light" || saved === "dark") return saved;
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}
