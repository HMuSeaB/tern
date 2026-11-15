import { useCallback, useMemo, useState } from "react";
import {
  cacheHitRate,
  deltaPercent,
  formatDelta,
  formatMoney2,
  formatMoneyHero,
  formatPercent,
  formatTime,
  formatTokens,
} from "./format";
import type { Panel } from "./types";

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
  const yesterdayTokens = useMemo(
    () =>
      yesterday.fresh_input +
      yesterday.output +
      yesterday.cache_read +
      yesterday.cache_write,
    [yesterday],
  );
  const hit = cacheHitRate(today);

  const costDelta = deltaPercent(Number(today.cost), Number(yesterday.cost));
  const reqDelta = deltaPercent(today.requests, yesterday.requests);
  const outDelta = deltaPercent(today.output, yesterday.output);
  const tokDelta = deltaPercent(totalTokens, yesterdayTokens);
  const yHit = cacheHitRate(yesterday);
  const hitDelta = hit !== null && yHit !== null ? hit - yHit : null;

  /** 展开请求流并滚过去。三条告警都跳这儿，抽出来免得写三遍；
   *  不滚的话用户点了什么都没看见——表在页面下方。 */
  const openRecent = useCallback(() => {
    setShowRecent(true);
    requestAnimationFrame(() =>
      document
        .getElementById("recent-requests")
        ?.scrollIntoView({ behavior: "smooth", block: "start" }),
    );
  }, []);

  if (panel.first_run) {
    return <FirstRun dbPath={panel.db_path} onRefresh={onRefresh} />;
  }

  return (
    <div className="panel">
      {/* 告警区。顺序就是"有多该被看见"：
          模型被换排第一——请求全都成功、日志里一条错都没有，不说永远不知道；
          失败排后面——客户端自己就会报错，用户已经知道出事了。
          三条，不是四条：失败聚类那条同时兼了"最近失败横幅"的活（见 FailureNotice），
          同一条信息不占两行。 */}
      {panel.mapped_models.length > 0 && (
        <MappedModelNotice panel={panel} onOpen={openRecent} />
      )}
      {panel.failures.length > 0 && (
        <FailureNotice panel={panel} onOpen={openRecent} />
      )}
      {panel.unpriced_models.length > 0 && (
        <UnpricedNotice panel={panel} />
      )}

      {/* hero 是花费不是 token：打开面板想问的是"今天花了多少"，
          而且它和下面的「缓存省下」同一币单位，摆一起才读得出关系 */}
      <section className="card hero">
        <div className="hero-label">今日花费</div>
        <div className="hero-figure">
          <span className="hero-value tnum">{formatMoneyHero(today.cost)}</span>
          {/* 较昨日。配色沿用 .side-delta 的 up=暖 / down=绿：花得更多是暖色
              而不是绿色——这是用量面板，不是股价 */}
          <span
            className={`side-delta hero-delta ${
              costDelta !== null && costDelta >= 0 ? "up" : "down"
            }`}
          >
            {costDelta === null ? "昨日 0" : `昨日 ${formatDelta(costDelta)}`}
          </span>
        </div>
        <div className="hero-side">
          <SideStat label="总请求数" value={String(today.requests)} delta={reqDelta} />
          <SideStat
            label="消耗 Tokens"
            value={formatTokens(totalTokens)}
            delta={tokDelta}
          />
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

      {/* 省下的钱是 Claude Code 用户最关心、cc-switch 又没直接给的数字。
          和 hero 的今日花费同一个格式化函数：两者同一币单位，摆在一起
          才读得出"花了多少 / 其中多少是靠缓存省下的" */}
      {Number(today.cache_savings) > 0 && (
        <section className="card saved">
          <span className="saved-label">缓存省下</span>
          <span className="saved-value tnum">
            {formatMoneyHero(today.cache_savings)}
          </span>
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

/** 模型被换告警——护城河上首屏的那一条，也是唯一一条"没错但钱花错了"的提示。
 *
 *  为什么排第一：失败用户自己知道（客户端会报错）；模型被换不会。请求全部
 *  成功、日志里一条错都没有，而 token 和钱都按换后的模型算。cc-switch 的
 *  真实数据里 212 条 `claude-sonnet-4-6 → claude-opus-5.5` 全是子代理被
 *  供应商这么换掉的，在原界面上完全看不出来。
 *
 *  用 warn 不用 err：没有东西坏了，是计费口径和用户以为的不一样。
 *  做成红色会跟旁边的真失败抢注意力，而这两件事该有的紧迫程度不同。
 *
 *  次数用 `panel.mapped_total`（SQL 数的真总数），不是把列出来的这几条加起来——
 *  `mapped_models` 最多 5 条，加起来当总数会在映射组合多的时候说少了。 */
function MappedModelNotice({
  panel,
  onOpen,
}: {
  panel: Panel;
  onOpen: () => void;
}) {
  const top = panel.mapped_models[0];
  return (
    <div className="notice warn">
      <span className="notice-ic">⇄</span>
      <span className="notice-body">
        <b>今天 {panel.mapped_total} 次请求的模型被供应商换掉了</b>：客户端要{" "}
        <code>{top.client_model}</code>，实际按 <code>{top.response_model}</code>{" "}
        计费。请求全都成功，日志里一条错都没有。
      </span>
      <button className="btn ghost" onClick={onOpen}>
        看请求流
      </button>
    </div>
  );
}

/** 未定价：成本图会因此偏低，必须显眼。
 *
 *  token 总量用 `panel.unpriced_tokens`（SQL 数的），模型个数用
 *  `today.unpriced`（daily 表现在就有）——列表本身 `LIMIT 8`，把列出来的几条
 *  加起来当总数会在未定价模型多的时候少算。 */
function UnpricedNotice({ panel }: { panel: Panel }) {
  const models = panel.unpriced_models;
  const requests = panel.today.unpriced;
  const plural = requests > 1;
  return (
    <div className="notice warn">
      <span className="notice-ic">$</span>
      <span className="notice-body">
        <b>
          {models[0].model}
          {plural ? ` 等模型` : ""}
        </b>
        {plural ? " 没定价 —— " : " 没定价 —— "}
        {requests} 次请求 {formatTokens(panel.unpriced_tokens)} token 未计费，
        成本偏低
      </span>
      <code className="notice-cmd">tern price set</code>
    </div>
  );
}

/** 失败聚类。同时兼了"最近失败横幅"的活——点它展开请求流并滚过去。
 *
 *  原来首屏有两条失败提示：这条（今日失败 N 次，按原因聚类）和一条
 *  `LatestFailure` 横幅（最近那次的时间）。同一条信息占两行，而首屏最缺的
 *  就是地方。合成一条：聚类给全貌，最近时间给"刚才那下"，都能点到同一张表。
 *
 *  次数用 `today.failures`（daily 表现在就有），不是把聚类结果加起来——
 *  聚类 `LIMIT 6` 组，加起来会在失败原因多的时候说少了。 */
function FailureNotice({ panel, onOpen }: { panel: Panel; onOpen: () => void }) {
  const failures = panel.failures;
  const total = panel.today.failures;
  const top = failures[0];
  const kind = errorLabel(top.error_kind);
  const who = top.provider_id ?? "未路由";
  // 多个供应商时"等"才成立
  const providers = new Set(failures.map((f) => f.provider_id));
  const whoText = providers.size > 1 ? `${who} 等 ${providers.size} 个供应商` : who;
  const latest = panel.recent.find((r) => r.outcome === "failed");
  return (
    <button
      className="notice err clickable"
      onClick={onOpen}
      title="展开最近请求，看每条失败的原因"
    >
      <span className="notice-ic">!</span>
      <span className="notice-body">
        <b>{whoText}</b> 今日失败 {total} 次（最多：{kind} {top.count} 次）。
        失败不混入模型统计
        {latest && (
          <span className="notice-tail tnum">
            最近 {formatTime(latest.started_at_ms)}
          </span>
        )}
      </span>
      <span className="notice-more">查看 →</span>
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
                {/* 走了哪条路。显式 = 写明了 provider/model，兜底 = 裸名落到默认供应商。
                    没有这个标记的话，"推前缀"到底有没有生效只能靠猜 */}
                {r.route_kind === "explicit" && (
                  <span className="route-tag" title="请求写明了供应商前缀，按前缀路由">
                    显式
                  </span>
                )}
                {r.route_kind === "fallback" && (
                  <span
                    className="route-tag fallback"
                    title="没写前缀，落到了默认供应商。在模型名前加 供应商/ 可指定走哪家"
                  >
                    兜底
                  </span>
                )}
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
