import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ServerStatus, ConfigSummary } from "./types";

/** 从 Rust 侧拿到的东西：配置摘要 + 网关运行状态 */
export interface AppBootstrap {
  firstRun: boolean;
  config: ConfigSummary | null;
  server: ServerStatus | null;
}

export function useTern() {
  const [boot, setBoot] = useState<AppBootstrap | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [firstRun, config, server] = await Promise.all([
        invoke<boolean>("first_run"),
        invoke<ConfigSummary>("config_summary").catch(() => null),
        invoke<ServerStatus>("server_status").catch(() => null),
      ]);
      setBoot({ firstRun, config, server });
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const start = useCallback(async () => {
    setBusy(true);
    try {
      await invoke<ServerStatus>("server_start");
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [refresh]);

  const stop = useCallback(async () => {
    setBusy(true);
    try {
      await invoke<ServerStatus>("server_stop");
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [refresh]);

  const importFromCcSwitch = useCallback(async () => {
    setBusy(true);
    try {
      const result = await invoke<CcSwitchPreview>("import_from_cc_switch");
      await refresh();
      return result;
    } catch (e) {
      setError(String(e));
      return null;
    } finally {
      setBusy(false);
    }
  }, [refresh]);

  const writeSample = useCallback(async () => {
    setBusy(true);
    try {
      await invoke<string>("write_sample_config");
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [refresh]);

  const openConfigDir = useCallback(async () => {
    try {
      await invoke<string>("open_config_dir");
    } catch (e) {
      setError(String(e));
    }
  }, []);

  return { boot, error, busy, refresh, start, stop, importFromCcSwitch, writeSample, openConfigDir };
}

/** 导入预览。found=false 表示本机没装 cc-switch */
export interface CcSwitchPreview {
  db_path: string;
  found: boolean;
  providers: {
    id: string;
    name: string;
    base_url: string;
    api_format: string;
  }[];
  skipped: string[];
  third_party_count: number;
}