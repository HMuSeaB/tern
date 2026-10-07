import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ConfigSummary } from "./types";

/**
 * 供应商列表：选一个当默认、看它的地址和状态。
 *
 * 为什么非有不可：这个产品最常用的操作就是切供应商（cc-switch 的核心
 * 能力就是这个），而面板此前只有用量页——38 个供应商导进来了却没得选，
 * 默认那个还是把失效 key。用量看得再细也补偿不了"换不了供应商"。
 *
 * 切一个立刻生效：路由表里 `default_provider` 换掉就行，不用重启网关。
 * 所以这里不做"保存"按钮——每次点完即时生效，少一个会骗人的中间态。
 */
export function Providers({
  running,
  onRefresh,
}: {
  /** 网关在不在跑。没跑时也允许切换（配置先改好，启动即生效），但要提示 */
  running: boolean;
  onRefresh?: () => void;
}) {
  const [config, setConfig] = useState<ConfigSummary | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState<string | null>(null);
  const [query, setQuery] = useState("");

  const reload = useCallback(async () => {
    try {
      setConfig(await invoke<ConfigSummary>("config_summary"));
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  const select = useCallback(
    async (id: string) => {
      setPending(id);
      try {
        setConfig(await invoke<ConfigSummary>("select_provider", { id }));
        setError(null);
        // useTern 的 boot 里带着 config，切换后要重新拉一次，
        // 否则上面那句"已配置 N 个供应商"和警告列表都是旧状态
        onRefresh?.();
      } catch (e) {
        setError(String(e));
      } finally {
        setPending(null);
      }
    },
    [onRefresh],
  );

  // 38 个起就该有搜索。按名字和地址都匹配——用户记得住"deepseek"，
  // 也记得住"moonshot"，但记不住那串 uuid
  const visible = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q || !config) return config?.providers ?? [];
    return config.providers.filter(
      (p) =>
        p.name.toLowerCase().includes(q) ||
        p.id.toLowerCase().includes(q) ||
        p.base_url.toLowerCase().includes(q),
    );
  }, [config, query]);

  if (!config) {
    return (
      <div className="card">
        <div className="head"><span className="title">供应商</span></div>
        <p className="empty-text">正在读取…</p>
      </div>
    );
  }

  const active = config.providers.find((p) => p.active);
  // 正在用的那个 key 是坏的：这是最该说的一句话，比用量数字更要紧
  const activeKeyBroken = active && active.key_state !== "real" && active.key_state !== "subscription";

  return (
    <div className="card">
      <div className="head">
        <span className="title">供应商</span>
        <span className="act">
          {config.providers.length} 个
          {active ? ` · 当前 ${active.name}` : " · 未设置默认"}
        </span>
      </div>

      {config.default_provider === null && (
        <p className="perm-hint">
          还没有默认供应商。点下面任意一个，之后不带前缀的模型名都会走它。
        </p>
      )}

      {activeKeyBroken && (
        <div className="notice err" style={{ marginBottom: 14 }}>
          <span className="notice-ic">!</span>
          <span className="notice-body">
            <b>正在用的「{active.name}」key 不能用了。</b>
            请求会全部失败。请在下面换一个——列表里 key 正常的旁边没有红字。
          </span>
        </div>
      )}

      {config.providers.length > 8 && (
        <input
          className="prov-search"
          type="search"
          value={query}
          placeholder={`搜索 ${config.providers.length} 个供应商（名称或地址）`}
          onChange={(e) => setQuery(e.target.value)}
        />
      )}

      {!running && (
        <p className="perm-foot" style={{ marginTop: 0 }}>
          网关没在跑，现在切换会在下次启动时生效。
        </p>
      )}

      <ul className="prov-list">
        {visible.map((p) => (
          <li key={p.id}>
            <button
              className={`prov-row ${p.active ? "is-active" : ""}`}
              onClick={() => void select(p.id)}
              disabled={pending !== null}
              title={`切换到 ${p.name}`}
            >
              <span className={`prov-radio ${p.active ? "on" : ""}`} aria-hidden />
              <span className="prov-main">
                <span className="prov-name">
                  {p.name}
                  {p.active && <span className="prov-tag">使用中</span>}
                </span>
                <span className="prov-url" title={p.base_url}>{p.base_url}</span>
              </span>
              <span className="prov-side">
                <code className="prov-fmt">{p.api_format}</code>
                {p.web_tools_at_risk && (
                  <span className="prov-risk" title="第三方网关：Claude Code 的联网搜索会失效">
                    联网受限
                  </span>
                )}
                {p.key_state === "placeholder" && <span className="prov-bad">占位符</span>}
                {p.key_state === "empty" && <span className="prov-bad">key 为空</span>}
              </span>
            </button>
          </li>
        ))}
        {visible.length === 0 && (
          <li className="prov-empty">没有匹配「{query}」的供应商</li>
        )}
      </ul>

      {error && (
        <div className="notice err" style={{ marginTop: 14 }}>
          <span className="notice-ic">!</span>
          <span className="notice-body">{error}</span>
          <button className="btn ghost" onClick={() => void reload()}>重试</button>
        </div>
      )}

      <p className="perm-foot">
        切换立即生效，不用重启网关。带前缀的模型名（<code>deepseek/xxx</code>）不受影响，
        它们总是按前缀走。
      </p>
    </div>
  );
}
