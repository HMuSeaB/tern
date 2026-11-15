import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Panel } from "./types";

/** 命令名与 Rust 侧 #[tauri::command] 一致 */
const CMD_PANEL = "panel_summary";

export type LoadState =
  | { kind: "loading" }
  | { kind: "ready"; panel: Panel }
  | { kind: "error"; message: string };

export function usePanel(pollMs = 15_000) {
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  // 组件卸载后不再 setState，避免 React 警告
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    try {
      const panel = await invoke<Panel>(CMD_PANEL);
      if (alive.current) setState({ kind: "ready", panel });
    } catch (error) {
      if (alive.current) {
        setState({ kind: "error", message: String(error) });
      }
    }
  }, []);

  useEffect(() => {
    void refresh();
    // 面板是观察窗口，轮询而不是推送：网关才是数据源，实时性要求不高
    const timer = window.setInterval(() => {
      // 窗口隐藏时(托盘常驻时关窗只是 hide)不查库:没人在看,
      // 每 15 秒开一次数据库只是白耗。切回可见时由下面的监听立即补拉
      if (document.visibilityState === "visible") void refresh();
    }, pollMs);
    const onVisible = () => {
      if (document.visibilityState === "visible") void refresh();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, [refresh, pollMs]);

  return { state, refresh };
}
