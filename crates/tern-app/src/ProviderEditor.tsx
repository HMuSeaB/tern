import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { ConfigSummary } from "./types";

/**
 * 供应商的新增 / 编辑 / 删除。
 *
 * # 为什么是"每行两个按钮 + 一个弹层"，不是每行内联展开
 *
 * 编辑一个供应商要填 5 个字段（id、名字、地址、协议、key、倍率），内联展开会把
 * 38 行的列表撑得忽长忽短，而且和"获取模型列表"的展开打架。放到弹层里，
 * 列表的行高恒定，一次只编辑一个也说得通——同时改两个供应商本来就是罕见的。
 *
 * # key 永远是空输入框
 *
 * Rust 侧**不**回传 key（`provider_detail` 里没有这个字段）。所以框里永远是空的，
 * 占位写"留空表示不修改"。这不是偷懒：把凭据搬进渲染进程等于把它交给了 webview，
 * 拿不回来。代价是改 key 必须整段重填，比"改一个字"麻烦——这个代价值得付。
 *
 * 订阅登录的供应商不显示 key 和地址两栏：它们由 `effective_base_url()` 托管，
 * 填了也不生效。
 */

/** Rust 侧 provider_detail 的返回。与 providets.rs 的 ProviderDetail 对齐。 */
export interface ProviderDetail {
  id: string;
  name: string;
  base_url: string;
  api_format: string;
  auth_kind: string;
  key_state: string;
  web_tools_at_risk: boolean;
  is_default: boolean;
  cost_multiplier: string | null;
}

export interface ProbeReport {
  reachable: boolean;
  http_status: number | null;
  message: string;
  models: number;
  url: string;
  elapsed_ms: number;
}

const FORMATS: ReadonlyArray<{ id: string; label: string; hint: string }> = [
  { id: "anthropic", label: "Anthropic", hint: "/v1/messages，Claude Code 直连用这个" },
  { id: "openai_chat", label: "OpenAI Chat", hint: "/chat/completions，多数中转站" },
  { id: "openai_responses", label: "OpenAI Responses", hint: "/responses，Codex 用的" },
  { id: "gemini_native", label: "Gemini", hint: "Gemini 原生接口" },
];

const AUTH_LABEL: Record<string, string> = {
  none: "无需认证（本地服务）",
  api_key: "API Key",
  google_oauth: "Gemini OAuth",
  github_copilot: "GitHub Copilot 订阅",
  codex_oauth: "ChatGPT 订阅",
  xai_oauth: "xAI Grok 订阅",
};

/** 订阅登录的三种：地址托管、key 在网关手里，两个框都不给改 */
function isManaged(authKind: string): boolean {
  return (
    authKind === "github_copilot" ||
    authKind === "codex_oauth" ||
    authKind === "xai_oauth" ||
    authKind === "google_oauth"
  );
}

export function ProviderEditor({
  mode,
  providerId,
  /** null = 新建 */
  detail,
  busy,
  onClose,
  onSaved,
  onDeleted,
}: {
  mode: "create" | "edit";
  providerId?: string;
  /** 编辑时外部已经拉好的详情；没给就自己拉 */
  detail?: ProviderDetail | null;
  busy: boolean;
  onClose: () => void;
  /** Rust 返回的新配置摘要。父层直接 setState，省一次往返 */
  onSaved: (summary: ConfigSummary) => void;
  onDeleted: (summary: ConfigSummary) => void;
}) {
  const [form, setForm] = useState({
    id: "",
    name: "",
    base_url: "",
    api_format: "anthropic",
    api_key: "",
    cost_multiplier: "",
  });
  const [loaded, setLoaded] = useState(mode === "create");
  const [error, setError] = useState<string | null>(null);
  const [probe, setProbe] = useState<ProbeReport | null>(null);
  const [probing, setProbing] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const managed = isManaged(detail?.auth_kind ?? "");

  // 编辑时拉详情。新建没有可拉的，直接给空表单
  useEffect(() => {
    if (mode === "create") {
      setLoaded(true);
      return;
    }
    if (detail) {
      applyDetail(detail);
      setLoaded(true);
      return;
    }
    let alive = true;
    invoke<ProviderDetail>("provider_detail", { id: providerId })
      .then((result) => {
        if (!alive) return;
        applyDetail(result);
        setLoaded(true);
      })
      .catch((e) => {
        if (alive) setError(String(e));
      });
    return () => {
      alive = false;
    };
    // detail 只在父层主动更新时才该重跑；providerId / mode 变化才是真的切换
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [mode, providerId]);

  function applyDetail(d: ProviderDetail) {
    setForm({
      id: d.id,
      name: d.name,
      base_url: d.base_url,
      api_format: d.api_format,
      // key 留空：Rust 侧不回传。见本文件注释
      api_key: "",
      cost_multiplier: d.cost_multiplier ?? "",
    });
  }

  const save = useCallback(async () => {
    setError(null);
    try {
      const summary = await invoke<ConfigSummary>("provider_save", { draft: form });
      onSaved(summary);
    } catch (e) {
      setError(String(e));
    }
  }, [form, onSaved]);

  const runProbe = useCallback(async () => {
    if (mode !== "edit" || !providerId) return;
    setProbing(true);
    setProbe(null);
    setError(null);
    try {
      setProbe(await invoke<ProbeReport>("provider_probe", { id: providerId }));
    } catch (e) {
      setError(String(e));
    } finally {
      setProbing(false);
    }
  }, [mode, providerId]);

  const remove = useCallback(async () => {
    setError(null);
    try {
      // fallback_id 不传：Rust 侧会清空 default_provider。传一个"猜的下一个"
      // 会在用户没同意的情况下悄悄换掉他正在用的供应商——那比空着更难发现
      const summary = await invoke<ConfigSummary>("provider_remove", {
        id: form.id,
        fallbackId: null,
      });
      onDeleted(summary);
    } catch (e) {
      setError(String(e));
      setConfirmDelete(false);
    }
  }, [form.id, onDeleted]);

  if (!loaded) {
    return (
      <div className="prov-modal-back" onClick={onClose}>
        <div className="prov-modal" role="dialog" onClick={(e) => e.stopPropagation()}>
          <p className="empty-text">正在读取…</p>
        </div>
      </div>
    );
  }

  return (
    <div className="prov-modal-back" onClick={onClose}>
      <div
        className="prov-modal"
        role="dialog"
        aria-label={mode === "create" ? "新增供应商" : `编辑「${form.name}」`}
        onClick={(e) => e.stopPropagation()}
      >
        <h3>{mode === "create" ? "新增供应商" : `编辑「${form.name || form.id}」`}</h3>

        <div className="prov-form">
          <label className="prov-field">
            <span className="prov-label">
              id
              <span className="prov-label-dim">模型名前缀，不能改</span>
            </span>
            <input
              className="prov-modal-input"
              value={form.id}
              // 编辑时钉死：id 是路由前缀，改了等于换了一个供应商
              readOnly={mode === "edit"}
              placeholder="deepseek"
              onChange={(e) => setForm({ ...form, id: e.target.value })}
            />
          </label>

          <label className="prov-field">
            <span className="prov-label">显示名</span>
            <input
              className="prov-modal-input"
              value={form.name}
              placeholder="DeepSeek 官方"
              onChange={(e) => setForm({ ...form, name: e.target.value })}
            />
          </label>

          {!managed && (
            <label className="prov-field">
              <span className="prov-label">
                地址
                <span className="prov-label-dim">base URL</span>
              </span>
              <input
                className="prov-modal-input"
                value={form.base_url}
                placeholder="https://api.deepseek.com/anthropic"
                onChange={(e) => setForm({ ...form, base_url: e.target.value })}
              />
            </label>
          )}

          <label className="prov-field">
            <span className="prov-label">协议</span>
            <select
              className="prov-modal-input"
              value={form.api_format}
              onChange={(e) => setForm({ ...form, api_format: e.target.value })}
            >
              {FORMATS.map((f) => (
                <option key={f.id} value={f.id}>
                  {f.label}
                </option>
              ))}
            </select>
            <span className="prov-hint">
              {FORMATS.find((f) => f.id === form.api_format)?.hint}
            </span>
          </label>

          {!managed && (
            <label className="prov-field">
              <span className="prov-label">
                API Key
                {mode === "edit" && <span className="prov-label-dim">留空 = 不修改</span>}
              </span>
              <input
                className="prov-modal-input"
                type="password"
                value={form.api_key}
                placeholder={mode === "edit" ? "留空表示保留原来的 key" : "sk-..."}
                autoComplete="new-password"
                onChange={(e) => setForm({ ...form, api_key: e.target.value })}
              />
              {mode === "edit" && (
                <span className="prov-hint">
                  出于安全不回显原 key：要换就整段重填。
                </span>
              )}
            </label>
          )}

          {managed && detail && (
            <p className="prov-hint">
              认证方式：{AUTH_LABEL[detail.auth_kind] ?? detail.auth_kind}。
              地址和 key 由订阅托管，这里改不了。
            </p>
          )}

          <label className="prov-field">
            <span className="prov-label">
              成本倍率
              <span className="prov-label-dim">可空</span>
            </span>
            <input
              className="prov-modal-input"
              value={form.cost_multiplier}
              placeholder="0.3"
              onChange={(e) => setForm({ ...form, cost_multiplier: e.target.value })}
            />
            <span className="prov-hint">
              中转站的折扣倍率，用于用量计价。填 0.3 表示实际花费是标价的 3 折。
            </span>
          </label>
        </div>

        {probe && (
          <div className={`notice ${probe.reachable ? "ok" : "err"}`}>
            <span className="notice-ic">{probe.reachable ? "✓" : "!"}</span>
            <span className="notice-body">
              <b>{probe.message}</b>
              <code className="notice-cmd">
                {probe.http_status ? `HTTP ${probe.http_status}` : "无响应"}
                {probe.models > 0 && ` · ${probe.models} 个模型`} · {probe.elapsed_ms}ms
              </code>
              <code className="notice-cmd" style={{ wordBreak: "break-all" }}>
                {probe.url}
              </code>
            </span>
          </div>
        )}

        {error && (
          <div className="notice err">
            <span className="notice-ic">!</span>
            <span className="notice-body">{error}</span>
          </div>
        )}

        <div className="prov-modal-actions">
          {mode === "edit" && !confirmDelete && (
            <button
              className="btn ghost danger-text"
              onClick={() => setConfirmDelete(true)}
              disabled={busy}
              title="从配置里删掉这个供应商"
            >
              删除
            </button>
          )}
          {confirmDelete && (
            <>
              {/* 删的是 default 时要说清楚后果：不带前缀的模型名会找不到路 */}
              <span className="prov-confirm">
                {detail?.is_default ? "它正在被使用，删掉后默认路由会空出来。" : "确定删除？"}
              </span>
              <button className="btn ghost danger-text" onClick={() => void remove()} disabled={busy}>
                确认删除
              </button>
              <button className="btn ghost" onClick={() => setConfirmDelete(false)} disabled={busy}>
                取消
              </button>
            </>
          )}
          <div className="spacer" />
          {mode === "edit" && (
            <button className="btn ghost" onClick={() => void runProbe()} disabled={probing || busy}>
              {probing ? "正在探测…" : "测连通性"}
            </button>
          )}
          <button className="btn ghost" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button className="btn primary" onClick={() => void save()} disabled={busy}>
            {busy ? "保存中…" : mode === "create" ? "创建" : "保存"}
          </button>
        </div>
      </div>
    </div>
  );
}
