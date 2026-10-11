// 布局探针专用：把 @tauri-apps/api/core 换成假数据。
// 只在 tools/vite.probe.config.ts 里通过 alias 生效，不进正式构建。
const NOW = 1_726_000_000_000;

function summary(over = {}) {
  return {
    requests: 1284,
    failures: 3,
    aborted: 7,
    fresh_input: 4_821_903,
    output: 1_204_551,
    cache_read: 18_442_010,
    cache_write: 2_104_338,
    cost: "12.4831",
    cache_savings: "9.2104",
    unpriced: 12,
    ...over,
  };
}

function provider(id, name, url, extra = {}) {
  return {
    id,
    name,
    base_url: url,
    api_format: "anthropic",
    web_tools_at_risk: false,
    active: false,
    key_state: "real",
    auth_kind: "api_key",
    folder: null,
    group_key: url.replace(/^https?:\/\//, "").split("/")[0],
    ...extra,
  };
}

/** 文件夹注册表的假数据。单独提出来是为了让 folders_set_expanded 能就地改 */
const FOLDERS = [
  { id: "f1", name: "官方直连", isExpanded: true },
  { id: "f2", name: "国内中转", isExpanded: true },
  { id: "f3", name: "按量计费小站", isExpanded: false },
];

const PROVIDERS = [
  provider("stepfun", "StepFun", "https://api.stepfun.com/step_plan", { active: true }),
  provider("deepseek", "DeepSeek", "https://api.deepseek.com/anthropic"),
  provider("kimi", "Kimi K2", "https://api.moonshot.cn/anthropic"),
  provider("glm", "智谱 GLM", "https://open.bigmodel.cn/api/anthropic"),
  provider("qwen", "通义千问", "https://dashscope.aliyuncs.com/apps/anthropic"),
  provider("minimax", "MiniMax", "https://api.minimax.chat/anthropic"),
  provider("doubao", "豆包", "https://ark.cn-beijing.volces.com/api/compatible-mode/v3/anthropic"),
  provider("hunyuan", "混元", "https://api.hunyuan.cloud.tencent.com/anthropic"),
  provider("spark", "讯飞星火", "https://spark-api.xf-yun.com/anthropic"),
  provider("baichuan", "百川", "https://api.baichuan-ai.com/anthropic"),
  provider("zeroone", "零一万物", "https://api.lingyiwanwu.com/anthropic"),
  provider("silicon", "硅基流动", "https://api.siliconflow.cn/anthropic"),
  // 真实数据里中转站爱在同一个域名下挂多个路径（/v1、/v2 常是两个上游）。
  // 全部单例域名测不出分组卡和批量收起，所以这里补两组同域名的
  provider("mcgrox", "gpt0.8", "https://www.mcgrox.top/v1"),
  provider("mcgrox2", "gpt copy copy copy", "https://www.mcgrox.top/v1"),
  provider("jisuanyun", "claude-opus-5", "https://www.jisuanyun01.com/anthropic"),
  provider("jisuanyun2", "claude-opus-5=", "https://www.jisuanyun01.com/anthropic"),
];

/** provider_id → 近 30 天加权请求数。
 *
 *  故意和 PROVIDERS 里的顺序不一致：doubao / silicon 在配置里排第 7 和第 12，
 *  这里给它们最高用量。排序要是没生效，平铺视图第一屏就不会是它们。
 *  mcgrox 和 mcgrox2 是同一个地址（同 group_key）——地址视图下这两份要
 *  累加成一组，那正是"按地址聚合再排序"的验证点。 */
const PROVIDER_USAGE = [
  { provider_id: "doubao", requests: 900 },
  { provider_id: "silicon", requests: 500 },
  { provider_id: "mcgrox", requests: 40 },
  { provider_id: "mcgrox2", requests: 35 },
  { provider_id: "jisuanyun", requests: 20 },
  { provider_id: "jisuanyun2", requests: 10 },
  { provider_id: "kimi", requests: 5 },
];

const PANEL = {
  db_path: "C:\\Users\\me\\.tern\\usage.db",
  first_run: false,
  today: summary(),
  yesterday: summary({ requests: 1102, cost: "11.9002", fresh_input: 4_100_000 }),
  unpriced_models: [{ model: "step-5-preview[1M]", requests: 42, tokens: 812_004 }],
  failures: [
    { error_kind: "rate_limited", provider_id: "stepfun", status: 429, count: 3, sample: "rate limit reached" },
    { error_kind: "timeout", provider_id: "deepseek", status: 0, count: 1, sample: "timeout" },
  ],
  recent: [
    {
      started_at_ms: NOW,
      client: "claude-code",
      provider_id: "stepfun",
      client_model: "claude-opus-4-6",
      response_model: "step-5-preview[1M]",
      role: "main",
      status: 429,
      outcome: "failed",
      error_kind: "rate_limited",
      fresh_input: 12_004,
      output: 0,
      cache_read: 88_210,
      cache_write: 0,
      cost: null,
      duration_ms: 218,
    },
    {
      started_at_ms: NOW - 60_000,
      client: "claude-code",
      provider_id: "stepfun",
      client_model: "claude-sonnet-4-5",
      response_model: "step-5-preview[1M]",
      role: "subagent",
      status: 200,
      outcome: "success",
      error_kind: null,
      fresh_input: 4_120,
      output: 9_880,
      cache_read: 42_100,
      cache_write: 1_002,
      cost: "0.0182",
      duration_ms: 4_120,
    },
  ],
};

const RESPONSES = {
  first_run: () => false,
  config_summary: () => ({
    path: "C:\\Users\\me\\.tern\\tern.json",
    listen: "127.0.0.1:15800",
    default_provider: "stepfun",
    providers: PROVIDERS,
    warnings: [],
  }),
  server_status: () => ({
    running: true,
    listen: "127.0.0.1:15800",
    provider_count: PROVIDERS.length,
    last_error: null,
    agent_version: "0.1.8",
    agent_up: true,
  }),
  panel_summary: () => PANEL,
  provider_usage: () => PROVIDER_USAGE,
  folders_list: () => FOLDERS,
  // 地址分组没有注册表项，Rust 侧对未知名字静默无事。这里照样返回注册表，
  // 好让"批量收起"的前端路径走通而不是被 catch 吞掉
  folders_set_expanded: (args) =>
    FOLDERS.map((f) =>
      f.name === args?.name ? { ...f, isExpanded: args?.expanded } : f,
    ),
  wire_status: () => ({
    wired: true,
    base_url: "http://127.0.0.1:15800",
    settings_path: "C:\\Users\\me\\.claude\\settings.json",
    has_token: true,
    backup_path: "C:\\Users\\me\\.claude\\settings.json.bak",
  }),
  list_permissions: () => [
    { rule: "Bash", source: "settings", enabled: true },
    { rule: "Read", source: "settings", enabled: true },
    { rule: "WebFetch", source: "settings", enabled: true },
    { rule: "mcp__git", source: "settings_local", enabled: false },
  ],
  claude_model: () => "claude-opus-4-6",
  provider_detail: (args) => ({
    id: args?.id,
    name: id,
    base_url: "https://api.stepfun.com/step_plan",
    api_format: "anthropic",
    auth_kind: "api_key",
    is_default: id === "stepfun",
    cost_multiplier: "0.3",
    client_env: {
      ANTHROPIC_DEFAULT_OPUS_MODEL: "step-5-preview[1M]",
      ANTHROPIC_DEFAULT_SONNET_MODEL: "step-5-preview",
    },
    folder: null,
  }),
  cc_switch_usage_preview: () => ({
    db_path: "C:\\Users\\me\\.cc-switch\\cc-switch.db",
    total: 17_153,
    imported: 16_963,
    skipped_session: 190,
    skipped_duplicate: 0,
    unpriced: 0,
    from_ms: NOW - 30 * 86_400_000,
    to_ms: NOW,
  }),
  cc_switch_backfill_model_env: () => ({
    total: 12,
    already: 30,
    filled: 12,
    providers: [],
  }),
};

export async function invoke(cmd, args) {
  await new Promise((r) => setTimeout(r, 30));
  const make = RESPONSES[cmd];
  if (!make) throw new Error(`探针没有为 ${cmd} 准备假数据`);
  // 传整个 args：大多数 mock 只取 id，但 set_expanded 这类要 name/expanded
  return make(args);
}

export async function transformCallback() {
  return 0;
}
export const convertFileSrc = (p) => p;
export const invoke_ = invoke;
