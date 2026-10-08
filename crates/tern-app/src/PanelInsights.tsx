import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Legend,
  Pie,
  PieChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from "recharts";
import { formatMoney2, formatTokens } from "./format";
import type { Summary } from "./types";

/**
 * 用量面板的二级视图（ROADMAP T+3）。
 *
 * Rust 侧给的是聚合结果，前端只负责摆。所以这里每个 hook 都是一次 invoke 拿
 * 一整块数据，**没有**"拉全量明细再在 JS 里 group by"——那个做法在明细上到
 * 十万行时会把渲染进程直接卡死，而用量库长起来是必然的。
 *
 * # 三张图各回答一个问题
 *
 * - **趋势堆叠柱**：钱随时间怎么花的，按供应商 / 模型 / 角色 / 客户端拆
 * - **花在哪（环形）**：这段时间的钱集中在哪些供应商、哪些模型
 * - **模型流向（流向条）**：客户端要的模型 → 实际花钱的模型
 * - **会话表**："这次重构花了多少"
 *
 * 不为了填满版面而多画。ROADMAP 里的活跃度热力图没做：它回答"我什么时候在干活"，
 * 而用户真正会盯着看的是"钱花在哪"，热力图的边际价值最低。
 *
 * # 颜色
 *
 * 用 CSS 变量 `--chart-1..8`（见 styles.css），随主题切换。刻意不用红绿配对
 * ——色弱用户分不清，而这里要的是"区分系列"，不是"好坏"。
 */

/** panel_trend / panel_breakdown / panel_sessions / panel_model_flow 共用的行。
 *  四个命令返回的形状一致（一个 key + 一个 Summary），所以一个类型就够。 */
export interface QueryRow {
  /** 维度键：趋势是天、占比是供应商 id / 模型名、会话是 session_id */
  key: string;
  /** 模型流向专用：客户端模型名（此时 key 是下游 / 实际模型） */
  day: string;
  summary: Summary;
  /** 会话专用：这次会话里出现过的角色 */
  roles?: string[];
  client?: string;
  started_at_ms?: number;
  ended_at_ms?: number;
}

const RANGES: ReadonlyArray<{ days: number; label: string }> = [
  { days: 7, label: "7 天" },
  { days: 30, label: "30 天" },
  { days: 90, label: "90 天" },
];

const DIMS: ReadonlyArray<{ id: string; label: string }> = [
  { id: "provider", label: "供应商" },
  { id: "model", label: "模型" },
  { id: "role", label: "角色" },
  { id: "client", label: "客户端" },
];

/** 图表系列色。顺序固定——切换日期范围时同一个供应商必须还是同一个颜色，
 *  否则用户刚建立的颜色记忆每切一次就废一次。 */
function chartColor(index: number): string {
  return `var(--chart-${(index % 8) + 1})`;
}

/** 拉一块聚合数据。加载中 / 出错 / 有数据三种状态由各卡片自己摆。 */
function usePanelQuery(command: string, days: number, extra: Record<string, unknown> = {}) {
  const [data, setData] = useState<QueryRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // extra 里放的是 limit 之类的标量，进依赖数组是安全的（每次渲染都是新对象，
  // 但它内容不变，比较时按值传入的 useCallback 依赖项）
  const limit = extra.limit;

  const load = useCallback(async () => {
    try {
      setData(await invoke<QueryRow[]>(command, { days, ...extra }));
      setError(null);
    } catch (e) {
      setError(String(e));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [command, days, limit]);

  useEffect(() => {
    void load();
  }, [load]);

  return { data, error, reload: load };
}

export function PanelInsights() {
  const [days, setDays] = useState(7);

  return (
    <div className="panel">
      <section className="card">
        <div className="prov-toolbar" style={{ marginBottom: 0 }}>
          <span className="side-label">时间范围</span>
          <div className="prov-seg" role="group" aria-label="时间范围">
            {RANGES.map((r) => (
              <button
                key={r.days}
                className={`prov-seg-btn ${days === r.days ? "on" : ""}`}
                onClick={() => setDays(r.days)}
                aria-pressed={days === r.days}
              >
                {r.label}
              </button>
            ))}
          </div>
        </div>
      </section>

      <TrendCard days={days} />
      <div className="insight-row">
        <BreakdownCard days={days} dim="provider" title="花在哪（供应商）" />
        <BreakdownCard days={days} dim="model" title="花在哪（模型）" donut />
      </div>
      <ModelFlowCard days={days} />
      <SessionsCard days={days} />
    </div>
  );
}

function CardFrame({
  title,
  hint,
  children,
}: {
  title: string;
  hint?: string;
  children: React.ReactNode;
}) {
  return (
    <section className="card">
      <div className="head">
        <span className="title">{title}</span>
        {hint && <span className="act">{hint}</span>}
      </div>
      {children}
    </section>
  );
}

/** recharts 的默认 tooltip 是英文键名且不带货币格式，所以自己写一个。 */
function MoneyTip({
  active,
  payload,
  label,
}: {
  active?: boolean;
  payload?: Array<{ name?: string; value?: number; color?: string; dataKey?: string | number }>;
  label?: string;
}) {
  if (!active || !payload?.length) return null;
  const total = payload.reduce((sum, entry) => sum + (entry.value ?? 0), 0);
  return (
    <div className="chart-tip">
      {label && <div className="chart-tip-head">{label}</div>}
      {payload.map((entry, i) => (
        <div key={i} className="chart-tip-row">
          <span className="chart-swatch" style={{ background: entry.color }} />
          <span className="chart-tip-name">{String(entry.name ?? entry.dataKey ?? "")}</span>
          <span className="chart-tip-value tnum">{formatMoney2(String(entry.value ?? 0))}</span>
        </div>
      ))}
      {payload.length > 1 && (
        <div className="chart-tip-row total">
          <span />
          <span className="chart-tip-name">合计</span>
          <span className="chart-tip-value tnum">{formatMoney2(String(total))}</span>
        </div>
      )}
    </div>
  );
}

/** 花费趋势：按天的堆叠柱。 */
function TrendCard({ days }: { days: number }) {
  const [dim, setDim] = useState("provider");
  const { data, error } = usePanelQuery("panel_trend", days, { dim });

  // Rust 给的是 (天, 维度键) 的笛卡尔积、缺的补 0。转成 recharts 要的"每天一个
  // 对象、每个维度一列"。补 0 的点让序列连续——断一天看着像那天没用。
  const { rows, keys, total } = useMemo(() => pivotTrend(data), [data]);

  if (error) return <ErrorMessage message={error} />;
  if (!data) return <Loading />;

  return (
    <CardFrame title="花费趋势" hint={`${days} 天 · 共 ${formatMoney2(total)}`}>
      <div className="prov-toolbar">
        <div className="prov-seg" role="group" aria-label="拆分维度">
          {DIMS.map((d) => (
            <button
              key={d.id}
              className={`prov-seg-btn ${dim === d.id ? "on" : ""}`}
              onClick={() => setDim(d.id)}
              aria-pressed={dim === d.id}
            >
              {d.label}
            </button>
          ))}
        </div>
      </div>
      {rows.length === 0 ? (
        <p className="empty-text">这个时间范围内没有记录。</p>
      ) : (
        <div className="chart-box">
          <ResponsiveContainer width="100%" height={220}>
            <BarChart data={rows} margin={{ top: 4, right: 4, left: 0, bottom: 0 }}>
              <CartesianGrid stroke="var(--line)" vertical={false} />
              <XAxis
                dataKey="day"
                tick={{ fontSize: 11, fill: "var(--dim)" }}
                tickFormatter={shortDay}
                stroke="var(--line)"
              />
              <YAxis
                tick={{ fontSize: 11, fill: "var(--dim)" }}
                tickFormatter={(v: number) => `$${v}`}
                width={54}
                stroke="var(--line)"
              />
              <Tooltip content={<MoneyTip />} cursor={{ fill: "var(--track)" }} />
              <Legend wrapperStyle={{ fontSize: 12 }} />
              {keys.map((key, i) => (
                <Bar
                  key={key}
                  dataKey={key}
                  name={key}
                  stackId="cost"
                  fill={chartColor(i)}
                  maxBarSize={30}
                />
              ))}
            </BarChart>
          </ResponsiveContainer>
        </div>
      )}
    </CardFrame>
  );
}

/** 笛卡尔积的点 → recharts 的行。**模块级纯函数**，不碰组件状态，好测。 */
function pivotTrend(points: QueryRow[] | null): {
  rows: Array<Record<string, string | number>>;
  keys: string[];
  total: string;
} {
  if (!points || points.length === 0) return { rows: [], keys: [], total: "0" };

  const keys: string[] = [];
  const byDay = new Map<string, Record<string, string | number>>();
  let total = 0;

  for (const point of points) {
    if (point.key && !keys.includes(point.key)) keys.push(point.key);
    let row = byDay.get(point.day);
    if (!row) {
      row = { day: point.day };
      byDay.set(point.day, row);
    }
    const cost = Number(point.summary.cost);
    total += cost;
    // 累加而不是赋值：同一天可能有多个角色 / 多条会话写到同一列，
    // 赋值的话后一个会把前一个覆盖掉，堆叠柱的总高就错了
    row[point.key || "其它"] = (Number(row[point.key || "其它"]) || 0) + cost;
  }

  return { rows: [...byDay.values()], keys, total: total.toFixed(6) };
}

/** 花在哪：占比。环形（donut）或饼图。 */
function BreakdownCard({
  days,
  dim,
  title,
  donut,
}: {
  days: number;
  dim: string;
  title: string;
  donut?: boolean;
}) {
  const { data, error } = usePanelQuery("panel_breakdown", days, { dim });
  if (error) return <ErrorMessage message={error} />;
  if (!data) return <Loading />;

  const rows = data
    .map((row) => ({
      name: row.key || "未路由",
      value: Number(row.summary.cost),
      requests: row.summary.requests,
    }))
    .filter((row) => row.value > 0)
    .sort((a, b) => b.value - a.value)
    .slice(0, 8);
  const total = rows.reduce((sum, row) => sum + row.value, 0);

  return (
    <CardFrame
      title={title}
      hint={total > 0 ? `${formatMoney2(total.toFixed(6))} · ${rows.length} 项` : undefined}
    >
      {rows.length === 0 ? (
        <p className="empty-text">这个时间范围内没有花费。</p>
      ) : (
        <>
          <div className="chart-box">
            <ResponsiveContainer width="100%" height={200}>
              <PieChart>
                <Pie
                  data={rows}
                  dataKey="value"
                  nameKey="name"
                  innerRadius={donut ? 54 : 0}
                  outerRadius={82}
                  paddingAngle={donut ? 2 : 0}
                  stroke="var(--panel)"
                  strokeWidth={2}
                >
                  {rows.map((_, i) => (
                    <Cell key={i} fill={chartColor(i)} />
                  ))}
                </Pie>
                <Tooltip content={<MoneyTip />} />
              </PieChart>
            </ResponsiveContainer>
          </div>
          {/* 图例单独列而不靠 recharts 的：它的图例只给色块和名字，
              而"占多少、多少钱"才是用户盯着看的两个数 */}
          <ul className="legend-list">
            {rows.map((row, i) => (
              <li key={row.name}>
                <span className="chart-swatch" style={{ background: chartColor(i) }} />
                <span className="legend-name" title={row.name}>
                  {row.name}
                </span>
                <span className="legend-share tnum">
                  {total > 0 ? ((row.value / total) * 100).toFixed(1) : "0.0"}%
                </span>
                <span className="legend-value tnum">{formatMoney2(String(row.value))}</span>
              </li>
            ))}
          </ul>
        </>
      )}
    </CardFrame>
  );
}

/** 模型流向：客户端模型 → 实际模型。回答"谁在花钱"。
 *
 *  这是 ROADMAP 点名要的那一张。cc-switch 的真实数据里 212 条
 *  `claude-sonnet-4-6 → claude-opus-5.5` 全是子代理请求被供应商映射走了，
 *  在原界面里完全看不出来。这里做成流向条而不是桑基图：桑基在窄面板里标签
 *  放不下，而"哪几条被换了"用列表一眼就能数完。 */
function ModelFlowCard({ days }: { days: number }) {
  const { data, error } = usePanelQuery("panel_model_flow", days);
  if (error) return <ErrorMessage message={error} />;
  if (!data) return <Loading />;

  const edges = data
    .map((row) => ({
      from: row.day,
      to: row.key,
      cost: Number(row.summary.cost),
      requests: row.summary.requests,
      tokens:
        row.summary.fresh_input +
        row.summary.output +
        row.summary.cache_read +
        row.summary.cache_write,
    }))
    .sort((a, b) => b.cost - a.cost || b.requests - a.requests)
    .slice(0, 12);
  // 被换过模型的排前面：那是"钱花在不是你要的模型上"的唯一线索
  const mapped = edges.filter((e) => e.from !== e.to);
  const ordered = [...mapped, ...edges.filter((e) => e.from === e.to)];

  if (edges.length === 0) {
    return (
      <CardFrame title="模型流向">
        <p className="empty-text">这段时间里没有成功的请求。</p>
      </CardFrame>
    );
  }

  return (
    <CardFrame
      title="模型流向"
      hint={mapped.length > 0 ? `${mapped.length} 条被换过模型` : "没有被换模型"}
    >
      <ul className="flow-list">
        {ordered.map((edge, i) => (
          <li key={`${edge.from}->${edge.to}-${i}`} className={edge.from !== edge.to ? "mapped" : ""}>
            <code className="flow-model" title={edge.from}>
              {edge.from}
            </code>
            <span className="flow-arrow" aria-hidden>
              →
            </span>
            <code className="flow-model target" title={edge.to}>
              {edge.to}
            </code>
            <span className="flow-side">
              <span className="tnum">{edge.requests} 次</span>
              <span className="dim tnum">{formatTokens(edge.tokens)}</span>
              <span className="tnum">{formatMoney2(String(edge.cost))}</span>
            </span>
          </li>
        ))}
      </ul>
    </CardFrame>
  );
}

/** 会话视图："这次重构花了多少"。
 *
 *  panel_sessions 给的是 SessionRow，其中 session_id / client / roles /
 *  started_at / ended_at 在 QueryRow 里都有位置（key 是 session_id）。 */
function SessionsCard({ days }: { days: number }) {
  const { data, error } = usePanelQuery("panel_sessions", days, { limit: 30 });
  if (error) return <ErrorMessage message={error} />;
  if (!data) return <Loading />;

  if (data.length === 0) {
    return (
      <CardFrame title="会话">
        <p className="empty-text">
          还没有带 session_id 的请求。会话 ID 由客户端自带，网关兜底生成的不入库
          （那个每轮都不同，聚合起来没有意义）。
        </p>
      </CardFrame>
    );
  }

  return (
    <CardFrame title="会话" hint={`${data.length} 次 · ${days} 天`}>
      <table className="recent">
        <thead>
          <tr>
            <th>会话</th>
            <th>角色</th>
            <th className="num">请求</th>
            <th className="num">Token</th>
            <th className="num">成本</th>
          </tr>
        </thead>
        <tbody>
          {data.slice(0, 20).map((row, i) => {
            const tokens =
              row.summary.fresh_input +
              row.summary.output +
              row.summary.cache_read +
              row.summary.cache_write;
            return (
              <tr key={`${row.key}-${i}`}>
                <td className="mono" title={row.key}>
                  {row.key.slice(0, 10)}
                </td>
                <td>
                  {(row.roles ?? []).map((r) => (
                    <span key={r} className="role">
                      {ROLE_LABEL[r] ?? r}
                    </span>
                  ))}
                </td>
                <td className="num tnum">{row.summary.requests}</td>
                <td className="num tnum">{formatTokens(tokens)}</td>
                <td className="num tnum">
                  {formatMoney2(row.summary.cost)}
                  {row.summary.aborted > 0 && <span className="dim"> ·{row.summary.aborted}中断</span>}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </CardFrame>
  );
}

const ROLE_LABEL: Record<string, string> = {
  main: "主对话",
  subagent: "子代理",
  compact: "压缩",
  background: "后台",
};

function Loading() {
  return (
    <section className="card">
      <p className="empty-text">正在读取…</p>
    </section>
  );
}

function ErrorMessage({ message }: { message: string }) {
  return (
    <div className="notice err">
      <span className="notice-ic">!</span>
      <span className="notice-body">{message}</span>
    </div>
  );
}

/** `2026-10-08` → `10-08`。30 天的图上放全宽日期会互相重叠。 */
function shortDay(day: string): string {
  return day.slice(5);
}
