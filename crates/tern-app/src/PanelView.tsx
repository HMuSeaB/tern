import { useMemo, useState } from "react";
import {
  cacheHitRate,
  deltaPercent,
  formatDelta,
  formatMoney2,
  formatPercent,
  formatTime,
  formatTokens,
} from "./format";
import type { Panel, RecentRequest } from "./types";

/** 主面板。版式照 design/b-workbench.html 的皮，但信息密度对齐 cc-switch 使用统计页：
 *  hero 一个大数字 + 4 小卡 + 一条命中率 + 两条醒目提示。趋势/占比放二级视图。 */

const OUTCOME_LABEL: Record<string, { text: string; cls: string }> = {
  success: { text: "成功", cls: "ok" },
  aborted: { text: "中断", cls: "abrt" },
  failed: { text: "失败", cls: "fail" },
};

const ROLE_LABEL: Record<string, string> = {
  main: "主对话",
  subagent: "子代理",
  compact: "压缩",
  background: "后台",
};

const ERROR_LABEL: Record<string, string> = {
  rate_limited: "限流",
  overloaded: "上游过载",
  auth: "鉴权失败",
  timeout: "超时",
  connection: "连接失败",
  upstream_rejected: "上游拒绝",
  upstream_server: "上游故障",
  invalid_request: "请求无效",
  transform: "转换失败",
  stream: "流式中断",
  internal: "内部错误",
  unknown: "未知",
};

/** 把 Rust 的 error_kind（serde 序列化成 snake_case）翻成中文 */
function errorLabel(kind: string): string {
  return ERROR_LABEL[kind] ?? kind;
}

export function PanelView({ panel, onRefresh }: { panel: Panel; onRefresh: () => void }) {
  const [showRecent, setShowRecent] = useState(false);
  const { today, yesterday } = panel;

  const totalTokens = useMemo(
    () =>
      today.fresh_input + today.output + today.cache_read + today.cache_write,
    [today],
  );
  const hit = cacheHitRate(today);

  const costDelta = deltaPercent(Number(today.cost), Number(yesterday.cost));
  const reqDelta = deltaPercent(today.requests, yesterday.requests);
  const outDelta = deltaPercent(today.output, yesterday.output);
  const yHit = cacheHitRate(yesterday);
  const hitDelta = hit !== null && yHit !== null ? hit - yHit : null;

  if (panel.first_run) {
    return <FirstRun dbPath={panel.db_path} onRefresh={onRefresh} />;
  }

  return (
    <div className="panel">
      {/* 未定价：成本图会因此偏低，必须显眼 */}
      {panel.unpriced_models.length > 0 && (
        <UnpricedNotice models={panel.unpriced_models} />
      )}
      {panel.failures.length > 0 && <FailureNotice failures={panel.failures} />}

      {/* 失败是最常见的"我刚才怎么了"。它不在首屏就得展开「最近请求」才能找到,
          所以最近一次失败直接挂在 hero 上面,点它展开请求流并滚到那一块 */}
      {panel.recent.some((r) => r.outcome === "failed") && (
        <LatestFailure
          request={panel.recent.find((r) => r.outcome === "failed")!}
          onOpen={() => {
            setShowRecent(true);
            // 展开后表在页面下方,不滚过去的话用户点了什么都没看见
            requestAnimationFrame(() =>
              document.getElementById("recent-requests")?.scrollIntoView({ behavior: "smooth", block: "start" }),
            );
          }}
        />
      )}

      <section className="card hero">
        <div className="hero-label">真实消耗 Tokens</div>
        <div className="hero-figure">
          <span className="hero-value tnum">{formatTokens(totalTokens)}</span>
        </div>
        <div className="hero-side">
          <SideStat label="总请求数" value={String(today.requests)} delta={reqDelta} />
          <SideStat label="总成本" value={formatMoney2(today.cost)} delta={costDelta} />
        </div>
      </section>

      <section className="card grid">
        <Stat
          label="新增输入"
          value={formatTokens(today.fresh_input)}
          delta={deltaPercent(today.fresh_input, yesterday.fresh_input)}
          hint="没走缓存的输入 token,按全价计费"
        />
        <Stat
          label="Output"
          value={formatTokens(today.output)}
          delta={outDelta}
          hint="模型生成的输出 token"
        />
        <Stat
          label="创建"
          value={formatTokens(today.cache_write)}
          delta={deltaPercent(today.cache_write, yesterday.cache_write)}
          hint="写入缓存的输入,比全价略贵,但之后能被命中"
        />
        <Stat
          label="命中"
          value={formatTokens(today.cache_read)}
          delta={deltaPercent(today.cache_read, yesterday.cache_read)}
          hint="从缓存读出的输入,按折扣价计费"
        />
      </section>

      <section className="card meter">
        <div className="meter-head">
          <span>缓存命中率</span>
          <span className="meter-value tnum">{formatPercent(hit)}</span>
        </div>
        <div className="meter-track">
          <div className="meter-fill" style={{ width: `${(hit ?? 0) * 100}%` }} />
        </div>
        <p className="meter-hint">
          命中率 = 命中 ÷ (新增输入 + 创建 + 命中)。越高说明同一段上下文被反复用上了。
        </p>
        {hitDelta !== null && (
          <div className={`meter-foot ${hitDelta >= 0 ? "up" : "down"}`}>
            较昨日 {hitDelta >= 0 ? "+" : ""}
            {(hitDelta * 100).toFixed(1)} 个百分点
          </div>
        )}
      </section>

      {/* 省下的钱是 Claude Code 用户最关心、cc-switch 又没直接给的数字 */}
      {Number(today.cache_savings) > 0 && (
        <section className="card saved">
          <span className="saved-label">缓存省下</span>
          <span className="saved-value tnum">{formatMoney2(today.cache_savings)}</span>
        </section>
      )}

      <section className="card" id="recent-requests">
        <button className="disclosure" onClick={() => setShowRecent((v) => !v)}>
          <span className={`chevron ${showRecent ? "open" : ""}`}>▸</span>
          最近请求
          <span className="disclosure-count tnum">{panel.recent.length}</span>
        </button>
        {showRecent && <RecentTable panel={panel} />}
      </section>
    </div>
  );
}

function SideStat({
  label,
  value,
  delta,
}: {
  label: string;
  value: string;
  delta: number | null;
}) {
  return (
    <div className="side-stat">
      <div className="side-label">{label}</div>
      <div className="side-value tnum">{value}</div>
      <div className={`side-delta ${delta !== null && delta >= 0 ? "up" : "down"}`}>
        {delta === null ? "昨日 0" : `昨日 ${formatDelta(delta)}`}
      </div>
    </div>
  );
}

function Stat({
  label,
  value,
  delta,
  hint,
}: {
  label: string;
  value: string;
  delta: number | null;
  hint?: string;
}) {
  return (
    <div className="stat" title={hint}>
      <div className="stat-label">{label}</div>
      <div className="stat-value tnum">{value}</div>
      <div className={`stat-delta ${delta !== null && delta >= 0 ? "up" : "down"}`}>
        {formatDelta(delta)}
      </div>
      {hint && <div className="stat-hint">{hint}</div>}
    </div>
  );
}

function UnpricedNotice({ models }: { models: Panel["unpriced_models"] }) {
  const tokens = models.reduce((sum, m) => sum + m.tokens, 0);
  const requests = models.reduce((sum, m) => sum + m.requests, 0);
  const plural = models.length > 1;
  return (
    <div className="notice warn">
      <span className="notice-ic">$</span>
      <span className="notice-body">
        <b>
          {models[0].model}
          {plural ? ` 等 ${models.length} 个模型` : ""}
        </b>
        {plural ? " 都没定价 —— " : " 没定价 —— "}
        {requests} 次请求 {formatTokens(tokens)} token 未计费，成本偏低
      </span>
      <code className="notice-cmd">tern price set</code>
    </div>
  );
}

function FailureNotice({ failures }: { failures: Panel["failures"] }) {
  const total = failures.reduce((sum, f) => sum + f.count, 0);
  const top = failures[0];
  const kind = errorLabel(top.error_kind);
  const who = top.provider_id ?? "未路由";
  // 多个供应商时"等"才成立
  const providers = new Set(failures.map((f) => f.provider_id));
  const whoText = providers.size > 1 ? `${who} 等 ${providers.size} 个供应商` : who;
  return (
    <div className="notice err">
      <span className="notice-ic">!</span>
      <span className="notice-body">
        <b>{whoText}</b> 今日失败 {total} 次（最多：{kind} {top.count} 次）。
        失败不混入模型统计
      </span>
    </div>
  );
}

function LatestFailure({
  request,
  onOpen,
}: {
  request: RecentRequest;
  onOpen: () => void;
}) {
  const who = request.provider_id ?? "未路由";
  return (
    <button className="latest-fail" onClick={onOpen} title="展开最近请求查看全部">
      <span className="latest-fail-tag">最近失败</span>
      <span className="latest-fail-kind">{errorLabel(request.error_kind ?? "unknown")}</span>
      <span className="latest-fail-who">{who}</span>
      <span className="latest-fail-time tnum">{formatTime(request.started_at_ms)}</span>
      <span className="latest-fail-more">查看 →</span>
    </button>
  );
}

function RecentTable({ panel }: { panel: Panel }) {
  return (
    <table className="recent">
      <thead>
        <tr>
          <th>时间</th>
          <th>模型</th>
          <th>客户端</th>
          <th className="num">Token</th>
          <th className="num">成本</th>
          <th>状态</th>
        </tr>
      </thead>
      <tbody>
        {panel.recent.map((r, i) => {
          const tokens = r.fresh_input + r.output + r.cache_read + r.cache_write;
          const outcome = OUTCOME_LABEL[r.outcome] ?? OUTCOME_LABEL.failed;
          const mapped = r.response_model !== null && r.response_model !== r.client_model;
          return (
            <tr key={`${r.started_at_ms}-${i}`}>
              <td className="mono dim">{formatTime(r.started_at_ms)}</td>
              {/* cell-clip：映射后是两个模型名，最长能到 255px。
                  不定列宽 + 不省略的话它靠折行把行高撑成两行 */}
              <td className="mono">
                <div className="cell-clip">
                  {mapped ? (
                    <>
                      <span className="requester">{r.client_model}</span>
                      <span className="arrow">→</span>
                      <span className="mapped">{r.response_model}</span>
                    </>
                  ) : (
                    r.response_model ?? r.client_model
                  )}
                </div>
              </td>
              <td>
                {r.client}
                <span className="role">{ROLE_LABEL[r.role] ?? r.role}</span>
              </td>
              <td className="num tnum">{r.outcome === "failed" ? "—" : formatTokens(tokens)}</td>
              <td className="num tnum">
                {r.outcome === "failed" ? (
                  <span className="dim">—</span>
                ) : r.cost === null ? (
                  <span className="unpriced">未定价</span>
                ) : (
                  formatMoney2(r.cost)
                )}
              </td>
              <td>
                <span className={`badge ${outcome.cls}`}>
                  <span className="dot" />
                  {r.outcome === "failed"
                    ? errorLabel(r.error_kind ?? "unknown")
                    : outcome.text}
                </span>
              </td>
            </tr>
          );
        })}
      </tbody>
    </table>
  );
}

function FirstRun({ dbPath, onRefresh }: { dbPath: string; onRefresh: () => void }) {
  return (
    <div className="card empty">
      <div className="empty-title">还没有用量数据</div>
      <p className="empty-text">
        面板只读取 <code>{dbPath}</code>，不自己跑网关。
      </p>
      <ol className="empty-steps">
        <li>
          <code>tern init</code> 生成配置
        </li>
        <li>
          <code>tern serve</code> 启动网关并开始记录
        </li>
        <li>用 Claude Code 或 Codex 跑一个会话</li>
      </ol>
      <button className="btn" onClick={onRefresh}>
        刷新
      </button>
    </div>
  );
}
