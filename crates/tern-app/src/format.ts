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

/** 首屏大数字用的金额。和 formatMoney2 的差别只在"不到一分钱时"：
 *
 *  自费跑第三方网关，一天常常就几美分甚至不到一分。那种量下 formatMoney2
 *  显示 $0.00 —— 而用户第一眼会读成"面板没记上"，不是"今天很便宜"。
 *  所以小于 1 美元时留四位小数并去掉尾零，让数字真的在那儿。 */
export function formatMoneyHero(s: string): string {
  const n = Number(s);
  if (!Number.isFinite(n) || n === 0) return "$0";
  if (n >= 1) return formatMoney2(s);
  const four = n.toFixed(4);
  // 不足 0.0001 美元：四位小数也会塌成 0，直接用 Rust 给的十进制串（它没截断）
  if (Number(four) === 0) return "$" + s;
  return "$" + four.replace(/\.?0+$/, "");
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
