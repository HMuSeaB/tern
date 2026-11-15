/**
 * 常用请求地址预设。新增供应商时点一下就把 名字 / 地址 / 协议 三栏填好。
 *
 * # 为什么只放官方直连，不放中转站
 *
 * 中转站的地址天天换（`www.mcgrox.top` 这类），写死进来就是长期维护负担，
 * 而且用户自己的列表里本来就有它们——预设的意义是省掉"查文档、拼路径"，
 * 不是替代他已经配好的东西。官方域名稳定，填错路径的成本也低（一眼能看出不对）。
 *
 * # 地址必须能在代码里找到出处
 *
 * 这份名单宁短勿错。加新条目时，先去仓库里搜一遍有没有现成的用法
 * （`grep -rn "api.xxx.com" crates/`），有就照抄；没有的话至少要在
 * tooltip 里说明"以官方文档为准"。
 * 已经这么做过：DeepSeek / Moonshot / Anthropic 三条来自
 * `tern-cli/src/config.rs` 的样例配置和 `tern-gateway/src/adapter/claude.rs`
 * 的常量，不是凭记忆写的。
 *
 * # 路径后缀为什么有的是 /anthropic 有的是 /v1
 *
 * 取决于上游说什么协议：
 * - Anthropic 协议的官方端点一般是 `/anthropic`（DeepSeek）或根路径（Anthropic 本家）
 * - OpenAI 系按 SDK 约定带版本段 `/v1`
 * 填错协议比填错地址更常见，所以预设把三栏一起给，不让用户自己拼。
 */

export interface UrlPreset {
  /** 预设 id，同时作为新建时的默认 provider id（用户可改） */
  id: string;
  /** 默认显示名 */
  name: string;
  base_url: string;
  /** 对应 FORMATS 里的一个 id */
  api_format: string;
  /** 鼠标悬停说明 */
  hint: string;
}

export const URL_PRESETS: ReadonlyArray<UrlPreset> = [
  {
    id: "anthropic",
    name: "Anthropic 官方",
    base_url: "https://api.anthropic.com",
    api_format: "anthropic",
    hint: "官方直连，联网搜索/读取都正常",
  },
  {
    id: "deepseek",
    name: "DeepSeek 官方",
    base_url: "https://api.deepseek.com/anthropic",
    api_format: "anthropic",
    hint: "DeepSeek 的 Anthropic 兼容端点",
  },
  {
    id: "kimi",
    name: "Moonshot 官方",
    base_url: "https://api.moonshot.cn/v1",
    api_format: "openai_chat",
    hint: "Kimi，OpenAI Chat 协议",
  },
  {
    id: "zhipu",
    name: "智谱 GLM",
    base_url: "https://open.bigmodel.cn/api/anthropic",
    api_format: "anthropic",
    hint: "智谱的 Anthropic 兼容端点，地址以官方文档为准",
  },
  {
    id: "qwen",
    name: "通义千问",
    base_url: "https://dashscope.aliyuncs.com/compatible-mode/v1",
    api_format: "openai_chat",
    hint: "阿里百炼的 OpenAI 兼容端点，地址以官方文档为准",
  },
  {
    id: "minimax",
    name: "MiniMax",
    base_url: "https://api.minimaxi.com/v1",
    api_format: "openai_chat",
    hint: "地址以官方文档为准",
  },
  {
    id: "siliconflow",
    name: "硅基流动",
    base_url: "https://api.siliconflow.cn/v1",
    api_format: "openai_chat",
    hint: "聚合多家模型的中转，地址以官方文档为准",
  },
  {
    id: "openrouter",
    name: "OpenRouter",
    base_url: "https://openrouter.ai/api/v1",
    api_format: "openai_chat",
    hint: "聚合多家模型的中转，地址以官方文档为准",
  },
];
