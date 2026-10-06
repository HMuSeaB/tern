import type { Summary } from "./types";

/** token 数缩写：1.23M / 45.6K / 820。与 `tern usage` 的 report::tokens 一致 */
export function formatTokens(n: number): string {
  if (n >= 1_000_000) return trimZero(n / 1_000_000) + "M";
  if (n >= 1_000) return trimZero(n / 1_000) + "K";
  return String(n);
}

/** 金额：Rust 给的是十进制字符串，这里只补 $ 并保留到分 */
export function formatMoney(s: string): string {
  if (!s) return "$0";
  return "$" + s;
}

/** 只用于显示的更精细格式：整数部分加千分位，小数保留两位 */
export function formatMoney2(s: string): string {
  const n = Number(s);
  if (!Number.isFinite(n)) return "$0";
  const [int, frac = "00"] = n.toFixed(2).split(".");
  return `$${int.replace(/\B(?=(\d{3})+(?!\d))/g, ",")}.${frac}`;
}

/** 缓存读占全部输入的比例 */
export function cacheHitRate(s: Summary): number | null {
  const input = s.fresh_input + s.cache_read + s.cache_write;
  if (input <= 0) return null;
  return s.cache_read / input;
}

/** 百分比，保留一位 */
export function formatPercent(v: number | null): string {
  if (v === null) return "—";
  return (v * 100).toFixed(1) + "%";
}

/** 较昨日的增减百分比。昨天为 0 时没有意义 */
export function deltaPercent(today: number, yesterday: number): number | null {
  if (yesterday <= 0) return null;
  return (today - yesterday) / yesterday;
}

export function formatDelta(v: number | null): string {
  if (v === null) return "—";
  const sign = v >= 0 ? "+" : "";
  return sign + (v * 100).toFixed(1) + "%";
}

function trimZero(v: number): string {
  const s = v.toFixed(1);
  return s.endsWith(".0") ? s.slice(0, -2) : s;
}

/** 把毫秒时间戳转成本地 HH:MM:SS */
export function formatTime(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}
