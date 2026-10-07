import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ConfigSummary } from "./types";

/**
 * 供应商列表：选一个当默认、看它的地址和状态，顺手拉一份模型列表。
 *
 * 三个功能，前两个是"必须"，第三个是"省得你去猜"：
 *
 * 1. **看有哪些**：38 个全列出，搜索框按名称/地址/id 都匹配——
 *    用户记得住 deepseek，记不住那串 uuid
 * 2. **换**：点一下切默认，即时生效（路由表换 default_provider，
 *    不用重启网关）。正在用的那个 key 坏了要直接说，否则用户
 *    只会看到"请求失败"，不知道是选错了供应商
 * 3. **拉模型列表**：中转站的模型名千奇百怪（step-3.5-flash-2603、
 *    kimi-k2.5），猜错只得到一句 404。上游有个 OpenAI 兼容的
 *    GET /v1/models，问它就行
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
  /** 展开模型列表的那个供应商 id。一次只展开一个：38 个全展开会把页面撑爆 */
  const [expanded, setExpanded] = useState<string | null>(null);

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
        // 否则"已配置 N 个供应商"和警告列表都是旧状态
        onRefresh?.();
      } catch (e) {
        setError(String(e));
      } finally {
        setPending(null);
      }
    },
    [onRefresh],
  );

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
  const activeKeyBroken =
    active && active.key_state !== "real" && active.key_state !== "subscription";

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
            请求会全部失败。请在下面换一个——key 正常的旁边没有红字。
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
            <div className={`prov-row ${p.active ? "is-active" : ""}`}>
              <button
                className="prov-pick"
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
              </button>
              <span className="prov-side">
                <code className="prov-fmt">{p.api_format}</code>
                {p.web_tools_at_risk && (
                  <span className="prov-risk" title="第三方网关：Claude Code 的联网搜索会失效">
                    联网受限
                  </span>
                )}
                {p.key_state === "placeholder" && <span className="prov-bad">占位符</span>}
                {p.key_state === "empty" && <span className="prov-bad">key 为空</span>}
                {/* 拉模型列表：key 是坏的就没必要问上游了，先让它醒目标出来 */}
                {(p.key_state === "real" || p.key_state === "subscription") && (
                  <button
                    className="prov-models-btn"
                    onClick={() => setExpanded(expanded === p.id ? null : p.id)}
                    title="获取这个供应商的模型列表"
                  >
                    {expanded === p.id ? "收起模型" : "获取模型列表"}
                  </button>
                )}
              </span>
            </div>
            {expanded === p.id && <ModelList providerId={p.id} running={running} />}
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

/** 一个供应商的模型列表。点「获取模型列表」才拉，不预取——38 个全拉一遍
 *  会把上游打烦，而且大部分 key 是坏的，拉回来一堆错误信息没意义。 */
function ModelList({ providerId, running }: { providerId: string; running: boolean }) {
  const [models, setModels] = useState<string[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const list = await invoke<string[]>("fetch_provider_models", { id: providerId });
      setModels(list);
      setError(null);
    } catch (e) {
      setModels(null);
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [providerId]);

  useEffect(() => {
    void load();
  }, [load]);

  if (busy && !models) {
    return <p className="prov-models">正在向上游询问…</p>;
  }

  if (error) {
    return (
      <div className="prov-models is-err">
        <span>{error}</span>
        <button className="btn ghost" onClick={() => void load()}>重试</button>
      </div>
    );
  }

  if (!models) return null;

  if (models.length === 0) {
    return <p className="prov-models">上游返回了空列表。</p>;
  }

  return (
    <div className="prov-models">
      <div className="prov-models-head">
        <span>{models.length} 个模型</span>
        <span className="dim">复制名字填进 Claude Code 的 model 里即可</span>
      </div>
      <ul className="prov-models-grid">
        {models.map((m) => (
          <li key={m}>
            <code>{m}</code>
          </li>
        ))}
      </ul>
      {!running && (
        <p className="dim" style={{ marginTop: 8, fontSize: 12 }}>
          网关没在跑，现在只能看模型名，要真发请求得先启动。
        </p>
      )}
    </div>
  );
}
