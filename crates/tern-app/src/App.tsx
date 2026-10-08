import { lazy, Suspense, useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { PanelView } from "./PanelView";
import { Permissions } from "./Permissions";
import { Providers } from "./Providers";
import { TabPage, TabShell, type TabDef } from "./Tabs";
import { usePanel } from "./usePanel";
import { useTern, type CcSwitchPreview } from "./useTern";
import { Welcome } from "./Welcome";
import { Wire } from "./Wire";
import type { ConfigSummary, ServerStatus } from "./types";

/** 二级视图按需加载。
 *
 *  recharts 一家就把包从 268 KB 顶到 677 KB，而它只在用户展开"趋势与占比"时
 *  才用得上——多数人打开面板只是想看一眼今天花了多少。拆成独立 chunk 之后
 *  首屏不用为它付下载和解析的钱。 */
const PanelInsights = lazy(() =>
  import("./PanelInsights").then((m) => ({ default: m.PanelInsights })),
);

type Theme = "light" | "dark";
const THEME_KEY = "tern-theme";
/** tab 的选择只在本次进程里有效：退出重开回到默认页比记住第 3 页更符合预期 */
const TAB_KEY = "tern-tab";

/** 各 tab 的 id。固定字符串，TabShell 只按它找当前页 */
const TABS = ["providers", "usage", "wire", "permissions"] as const;
type TabId = (typeof TABS)[number];

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
        <Tabs
          running={boot.server?.running ?? false}
          error={error}
          onRetry={refresh}
          panel={panel}
          warnings={boot.config?.warnings ?? []}
          providerCount={boot.config?.providers.length ?? 0}
          config={boot.config}
          server={boot.server}
          onRefresh={refresh}
        />
      )}
    </Shell>
  );
}

/**
 * 四块内容套一层 tab。
 *
 * 页序就是频率序：供应商（切它是最高频的操作）→ 用量（顺便看看）→ 接入（一次性设置）
 * → 权限（一次性设置）。所以供应商是默认页，两个设置页收了进去。
 *
 * 四块还是原封不动的组件，`running` / `onRefresh` 照旧透传——tab 只管切换，
 * 不掺合业务。原来上下堆叠时"网关没跑就显示 IdleState"的逻辑搬到了「用量」页里：
 * 那一页本来就是网关的下游，别的地方不该因为网关停着就变灰。
 */
function Tabs({
  running,
  error,
  onRetry,
  panel,
  warnings,
  providerCount,
  config,
  server,
  onRefresh,
}: {
  running: boolean;
  error: string | null;
  onRetry: () => void;
  panel: ReturnType<typeof usePanel>;
  warnings: string[];
  providerCount: number;
  config: ConfigSummary | null;
  server: ServerStatus | null;
  onRefresh: () => void;
}) {
  const [active, setActive] = useState<TabId>(() => {
    // 记住上次停在哪一页。窗口多半是被切到别处找东西，重开却回到第一页要多点一下
    const saved = localStorage.getItem(TAB_KEY);
    return TABS.includes(saved as TabId) ? (saved as TabId) : "providers";
  });

  /** 「用量」页里"趋势与占比"的展开状态。
   *
   *  默认收起不是因为它不重要，而是首屏的 hero + 小卡 + 提示是 ROADMAP
   *  阶段 6 定下的密度。二级视图要拉四次聚合查询，默认展开会让每次打开面板
   *  都先等四个请求——而多数人只是想看一眼今天花了多少。 */
  const [showInsights, setShowInsights] = useState(false);

  useEffect(() => {
    localStorage.setItem(TAB_KEY, active);
  }, [active]);

  // tab 栏上的提示点：把"被别的页挡着的提醒"提到台面上。
  // 只在有明确可指的事实时才亮——三个都亮等于都没亮
  const activeKeyBroken =
    config?.providers.some(
      (p) => p.active && p.key_state !== "real" && p.key_state !== "subscription",
    ) ?? false;
  const panelReady = panel.state.kind === "ready" ? panel.state.panel : null;
  const usageAlerts =
    panelReady !== null && (panelReady.today.unpriced > 0 || panelReady.failures.length > 0);

  const tabs: TabDef[] = [
    { id: "providers", label: "供应商", alert: activeKeyBroken || error !== null },
    { id: "usage", label: "用量", alert: usageAlerts },
    { id: "wire", label: "接入" },
    { id: "permissions", label: "权限" },
  ];

  return (
    <>
      {/* 错误条放在 tab 栏上面而不是某一页里：它多半来自 server_start 这种
          全局动作，塞进「供应商」页的话，在别的页点启动然后看不到原因 */}
      {error && <ErrorBar message={error} onRetry={onRetry} />}
      <TabShell tabs={tabs} active={active} onSelect={(id) => setActive(id as TabId)}>
        <TabPage id="providers" active={active === "providers"}>
          <Providers running={running} onRefresh={onRefresh} />
        </TabPage>

        <TabPage id="usage" active={active === "usage"}>
          {!running ? (
            // 网关没跑就地显示"未运行"，但**连着 IdleState 自己的修法指引**——
            // 它本来就分得清"没配供应商"和"配了但停了"两种情况，
            // 拼一个假 server 只会把 listen 地址和 agent 状态弄丢
            <IdleState server={server} warnings={warnings} providerCount={providerCount} />
          ) : panel.state.kind === "ready" ? (
            <>
              {/* 首屏（hero + 小卡 + 提示）永远在最上面。ROADMAP 阶段 6 定的：
                  "最常用的操作"先看到，二级视图要用户主动展开 */}
              <PanelView panel={panel.state.panel} onRefresh={panel.refresh} />
              <section className="card">
                <button
                  className="disclosure"
                  onClick={() => setShowInsights((v) => !v)}
                  aria-expanded={showInsights}
                >
                  <span className={`chevron ${showInsights ? "open" : ""}`}>▸</span>
                  趋势与占比
                  <span className="disclosure-count">花在哪 / 模型流向 / 会话</span>
                </button>
                {showInsights && (
                  <Suspense
                    fallback={
                      <p className="empty-text" style={{ padding: "18px 0" }}>
                        正在加载图表…
                      </p>
                    }
                  >
                    <PanelInsights />
                  </Suspense>
                )}
              </section>
            </>
          ) : panel.state.kind === "error" ? (
            /* 跑着却读不到库：和"网关停了"是两回事，说反了会让人白点启动 */
            <div className="notice err">
              <span className="notice-ic">!</span>
              <span className="notice-body">
                <b>读不到用量数据库。</b>
                {panel.state.message}
              </span>
              <button className="btn ghost" onClick={panel.refresh}>
                重试
              </button>
            </div>
          ) : (
            <div className="card empty">
              <div className="empty-title">正在读取…</div>
            </div>
          )}
        </TabPage>

        {/* 接线是网关的下游：得知道在不在跑、监听哪个口 */}
        <TabPage id="wire" active={active === "wire"}>
          <Wire running={running} onChanged={panel.refresh} />
        </TabPage>

        {/* 权限与网关无关，任何时候都该能点——包括网关还没启动时 */}
        <TabPage id="permissions" active={active === "permissions"}>
          <Permissions />
        </TabPage>
      </TabShell>
    </>
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
  server: { running: boolean; listen: string | null; last_error: string | null; agent_up: boolean } | null;
  warnings: string[];
  providerCount: number;
}) {
  return (
    <div className="card empty">
      <div className="empty-title">网关未运行</div>
      {/* agent 不在是另一回事：不是"没启动"，是常驻进程没跟着装上。
          两种原因的修法完全不同，混成一句话用户会白点半天「启动」 */}
      {server && !server.agent_up && (
        <p className="empty-text warn-text">
          没找到常驻进程 tern-agent.exe。它和面板应当装在同一个目录；
          重新安装通常能修好。
        </p>
      )}
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
        。关掉这个窗口网关照跑，用量继续记。
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
