/// 与 src-tauri/src/commands.rs 的 DTO 逐字段对应。成本是字符串，
/// 因为 Rust 侧给的就是十进制字符串（避免 JS float 丢精度）。

export interface Summary {
  requests: number;
  failures: number;
  aborted: number;
  fresh_input: number;
  output: number;
  cache_read: number;
  cache_write: number;
  cost: string;
  cache_savings: string;
  unpriced: number;
}

export interface UnpricedModel {
  model: string;
  requests: number;
  tokens: number;
}

export interface FailureGroup {
  error_kind: string;
  provider_id: string | null;
  status: number;
  count: number;
  sample: string | null;
}

export interface RecentRequest {
  started_at_ms: number;
  client: string;
  provider_id: string | null;
  client_model: string;
  response_model: string | null;
  role: string;
  status: number;
  outcome: string;
  error_kind: string | null;
  fresh_input: number;
  output: number;
  cache_read: number;
  cache_write: number;
  cost: string | null;
  duration_ms: number;
}

export interface Panel {
  db_path: string;
  first_run: boolean;
  today: Summary;
  yesterday: Summary;
  unpriced_models: UnpricedModel[];
  failures: FailureGroup[];
  recent: RecentRequest[];
}

/** config_summary 命令的返回 */
export interface ConfigSummary {
  path: string;
  listen: string;
  default_provider: string | null;
  providers: ProviderSummary[];
  warnings: string[];
}

export interface ProviderSummary {
  id: string;
  name: string;
  base_url: string;
  api_format: string;
  /** 第三方网关：Claude Code 的联网工具会失效 */
  web_tools_at_risk: boolean;
  /** 是不是当前在用的那个 */
  active: boolean;
  /** real / placeholder / empty / subscription。前端据此提醒，不在前端判 key */
  key_state: "real" | "placeholder" | "empty" | "subscription";
  auth_kind: string;
  /** 自定义文件夹名；null = 未分组 */
  folder: string | null;
  /** 规范化后的请求地址，"按地址归类"的分组键。空串 = 没有地址 */
  group_key: string;
}

/** 一个自定义文件夹。与 Rust 侧 folders::ProviderFolder 逐字段对应 */
export interface ProviderFolder {
  id: string;
  name: string;
  sortIndex?: number;
  isExpanded?: boolean;
}

/** folders_group_by_domain 的结果：一个域名根 + 归到它下面的供应商 */
export interface DomainGroup {
  name: string;
  providerIds: string[];
  isNew: boolean;
}

/** 未分组的显示名。固定字符串而不是可配置的：它同时是 Rust 侧
 *  "归属值为空"的语义对照，改了显示就两头对不上。 */
export const UNGROUPED_LABEL = "未分组";
/** 没有请求地址时的显示名 */
export const NO_URL_LABEL = "未配置地址";

/** 一个分组：文件夹模式下是文件夹，地址模式下是同一个请求地址。 */
export interface Group<T> {
  /** 折叠区的 key，也用作 React key */
  key: string;
  /** 标题：文件夹名或规范化地址 */
  title: string;
  providers: T[];
  /** 当前在用的供应商在不在这一组里——高亮用，省得用户满页找 */
  containsActive: boolean;
  /** 是不是用户自建的文件夹。地址分组永远是 false（那些组没有名字可改） */
  custom: boolean;
}

/** server_status / server_start / server_stop 的返回 */
export interface ServerStatus {
  running: boolean;
  listen: string | null;
  provider_count: number;
  last_error: string | null;
  /** 常驻进程的版本。面板出问题时先确认两边是不是同一套 */
  agent_version: string | null;
  /**
   * 常驻进程起来了没。false 表示连它都没找到——
   * 那是"没装/没跟着一起打包"，和"网关停了"是两回事，提示要分开说
   */
  agent_up: boolean;
}
