import { useEffect, useState } from "react";
import { PanelView } from "./PanelView";
import { usePanel } from "./usePanel";

type Theme = "light" | "dark";
const THEME_KEY = "tern-theme";

export default function App() {
  const { state, refresh } = usePanel();
  const [theme, setTheme] = useState<Theme>(initialTheme);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    localStorage.setItem(THEME_KEY, theme);
  }, [theme]);

  return (
    <div className="app">
      <header className="topbar">
        <div className="brand">
          <span className="brand-mark">t</span>
          <span className="brand-name">tern</span>
        </div>
        <span className="brand-sub">用量面板</span>
        <div className="spacer" />
        <button
          className="btn icon"
          onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
          title={theme === "dark" ? "切换到浅色" : "切换到深色"}
        >
          {theme === "dark" ? "☀" : "☾"}
        </button>
      </header>

      <main className="content">
        {state.kind === "loading" && <Loading />}
        {state.kind === "error" && <ErrorState message={state.message} onRetry={refresh} />}
        {state.kind === "ready" && <PanelView panel={state.panel} onRefresh={refresh} />}
      </main>
    </div>
  );
}

function Loading() {
  return (
    <div className="card empty">
      <div className="empty-title">正在读取用量…</div>
    </div>
  );
}

function ErrorState({ message, onRetry }: { message: string; onRetry: () => void }) {
  return (
    <div className="card empty">
      <div className="empty-title">读不到用量数据库</div>
      <p className="empty-text">{message}</p>
      <p className="empty-text dim">
        先用 <code>tern serve</code> 跑一次网关；数据在别处可以用环境变量{" "}
        <code>TERN_DB</code> 指过去。
      </p>
      <button className="btn" onClick={onRetry}>
        重试
      </button>
    </div>
  );
}

function initialTheme(): Theme {
  const saved = localStorage.getItem(THEME_KEY);
  if (saved === "light" || saved === "dark") return saved;
  // 没手动选过就跟系统
  return window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}
