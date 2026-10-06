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
