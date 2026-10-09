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
  /** 这家的模型档位映射,按 Claude Code 的档位层级排好序 */
  model_env: ModelEnvEntry[];
}

/** 一个模型档位。key 是 env 键名,value 是模型名 */
export interface ModelEnvEntry {
  key: string;
  value: string;
}

export interface ProbeReport {
  reachable: boolean;
  http_status: number | null;
  /** 实际探测用的模型名。上游说 "model does not exist" 时要显示它 */
  model: string;
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

/** env 键名 → 人话。`ANTHROPIC_DEFAULT_OPUS_MODEL` 对用户没有意义,
 *  而且它比 `ANTHROPIC_MODEL` 长得多,原样显示会把输入框挤没。
 *  认不出的键原样显示(cc-switch 的键集会变)。 */
const TIER_LABEL: Record<string, string> = {
  ANTHROPIC_MODEL: "主模型",
  ANTHROPIC_DEFAULT_OPUS_MODEL: "Opus 档",
  ANTHROPIC_DEFAULT_SONNET_MODEL: "Sonnet 档",
  ANTHROPIC_DEFAULT_HAIKU_MODEL: "Haiku 档",
  ANTHROPIC_DEFAULT_FABLE_MODEL: "Fable 档",
  ANTHROPIC_REASONING_MODEL: "推理档",
  CLAUDE_CODE_SUBAGENT_MODEL: "子代理",
  CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS: "Agent Teams",
};

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

  /** 这家的模型档位映射。独立于 form：它有增删逻辑,
   *  塞进 form 会让改一个模型名就重建整个表单对象 */
  const [modelEnv, setModelEnv] = useState<ModelEnvEntry[]>([]);

  // ---- 模型选择 ----
  const [models, setModels] = useState<string[]>([]);
  const [modelLoading, setModelLoading] = useState(false);
  const [modelError, setModelError] = useState<string | null>(null);
  /** 下拉里选中的那个。空串 = 没选 */
  const [model, setModel] = useState("");
  /** Claude Code 现在实际在用的模型名。用来在下拉旁说明"会改成什么" */
  const [currentModel, setCurrentModel] = useState<string | null>(null);

  const managed = isManaged(detail?.auth_kind ?? "");

  // 进来时读一次 Claude Code 当前在用的模型。弹层不是常驻的，
  // 每次打开重读才对——用户可能刚在 Settings 里改过
  useEffect(() => {
    let alive = true;
    invoke<string | null>("claude_model")
      .then((value) => alive && setCurrentModel(value))
      .catch(() => alive && setCurrentModel(null));
    return () => {
      alive = false;
    };
  }, []);

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
    setModelEnv(d.model_env ?? []);
  }

  const save = useCallback(async () => {
    setError(null);
    try {
      const summary = await invoke<ConfigSummary>("provider_save", {
        draft: { ...form, model_env: modelEnv },
      });
      onSaved(summary);
    } catch (e) {
      setError(String(e));
    }
  }, [form, modelEnv, onSaved]);

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

  /** 向上游问它有哪些模型。复用 providers 列表行的那个命令——
   *  同一个上游同一个问题，答案必须一样，所以不另写一个。 */
  const loadModels = useCallback(async () => {
    if (mode !== "edit" || !providerId) return;
    setModelLoading(true);
    setModelError(null);
    try {
      const list = await invoke<string[]>("fetch_provider_models", { id: providerId });
      setModels(list);
      // 拉到了但里面已经有当前在用的那个，就选中它——用户多半是想改成同一家
      // 的另一个模型，而不是从零挑
      if (currentModel && list.includes(currentModel)) setModel(currentModel);
    } catch (e) {
      setModelError(String(e));
      setModels([]);
    } finally {
      setModelLoading(false);
    }
  }, [mode, providerId, currentModel]);

  /** 选中一个模型 → 写进 Claude Code 的 settings.json。
   *
   *  立刻生效，不等"保存供应商"：选模型和存供应商信息是两件事，
   *  把它们绑在一起会让用户为了换个模型而被迫把整个表单再确认一遍。 */
  const pickModel = useCallback(
    async (value: string) => {
      setModel(value);
      if (!value) return;
      setModelError(null);
      try {
        await invoke<string[]>("set_claude_model", { model: value });
        const latest = await invoke<string | null>("claude_model");
        setCurrentModel(latest);
      } catch (e) {
        setModelError(`模型没写进去：${String(e)}`);
      }
    },
    [],
  );

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

          {/* 模型选择。cc-switch 有这一步，tern 之前没有——用户只能去手改
              settings.json。放在地址和 key 后面：它是"这家有哪些模型可选"的
              答案，得先知道地址和 key 才拉得到 */}
          {!managed && (
            <div className="prov-field">
              <span className="prov-label">
                默认模型
                <span className="prov-label-dim">
                  {mode === "edit" ? "写入 ~/.claude/settings.json" : "保存后可选"}
                </span>
              </span>
              <div className="model-row">
                <select
                  className="prov-modal-input"
                  value={model}
                  disabled={modelLoading || models.length === 0}
                  onChange={(e) => void pickModel(e.target.value)}
                >
                  <option value="">
                    {models.length === 0 ? "（尚未获取）" : "选一个模型…"}
                  </option>
                  {models.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
                <button
                  className="prov-mini"
                  onClick={() => void loadModels()}
                  disabled={modelLoading || mode !== "edit"}
                  title={
                    mode === "edit" ? "向这个供应商询问它有哪些模型" : "先保存，再回来选模型"
                  }
                >
                  {modelLoading ? "拉取中…" : models.length ? "刷新" : "获取模型"}
                </button>
              </div>
              {modelError && <span className="prov-hint err-text">{modelError}</span>}
              {currentModel && model && currentModel !== model && (
                <span className="prov-hint">
                  Claude Code 现在用的是 <code>{currentModel}</code>，选了会改成这个。
                </span>
              )}
              {model === currentModel && model !== "" && (
                <span className="prov-hint ok-text">当前生效的就是这个模型。</span>
              )}
              {/* 没解释的话，用户不会知道这一步动的是别处的文件 */}
              <span className="prov-hint">
                选中的模型名会写进 Claude Code 的
                <code> ANTHROPIC_MODEL</code> 等四个档位（已有值的档位不覆盖）。
                不选就不动它。
              </span>
            </div>
          )}

          {/* 模型档位映射：cc-switch 里给每家逐个挑的那一套。
              和上面的「默认模型」是两件事：那个是全局的、立刻改四个键；
              这里是「这家配套的档位组合」，切到这家时才应用 */}
          {!managed && (
            <div className="prov-field">
              <span className="prov-label">
                模型档位映射
                <span className="prov-label-dim">切到这家时生效</span>
              </span>
              {modelEnv.length === 0 ? (
                <span className="prov-hint">
                  这家没有配档位映射。从 cc-switch 导入的供应商会带上（每家不一样，
                  StepFun 的 Opus 档可能是 <code>step-5-preview[1M]</code>）。
                </span>
              ) : (
                <>
                  {modelEnv.map((entry, i) => (
                    <div className="model-row" key={entry.key}>
                      <code className="model-env-key" title={entry.key}>
                        {TIER_LABEL[entry.key] ?? entry.key}
                      </code>
                      <input
                        className="prov-modal-input"
                        value={entry.value}
                        placeholder="模型名，留空删除这一档"
                        onChange={(e) => {
                          const next = [...modelEnv];
                          next[i] = { ...entry, value: e.target.value };
                          setModelEnv(next);
                        }}
                      />
                    </div>
                  ))}
                  <span className="prov-hint">
                    切到这家时，这些键会写进 <code>~/.claude/settings.json</code>
                    （该家配了的档位以它为准，没配的保留现值）。上面「默认模型」是
                    全局立即生效的，这里是这家配套的组合。
                  </span>
                </>
              )}
            </div>
          )}
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
              {/* 发的是哪个模型名要说出来。上游回 "model does not exist" 时，
                  用户得看见那个名字才知道去换——用 claude-sonnet-4-6 探测一家
                  没有它的中转站，报错看起来会像"地址错了" */}
              {probe.model && <code className="notice-cmd">探测模型: {probe.model}</code>}
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
