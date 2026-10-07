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
  auth_kind: string;
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
